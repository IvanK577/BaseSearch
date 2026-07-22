use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};

use axum::body::{Body, to_bytes};
use axum::extract::ConnectInfo;
use axum::http::{Method, Request, StatusCode, header};
use tower::ServiceExt;

use super::api;
use super::auth::{AuthStore, CSRF_HEADER_NAME, Permission, Role, Sessions};
use super::jobs::JobRegistry;
use super::state::{AppState, AppStateInner};
use crate::db::Db;

fn lan_state(temp: &tempfile::TempDir) -> AppState {
    let db_path = temp.path().join("workspace.db");
    Db::open(&db_path).unwrap();
    let auth = AuthStore::open(&db_path).unwrap();
    auth.add_user("owner", "strong-password", Role::Owner)
        .unwrap();
    Arc::new(AppStateInner {
        db_path,
        jobs: JobRegistry::new(),
        uploads_dir: temp.path().join("uploads"),
        exports_dir: temp.path().join("exports"),
        lan_exposed: true,
        require_auth: true,
        auth,
        sessions: Sessions::new(),
        projection_trust: Mutex::new(None),
    })
}

fn personal_state(temp: &tempfile::TempDir) -> AppState {
    let db_path = temp.path().join("personal.db");
    Db::open(&db_path).unwrap();
    Arc::new(AppStateInner {
        auth: AuthStore::open(&db_path).unwrap(),
        db_path,
        jobs: JobRegistry::new(),
        uploads_dir: temp.path().join("uploads"),
        exports_dir: temp.path().join("exports"),
        lan_exposed: false,
        require_auth: false,
        sessions: Sessions::new(),
        projection_trust: Mutex::new(None),
    })
}

fn request_with_peer(method: Method, uri: &str, body: Body, peer: SocketAddr) -> Request<Body> {
    let mut request = Request::builder()
        .method(method)
        .uri(uri)
        .body(body)
        .unwrap();
    request.extensions_mut().insert(ConnectInfo(peer));
    request
}

fn assert_server_request_id(response: &axum::response::Response) -> String {
    let request_id = response
        .headers()
        .get("x-request-id")
        .expect("every API response must include a request id")
        .to_str()
        .expect("request ids must be valid HTTP header values")
        .to_string();
    assert_eq!(request_id.len(), 35);
    assert!(request_id.starts_with("bs-"));
    assert!(request_id[3..].bytes().all(|byte| byte.is_ascii_hexdigit()));
    request_id
}

fn json_contains_text(value: &serde_json::Value, needle: &str) -> bool {
    match value {
        serde_json::Value::String(text) => text.contains(needle),
        serde_json::Value::Array(values) => {
            values.iter().any(|value| json_contains_text(value, needle))
        }
        serde_json::Value::Object(values) => values
            .values()
            .any(|value| json_contains_text(value, needle)),
        _ => false,
    }
}

fn login_request(
    uri: &str,
    username: &str,
    password: &str,
    peer: SocketAddr,
    forwarded_for: &str,
) -> Request<Body> {
    let body = serde_json::json!({
        "username": username,
        "password": password,
    })
    .to_string();
    let mut request = request_with_peer(Method::POST, uri, Body::from(body), peer);
    request
        .headers_mut()
        .insert(header::CONTENT_TYPE, "application/json".parse().unwrap());
    request
        .headers_mut()
        .insert("x-forwarded-for", forwarded_for.parse().unwrap());
    request
}

#[test]
fn role_permissions_remain_least_privilege_and_explicit() {
    use Permission::*;

    let permissions = [
        Read,
        Analyze,
        Export,
        Import,
        ManageMappings,
        ManageSavedQueries,
        ManageUsers,
        ManageNetwork,
        MaintainDatabase,
        ManageOwners,
    ];
    let editor = [
        Read,
        Analyze,
        Export,
        Import,
        ManageMappings,
        ManageSavedQueries,
    ];
    let viewer = [Read, Analyze, Export];

    for permission in permissions {
        assert!(Role::Owner.allows(permission));
        assert_eq!(
            Role::Admin.allows(permission),
            permission != ManageOwners,
            "admin: {permission:?}"
        );
        assert_eq!(
            Role::Editor.allows(permission),
            editor.contains(&permission),
            "editor: {permission:?}"
        );
        assert_eq!(
            Role::Viewer.allows(permission),
            viewer.contains(&permission),
            "viewer: {permission:?}"
        );
    }
}

#[tokio::test]
async fn canonical_and_compatibility_prefixes_share_public_health_route() {
    let temp = tempfile::tempdir().unwrap();
    let app = api::router(lan_state(&temp));

    let mut request_ids = Vec::new();
    for path in ["/api/health", "/api/v2/health"] {
        let mut request = request_with_peer(
            Method::GET,
            path,
            Body::empty(),
            SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 41000),
        );
        request
            .headers_mut()
            .insert("x-request-id", "client-controlled-id".parse().unwrap());
        let response = app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK, "path: {path}");
        let request_id = assert_server_request_id(&response);
        assert_ne!(request_id, "client-controlled-id");
        request_ids.push(request_id);
    }
    assert_ne!(request_ids[0], request_ids[1]);
}

#[tokio::test]
async fn v2_keeps_personal_loopback_mode_password_free() {
    let temp = tempfile::tempdir().unwrap();
    let app = api::router(personal_state(&temp));
    let request = Request::builder()
        .method(Method::GET)
        .uri("/api/v2/auth/me")
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body: serde_json::Value =
        serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert_eq!(body["required"], false);
    assert_eq!(body["authenticated"], true);
    assert_eq!(body["user"]["role"], "owner");
}

#[tokio::test]
async fn local_paths_are_visible_only_to_personal_owner_or_privileged_lan_roles() {
    let personal_temp = tempfile::tempdir().unwrap();
    let personal = personal_state(&personal_temp);
    let personal_path = personal.db_path.display().to_string();
    let personal_response = api::router(personal)
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/api/v2/status")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(personal_response.status(), StatusCode::OK);
    let personal_body: serde_json::Value = serde_json::from_slice(
        &to_bytes(personal_response.into_body(), usize::MAX)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(personal_body["db_path"], personal_path);

    let lan_temp = tempfile::tempdir().unwrap();
    let state = lan_state(&lan_temp);
    for (username, role) in [
        ("admin", Role::Admin),
        ("editor", Role::Editor),
        ("viewer", Role::Viewer),
    ] {
        state
            .auth
            .add_user(username, "strong-password", role)
            .unwrap();
    }
    #[cfg(feature = "duckdb-olap")]
    crate::duckdb_olap::build_projection_atomic(
        &state.db_path,
        &crate::duckdb_olap::default_projection_path(&state.db_path),
    )
    .unwrap();
    let app = api::router(state.clone());
    let absolute_db_path = state.db_path.display().to_string();

    for (username, paths_visible) in [
        ("owner", true),
        ("admin", true),
        ("editor", false),
        ("viewer", false),
    ] {
        let credentials = state.auth.create_session(username).unwrap();
        for endpoint in ["/api/v2/status", "/api/v2/engines"] {
            let mut request = Request::builder()
                .method(Method::GET)
                .uri(endpoint)
                .body(Body::empty())
                .unwrap();
            request.headers_mut().insert(
                header::COOKIE,
                format!("bs_session={}", credentials.token).parse().unwrap(),
            );
            let response = app.clone().oneshot(request).await.unwrap();
            assert_eq!(response.status(), StatusCode::OK, "{username}: {endpoint}");
            let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
            let body_text = String::from_utf8(body.to_vec()).unwrap();
            let body_json: serde_json::Value = serde_json::from_str(&body_text).unwrap();
            if endpoint.ends_with("/status") {
                assert_eq!(
                    body_json.get("db_path").is_some(),
                    paths_visible,
                    "{username}: {endpoint}: {body_text}"
                );
                assert_eq!(
                    body_json.get("db_path").and_then(serde_json::Value::as_str),
                    paths_visible.then_some(absolute_db_path.as_str()),
                    "{username}: {endpoint}: {body_text}"
                );
            }
            #[cfg(feature = "duckdb-olap")]
            if endpoint.ends_with("/engines") {
                assert_eq!(
                    body_json
                        .pointer("/projection/path")
                        .and_then(serde_json::Value::as_str)
                        .is_some(),
                    paths_visible,
                    "{username}: {endpoint}: {body_text}"
                );
            }
            if !paths_visible {
                assert!(
                    !json_contains_text(&body_json, lan_temp.path().to_string_lossy().as_ref()),
                    "{username} must not receive any absolute workspace path: {body_text}"
                );
            }
        }
    }
}

#[tokio::test]
async fn unknown_api_routes_never_fall_through_to_the_browser_shell() {
    let temp = tempfile::tempdir().unwrap();
    let app = api::router(personal_state(&temp));

    for path in ["/api/does-not-exist", "/api/v2/does-not-exist"] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri(path)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "path: {path}");
        assert_eq!(
            response.headers().get(header::CONTENT_TYPE).unwrap(),
            "application/json"
        );
        assert_server_request_id(&response);
    }
}

#[tokio::test]
async fn v2_routes_enforce_the_same_viewer_permissions_as_compatibility_routes() {
    let temp = tempfile::tempdir().unwrap();
    let state = lan_state(&temp);
    state
        .auth
        .add_user("viewer", "viewer-password", Role::Viewer)
        .unwrap();
    let credentials = state.auth.create_session("viewer").unwrap();
    let app = api::router(state);

    for path in ["/api/imports", "/api/v2/imports"] {
        let mut request = request_with_peer(
            Method::POST,
            path,
            Body::empty(),
            "192.0.2.10:42000".parse().unwrap(),
        );
        request.headers_mut().insert(
            header::COOKIE,
            format!(
                "bs_session={}; bs_csrf={}",
                credentials.token, credentials.csrf_token
            )
            .parse()
            .unwrap(),
        );
        request
            .headers_mut()
            .insert(CSRF_HEADER_NAME, credentials.csrf_token.parse().unwrap());
        let response = app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN, "path: {path}");
    }
}

#[tokio::test]
async fn login_backoff_uses_peer_ip_and_ignores_forwarded_headers() {
    let temp = tempfile::tempdir().unwrap();
    let app = api::router(lan_state(&temp));
    let peer: SocketAddr = "192.0.2.20:43000".parse().unwrap();

    for attempt in 0..5 {
        let response = app
            .clone()
            .oneshot(login_request(
                "/api/v2/auth/login",
                &format!("missing-{attempt}"),
                "wrong-password",
                peer,
                &format!("198.51.100.{}", attempt + 1),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    let response = app
        .oneshot(login_request(
            "/api/v2/auth/login",
            "another-missing-user",
            "wrong-password",
            peer,
            "203.0.113.99",
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    assert!(response.headers().contains_key(header::RETRY_AFTER));
}

#[tokio::test]
async fn login_failures_do_not_reveal_whether_the_username_exists() {
    let temp = tempfile::tempdir().unwrap();
    let app = api::router(lan_state(&temp));

    let existing = app
        .clone()
        .oneshot(login_request(
            "/api/auth/login",
            "owner",
            "wrong-password",
            "192.0.2.30:44000".parse().unwrap(),
            "203.0.113.30",
        ))
        .await
        .unwrap();
    let missing = app
        .oneshot(login_request(
            "/api/auth/login",
            "missing",
            "wrong-password",
            "192.0.2.31:44001".parse().unwrap(),
            "203.0.113.31",
        ))
        .await
        .unwrap();

    assert_eq!(existing.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(missing.status(), StatusCode::UNAUTHORIZED);
    let existing_body = to_bytes(existing.into_body(), usize::MAX).await.unwrap();
    let missing_body = to_bytes(missing.into_body(), usize::MAX).await.unwrap();
    assert_eq!(existing_body, missing_body);
}

#[tokio::test]
async fn user_mutation_reports_created_or_updated_role_without_implicit_owner_promotion() {
    let temp = tempfile::tempdir().unwrap();
    let state = lan_state(&temp);
    let credentials = state.auth.create_session("owner").unwrap();
    let app = api::router(state.clone());
    let peer: SocketAddr = "192.0.2.40:45000".parse().unwrap();

    for (requested_role, expected_role, expected_action) in [
        (Some("viewer"), "viewer", "created"),
        (Some("admin"), "admin", "updated"),
        (None, "admin", "updated"),
    ] {
        let mut payload = serde_json::json!({
            "username": "analyst",
            "password": "analyst-password",
        });
        if let Some(requested_role) = requested_role {
            payload["role"] = requested_role.into();
        }
        let body = payload.to_string();
        let mut request =
            request_with_peer(Method::POST, "/api/v2/admin/users", Body::from(body), peer);
        request
            .headers_mut()
            .insert(header::CONTENT_TYPE, "application/json".parse().unwrap());
        request.headers_mut().insert(
            header::COOKIE,
            format!(
                "bs_session={}; bs_csrf={}",
                credentials.token, credentials.csrf_token
            )
            .parse()
            .unwrap(),
        );
        request
            .headers_mut()
            .insert(CSRF_HEADER_NAME, credentials.csrf_token.parse().unwrap());

        let response = app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body: serde_json::Value =
            serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap())
                .unwrap();
        assert_eq!(body["role"], expected_role);
        assert_eq!(body["action"], expected_action);
        assert_eq!(
            state.auth.user_role("analyst").unwrap().unwrap().as_str(),
            expected_role
        );
    }

    let body = serde_json::json!({
        "username": "ambiguous-role",
        "password": "analyst-password",
        "role": "auditor",
    })
    .to_string();
    let mut request =
        request_with_peer(Method::POST, "/api/v2/admin/users", Body::from(body), peer);
    request
        .headers_mut()
        .insert(header::CONTENT_TYPE, "application/json".parse().unwrap());
    request.headers_mut().insert(
        header::COOKIE,
        format!(
            "bs_session={}; bs_csrf={}",
            credentials.token, credentials.csrf_token
        )
        .parse()
        .unwrap(),
    );
    request
        .headers_mut()
        .insert(CSRF_HEADER_NAME, credentials.csrf_token.parse().unwrap());
    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(state.auth.user_role("ambiguous-role").unwrap(), None);
}
