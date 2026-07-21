use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::{Body, to_bytes};
use axum::extract::{ConnectInfo, State};
use axum::http::{Method, Request, StatusCode, header};
use axum::middleware::from_fn_with_state;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::json;
use tokio::sync::Notify;
use tower::ServiceExt;

use super::observability::{
    HttpRequestControl, admit_heavy_reads, is_heavy_read, normalized_api_route, observe_requests,
};
use super::security::{TransportSecurity, enforce_transport_security};
use super::{api, auth, jobs, state};
use crate::db::Db;
use crate::server::error::ApiError;

#[derive(Clone)]
struct HeldRequest {
    entered: Arc<Notify>,
    release: Arc<Notify>,
}

async fn held_read(State(held): State<HeldRequest>) -> Json<serde_json::Value> {
    held.entered.notify_one();
    held.release.notified().await;
    Json(json!({ "ok": true }))
}

async fn light_read() -> Json<serde_json::Value> {
    Json(json!({ "ok": true }))
}

async fn internal_failure() -> Result<Json<serde_json::Value>, ApiError> {
    Err(ApiError::internal(
        "read record",
        "private-file.xlsx record-secret",
    ))
}

fn request(method: Method, uri: &str, body: Body, peer: SocketAddr) -> Request<Body> {
    let mut request = Request::builder()
        .method(method)
        .uri(uri)
        .body(body)
        .unwrap();
    request.extensions_mut().insert(ConnectInfo(peer));
    request
}

fn captured_control(
    active: usize,
    queued: usize,
    wait: Duration,
) -> (HttpRequestControl, Arc<Mutex<Vec<String>>>) {
    let logs = Arc::new(Mutex::new(Vec::new()));
    let captured = logs.clone();
    let control = HttpRequestControl::with_log_sink(
        active,
        queued,
        wait,
        Arc::new(move |line| captured.lock().unwrap().push(line.to_string())),
    );
    (control, logs)
}

fn held_router(control: HttpRequestControl, held: HeldRequest) -> Router {
    Router::new()
        .route("/api/search", post(held_read))
        .route("/api/v2/search", post(held_read))
        .route("/api/count", post(held_read))
        .route("/api/v2/count", post(held_read))
        .route("/api/health", get(light_read))
        .route("/api/v2/health", get(light_read))
        .route("/api/status", get(light_read))
        .route("/api/v2/status", get(light_read))
        .route("/api/auth/me", get(light_read))
        .route("/api/v2/auth/me", get(light_read))
        .route("/api/jobs/{id}", get(light_read))
        .route("/api/v2/jobs/{id}", get(light_read))
        .with_state(held)
        .layer(from_fn_with_state(control.clone(), admit_heavy_reads))
        .layer(from_fn_with_state(control, observe_requests))
}

#[test]
fn heavy_route_classification_is_identical_for_both_api_prefixes() {
    for (method, path) in [
        (Method::POST, "/api/search"),
        (Method::POST, "/api/v2/count"),
        (Method::POST, "/api/analytics"),
        (Method::POST, "/api/v2/analytics/overview"),
        (Method::POST, "/api/analytics/section"),
        (Method::POST, "/api/v2/analytics/pivot"),
        (Method::POST, "/api/pivot"),
        (Method::POST, "/api/v2/compare"),
        (Method::POST, "/api/analytics/undervaluation"),
        (Method::GET, "/api/v2/company/12345678"),
        (Method::GET, "/api/records/42"),
    ] {
        let route = normalized_api_route(path).expect("API route");
        assert!(is_heavy_read(&method, route), "{method} {path}");
    }

    for (method, path) in [
        (Method::GET, "/api/health"),
        (Method::GET, "/api/v2/status"),
        (Method::GET, "/api/auth/me"),
        (Method::GET, "/api/v2/me"),
        (Method::GET, "/api/admin/users"),
        (Method::GET, "/api/v2/jobs/42"),
    ] {
        let route = normalized_api_route(path).expect("API route");
        assert!(!is_heavy_read(&method, route), "{method} {path}");
    }
}

#[tokio::test]
async fn structured_request_log_contains_only_allowlisted_metadata() {
    let (control, logs) = captured_control(2, 4, Duration::from_millis(25));
    let app = Router::new()
        .route("/api/v2/search", post(light_read))
        .layer(from_fn_with_state(control.clone(), admit_heavy_reads))
        .layer(from_fn_with_state(control, observe_requests));
    let peer: SocketAddr = "192.0.2.41:45123".parse().unwrap();
    let mut request = request(
        Method::POST,
        "/api/v2/search?token=query-secret",
        Body::from(r#"{"query":"record-secret","filename":"private-file.xlsx"}"#),
        peer,
    );
    request
        .headers_mut()
        .insert(header::COOKIE, "bs_session=session-secret".parse().unwrap());
    request.headers_mut().insert(
        header::AUTHORIZATION,
        "Bearer credential-secret".parse().unwrap(),
    );
    request
        .headers_mut()
        .insert("x-forwarded-for", "203.0.113.99".parse().unwrap());
    request
        .headers_mut()
        .insert("x-request-id", "client-secret-id".parse().unwrap());

    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let logs = logs.lock().unwrap();
    assert_eq!(logs.len(), 1);
    let line = &logs[0];
    for secret in [
        "query-secret",
        "record-secret",
        "private-file.xlsx",
        "session-secret",
        "credential-secret",
        "203.0.113.99",
        "client-secret-id",
    ] {
        assert!(
            !line.contains(secret),
            "request log leaked {secret}: {line}"
        );
    }
    let event: serde_json::Value = serde_json::from_str(line).unwrap();
    assert_eq!(event["event"], "http_request");
    assert_eq!(event["method"], "POST");
    assert_eq!(event["route"], "/api/v2/search");
    assert_eq!(event["status"], 200);
    assert_eq!(event["peer_ip"], "192.0.2.41");
    assert!(event["request_id"].as_str().unwrap().starts_with("bs-"));
    assert!(event["duration_ms"].is_number());
}

#[tokio::test]
async fn internal_error_logs_keep_context_but_never_raw_error_detail() {
    let (control, logs) = captured_control(2, 4, Duration::from_millis(25));
    let app = Router::new()
        .route("/api/v2/records/{id}", get(internal_failure))
        .layer(from_fn_with_state(control.clone(), admit_heavy_reads))
        .layer(from_fn_with_state(control, observe_requests));
    let peer: SocketAddr = "192.0.2.45:45567".parse().unwrap();

    let response = app
        .oneshot(request(
            Method::GET,
            "/api/v2/records/99",
            Body::empty(),
            peer,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let logs = logs.lock().unwrap();
    assert_eq!(logs.len(), 1);
    assert!(!logs[0].contains("private-file.xlsx"));
    assert!(!logs[0].contains("record-secret"));
    let event: serde_json::Value = serde_json::from_str(&logs[0]).unwrap();
    assert_eq!(event["error_code"], "internal");
    assert_eq!(event["error_context"], "read record");
}

#[tokio::test]
async fn heavy_reads_overload_cleanly_while_health_and_job_progress_stay_responsive() {
    let (control, _logs) = captured_control(1, 1, Duration::from_millis(30));
    let held = HeldRequest {
        entered: Arc::new(Notify::new()),
        release: Arc::new(Notify::new()),
    };
    let app = held_router(control, held.clone());
    let peer: SocketAddr = "192.0.2.42:45234".parse().unwrap();

    let first_app = app.clone();
    let first = tokio::spawn(async move {
        first_app
            .oneshot(request(Method::POST, "/api/search", Body::empty(), peer))
            .await
            .unwrap()
    });
    tokio::time::timeout(Duration::from_secs(1), held.entered.notified())
        .await
        .expect("the first heavy request should acquire admission");

    for path in [
        "/api/v2/health",
        "/api/status",
        "/api/v2/auth/me",
        "/api/jobs/7",
    ] {
        let response = tokio::time::timeout(
            Duration::from_millis(100),
            app.clone()
                .oneshot(request(Method::GET, path, Body::empty(), peer)),
        )
        .await
        .expect("light endpoints must bypass the heavy-read gate")
        .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "path: {path}");
    }

    let overloaded = app
        .clone()
        .oneshot(request(Method::POST, "/api/v2/count", Body::empty(), peer))
        .await
        .unwrap();
    assert_eq!(overloaded.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(overloaded.headers()[header::RETRY_AFTER], "1");
    assert!(overloaded.headers().contains_key("x-request-id"));
    let body = to_bytes(overloaded.into_body(), usize::MAX).await.unwrap();
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&body).unwrap(),
        json!({
            "error": {
                "code": "server_busy",
                "message": "The server is busy. Try again shortly."
            }
        })
    );

    held.release.notify_one();
    assert_eq!(first.await.unwrap().status(), StatusCode::OK);
}

#[tokio::test]
async fn transport_rejections_still_receive_a_request_id_and_structured_log() {
    let (control, logs) = captured_control(1, 1, Duration::from_millis(20));
    let app = Router::new()
        .route("/api/v2/health", get(light_read))
        .layer(from_fn_with_state(
            TransportSecurity::new("127.0.0.1".parse().unwrap(), 7833),
            enforce_transport_security,
        ))
        .layer(from_fn_with_state(control, observe_requests));
    let peer: SocketAddr = "192.0.2.43:45345".parse().unwrap();
    let mut request = request(Method::GET, "/api/v2/health", Body::empty(), peer);
    request
        .headers_mut()
        .insert(header::HOST, "attacker.example:7833".parse().unwrap());

    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::MISDIRECTED_REQUEST);
    assert!(response.headers().contains_key("x-request-id"));
    let logs = logs.lock().unwrap();
    assert_eq!(logs.len(), 1);
    let event: serde_json::Value = serde_json::from_str(&logs[0]).unwrap();
    assert_eq!(event["route"], "/api/v2/health");
    assert_eq!(event["status"], 421);
}

#[tokio::test]
async fn transport_rejection_takes_precedence_over_heavy_read_admission() {
    let (control, _logs) = captured_control(1, 0, Duration::ZERO);
    let held = HeldRequest {
        entered: Arc::new(Notify::new()),
        release: Arc::new(Notify::new()),
    };
    let app = Router::new()
        .route("/api/v2/search", post(held_read))
        .route("/api/v2/count", post(held_read))
        .with_state(held.clone())
        .layer(from_fn_with_state(control.clone(), admit_heavy_reads))
        .layer(from_fn_with_state(
            TransportSecurity::new("127.0.0.1".parse().unwrap(), 7833),
            enforce_transport_security,
        ))
        .layer(from_fn_with_state(control, observe_requests));
    let peer: SocketAddr = "192.0.2.44:45457".parse().unwrap();

    let first_app = app.clone();
    let first = tokio::spawn(async move {
        let mut request = request(Method::POST, "/api/v2/search", Body::empty(), peer);
        request
            .headers_mut()
            .insert(header::HOST, "127.0.0.1:7833".parse().unwrap());
        first_app.oneshot(request).await.unwrap()
    });
    tokio::time::timeout(Duration::from_secs(1), held.entered.notified())
        .await
        .expect("the valid heavy request should hold the admission slot");

    let mut rejected = request(Method::POST, "/api/v2/count", Body::empty(), peer);
    rejected
        .headers_mut()
        .insert(header::HOST, "attacker.example:7833".parse().unwrap());
    let response = app.clone().oneshot(rejected).await.unwrap();
    assert_eq!(response.status(), StatusCode::MISDIRECTED_REQUEST);
    assert!(response.headers().contains_key("x-request-id"));

    held.release.notify_one();
    assert_eq!(first.await.unwrap().status(), StatusCode::OK);
}

#[tokio::test]
async fn authenticated_identity_is_logged_only_after_server_authentication() {
    let temp = tempfile::tempdir().unwrap();
    let db_path = temp.path().join("observed.db");
    Db::open(&db_path).unwrap();
    let state = Arc::new(state::AppStateInner {
        auth: auth::AuthStore::open(&db_path).unwrap(),
        db_path,
        jobs: jobs::JobRegistry::new(),
        uploads_dir: temp.path().join("uploads"),
        exports_dir: temp.path().join("exports"),
        lan_exposed: false,
        require_auth: false,
        sessions: auth::Sessions::new(),
        projection_trust: Mutex::new(None),
    });
    let (control, logs) = captured_control(1, 1, Duration::from_millis(20));
    let app = api::router_with_control(state, control);
    let peer: SocketAddr = "127.0.0.1:45456".parse().unwrap();

    let response = app
        .oneshot(request(Method::GET, "/api/v2/health", Body::empty(), peer))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let logs = logs.lock().unwrap();
    assert_eq!(logs.len(), 1);
    let event: serde_json::Value = serde_json::from_str(&logs[0]).unwrap();
    assert_eq!(event["user_id"], "local-owner");
    assert_eq!(event["role"], "owner");
}
