//! Safe request correlation, structured access logs, and bounded admission for
//! API reads that can occupy a database worker for a long time.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use axum::extract::{ConnectInfo, Request, State};
use axum::http::{HeaderName, HeaderValue, Method};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use super::auth::Identity;
use super::config::{
    DEFAULT_HEAVY_READ_QUEUE_WAIT, DEFAULT_MAX_HEAVY_READS, DEFAULT_MAX_QUEUED_HEAVY_READS,
};
use super::error::{ApiError, ApiErrorMetadata};

pub(crate) const X_REQUEST_ID: HeaderName = HeaderName::from_static("x-request-id");

static FALLBACK_REQUEST_COUNTER: AtomicU64 = AtomicU64::new(1);
static STDERR_LOG_LOCK: Mutex<()> = Mutex::new(());

const MAX_ACTIVE_HEAVY_READS: usize = 32;
const MAX_QUEUED_HEAVY_READS: usize = 256;
const MAX_HEAVY_READ_WAIT: Duration = Duration::from_secs(2);

pub(crate) type RequestLogSink = Arc<dyn Fn(&str) + Send + Sync>;

#[derive(Clone)]
pub(crate) struct HttpRequestControl {
    active_heavy_reads: Arc<Semaphore>,
    queued_heavy_reads: Arc<Semaphore>,
    heavy_read_wait: Duration,
    log_sink: RequestLogSink,
}

impl Default for HttpRequestControl {
    fn default() -> Self {
        Self::new(
            DEFAULT_MAX_HEAVY_READS,
            DEFAULT_MAX_QUEUED_HEAVY_READS,
            DEFAULT_HEAVY_READ_QUEUE_WAIT,
        )
    }
}

impl HttpRequestControl {
    pub(crate) fn new(active: usize, queued: usize, wait: Duration) -> Self {
        Self::with_log_sink(active, queued, wait, Arc::new(write_stderr_log))
    }

    pub(crate) fn with_log_sink(
        active: usize,
        queued: usize,
        wait: Duration,
        log_sink: RequestLogSink,
    ) -> Self {
        Self {
            active_heavy_reads: Arc::new(Semaphore::new(active.clamp(1, MAX_ACTIVE_HEAVY_READS))),
            queued_heavy_reads: Arc::new(Semaphore::new(queued.min(MAX_QUEUED_HEAVY_READS))),
            heavy_read_wait: wait.min(MAX_HEAVY_READ_WAIT),
            log_sink,
        }
    }

    async fn admit_heavy_read(&self) -> Result<OwnedSemaphorePermit, AdmissionError> {
        if let Ok(permit) = self.active_heavy_reads.clone().try_acquire_owned() {
            return Ok(permit);
        }

        let queue_slot = self
            .queued_heavy_reads
            .clone()
            .try_acquire_owned()
            .map_err(|_| AdmissionError)?;
        let permit = tokio::time::timeout(
            self.heavy_read_wait,
            self.active_heavy_reads.clone().acquire_owned(),
        )
        .await
        .map_err(|_| AdmissionError)?
        .map_err(|_| AdmissionError)?;
        drop(queue_slot);
        Ok(permit)
    }

    fn write_log(&self, event: &HttpRequestEvent<'_>) {
        if let Ok(line) = serde_json::to_string(event) {
            (self.log_sink)(&line);
        }
    }
}

#[derive(Clone)]
pub(crate) struct RequestContext {
    request_id: Arc<str>,
    identity: Arc<Mutex<Option<LoggedIdentity>>>,
}

impl RequestContext {
    fn new(request_id: String) -> Self {
        Self {
            request_id: Arc::from(request_id),
            identity: Arc::new(Mutex::new(None)),
        }
    }

    pub(crate) fn record_identity(&self, identity: &Identity) {
        if let Ok(mut current) = self.identity.lock() {
            *current = Some(LoggedIdentity {
                user_id: identity.user_id.clone(),
                role: identity.role.as_str(),
            });
        }
    }

    fn identity(&self) -> Option<LoggedIdentity> {
        self.identity
            .lock()
            .ok()
            .and_then(|identity| identity.clone())
    }
}

#[derive(Clone)]
struct LoggedIdentity {
    user_id: String,
    role: &'static str,
}

#[derive(Serialize)]
struct HttpRequestEvent<'a> {
    event: &'static str,
    request_id: &'a str,
    method: &'a str,
    route: &'a str,
    status: u16,
    duration_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    peer_ip: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    user_id: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    role: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error_code: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error_context: Option<&'a str>,
}

#[derive(Clone, Copy)]
struct AdmissionError;

pub(crate) async fn observe_requests(
    State(control): State<HttpRequestControl>,
    mut request: Request,
    next: Next,
) -> Response {
    let Some(route) = normalized_api_route(request.uri().path()) else {
        return next.run(request).await;
    };

    // Production wraps transport security with this same middleware. The API
    // router also carries it so direct router tests and embedded callers get the
    // contract. A shared context makes the inner pass deliberately idempotent.
    if request.extensions().get::<RequestContext>().is_some() {
        return next.run(request).await;
    }

    let context = RequestContext::new(generate_request_id());
    let method = normalized_method(request.method());
    let peer_ip = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|ConnectInfo(peer)| peer.ip().to_string());
    request.headers_mut().remove(&X_REQUEST_ID);
    request.extensions_mut().insert(context.clone());
    let started = Instant::now();

    let mut response = next.run(request).await;

    response.headers_mut().insert(
        X_REQUEST_ID,
        HeaderValue::from_str(context.request_id.as_ref())
            .expect("generated request id is a safe header value"),
    );
    let status = response.status().as_u16();
    let duration_ms = started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
    let identity = context.identity();
    let api_error = response.extensions().get::<ApiErrorMetadata>().copied();
    control.write_log(&HttpRequestEvent {
        event: "http_request",
        request_id: context.request_id.as_ref(),
        method,
        route,
        status,
        duration_ms,
        peer_ip: peer_ip.as_deref(),
        user_id: identity.as_ref().map(|identity| identity.user_id.as_str()),
        role: identity.as_ref().map(|identity| identity.role),
        error_code: api_error.map(|error| error.code),
        error_context: api_error.and_then(|error| error.context),
    });
    response
}

pub(crate) async fn admit_heavy_reads(
    State(control): State<HttpRequestControl>,
    request: Request,
    next: Next,
) -> Response {
    let Some(route) = normalized_api_route(request.uri().path()) else {
        return next.run(request).await;
    };
    if !is_heavy_read(request.method(), route) {
        return next.run(request).await;
    }

    match control.admit_heavy_read().await {
        Ok(_permit) => next.run(request).await,
        Err(_) => ApiError::server_busy().into_response(),
    }
}

pub(crate) fn normalized_api_route(path: &str) -> Option<&'static str> {
    let relative = path
        .strip_prefix("/api/v2")
        .filter(|suffix| suffix.is_empty() || suffix.starts_with('/'))
        .or_else(|| {
            path.strip_prefix("/api")
                .filter(|suffix| suffix.is_empty() || suffix.starts_with('/'))
        })?;
    let segments: Vec<&str> = relative
        .trim_matches('/')
        .split('/')
        .filter(|segment| !segment.is_empty())
        .take(4)
        .collect();

    let route = match segments.as_slice() {
        [] => "/api/v2",
        ["health"] => "/api/v2/health",
        ["status"] => "/api/v2/status",
        ["auth", "login"] => "/api/v2/auth/login",
        ["auth", "logout"] => "/api/v2/auth/logout",
        ["auth", "me"] => "/api/v2/auth/me",
        ["auth", "users"] => "/api/v2/auth/users",
        ["auth", "users", _] => "/api/v2/auth/users/{username}",
        ["me"] => "/api/v2/me",
        ["admin", "users"] => "/api/v2/admin/users",
        ["admin", "users", _] => "/api/v2/admin/users/{username}",
        ["admin", "duckdb", "rebuild"] => "/api/v2/admin/duckdb/rebuild",
        ["schema"] => "/api/v2/schema",
        ["schema", "fixed-values"] => "/api/v2/schema/fixed-values",
        ["columns", _, "semantic"] => "/api/v2/columns/{id}/semantic",
        ["search"] => "/api/v2/search",
        ["count"] => "/api/v2/count",
        ["records", _] => "/api/v2/records/{id}",
        ["analytics"] => "/api/v2/analytics",
        ["analytics", "overview"] => "/api/v2/analytics/overview",
        ["analytics", "section"] => "/api/v2/analytics/section",
        ["analytics", "pivot"] => "/api/v2/analytics/pivot",
        ["analytics", "undervaluation"] => "/api/v2/analytics/undervaluation",
        ["pivot"] => "/api/v2/pivot",
        ["compare"] => "/api/v2/compare",
        ["company", _] => "/api/v2/company/{edrpou}",
        ["engines"] => "/api/v2/engines",
        ["imports"] => "/api/v2/imports",
        ["imports", "peek"] => "/api/v2/imports/peek",
        ["imports", "log"] => "/api/v2/imports/log",
        ["exports"] => "/api/v2/exports",
        ["exports", _, "download"] => "/api/v2/exports/{id}/download",
        ["export"] => "/api/v2/export",
        ["export", _, "download"] => "/api/v2/export/{id}/download",
        ["jobs"] => "/api/v2/jobs",
        ["jobs", _] => "/api/v2/jobs/{id}",
        ["jobs", _, "cancel"] => "/api/v2/jobs/{id}/cancel",
        ["database", "stats"] => "/api/v2/database/stats",
        ["database", "optimize"] => "/api/v2/database/optimize",
        ["database", "compact"] => "/api/v2/database/compact",
        ["database", "reindex"] => "/api/v2/database/reindex",
        ["database", "olap"] => "/api/v2/database/olap",
        ["database", "clear"] => "/api/v2/database/clear",
        _ => "/api/v2/{unmatched}",
    };

    Some(route)
}

pub(crate) fn is_heavy_read(method: &Method, route: &str) -> bool {
    matches!(
        (method, route),
        (&Method::POST, "/api/v2/search")
            | (&Method::POST, "/api/v2/count")
            | (&Method::POST, "/api/v2/analytics")
            | (&Method::POST, "/api/v2/analytics/overview")
            | (&Method::POST, "/api/v2/analytics/section")
            | (&Method::POST, "/api/v2/analytics/pivot")
            | (&Method::POST, "/api/v2/analytics/undervaluation")
            | (&Method::POST, "/api/v2/pivot")
            | (&Method::POST, "/api/v2/compare")
            | (&Method::GET, "/api/v2/company/{edrpou}")
            | (&Method::GET, "/api/v2/records/{id}")
    )
}

fn normalized_method(method: &Method) -> &'static str {
    match *method {
        Method::GET => "GET",
        Method::POST => "POST",
        Method::PUT => "PUT",
        Method::PATCH => "PATCH",
        Method::DELETE => "DELETE",
        Method::HEAD => "HEAD",
        Method::OPTIONS => "OPTIONS",
        _ => "OTHER",
    }
}

fn generate_request_id() -> String {
    let mut bytes = [0_u8; 16];
    if getrandom::getrandom(&mut bytes).is_err() {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or_default();
        let counter = u128::from(FALLBACK_REQUEST_COUNTER.fetch_add(1, Ordering::Relaxed));
        bytes = timestamp.wrapping_add(counter).to_be_bytes();
    }

    let mut request_id = String::with_capacity(35);
    request_id.push_str("bs-");
    for byte in bytes {
        use std::fmt::Write as _;
        let _ = write!(request_id, "{byte:02x}");
    }
    request_id
}

fn write_stderr_log(line: &str) {
    let _guard = STDERR_LOG_LOCK.lock().ok();
    eprintln!("{line}");
}
