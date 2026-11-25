use std::sync::{Arc, Mutex};

use axum::body::{Body, to_bytes};
use axum::http::{Method, Request, StatusCode, header};
use serde_json::{Value, json};
use tower::ServiceExt;

use super::router;
use crate::db::Db;
use crate::server::auth::{AuthStore, CSRF_HEADER_NAME, Role, SessionCredentials, Sessions};
use crate::server::jobs::JobRegistry;
use crate::server::state::{AppState, AppStateInner};

struct Actor {
    username: &'static str,
    role: Role,
    credentials: SessionCredentials,
}

fn state(temp: &tempfile::TempDir, require_auth: bool) -> AppState {
    let db_path = temp.path().join("workspace.db");
    Db::open(&db_path).unwrap();
    Arc::new(AppStateInner {
        auth: AuthStore::open(&db_path).unwrap(),
        db_path,
        jobs: JobRegistry::new(),
        uploads_dir: temp.path().join("uploads"),
        exports_dir: temp.path().join("exports"),
        lan_exposed: require_auth,
        require_auth,
        sessions: Sessions::new(),
        projection_trust: Mutex::new(None),
    })
}

fn add_actor(state: &AppState, username: &'static str, role: Role) -> Actor {
    state
        .auth
        .add_user(username, "contract-test-password", role)
        .unwrap();
    Actor {
        username,
        role,
        credentials: state.auth.create_session(username).unwrap(),
    }
}

fn json_request(method: Method, uri: &str, body: Value, actor: Option<&Actor>) -> Request<Body> {
    let mut request = Request::builder()
        .method(method.clone())
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    if let Some(actor) = actor {
        request.headers_mut().insert(
            header::COOKIE,
            format!(
                "bs_session={}; bs_csrf={}",
                actor.credentials.token, actor.credentials.csrf_token
            )
            .parse()
            .unwrap(),
        );
        if matches!(
            method,
            Method::POST | Method::PUT | Method::PATCH | Method::DELETE
        ) {
            request.headers_mut().insert(
                CSRF_HEADER_NAME,
                actor.credentials.csrf_token.parse().unwrap(),
            );
        }
    }
    request
}

async fn response_json(response: axum::response::Response) -> Value {
    serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap()).unwrap()
}

#[cfg(feature = "duckdb-olap")]
async fn wait_for_job(app: &axum::Router, id: u64, actor: Option<&Actor>) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let response = app
            .clone()
            .oneshot(json_request(
                Method::GET,
                &format!("/api/v2/jobs/{id}"),
                json!({}),
                actor,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let job = response_json(response).await;
        match job["status"].as_str() {
            Some("succeeded" | "failed" | "cancelled") => break,
            _ => {
                assert!(std::time::Instant::now() < deadline, "job {id} timed out");
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        }
    }
}

fn overview_body() -> Value {
    json!({ "query": {}, "limit": 10, "engine": "sqlite" })
}

fn section_body() -> Value {
    json!({
        "query": {},
        "scope": "companies",
        "hs_level": 10,
        "limit": 10,
        "engine": "sqlite"
    })
}

fn pivot_body() -> Value {
    json!({
        "query": {},
        "row_dim": "recipient",
        "col_dim": "year",
        "metric": "rows",
        "rows": 10,
        "cols": 10
    })
}

fn compare_body() -> Value {
    json!({
        "left": { "label": "Current", "query": {} },
        "right": { "label": "Previous", "query": { "filters": { "year": "2024" } } },
        "limit": 10,
        "engine": "sqlite"
    })
}

#[tokio::test]
async fn canonical_v2_routes_expose_only_the_documented_methods() {
    let temp = tempfile::tempdir().unwrap();
    let app = router(state(&temp, false));

    let cases = [
        (Method::GET, "/api/v2/me", json!({}), StatusCode::OK),
        (
            Method::POST,
            "/api/v2/analytics/overview",
            overview_body(),
            StatusCode::OK,
        ),
        (
            Method::POST,
            "/api/v2/analytics/section",
            section_body(),
            StatusCode::OK,
        ),
        (Method::POST, "/api/v2/pivot", pivot_body(), StatusCode::OK),
        (
            Method::POST,
            "/api/v2/compare",
            compare_body(),
            StatusCode::OK,
        ),
        (
            Method::POST,
            "/api/v2/export",
            json!({ "format": "csv" }),
            StatusCode::OK,
        ),
        (
            Method::GET,
            "/api/v2/admin/users",
            json!({}),
            StatusCode::OK,
        ),
    ];

    for (method, path, body, expected) in cases {
        let response = app
            .clone()
            .oneshot(json_request(method, path, body, None))
            .await
            .unwrap();
        assert_eq!(response.status(), expected, "{path}");
    }

    for (method, path) in [
        (Method::POST, "/api/v2/me"),
        (Method::GET, "/api/v2/analytics/overview"),
        (Method::GET, "/api/v2/analytics/section"),
        (Method::GET, "/api/v2/pivot"),
        (Method::GET, "/api/v2/compare"),
        (Method::GET, "/api/v2/export"),
        (Method::DELETE, "/api/v2/admin/users"),
        (Method::GET, "/api/v2/admin/duckdb/rebuild"),
    ] {
        let response = app
            .clone()
            .oneshot(json_request(method, path, json!({}), None))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED, "{path}");
    }

    let missing_scope = app
        .clone()
        .oneshot(json_request(
            Method::POST,
            "/api/v2/analytics/section",
            json!({ "query": {} }),
            None,
        ))
        .await
        .unwrap();
    assert_eq!(missing_scope.status(), StatusCode::UNPROCESSABLE_ENTITY);

    let missing_label = app
        .oneshot(json_request(
            Method::POST,
            "/api/v2/compare",
            json!({
                "left": { "label": " ", "query": {} },
                "right": { "label": "Previous", "query": {} }
            }),
            None,
        ))
        .await
        .unwrap();
    assert_eq!(missing_label.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn compare_contract_keeps_typed_queries_and_explicit_labels() {
    let temp = tempfile::tempdir().unwrap();
    let app = router(state(&temp, false));
    let response = app
        .oneshot(json_request(
            Method::POST,
            "/api/v2/compare",
            compare_body(),
            None,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["left"]["label"], "Current");
    assert_eq!(body["right"]["label"], "Previous");
    assert_eq!(body["left"]["query"]["text"], "");
    assert_eq!(body["right"]["query"]["filters"]["year"], "2024");
    assert_eq!(body["left"]["engine"], "sqlite");
    assert_eq!(body["right"]["engine"], "sqlite");
    assert!(body["left"]["data"]["overview"].is_object());
    assert!(body["right"]["data"]["overview"].is_object());
}

#[tokio::test]
async fn personal_mode_is_owner_for_every_canonical_contract_group() {
    let temp = tempfile::tempdir().unwrap();
    let state = state(&temp, false);
    state
        .auth
        .add_user("personal-owner", "personal-owner-password", Role::Owner)
        .unwrap();
    let app = router(state);

    for (method, path, body) in [
        (Method::GET, "/api/v2/me", json!({})),
        (Method::POST, "/api/v2/analytics/overview", overview_body()),
        (Method::POST, "/api/v2/analytics/section", section_body()),
        (Method::POST, "/api/v2/pivot", pivot_body()),
        (Method::POST, "/api/v2/compare", compare_body()),
        (Method::POST, "/api/v2/export", json!({ "format": "csv" })),
        (Method::GET, "/api/v2/admin/users", json!({})),
    ] {
        let response = app
            .clone()
            .oneshot(json_request(method, path, body, None))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{path}");
    }

    let create = app
        .clone()
        .oneshot(json_request(
            Method::POST,
            "/api/v2/admin/users",
            json!({
                "username": "personal-created",
                "password": "personal-password",
                "role": "viewer"
            }),
            None,
        ))
        .await
        .unwrap();
    assert_eq!(create.status(), StatusCode::OK);

    let delete = app
        .clone()
        .oneshot(json_request(
            Method::DELETE,
            "/api/v2/admin/users/personal-created",
            json!({}),
            None,
        ))
        .await
        .unwrap();
    assert_eq!(delete.status(), StatusCode::OK);

    let rebuild = app
        .oneshot(json_request(
            Method::POST,
            "/api/v2/admin/duckdb/rebuild",
            json!({}),
            None,
        ))
        .await
        .unwrap();
    #[cfg(feature = "duckdb-olap")]
    assert_eq!(rebuild.status(), StatusCode::OK);
    #[cfg(not(feature = "duckdb-olap"))]
    assert_eq!(rebuild.status(), StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn lan_roles_follow_the_v2_contract_permission_matrix() {
    let temp = tempfile::tempdir().unwrap();
    let state = state(&temp, true);
    let actors = [
        add_actor(&state, "owner", Role::Owner),
        add_actor(&state, "admin", Role::Admin),
        add_actor(&state, "editor", Role::Editor),
        add_actor(&state, "viewer", Role::Viewer),
    ];
    let app = router(state.clone());

    for actor in &actors {
        for (path, body) in [
            ("/api/v2/analytics/overview", overview_body()),
            ("/api/v2/analytics/section", section_body()),
            ("/api/v2/pivot", pivot_body()),
            ("/api/v2/compare", compare_body()),
            ("/api/v2/export", json!({ "format": "csv" })),
        ] {
            let response = app
                .clone()
                .oneshot(json_request(Method::POST, path, body, Some(actor)))
                .await
                .unwrap();
            assert_eq!(
                response.status(),
                StatusCode::OK,
                "{} must access {path}",
                actor.username
            );
        }

        let me = app
            .clone()
            .oneshot(json_request(
                Method::GET,
                "/api/v2/me",
                json!({}),
                Some(actor),
            ))
            .await
            .unwrap();
        assert_eq!(me.status(), StatusCode::OK, "{} GET /me", actor.username);
        let me = response_json(me).await;
        assert_eq!(me["user"]["username"], actor.username);
        assert_eq!(me["user"]["role"], actor.role.as_str());

        let users = app
            .clone()
            .oneshot(json_request(
                Method::GET,
                "/api/v2/admin/users",
                json!({}),
                Some(actor),
            ))
            .await
            .unwrap();
        let expected = if matches!(actor.role, Role::Owner | Role::Admin) {
            StatusCode::OK
        } else {
            StatusCode::FORBIDDEN
        };
        assert_eq!(users.status(), expected, "{} GET users", actor.username);

        let target = format!("created-by-{}", actor.username);
        let create = app
            .clone()
            .oneshot(json_request(
                Method::POST,
                "/api/v2/admin/users",
                json!({
                    "username": target,
                    "password": "created-password",
                    "role": "viewer"
                }),
                Some(actor),
            ))
            .await
            .unwrap();
        assert_eq!(create.status(), expected, "{} POST users", actor.username);

        if matches!(actor.role, Role::Owner | Role::Admin) {
            let delete = app
                .clone()
                .oneshot(json_request(
                    Method::DELETE,
                    &format!("/api/v2/admin/users/created-by-{}", actor.username),
                    json!({}),
                    Some(actor),
                ))
                .await
                .unwrap();
            assert_eq!(
                delete.status(),
                StatusCode::OK,
                "{} DELETE user",
                actor.username
            );
        } else {
            let delete = app
                .clone()
                .oneshot(json_request(
                    Method::DELETE,
                    "/api/v2/admin/users/owner",
                    json!({}),
                    Some(actor),
                ))
                .await
                .unwrap();
            assert_eq!(
                delete.status(),
                StatusCode::FORBIDDEN,
                "{} DELETE user",
                actor.username
            );
        }

        let rebuild = app
            .clone()
            .oneshot(json_request(
                Method::POST,
                "/api/v2/admin/duckdb/rebuild",
                json!({}),
                Some(actor),
            ))
            .await
            .unwrap();
        if matches!(actor.role, Role::Owner | Role::Admin) {
            #[cfg(feature = "duckdb-olap")]
            {
                assert_eq!(
                    rebuild.status(),
                    StatusCode::OK,
                    "{} rebuild",
                    actor.username
                );
                let job = response_json(rebuild).await;
                wait_for_job(&app, job["id"].as_u64().unwrap(), Some(actor)).await;
            }
            #[cfg(not(feature = "duckdb-olap"))]
            assert_eq!(
                rebuild.status(),
                StatusCode::UNPROCESSABLE_ENTITY,
                "{} rebuild",
                actor.username
            );
        } else {
            assert_eq!(
                rebuild.status(),
                StatusCode::FORBIDDEN,
                "{} rebuild",
                actor.username
            );
        }
    }
}

#[tokio::test]
async fn canonical_and_compatibility_aliases_return_matching_contract_shapes() {
    let temp = tempfile::tempdir().unwrap();
    let app = router(state(&temp, false));

    for (method, canonical, compatibility, body) in [
        (Method::GET, "/api/v2/me", "/api/v2/auth/me", json!({})),
        (
            Method::POST,
            "/api/v2/analytics/overview",
            "/api/v2/analytics",
            overview_body(),
        ),
        (
            Method::POST,
            "/api/v2/pivot",
            "/api/v2/analytics/pivot",
            pivot_body(),
        ),
        (
            Method::POST,
            "/api/v2/export",
            "/api/v2/exports",
            json!({ "format": "csv" }),
        ),
        (
            Method::GET,
            "/api/v2/admin/users",
            "/api/v2/auth/users",
            json!({}),
        ),
    ] {
        let canonical_response = app
            .clone()
            .oneshot(json_request(method.clone(), canonical, body.clone(), None))
            .await
            .unwrap();
        let compatibility_response = app
            .clone()
            .oneshot(json_request(method, compatibility, body, None))
            .await
            .unwrap();
        assert_eq!(canonical_response.status(), StatusCode::OK, "{canonical}");
        assert_eq!(
            compatibility_response.status(),
            StatusCode::OK,
            "{compatibility}"
        );
        let canonical_body = response_json(canonical_response).await;
        let compatibility_body = response_json(compatibility_response).await;
        assert_eq!(
            canonical_body.is_array(),
            compatibility_body.is_array(),
            "{canonical} vs {compatibility}"
        );
        assert_eq!(
            canonical_body.is_object(),
            compatibility_body.is_object(),
            "{canonical} vs {compatibility}"
        );
    }

    for path in ["/api/me", "/api/analytics/overview", "/api/pivot"] {
        let method = if path == "/api/me" {
            Method::GET
        } else {
            Method::POST
        };
        let body = if path.ends_with("overview") {
            overview_body()
        } else if path.ends_with("pivot") {
            pivot_body()
        } else {
            json!({})
        };
        let response = app
            .clone()
            .oneshot(json_request(method, path, body, None))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{path}");
    }
}

#[tokio::test]
async fn duckdb_rebuild_keeps_the_existing_database_olap_aliases() {
    let temp = tempfile::tempdir().unwrap();
    let app = router(state(&temp, false));
    for path in [
        "/api/v2/admin/duckdb/rebuild",
        "/api/v2/database/olap",
        "/api/database/olap",
    ] {
        let response = app
            .clone()
            .oneshot(json_request(Method::POST, path, json!({}), None))
            .await
            .unwrap();
        #[cfg(feature = "duckdb-olap")]
        {
            assert_eq!(response.status(), StatusCode::OK, "{path}");
            let job = response_json(response).await;
            wait_for_job(&app, job["id"].as_u64().unwrap(), None).await;
        }
        #[cfg(not(feature = "duckdb-olap"))]
        assert_eq!(
            response.status(),
            StatusCode::UNPROCESSABLE_ENTITY,
            "{path}"
        );
    }
}

#[tokio::test]
async fn completed_exports_publish_a_v2_download_url() {
    let temp = tempfile::tempdir().unwrap();
    let state = state(&temp, false);
    let app = router(state.clone());
    let response = app
        .clone()
        .oneshot(json_request(
            Method::POST,
            "/api/v2/export",
            json!({ "format": "csv", "filename": "contract" }),
            None,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let job = response_json(response).await;
    let id = job["id"].as_u64().unwrap();

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    loop {
        let response = app
            .clone()
            .oneshot(json_request(
                Method::GET,
                &format!("/api/v2/jobs/{id}"),
                json!({}),
                None,
            ))
            .await
            .unwrap();
        let job = response_json(response).await;
        match job["status"].as_str() {
            Some("succeeded") => {
                let url = job["result"]["download_url"].as_str().unwrap();
                assert!(url.starts_with("/api/v2/"), "{url}");
                let download = app
                    .clone()
                    .oneshot(json_request(Method::GET, url, json!({}), None))
                    .await
                    .unwrap();
                assert_eq!(download.status(), StatusCode::OK);
                assert_eq!(
                    download.headers().get(header::CONTENT_TYPE).unwrap(),
                    "text/csv; charset=utf-8"
                );
                break;
            }
            Some("failed") => panic!("export failed: {job}"),
            _ => {
                assert!(std::time::Instant::now() < deadline, "export timed out");
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        }
    }
}
