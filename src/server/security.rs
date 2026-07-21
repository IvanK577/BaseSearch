//! Transport-level protections for the local and LAN browser workspace.
//!
//! These checks run before authentication and cover both API routes and static
//! assets. They prevent a browser from reaching the local server through an
//! attacker-controlled hostname and reject cross-origin state changes.

use std::net::{IpAddr, Ipv6Addr};

use axum::Json;
use axum::extract::{Request, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, Uri, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use super::network::{is_loopback_ipv4, is_trusted_lan_ipv4};

pub const CONTENT_SECURITY_POLICY: &str = "default-src 'self'; base-uri 'none'; object-src 'none'; frame-ancestors 'none'; form-action 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; font-src 'self'; connect-src 'self'; worker-src 'self'; manifest-src 'self'";
pub const PERMISSIONS_POLICY: &str = "accelerometer=(), autoplay=(), camera=(), display-capture=(), geolocation=(), gyroscope=(), microphone=(), payment=(), usb=()";

const SEC_FETCH_SITE: HeaderName = HeaderName::from_static("sec-fetch-site");
const PERMISSIONS_POLICY_HEADER: HeaderName = HeaderName::from_static("permissions-policy");

#[derive(Clone, Debug)]
pub struct TransportSecurity {
    bind_host: IpAddr,
    port: u16,
}

impl TransportSecurity {
    pub fn new(bind_host: IpAddr, port: u16) -> Self {
        Self { bind_host, port }
    }

    /// Validates the request authority and browser fetch context before any
    /// route, authentication, or static-asset handler sees the request.
    pub fn validate(
        &self,
        method: &Method,
        headers: &HeaderMap,
    ) -> Result<(), TransportSecurityError> {
        let request_authority = headers
            .get(header::HOST)
            .and_then(|value| value.to_str().ok())
            .and_then(parse_authority)
            .filter(|authority| self.is_approved_authority(authority))
            .ok_or_else(TransportSecurityError::unapproved_host)?;

        if !is_mutating(method) {
            return Ok(());
        }

        if let Some(fetch_site) = headers.get(&SEC_FETCH_SITE) {
            let fetch_site = fetch_site
                .to_str()
                .map_err(|_| TransportSecurityError::cross_site_request())?;
            if fetch_site.eq_ignore_ascii_case("cross-site") {
                return Err(TransportSecurityError::cross_site_request());
            }
        }

        if let Some(origin) = headers.get(header::ORIGIN) {
            let origin = origin
                .to_str()
                .ok()
                .and_then(parse_http_origin)
                .filter(|origin| origin == &request_authority)
                .ok_or_else(TransportSecurityError::foreign_origin)?;
            debug_assert_eq!(origin, request_authority);
        }

        Ok(())
    }

    fn is_approved_authority(&self, authority: &Authority) -> bool {
        if authority.port != self.port {
            return false;
        }

        match &authority.host {
            Host::Name(name) => name == "localhost",
            Host::Ip(ip) => self.is_approved_ip(*ip),
        }
    }

    fn is_approved_ip(&self, host: IpAddr) -> bool {
        match host {
            IpAddr::V4(address) if is_loopback_ipv4(address) => true,
            IpAddr::V6(address) if address.is_loopback() => true,
            IpAddr::V4(address) => match self.bind_host {
                IpAddr::V4(bind) if bind.is_unspecified() => is_trusted_lan_ipv4(address),
                IpAddr::V4(bind) if is_trusted_lan_ipv4(bind) => address == bind,
                _ => false,
            },
            IpAddr::V6(_) => false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Authority {
    host: Host,
    port: u16,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Host {
    Name(String),
    Ip(IpAddr),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TransportSecurityError {
    status: StatusCode,
    code: &'static str,
    message: &'static str,
}

impl TransportSecurityError {
    fn unapproved_host() -> Self {
        Self {
            status: StatusCode::MISDIRECTED_REQUEST,
            code: "unapproved_host",
            message: "Host is not approved for this Base Search workspace.",
        }
    }

    fn foreign_origin() -> Self {
        Self {
            status: StatusCode::FORBIDDEN,
            code: "foreign_origin",
            message: "Origin does not match this Base Search workspace.",
        }
    }

    fn cross_site_request() -> Self {
        Self {
            status: StatusCode::FORBIDDEN,
            code: "cross_site_request",
            message: "Cross-site browser requests are not allowed.",
        }
    }

    pub fn status(&self) -> StatusCode {
        self.status
    }

    pub fn code(&self) -> &'static str {
        self.code
    }

    pub fn message(&self) -> &'static str {
        self.message
    }
}

impl IntoResponse for TransportSecurityError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(serde_json::json!({
                "error": self.code,
                "message": self.message,
            })),
        )
            .into_response()
    }
}

/// Axum middleware that enforces transport checks and adds browser hardening
/// headers to successful and rejected responses alike.
pub async fn enforce_transport_security(
    State(policy): State<TransportSecurity>,
    request: Request,
    next: Next,
) -> Response {
    let validation = policy.validate(request.method(), request.headers());
    let mut response = match validation {
        Ok(()) => next.run(request).await,
        Err(error) => error.into_response(),
    };
    apply_security_headers(response.headers_mut());
    response
}

pub fn apply_security_headers(headers: &mut HeaderMap) {
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(CONTENT_SECURITY_POLICY),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    headers.insert(
        PERMISSIONS_POLICY_HEADER,
        HeaderValue::from_static(PERMISSIONS_POLICY),
    );
    headers.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
}

fn is_mutating(method: &Method) -> bool {
    matches!(
        *method,
        Method::POST | Method::PUT | Method::PATCH | Method::DELETE
    )
}

fn parse_http_origin(raw: &str) -> Option<Authority> {
    let uri = raw.parse::<Uri>().ok()?;
    if uri.scheme_str() != Some("http") {
        return None;
    }
    if let Some(path) = uri.path_and_query()
        && path.as_str() != "/"
    {
        return None;
    }
    parse_authority(uri.authority()?.as_str())
}

fn parse_authority(raw: &str) -> Option<Authority> {
    if raw.is_empty()
        || raw.bytes().any(|byte| byte.is_ascii_whitespace())
        || raw.contains(['/', '\\', '@', '#', '?'])
    {
        return None;
    }

    let (host, port) = if let Some(bracketed) = raw.strip_prefix('[') {
        let close = bracketed.find(']')?;
        let host = bracketed[..close].parse::<Ipv6Addr>().ok()?;
        let port = bracketed[close + 1..].strip_prefix(':')?.parse().ok()?;
        (Host::Ip(IpAddr::V6(host)), port)
    } else {
        let (host, port) = raw.rsplit_once(':')?;
        if host.is_empty() || host.contains(':') {
            return None;
        }
        let host = match host.parse::<IpAddr>() {
            Ok(ip) => Host::Ip(ip),
            Err(_) => Host::Name(host.to_ascii_lowercase()),
        };
        (host, port.parse().ok()?)
    };

    Some(Authority { host, port })
}
