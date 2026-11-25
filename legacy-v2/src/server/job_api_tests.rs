use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use axum::body::{Body, to_bytes};
use axum::http::{Method, Request, StatusCode, header};
use serde_json::Value;
use tower::ServiceExt;

use super::api;
use super::auth::{
    AuthStore, COOKIE_NAME, CSRF_COOKIE_NAME, CSRF_HEADER_NAME, Identity, Role, Sessions,
};
use super::jobs::{
    JobKind, JobQueueLimits, JobRegistry, JobStatus, JobVisibility, spawn_job_for,
    spawn_job_for_with_input,
};
use super::state::{AppState, AppStateInner};
use crate::db::Db;

struct Account {
    identity: Identity,
    cookie: String,
    csrf: String,
}

fn account(auth: &AuthStore, username: &str, role: Role) -> Account {
    auth.add_user(username, "strong-password", role).unwrap();
    let credentials = auth.create_session(username).unwrap();
    let identity = auth.identify_session(&credentials.token).unwrap().unwrap();
    Account {
        identity,
        cookie: format!(
            "{COOKIE_NAME}={}; {CSRF_COOKIE_NAME}={}",
            credentials.token, credentials.csrf_token
        ),
        csrf: credentials.csrf_token,
    }
}

fn lan_state(temp: &tempfile::TempDir) -> (AppState, Account, Account, Account, Account) {
    let db_path = temp.path().join("workspace.db");
    Db::open(&db_path).unwrap();
    let auth = AuthStore::open(&db_path).unwrap();
    let _owner = account(&auth, "workspace-owner", Role::Owner);
    let editor_a = account(&auth, "editor-a", Role::Editor);
    let editor_b = account(&auth, "editor-b", Role::Editor);
    let admin = account(&auth, "admin", Role::Admin);
    let viewer = account(&auth, "viewer", Role::Viewer);
    let state = Arc::new(AppStateInner {
        db_path,
        jobs: JobRegistry::new(),
        uploads_dir: temp.path().join("uploads"),
        exports_dir: temp.path().join("exports"),
        lan_exposed: true,
        require_auth: true,
        auth,
        sessions: Sessions::new(),
        projection_trust: Mutex::new(None),
    });
    (state, editor_a, editor_b, admin, viewer)
}

fn request(method: Method, uri: &str, account: &Account) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header(header::COOKIE, &account.cookie)
        .header(CSRF_HEADER_NAME, &account.csrf)
        .body(Body::empty())
        .unwrap()
}

#[tokio::test]
async fn jobs_api_filters_private_jobs_by_owner_and_keeps_workspace_jobs_visible() {
    let temp = tempfile::tempdir().unwrap();
    let (state, editor_a, editor_b, admin, _viewer) = lan_state(&temp);
    let private_a = spawn_job_for(
        &state.jobs,
        &editor_a.identity,
        JobKind::Export,
        JobVisibility::Private,
        "A private export",
        |handle| handle.succeed(None),
    )
    .unwrap();
    let private_b = spawn_job_for(
        &state.jobs,
        &editor_b.identity,
        JobKind::Export,
        JobVisibility::Private,
        "B private export",
        |handle| handle.succeed(None),
    )
    .unwrap();
    let workspace = spawn_job_for(
        &state.jobs,
        &admin.identity,
        JobKind::Optimize,
        JobVisibility::Workspace,
        "Shared maintenance",
        |handle| handle.succeed(None),
    )
    .unwrap();
    for id in [private_a.id, private_b.id, workspace.id] {
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while state.jobs.snapshot(id).unwrap().status != JobStatus::Succeeded {
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    let response = api::router(state)
        .oneshot(request(Method::GET, "/api/jobs", &editor_a))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap()).unwrap();
    let ids: Vec<u64> = body["jobs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|job| job["id"].as_u64().unwrap())
        .collect();
    assert!(ids.contains(&private_a.id));
    assert!(ids.contains(&workspace.id));
    assert!(!ids.contains(&private_b.id));
}

#[tokio::test]
async fn jobs_api_rejects_cancelling_another_users_job() {
    let temp = tempfile::tempdir().unwrap();
    let (state, editor_a, editor_b, _admin, _viewer) = lan_state(&temp);
    let (release_tx, release_rx) = mpsc::channel();
    let owned = spawn_job_for(
        &state.jobs,
        &editor_a.identity,
        JobKind::Export,
        JobVisibility::Private,
        "A running export",
        move |handle| {
            let _ = release_rx.recv();
            if handle.is_cancelled() {
                handle.mark_cancelled();
            } else {
                handle.succeed(None);
            }
        },
    )
    .unwrap();

    let response = api::router(state)
        .oneshot(request(
            Method::POST,
            &format!("/api/jobs/{}/cancel", owned.id),
            &editor_b,
        ))
        .await
        .unwrap();
    release_tx.send(()).unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn viewer_job_api_redacts_shared_import_payloads() {
    let temp = tempfile::tempdir().unwrap();
    let (state, editor, _editor_b, _admin, viewer) = lan_state(&temp);
    let secret = "sensitive-customer.xlsx";
    let import = spawn_job_for_with_input(
        &state.jobs,
        &editor.identity,
        JobKind::Import,
        JobVisibility::Workspace,
        format!("Importing {secret}"),
        Some(serde_json::json!({
            "files": [secret],
            "selected_sheets": ["Private"],
            "artifact_token": "private-token"
        })),
        move |handle| {
            handle.set_message(format!("Reading {secret}"));
            handle.succeed(Some(serde_json::json!({
                "files": [{"file_name": secret, "error": "private error"}]
            })));
        },
    )
    .unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while state.jobs.snapshot(import.id).unwrap().status != JobStatus::Succeeded {
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    }

    let response = api::router(state)
        .oneshot(request(
            Method::GET,
            &format!("/api/jobs/{}", import.id),
            &viewer,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let text = String::from_utf8(body.to_vec()).unwrap();
    assert!(!text.contains(secret));
    assert!(!text.contains("Private"));
    assert!(!text.contains("private-token"));
    assert!(!text.contains("private error"));
    let value: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(value["status"], "succeeded");
    assert!(value.get("input").is_none());
    assert!(value.get("result").is_none());
}

#[tokio::test]
async fn job_creating_endpoints_assign_owner_visibility_and_enforce_roles() {
    let temp = tempfile::tempdir().unwrap();
    let (state, editor, _editor_b, admin, viewer) = lan_state(&temp);
    let app = api::router(state.clone());

    let export_request = Request::builder()
        .method(Method::POST)
        .uri("/api/exports")
        .header(header::COOKIE, &editor.cookie)
        .header(CSRF_HEADER_NAME, &editor.csrf)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(r#"{"format":"csv","filename":"owned"}"#))
        .unwrap();
    let export_response = app.clone().oneshot(export_request).await.unwrap();
    assert_eq!(export_response.status(), StatusCode::OK);
    let export: Value = serde_json::from_slice(
        &to_bytes(export_response.into_body(), usize::MAX)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(export["owner_user_id"], editor.identity.user_id);
    assert_eq!(export["visibility"], "private");

    let boundary = "owned-import-boundary";
    let import_body = format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"files\"; filename=\"rows.csv\"\r\nContent-Type: text/csv\r\n\r\nname,value\r\nrow,1\r\n--{boundary}--\r\n"
    );
    let import_request = Request::builder()
        .method(Method::POST)
        .uri("/api/imports")
        .header(header::COOKIE, &editor.cookie)
        .header(CSRF_HEADER_NAME, &editor.csrf)
        .header(
            header::CONTENT_TYPE,
            format!("multipart/form-data; boundary={boundary}"),
        )
        .body(Body::from(import_body))
        .unwrap();
    let import_response = app.clone().oneshot(import_request).await.unwrap();
    assert_eq!(import_response.status(), StatusCode::OK);
    let import: Value = serde_json::from_slice(
        &to_bytes(import_response.into_body(), usize::MAX)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(import["owner_user_id"], editor.identity.user_id);
    assert_eq!(import["visibility"], "workspace");

    let maintenance_response = app
        .clone()
        .oneshot(request(Method::POST, "/api/database/optimize", &admin))
        .await
        .unwrap();
    assert_eq!(maintenance_response.status(), StatusCode::OK);
    let maintenance: Value = serde_json::from_slice(
        &to_bytes(maintenance_response.into_body(), usize::MAX)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(maintenance["owner_user_id"], admin.identity.user_id);
    assert_eq!(maintenance["visibility"], "workspace");

    let viewer_import = Request::builder()
        .method(Method::POST)
        .uri("/api/imports")
        .header(header::COOKIE, &viewer.cookie)
        .header(CSRF_HEADER_NAME, &viewer.csrf)
        .header(
            header::CONTENT_TYPE,
            "multipart/form-data; boundary=viewer-denied",
        )
        .body(Body::empty())
        .unwrap();
    let viewer_response = app.oneshot(viewer_import).await.unwrap();
    assert_eq!(viewer_response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn export_api_validates_fields_and_persists_a_context_snapshot() {
    let temp = tempfile::tempdir().unwrap();
    let (state, editor, _editor_b, _admin, _viewer) = lan_state(&temp);
    let app = api::router(state.clone());

    let invalid = Request::builder()
        .method(Method::POST)
        .uri("/api/exports")
        .header(header::COOKIE, &editor.cookie)
        .header(CSRF_HEADER_NAME, &editor.csrf)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(r#"{"format":"csv","field_ids":[]}"#))
        .unwrap();
    let invalid_response = app.clone().oneshot(invalid).await.unwrap();
    assert_eq!(invalid_response.status(), StatusCode::BAD_REQUEST);

    let request = Request::builder()
        .method(Method::POST)
        .uri("/api/exports")
        .header(header::COOKIE, &editor.cookie)
        .header(CSRF_HEADER_NAME, &editor.csrf)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            r#"{
                "format":"csv",
                "filename":"context",
                "field_ids":["recipient","sender"],
                "sort":{"field":"sender","descending":true},
                "query":{"record_scope":"occurrences"}
            }"#,
        ))
        .unwrap();
    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let queued: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap()).unwrap();
    let id = queued["id"].as_u64().unwrap();

    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    let snapshot = loop {
        let snapshot = state.jobs.snapshot(id).unwrap();
        if snapshot.status == JobStatus::Succeeded {
            break snapshot;
        }
        assert_ne!(snapshot.status, JobStatus::Failed, "{:?}", snapshot.error);
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(10));
    };
    let result = snapshot.result.unwrap();
    assert_eq!(result["count"], 0);
    assert_eq!(
        result["field_ids"],
        serde_json::json!(["recipient", "sender"])
    );
    assert_eq!(result["fields"][0]["id"], "recipient");
    assert_eq!(result["fields"][1]["id"], "sender");
    assert_eq!(result["sort"]["field"], "sender");
    assert_eq!(result["sort"]["descending"], true);
    assert_eq!(result["record_scope"], "occurrences");
    assert_eq!(result["query"]["record_scope"], "occurrences");
    assert_eq!(result["format"], "csv");
}

#[tokio::test]
async fn personal_mode_creates_and_lists_jobs_without_an_account() {
    let temp = tempfile::tempdir().unwrap();
    let db_path = temp.path().join("workspace.db");
    Db::open(&db_path).unwrap();
    let state = Arc::new(AppStateInner {
        auth: AuthStore::open(&db_path).unwrap(),
        db_path,
        jobs: JobRegistry::new(),
        uploads_dir: temp.path().join("uploads"),
        exports_dir: temp.path().join("exports"),
        lan_exposed: false,
        require_auth: false,
        sessions: Sessions::new(),
        projection_trust: Mutex::new(None),
    });
    let app = api::router(state);
    let create = Request::builder()
        .method(Method::POST)
        .uri("/api/exports")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(r#"{"format":"csv","filename":"personal"}"#))
        .unwrap();
    let response = app.clone().oneshot(create).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let job: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert_eq!(job["owner_user_id"], "local-owner");
    assert_eq!(job["visibility"], "private");

    let list = app
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/api/jobs")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(list.status(), StatusCode::OK);
    let body: Value =
        serde_json::from_slice(&to_bytes(list.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert_eq!(body["jobs"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn full_export_queue_is_rejected_before_an_export_directory_is_created() {
    let temp = tempfile::tempdir().unwrap();
    let db_path = temp.path().join("workspace.db");
    Db::open(&db_path).unwrap();
    let jobs = JobRegistry::with_limits(JobQueueLimits {
        workspace_pending: 1,
        per_user_pending: 1,
        concurrent_reads: 1,
        concurrent_previews: 1,
    });
    let _held = jobs
        .reserve_for(&Identity::local_owner(), JobKind::Export)
        .unwrap();
    let exports_dir = temp.path().join("exports");
    let state = Arc::new(AppStateInner {
        auth: AuthStore::open(&db_path).unwrap(),
        db_path,
        jobs,
        uploads_dir: temp.path().join("uploads"),
        exports_dir: exports_dir.clone(),
        lan_exposed: false,
        require_auth: false,
        sessions: Sessions::new(),
        projection_trust: Mutex::new(None),
    });
    let request = Request::builder()
        .method(Method::POST)
        .uri("/api/exports")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(r#"{"format":"csv","filename":"rejected"}"#))
        .unwrap();

    let response = api::router(state).oneshot(request).await.unwrap();

    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    assert!(
        !exports_dir.exists() || std::fs::read_dir(&exports_dir).unwrap().next().is_none(),
        "queue rejection must not leave export artifacts"
    );
}
