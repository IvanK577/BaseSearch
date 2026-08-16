//! API router assembly. Each resource lives in its own module; this file only
//! wires paths to handlers and applies transport-level layers.

mod analytics;
mod auth;
#[cfg(test)]
mod contract_tests;
mod engines;
mod exports;
mod health;
mod imports;
mod jobs;
mod maintenance;
mod record;
mod schema;
mod search;
mod source_profiles;

use axum::Router;
use axum::extract::DefaultBodyLimit;
use axum::routing::{delete, get, post};
use tower_http::compression::CompressionLayer;

use crate::server::assets;
use crate::server::error::ApiError;
use crate::server::state::AppState;

#[cfg(test)]
pub fn router(state: AppState) -> Router {
    router_with_control(
        state,
        crate::server::observability::HttpRequestControl::default(),
    )
}

pub(crate) fn router_with_control(
    state: AppState,
    request_control: crate::server::observability::HttpRequestControl,
) -> Router {
    let api = api_router();
    Router::new()
        .nest("/api", api.clone())
        .nest("/api/v2", api)
        .fallback(assets::static_handler)
        .layer(axum::middleware::from_fn_with_state(
            request_control.clone(),
            crate::server::observability::admit_heavy_reads,
        ))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            crate::server::auth::require_auth_mw,
        ))
        .layer(CompressionLayer::new())
        .layer(axum::middleware::from_fn_with_state(
            request_control,
            crate::server::observability::observe_requests,
        ))
        .with_state(state)
}

fn api_router() -> Router<AppState> {
    Router::new()
        .route("/health", get(health::health))
        .route("/status", get(health::status))
        .route("/auth/login", post(auth::login))
        .route("/auth/logout", post(auth::logout))
        .route("/auth/me", get(auth::me))
        .route("/auth/users", get(auth::list_users).post(auth::create_user))
        .route("/auth/users/{username}", delete(auth::delete_user))
        .route("/me", get(auth::me))
        .route(
            "/admin/users",
            get(auth::list_users).post(auth::create_user),
        )
        .route("/admin/users/{username}", delete(auth::delete_user))
        .route("/schema", get(schema::schema))
        .route("/schema/fixed-values", post(schema::set_fixed_values))
        .route("/columns/{id}/semantic", post(schema::set_semantic))
        .route("/search", post(search::search))
        .route("/count", post(search::count))
        .route("/records/{id}", get(record::record))
        .route("/analytics", post(analytics::analytics))
        .route("/analytics/overview", post(analytics::overview))
        .route("/analytics/section", post(analytics::section))
        .route("/engines", get(engines::status))
        .route("/analytics/pivot", post(analytics::pivot))
        .route("/pivot", post(analytics::pivot))
        .route("/compare", post(analytics::compare))
        .route("/analytics/undervaluation", post(analytics::undervaluation))
        .route("/company/{edrpou}", get(analytics::company))
        .route(
            "/imports",
            post(imports::upload).layer(DefaultBodyLimit::max(
                imports::MAX_BATCH_BODY_BYTES as usize,
            )),
        )
        .route(
            "/imports/peek",
            post(imports::peek).layer(DefaultBodyLimit::max(imports::MAX_FILE_BODY_BYTES as usize)),
        )
        .route(
            "/imports/profiles",
            get(source_profiles::list)
                .post(source_profiles::upsert)
                .layer(DefaultBodyLimit::max(
                    source_profiles::MAX_PROFILE_BODY_BYTES,
                )),
        )
        .route("/imports/profiles/suggest", get(source_profiles::suggest))
        .route(
            "/imports/profiles/{id}",
            get(source_profiles::get).delete(source_profiles::delete),
        )
        .route("/imports/log", get(imports::log))
        .route("/exports", post(exports::create))
        .route("/exports/{id}/download", get(exports::download))
        .route("/export", post(exports::create))
        .route("/export/{id}/download", get(exports::download))
        .route("/jobs", get(jobs::list))
        .route("/jobs/{id}", get(jobs::get))
        .route("/jobs/{id}/cancel", post(jobs::cancel))
        .route("/database/stats", get(maintenance::stats))
        .route("/database/optimize", post(maintenance::optimize))
        .route("/database/compact", post(maintenance::compact))
        .route("/database/reindex", post(maintenance::reindex))
        .route("/database/olap", post(maintenance::olap))
        .route("/admin/duckdb/rebuild", post(maintenance::olap))
        // `/admin/network` is intentionally reserved for the bind-policy worker.
        // Do not add a route here until its interface-selection contract lands.
        .route("/database/clear", post(maintenance::clear))
        .fallback(api_not_found)
}

async fn api_not_found() -> ApiError {
    ApiError::not_found("No such API route.")
}
