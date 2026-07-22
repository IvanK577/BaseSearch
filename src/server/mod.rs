//! Local browser workspace: an Axum server on `127.0.0.1` that exposes the
//! same Base Search core (import, search, analytics, export, maintenance) over
//! canonical JSON `/api/v2` routes. The legacy `/api` prefix remains available
//! during the V2 compatibility window.
//!
//! The server never sends data anywhere; it only binds a local socket. LAN
//! exposure requires the caller to pass an explicit non-loopback host.

mod api;
mod assets;
mod auth;
mod config;
mod dto;
mod error;
#[cfg(test)]
mod job_api_tests;
#[cfg(test)]
mod job_queue_tests;
mod job_store;
#[cfg(test)]
mod job_store_tests;
mod jobs;
mod login_limit;
pub mod network;
mod observability;
#[cfg(test)]
mod observability_tests;
#[cfg(feature = "duckdb-olap")]
mod olap;
#[cfg(test)]
mod router_security_tests;
pub mod security;
#[cfg(test)]
mod source_mapping_profile_api_tests;
mod state;

use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use tokio::net::TcpListener;

use crate::db::Db;

pub use config::{DEFAULT_PORT, ServerConfig};

use jobs::{JobKind, JobRegistry, JobVisibility, spawn_job_for};
use state::{AppState, AppStateInner};

/// Wall-clock budget for a single analytical/search statement. A broader query
/// is interrupted and reported as a timeout instead of blocking a worker.
pub(crate) const DB_STATEMENT_TIMEOUT: Duration = Duration::from_secs(60);

/// Blocks the calling (sync) thread, builds a Tokio runtime, and serves the
/// workspace until the process is stopped. Called from `main.rs` and the CLI.
pub fn run(config: ServerConfig) -> Result<(), String> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|err| format!("failed to start async runtime: {err}"))?;
    runtime.block_on(serve(config))
}

async fn serve(config: ServerConfig) -> Result<(), String> {
    config.validate_bind_policy()?;
    let db_path = config.db_path.clone();
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|err| format!("cannot create database folder: {err}"))?;
    }
    // Open once through the migrating path so the schema is up to date; request
    // handlers then use fast runtime connections.
    Db::open(&db_path).map_err(|err| format!("cannot open database: {err}"))?;

    let base_dir = db_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| Path::new(".").to_path_buf());
    let uploads_dir = base_dir.join("uploads");
    let exports_dir = base_dir.join("exports");
    std::fs::create_dir_all(&uploads_dir).ok();
    std::fs::create_dir_all(&exports_dir).ok();

    let require_auth = !config.is_loopback();
    let account_store = auth::AuthStore::open(&db_path)
        .map_err(|err| format!("cannot open account store: {err}"))?;
    if require_auth && account_store.user_count().unwrap_or(0) == 0 {
        return Err(format!(
            "This server is bound to a network address, which requires sign-in, but no accounts \
             exist yet.\nCreate an administrator locally first, then start again:\n  \
             base-search-cli user-add \"{}\" <username>",
            db_path.display()
        ));
    }

    let jobs =
        JobRegistry::open(&db_path).map_err(|err| format!("cannot open job history: {err}"))?;
    let state: AppState = Arc::new(AppStateInner {
        db_path: db_path.clone(),
        jobs,
        uploads_dir,
        exports_dir,
        lan_exposed: !config.is_loopback(),
        require_auth,
        auth: account_store,
        sessions: auth::Sessions::new(),
        projection_trust: std::sync::Mutex::new(None),
    });
    let weak_state = Arc::downgrade(&state);
    state.jobs.set_authorizer(move |owner_user_id, permission| {
        if owner_user_id == "local-owner" {
            return true;
        }
        let Some(state) = weak_state.upgrade() else {
            return false;
        };
        state
            .auth
            .list_users()
            .ok()
            .and_then(|users| {
                users
                    .into_iter()
                    .find(|user| user.id == owner_user_id && user.enabled)
            })
            .and_then(|user| auth::Role::parse(&user.role))
            .is_some_and(|role| role.allows(permission))
    });
    state.jobs.start_artifact_cleanup_task();

    spawn_startup_reindex(&state);

    let request_control = observability::HttpRequestControl::new(
        config.max_concurrent_heavy_reads,
        config.max_queued_heavy_reads,
        config.heavy_read_queue_wait,
    );
    let app = api::router_with_control(state.clone(), request_control.clone())
        .layer(axum::middleware::from_fn_with_state(
            security::TransportSecurity::new(config.host, config.port),
            security::enforce_transport_security,
        ))
        .layer(axum::middleware::from_fn_with_state(
            request_control,
            observability::observe_requests,
        ));
    let mut listeners = Vec::new();
    for host in config.listener_hosts() {
        let addr = SocketAddr::new(host, config.port);
        let listener = TcpListener::bind(addr).await.map_err(|err| {
            if err.kind() == std::io::ErrorKind::AddrInUse {
                format!(
                    "port {} is already in use on {host}. Close the other Base Search window or pick another port with --port.",
                    config.port
                )
            } else {
                format!("cannot bind {addr}: {err}")
            }
        })?;
        listeners.push((addr, listener));
    }
    let (primary_addr, primary_listener) = listeners.remove(0);

    let url = config.workspace_url();
    println!("Base Search workspace is running at {url}");
    println!("Database: {}", db_path.display());
    if state.lan_exposed {
        if let Some(lan_url) = config.lan_url() {
            println!(
                "Reachable from your selected network interface at {lan_url}. Sign-in is required."
            );
        }
        println!(
            "SECURITY: this connection is NOT encrypted; passwords and data travel in the clear. \
             Put it behind a TLS reverse proxy or keep it on a trusted LAN, and never expose it to \
             the internet."
        );
    } else {
        println!("Only this computer can reach it (loopback). Press Ctrl+C to stop.");
    }
    if config.open_browser {
        open_url(&url);
    }

    if let Some((loopback_addr, loopback_listener)) = listeners.pop() {
        let primary = axum::serve(
            primary_listener,
            app.clone()
                .into_make_service_with_connect_info::<SocketAddr>(),
        );
        let loopback = axum::serve(
            loopback_listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        );
        tokio::try_join!(primary, loopback)
            .map(|_| ())
            .map_err(|err| {
                format!("server error while listening on {primary_addr} and {loopback_addr}: {err}")
            })
    } else {
        axum::serve(
            primary_listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .map_err(|err| format!("server error on {primary_addr}: {err}"))
    }
}

/// If a previous run left rows unindexed (interrupted import), finish the
/// search index in the background so search works right away.
fn spawn_startup_reindex(state: &AppState) {
    let needs = Db::open_runtime(&state.db_path)
        .map(|db| db.unindexed_rows())
        .unwrap_or(0);
    if needs == 0 {
        return;
    }
    let db_path = state.db_path.clone();
    let _ = spawn_job_for(
        &state.jobs,
        &auth::Identity::local_owner(),
        JobKind::Reindex,
        JobVisibility::Workspace,
        "Finishing the search index",
        move |handle| {
            let mut db = match Db::open(&db_path) {
                Ok(db) => db,
                Err(err) => {
                    handle.fail(err);
                    return;
                }
            };
            let cancel = handle.cancel_flag();
            let result = db.index_fts(&cancel, |done, total| {
                handle.set_progress("indexing", done, total);
            });
            match result {
                Ok(_) if handle.is_cancelled() => handle.mark_cancelled(),
                Ok((indexed, _)) => {
                    handle.set_message(format!("Indexed {indexed} rows"));
                    handle.succeed(None);
                }
                Err(err) => handle.fail(err.to_string()),
            }
        },
    );
}

/// Opens the workspace URL in the default browser. Best-effort: a failure to
/// launch a browser is not fatal; the URL is printed for manual use.
fn open_url(url: &str) {
    #[cfg(target_os = "windows")]
    let mut command = {
        let mut command = std::process::Command::new("cmd");
        command.args(["/C", "start", "", url]);
        command
    };
    #[cfg(target_os = "macos")]
    let mut command = {
        let mut command = std::process::Command::new("open");
        command.arg(url);
        command
    };
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let mut command = {
        let mut command = std::process::Command::new("xdg-open");
        command.arg(url);
        command
    };
    let _ = command.spawn();
}

/// Adds or replaces a local account. Used by the CLI to bootstrap the first
/// administrator before a networked server is started.
pub fn add_account(
    db_path: &Path,
    username: &str,
    password: &str,
    role: &str,
) -> Result<(), String> {
    let role =
        auth::Role::parse(role).ok_or_else(|| "role must be 'admin' or 'viewer'".to_string())?;
    auth::AuthStore::open(db_path)?.add_user(username, password, role)
}

/// Lists local accounts as `(username, role, created_at)`.
pub fn list_accounts(db_path: &Path) -> Result<Vec<(String, String, String)>, String> {
    Ok(auth::AuthStore::open(db_path)?
        .list_users()?
        .into_iter()
        .map(|user| (user.username, user.role, user.created_at))
        .collect())
}

/// Removes a local account. Returns false when it did not exist.
pub fn remove_account(db_path: &Path, username: &str) -> Result<bool, String> {
    auth::AuthStore::open(db_path)?.remove_user(username)
}

/// Removes path separators from an uploaded/derived filename so it can only
/// ever land inside our upload/export directory.
pub(crate) fn sanitize_file_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|ch| match ch {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' | '\0' => '_',
            other => other,
        })
        .collect();
    let trimmed = cleaned.trim().trim_matches('.').trim();
    if trimmed.is_empty() {
        "file".to_string()
    } else {
        trimmed.chars().take(180).collect()
    }
}
