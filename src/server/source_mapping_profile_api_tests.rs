use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};

use axum::body::{Body, to_bytes};
use axum::extract::ConnectInfo;
use axum::http::{Method, Request, StatusCode, header};
use serde_json::{Value, json};
use tower::ServiceExt;

use super::api;
use super::auth::{AuthStore, CSRF_HEADER_NAME, Role, SessionCredentials, Sessions};
use super::jobs::JobRegistry;
use super::state::{AppState, AppStateInner};
use crate::db::{Db, SourceMappingColumn, SourceMappingProfileUpsert, source_mapping_signature};
use crate::domain::table::SemanticField;
use crate::import;

fn state(temp: &tempfile::TempDir, require_auth: bool) -> AppState {
    let db_path = temp.path().join("workspace.db");
    Db::open(&db_path).unwrap();
    let auth = AuthStore::open(&db_path).unwrap();
    Arc::new(AppStateInner {
        db_path,
        jobs: JobRegistry::open(&temp.path().join("workspace.db")).unwrap(),
        uploads_dir: temp.path().join("uploads"),
        exports_dir: temp.path().join("exports"),
        lan_exposed: require_auth,
        require_auth,
        auth,
        sessions: Sessions::new(),
        projection_trust: Mutex::new(None),
    })
}

fn request(method: Method, uri: &str, body: Body) -> Request<Body> {
    let mut request = Request::builder()
        .method(method)
        .uri(uri)
        .body(body)
        .unwrap();
    request.extensions_mut().insert(ConnectInfo(SocketAddr::new(
        IpAddr::V4(Ipv4Addr::LOCALHOST),
        45100,
    )));
    request
}

fn authenticated_request(
    method: Method,
    uri: &str,
    body: Body,
    credentials: &SessionCredentials,
) -> Request<Body> {
    let mut request = request(method, uri, body);
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
    request
}

fn json_request(
    method: Method,
    uri: &str,
    payload: Value,
    credentials: Option<&SessionCredentials>,
) -> Request<Body> {
    let mut request = match credentials {
        Some(credentials) => {
            authenticated_request(method, uri, Body::from(payload.to_string()), credentials)
        }
        None => request(method, uri, Body::from(payload.to_string())),
    };
    request
        .headers_mut()
        .insert(header::CONTENT_TYPE, "application/json".parse().unwrap());
    request
}

async fn json_body(response: axum::response::Response) -> Value {
    serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap()).unwrap()
}

fn multipart_request(uri: &str, boundary: &str, body: String) -> Request<Body> {
    let mut request = request(Method::POST, uri, Body::from(body));
    request.headers_mut().insert(
        header::CONTENT_TYPE,
        format!("multipart/form-data; boundary={boundary}")
            .parse()
            .unwrap(),
    );
    request
}

fn csv_signature(temp: &tempfile::TempDir, file_name: &str, contents: &str) -> String {
    let path = temp.path().join(file_name);
    std::fs::write(&path, contents).unwrap();
    let peek = import::peek_file(&path, 8).unwrap();
    source_mapping_signature(
        &peek.sheets[0]
            .columns
            .iter()
            .map(|column| SourceMappingColumn {
                header: column.header.clone(),
                role: column.role,
            })
            .collect::<Vec<_>>(),
    )
}

fn save_profile(
    state: &AppState,
    name: &str,
    signature: String,
    mapping: Vec<Option<SemanticField>>,
    fixed_values: std::collections::BTreeMap<SemanticField, String>,
) -> i64 {
    Db::open_runtime(&state.db_path)
        .unwrap()
        .upsert_source_mapping_profile(SourceMappingProfileUpsert {
            id: None,
            name: name.to_string(),
            signature,
            mapping,
            fixed_values,
        })
        .unwrap()
        .id
}

async fn wait_for_job(state: &AppState, id: u64) -> super::jobs::JobSnapshot {
    for _ in 0..250 {
        let snapshot = state.jobs.snapshot(id).unwrap();
        if matches!(
            snapshot.status,
            super::jobs::JobStatus::Succeeded
                | super::jobs::JobStatus::Failed
                | super::jobs::JobStatus::Cancelled
        ) {
            return snapshot;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    panic!("job {id} did not finish");
}

fn profile_payload(name: &str, signature: &str) -> Value {
    json!({
        "name": name,
        "signature": signature,
        "mapping": ["Recipient", "Value"],
        "fixed_values": { "Currency": "USD", "WeightUnit": "kg" }
    })
}

#[tokio::test]
async fn profile_routes_allow_viewers_to_read_but_only_editors_to_mutate() {
    let temp = tempfile::tempdir().unwrap();
    let state = state(&temp, true);
    state
        .auth
        .add_user("owner", "owner-password", Role::Owner)
        .unwrap();
    state
        .auth
        .add_user("viewer", "viewer-password", Role::Viewer)
        .unwrap();
    state
        .auth
        .add_user("editor", "editor-password", Role::Editor)
        .unwrap();
    let viewer = state.auth.create_session("viewer").unwrap();
    let editor = state.auth.create_session("editor").unwrap();
    let app = api::router(state);
    let signature = format!("smp1:2:{}", "a".repeat(64));

    let response = app
        .clone()
        .oneshot(authenticated_request(
            Method::GET,
            "/api/v2/imports/profiles",
            Body::empty(),
            &viewer,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let response = app
        .clone()
        .oneshot(authenticated_request(
            Method::GET,
            &format!("/api/v2/imports/profiles/suggest?signature={signature}"),
            Body::empty(),
            &viewer,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let response = app
        .clone()
        .oneshot(json_request(
            Method::POST,
            "/api/v2/imports/profiles",
            profile_payload("Viewer profile", &signature),
            Some(&viewer),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);

    let response = app
        .oneshot(json_request(
            Method::POST,
            "/api/v2/imports/profiles",
            profile_payload("Editor profile", &signature),
            Some(&editor),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn profile_crud_suggest_and_corrupt_rows_are_exposed_safely() {
    let temp = tempfile::tempdir().unwrap();
    let state = state(&temp, false);
    let app = api::router(state.clone());
    let signature = format!("smp1:2:{}", "b".repeat(64));

    let created = app
        .clone()
        .oneshot(json_request(
            Method::POST,
            "/api/v2/imports/profiles",
            profile_payload("Reusable source", &signature),
            None,
        ))
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::OK);
    let created = json_body(created).await;
    let id = created["id"].as_i64().unwrap();

    let compatible_list = app
        .clone()
        .oneshot(request(Method::GET, "/api/imports/profiles", Body::empty()))
        .await
        .unwrap();
    assert_eq!(compatible_list.status(), StatusCode::OK);
    assert_eq!(json_body(compatible_list).await["profiles"][0]["id"], id);

    let mut update = profile_payload("Updated source", &signature);
    update["id"] = json!(id);
    update["mapping"] = json!(["Sender", "Value"]);
    let updated = app
        .clone()
        .oneshot(json_request(
            Method::POST,
            "/api/v2/imports/profiles",
            update,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(updated.status(), StatusCode::OK);
    let updated = json_body(updated).await;
    assert_eq!(updated["id"], id);
    assert_eq!(updated["name"], "Updated source");
    assert_eq!(updated["mapping"][0], "Sender");

    let fetched = app
        .clone()
        .oneshot(request(
            Method::GET,
            &format!("/api/v2/imports/profiles/{id}"),
            Body::empty(),
        ))
        .await
        .unwrap();
    assert_eq!(fetched.status(), StatusCode::OK);
    assert_eq!(json_body(fetched).await["name"], "Updated source");

    let suggested = app
        .clone()
        .oneshot(request(
            Method::GET,
            &format!("/api/v2/imports/profiles/suggest?signature={signature}"),
            Body::empty(),
        ))
        .await
        .unwrap();
    assert_eq!(suggested.status(), StatusCode::OK);
    assert_eq!(json_body(suggested).await["profiles"][0]["id"], id);

    rusqlite::Connection::open(&state.db_path)
        .unwrap()
        .execute(
            "INSERT INTO source_mapping_profiles(name, name_key, signature, mapping_json, fixed_values_json) VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params!["Broken", "broken", &signature, "not-json", "{}"],
        )
        .unwrap();
    let listed = app
        .clone()
        .oneshot(request(
            Method::GET,
            "/api/v2/imports/profiles",
            Body::empty(),
        ))
        .await
        .unwrap();
    assert_eq!(listed.status(), StatusCode::OK);
    let listed = json_body(listed).await;
    assert_eq!(listed["profiles"].as_array().unwrap().len(), 1);
    assert_eq!(listed["ignored_corrupt_rows"].as_array().unwrap().len(), 1);

    let deleted = app
        .oneshot(request(
            Method::DELETE,
            &format!("/api/v2/imports/profiles/{id}"),
            Body::empty(),
        ))
        .await
        .unwrap();
    assert_eq!(deleted.status(), StatusCode::OK);
    assert_eq!(json_body(deleted).await["deleted"], true);
}

#[tokio::test]
async fn profile_json_body_is_bounded() {
    let temp = tempfile::tempdir().unwrap();
    let state = state(&temp, false);
    let app = api::router(state);
    let signature = format!("smp1:2:{}", "d".repeat(64));
    let payload = json!({
        "name": "x".repeat(513 * 1024),
        "signature": signature,
        "mapping": ["Recipient", "Value"],
        "fixed_values": {}
    });

    let response = app
        .oneshot(json_request(
            Method::POST,
            "/api/v2/imports/profiles",
            payload,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
}

#[tokio::test]
async fn workbook_peek_returns_stable_signatures_and_exact_profile_suggestions() {
    let temp = tempfile::tempdir().unwrap();
    let state = state(&temp, false);
    let csv = "Alpha,Beta\nACME,1250\n";
    let signature = csv_signature(&temp, "preview-source.csv", csv);
    let profile_id = save_profile(
        &state,
        "Preview profile",
        signature.clone(),
        vec![Some(SemanticField::Recipient), Some(SemanticField::Value)],
        Default::default(),
    );
    let app = api::router(state);
    let boundary = "base-search-profile-peek";
    let body = format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"files\"; filename=\"preview-source.csv\"\r\nContent-Type: text/csv\r\n\r\n{csv}\r\n--{boundary}--\r\n"
    );

    let response = app
        .oneshot(multipart_request("/api/v2/imports/peek", boundary, body))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    assert_eq!(body["sheets"][0]["name"], "preview-source.csv");
    assert_eq!(body["sheets"][0]["signature"], signature);
    assert_eq!(
        body["sheets"][0]["profile_suggestions"]["profiles"][0]["id"],
        profile_id
    );
}

#[tokio::test]
async fn upload_rejects_a_selected_profile_when_the_actual_sheet_signature_differs() {
    let temp = tempfile::tempdir().unwrap();
    let state = state(&temp, false);
    let signature = csv_signature(&temp, "expected.csv", "Alpha,Beta\none,1\n");
    let profile_id = save_profile(
        &state,
        "Expected source",
        signature,
        vec![Some(SemanticField::Recipient), Some(SemanticField::Value)],
        Default::default(),
    );
    let app = api::router(state.clone());
    let boundary = "base-search-profile-mismatch";
    let body = format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"selected_sheets\"\r\n\r\n[\"different.csv\"]\r\n\
         --{boundary}\r\nContent-Disposition: form-data; name=\"sheet_profiles\"\r\n\r\n{{\"different.csv\":{profile_id}}}\r\n\
         --{boundary}\r\nContent-Disposition: form-data; name=\"files\"; filename=\"different.csv\"\r\nContent-Type: text/csv\r\n\r\nGamma,Delta\ntwo,2\n\r\n\
         --{boundary}--\r\n"
    );

    let response = app
        .oneshot(multipart_request("/api/v2/imports", boundary, body))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = json_body(response).await;
    assert_eq!(body["error"]["code"], "profile_signature_mismatch");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("different.csv")
    );
    assert!(state.jobs.list().is_empty());
}

#[tokio::test]
async fn explicit_column_overrides_win_over_the_selected_profile_mapping() {
    let temp = tempfile::tempdir().unwrap();
    let state = state(&temp, false);
    let csv = "Alpha,Beta\nACME IMPORT,1250.50\n";
    let signature = csv_signature(&temp, "override.csv", csv);
    let profile_id = save_profile(
        &state,
        "Sender layout",
        signature,
        vec![Some(SemanticField::Sender), Some(SemanticField::Value)],
        Default::default(),
    );
    let app = api::router(state.clone());
    let boundary = "base-search-profile-override";
    let body = format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"selected_sheets\"\r\n\r\n[\"override.csv\"]\r\n\
         --{boundary}\r\nContent-Disposition: form-data; name=\"sheet_profiles\"\r\n\r\n{{\"override.csv\":{profile_id}}}\r\n\
         --{boundary}\r\nContent-Disposition: form-data; name=\"sheet_semantics\"\r\n\r\n{{\"override.csv\":{{\"0\":\"Recipient\"}}}}\r\n\
         --{boundary}\r\nContent-Disposition: form-data; name=\"files\"; filename=\"override.csv\"\r\nContent-Type: text/csv\r\n\r\n{csv}\r\n\
         --{boundary}--\r\n"
    );
    let response = app
        .oneshot(multipart_request("/api/v2/imports", boundary, body))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let id = json_body(response).await["id"].as_u64().unwrap();
    let job = wait_for_job(&state, id).await;
    assert_eq!(job.status, super::jobs::JobStatus::Succeeded);
    assert_eq!(
        job.input.as_ref().unwrap()["source_profiles"]["override.csv"]["id"],
        profile_id
    );
    assert_eq!(
        job.input.as_ref().unwrap()["sheet_semantics"]["override.csv"]["0"],
        "Recipient"
    );

    let db = Db::open_runtime(&state.db_path).unwrap();
    let recipient_query = crate::db_types::Query {
        filters: crate::db_types::Filters {
            recipient: "ACME IMPORT".to_string(),
            ..Default::default()
        },
        ..Default::default()
    };
    let sender_query = crate::db_types::Query {
        filters: crate::db_types::Filters {
            sender: "ACME IMPORT".to_string(),
            ..Default::default()
        },
        ..Default::default()
    };
    assert_eq!(db.count(&recipient_query).unwrap(), 1);
    assert_eq!(db.count(&sender_query).unwrap(), 0);

    let persisted_input = job.input.clone();
    let reopened = JobRegistry::open(&state.db_path).unwrap();
    let restored = reopened.snapshot(id).unwrap();
    assert_eq!(restored.status, super::jobs::JobStatus::Succeeded);
    assert_eq!(restored.input, persisted_input);
}

#[tokio::test]
async fn selected_profile_fixed_values_reach_price_risk_semantics() {
    let temp = tempfile::tempdir().unwrap();
    let state = state(&temp, false);
    let mut csv = String::from("Doc,When,SKU,Amount,Mass\n");
    for index in 0..20 {
        csv.push_str(&format!(
            "DOC-{index},2024-02-15,SKU-FIXED,{},1\n",
            100 + index
        ));
    }
    csv.push_str("TARGET,2024-02-15,SKU-FIXED,10,1\n");
    let signature = csv_signature(&temp, "risk-fixed.csv", &csv);
    let profile_id = save_profile(
        &state,
        "Risk defaults",
        signature,
        vec![
            Some(SemanticField::DeclarationNumber),
            Some(SemanticField::Date),
            Some(SemanticField::ProductCode),
            Some(SemanticField::Value),
            Some(SemanticField::NetWeight),
        ],
        [
            (SemanticField::Currency, "USD".to_string()),
            (SemanticField::WeightUnit, "kg".to_string()),
        ]
        .into_iter()
        .collect(),
    );
    let app = api::router(state.clone());
    let boundary = "base-search-profile-fixed-values";
    let body = format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"selected_sheets\"\r\n\r\n[\"risk-fixed.csv\"]\r\n\
         --{boundary}\r\nContent-Disposition: form-data; name=\"sheet_profiles\"\r\n\r\n{{\"risk-fixed.csv\":{profile_id}}}\r\n\
         --{boundary}\r\nContent-Disposition: form-data; name=\"files\"; filename=\"risk-fixed.csv\"\r\nContent-Type: text/csv\r\n\r\n{csv}\r\n\
         --{boundary}--\r\n"
    );
    let response = app
        .oneshot(multipart_request("/api/v2/imports", boundary, body))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let id = json_body(response).await["id"].as_u64().unwrap();
    let job = wait_for_job(&state, id).await;
    assert_eq!(job.status, super::jobs::JobStatus::Succeeded);
    assert_eq!(
        job.input.as_ref().unwrap()["sheet_fixed_values"]["risk-fixed.csv"]["Currency"],
        "USD"
    );
    assert_eq!(
        job.input.as_ref().unwrap()["sheet_fixed_values"]["risk-fixed.csv"]["WeightUnit"],
        "kg"
    );

    let db = Db::open_runtime(&state.db_path).unwrap();
    let risk = db
        .undervaluation(&crate::db_types::Query::default(), 0.7, 20, 100)
        .unwrap();
    assert!(risk.available, "{:?}", risk.limitations);
    let target = risk
        .rows
        .iter()
        .find(|row| row.declaration_number == "TARGET")
        .expect("target should be evaluated as price risk");
    assert_eq!(target.cohort.currency, "USD");
    assert_eq!(target.cohort.weight_unit, "KG");
}
