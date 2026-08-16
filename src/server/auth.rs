//! Persistent authentication and authorization for local and LAN workspaces.
//!
//! Accounts and sessions live in the existing `<db>.auth.db` companion file so
//! clearing or replacing the searchable dataset never removes access control.
//! Personal loopback mode uses a synthetic local owner; LAN mode always uses a
//! password-backed account and a persistent, server-side session.

use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex, MutexGuard};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::extract::{Request, State};
use axum::http::{HeaderMap, Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use rusqlite::{Connection, OptionalExtension, Transaction};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use super::error::ApiError;
use super::login_limit::LoginRateLimiter;
use super::state::AppState;

pub const COOKIE_NAME: &str = "bs_session";
pub const CSRF_COOKIE_NAME: &str = "bs_csrf";
#[allow(dead_code)]
pub const CSRF_HEADER_NAME: &str = "x-bs-csrf";

const IDLE_SESSION_TTL: Duration = Duration::from_secs(12 * 3600);
const ABSOLUTE_SESSION_TTL: Duration = Duration::from_secs(7 * 24 * 3600);
const SESSION_TOUCH_INTERVAL: Duration = Duration::from_secs(5 * 60);
const TOKEN_BYTES: usize = 32;
const TOKEN_HEX_LENGTH: usize = TOKEN_BYTES * 2;
const MAX_PASSWORD_BYTES: usize = 1_024;
const MAX_ACTIVE_SESSIONS_PER_USER: usize = 10;
const MAX_CONCURRENT_PASSWORD_VERIFICATIONS: usize = 2;

static DUMMY_PASSWORD_HASH: LazyLock<String> = LazyLock::new(|| {
    use argon2::password_hash::SaltString;
    use argon2::{Argon2, PasswordHasher};

    let salt = SaltString::encode_b64(b"base-search-login")
        .expect("the fixed dummy-login salt must be valid");
    Argon2::default()
        .hash_password(b"base-search-invalid-account", &salt)
        .expect("the fixed dummy-login password must hash")
        .to_string()
});

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Owner,
    Admin,
    Editor,
    Viewer,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Role::Owner => "owner",
            Role::Admin => "admin",
            Role::Editor => "editor",
            Role::Viewer => "viewer",
        }
    }

    pub fn parse(value: &str) -> Option<Role> {
        match value.trim().to_ascii_lowercase().as_str() {
            "owner" => Some(Role::Owner),
            "admin" => Some(Role::Admin),
            "editor" => Some(Role::Editor),
            "viewer" => Some(Role::Viewer),
            _ => None,
        }
    }

    pub fn allows(self, permission: Permission) -> bool {
        use Permission::*;

        match self {
            Role::Owner => true,
            Role::Admin => !matches!(permission, ManageOwners),
            Role::Editor => matches!(
                permission,
                Read | Analyze | Export | Import | ManageMappings | ManageSavedQueries
            ),
            Role::Viewer => matches!(permission, Read | Analyze | Export),
        }
    }

    pub(crate) fn is_privileged(self) -> bool {
        matches!(self, Role::Owner | Role::Admin)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Permission {
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
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AuthorizationError {
    pub permission: Permission,
}

impl std::fmt::Display for AuthorizationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "permission {:?} is required", self.permission)
    }
}

pub fn authorize(identity: &Identity, permission: Permission) -> Result<(), AuthorizationError> {
    if identity.role.allows(permission) {
        Ok(())
    } else {
        Err(AuthorizationError { permission })
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct UserInfo {
    pub id: String,
    pub username: String,
    pub role: String,
    pub enabled: bool,
    pub created_at: String,
}

#[derive(Clone, Debug)]
pub struct Identity {
    pub user_id: String,
    pub username: String,
    pub role: Role,
}

impl Identity {
    pub fn local_owner() -> Self {
        Self {
            user_id: "local-owner".to_string(),
            username: "local-owner".to_string(),
            role: Role::Owner,
        }
    }
}

#[derive(Clone, Debug)]
pub struct SessionCredentials {
    pub token: String,
    pub csrf_token: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct UserMutation {
    pub role: Role,
    pub created: bool,
}

/// The established `<db>.auth.db` path. This intentionally preserves the V1/V2
/// companion-file name so existing accounts migrate in place.
pub fn auth_db_path(db_path: &Path) -> PathBuf {
    db_path.with_extension("auth.db")
}

pub struct AuthStore {
    conn: Mutex<Connection>,
}

impl AuthStore {
    pub fn open(db_path: &Path) -> Result<Self, String> {
        let _ = DUMMY_PASSWORD_HASH.as_str();
        let mut conn = Connection::open(auth_db_path(db_path)).map_err(|err| err.to_string())?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA busy_timeout=5000;
             PRAGMA foreign_keys=ON;",
        )
        .map_err(|err| err.to_string())?;
        migrate_auth_schema(&mut conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    fn connection(&self) -> Result<MutexGuard<'_, Connection>, String> {
        self.conn
            .lock()
            .map_err(|_| "Authentication database lock is poisoned.".to_string())
    }

    /// Returns the number of enabled accounts. The LAN startup check therefore
    /// refuses to start when every account has been disabled.
    pub fn user_count(&self) -> Result<u64, String> {
        let conn = self.connection()?;
        conn.query_row("SELECT COUNT(*) FROM users WHERE enabled = 1", [], |row| {
            row.get::<_, i64>(0)
        })
        .map(|count| count as u64)
        .map_err(|err| err.to_string())
    }

    /// Compatibility wrapper for callers that do not need mutation details.
    pub fn add_user(&self, username: &str, password: &str, role: Role) -> Result<(), String> {
        self.add_user_with_result(username, password, role)
            .map(|_| ())
    }

    /// Creates or updates one account without changing its stable identity.
    /// The first account must explicitly request the Owner role.
    pub(crate) fn add_user_with_result(
        &self,
        username: &str,
        password: &str,
        role: Role,
    ) -> Result<UserMutation, String> {
        let username = validate_username(username)?;
        validate_password(password)?;
        let password_hash = hash_password(password)?;
        let now_text = chrono::Utc::now().to_rfc3339();
        let now = unix_timestamp()?;

        let mut conn = self.connection()?;
        let tx = conn.transaction().map_err(|err| err.to_string())?;
        let existing = user_by_username(&tx, username)?;

        let mutation = if let Some(existing) = existing {
            protect_owner_transition(&tx, existing.role, existing.enabled, role, true)?;
            tx.execute(
                "UPDATE users
                 SET password_hash = ?1,
                     role = ?2,
                     enabled = 1,
                     session_epoch = session_epoch + 1,
                     updated_at = ?3
                 WHERE id = ?4",
                rusqlite::params![password_hash, role.as_str(), now_text, existing.id],
            )
            .map_err(|err| err.to_string())?;
            revoke_user_sessions_tx(&tx, &existing.id, now)?;
            UserMutation {
                role,
                created: false,
            }
        } else {
            if active_owner_count(&tx)? == 0 && role != Role::Owner {
                return Err("The first account must explicitly use the owner role.".to_string());
            }
            let id = random_identifier()?;
            tx.execute(
                "INSERT INTO users(
                     id, username, password_hash, role, enabled, session_epoch,
                     created_at, updated_at
                 ) VALUES (?1, ?2, ?3, ?4, 1, 0, ?5, ?5)",
                rusqlite::params![id, username, password_hash, role.as_str(), now_text],
            )
            .map_err(|err| err.to_string())?;
            UserMutation {
                role,
                created: true,
            }
        };
        tx.commit().map_err(|err| err.to_string())?;
        Ok(mutation)
    }

    #[allow(dead_code)]
    pub fn set_user_role(&self, username: &str, role: Role) -> Result<(), String> {
        let username = validate_username(username)?;
        let now = unix_timestamp()?;
        let now_text = chrono::Utc::now().to_rfc3339();
        let mut conn = self.connection()?;
        let tx = conn.transaction().map_err(|err| err.to_string())?;
        let existing =
            user_by_username(&tx, username)?.ok_or_else(|| "No such user.".to_string())?;
        if existing.role == role {
            return Ok(());
        }
        protect_owner_transition(&tx, existing.role, existing.enabled, role, existing.enabled)?;
        tx.execute(
            "UPDATE users
             SET role = ?1, session_epoch = session_epoch + 1, updated_at = ?2
             WHERE id = ?3",
            rusqlite::params![role.as_str(), now_text, existing.id],
        )
        .map_err(|err| err.to_string())?;
        revoke_user_sessions_tx(&tx, &existing.id, now)?;
        tx.commit().map_err(|err| err.to_string())
    }

    #[allow(dead_code)]
    pub fn set_user_enabled(&self, username: &str, enabled: bool) -> Result<(), String> {
        let username = validate_username(username)?;
        let now = unix_timestamp()?;
        let now_text = chrono::Utc::now().to_rfc3339();
        let mut conn = self.connection()?;
        let tx = conn.transaction().map_err(|err| err.to_string())?;
        let existing =
            user_by_username(&tx, username)?.ok_or_else(|| "No such user.".to_string())?;
        if existing.enabled == enabled {
            return Ok(());
        }
        protect_owner_transition(&tx, existing.role, existing.enabled, existing.role, enabled)?;
        tx.execute(
            "UPDATE users
             SET enabled = ?1, session_epoch = session_epoch + 1, updated_at = ?2
             WHERE id = ?3",
            rusqlite::params![i64::from(enabled), now_text, existing.id],
        )
        .map_err(|err| err.to_string())?;
        revoke_user_sessions_tx(&tx, &existing.id, now)?;
        tx.commit().map_err(|err| err.to_string())
    }

    #[allow(dead_code)]
    pub fn set_password(&self, username: &str, password: &str) -> Result<(), String> {
        let username = validate_username(username)?;
        validate_password(password)?;
        let password_hash = hash_password(password)?;
        let now = unix_timestamp()?;
        let now_text = chrono::Utc::now().to_rfc3339();
        let mut conn = self.connection()?;
        let tx = conn.transaction().map_err(|err| err.to_string())?;
        let existing =
            user_by_username(&tx, username)?.ok_or_else(|| "No such user.".to_string())?;
        tx.execute(
            "UPDATE users
             SET password_hash = ?1, session_epoch = session_epoch + 1, updated_at = ?2
             WHERE id = ?3",
            rusqlite::params![password_hash, now_text, existing.id],
        )
        .map_err(|err| err.to_string())?;
        revoke_user_sessions_tx(&tx, &existing.id, now)?;
        tx.commit().map_err(|err| err.to_string())
    }

    /// Removes a user while preserving at least one enabled workspace owner.
    pub fn remove_user(&self, username: &str) -> Result<bool, String> {
        let username = validate_username(username)?;
        let mut conn = self.connection()?;
        let tx = conn.transaction().map_err(|err| err.to_string())?;
        let Some(existing) = user_by_username(&tx, username)? else {
            return Ok(false);
        };
        if existing.enabled && existing.role == Role::Owner && active_owner_count(&tx)? <= 1 {
            return Err("Cannot remove the last enabled owner.".to_string());
        }
        tx.execute("DELETE FROM users WHERE id = ?1", [&existing.id])
            .map_err(|err| err.to_string())?;
        tx.commit().map_err(|err| err.to_string())?;
        Ok(true)
    }

    pub fn list_users(&self) -> Result<Vec<UserInfo>, String> {
        let conn = self.connection()?;
        let mut stmt = conn
            .prepare(
                "SELECT id, username, role, enabled, created_at
                 FROM users
                 ORDER BY username COLLATE NOCASE",
            )
            .map_err(|err| err.to_string())?;
        let rows = stmt
            .query_map([], |row| {
                Ok(UserInfo {
                    id: row.get(0)?,
                    username: row.get(1)?,
                    role: row.get(2)?,
                    enabled: row.get::<_, i64>(3)? != 0,
                    created_at: row.get(4)?,
                })
            })
            .map_err(|err| err.to_string())?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|err| err.to_string())
    }

    pub fn user_role(&self, username: &str) -> Result<Option<Role>, String> {
        let conn = self.connection()?;
        conn.query_row(
            "SELECT role FROM users WHERE username = ?1 COLLATE NOCASE",
            [username.trim()],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map(|role| role.and_then(|value| Role::parse(&value)))
        .map_err(|err| err.to_string())
    }

    /// Returns the enabled account's role when the password is correct.
    pub fn verify(&self, username: &str, password: &str) -> Result<Option<Role>, String> {
        if password.len() > MAX_PASSWORD_BYTES {
            return Ok(None);
        }
        let conn = self.connection()?;
        let found = conn
            .query_row(
                "SELECT password_hash, role
                 FROM users
                 WHERE username = ?1 COLLATE NOCASE AND enabled = 1",
                [username.trim()],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()
            .map_err(|err| err.to_string())?;
        drop(conn);
        let candidate_hash = found
            .as_ref()
            .map(|(hash, _)| hash.as_str())
            .unwrap_or(DUMMY_PASSWORD_HASH.as_str());
        let password_matches = verify_password(password, candidate_hash);
        match (found, password_matches) {
            (Some((_, role)), true) => Ok(Role::parse(&role).or(Some(Role::Viewer))),
            _ => Ok(None),
        }
    }

    /// Creates a persistent session. Only hashes are written to SQLite; raw
    /// session and CSRF tokens are returned once to the browser.
    pub fn create_session(&self, username: &str) -> Result<SessionCredentials, String> {
        let token = random_token()?;
        let csrf_token = random_token()?;
        let session_token_hash = token_hash(&token);
        let csrf_hash = token_hash(&csrf_token);
        let now = unix_timestamp()?;
        let idle_expires_at = now + IDLE_SESSION_TTL.as_secs() as i64;
        let absolute_expires_at = now + ABSOLUTE_SESSION_TTL.as_secs() as i64;

        let mut conn = self.connection()?;
        let tx = conn.transaction().map_err(|err| err.to_string())?;
        let user = user_by_username(&tx, username)?
            .filter(|user| user.enabled)
            .ok_or_else(|| "Account is disabled or does not exist.".to_string())?;
        prune_sessions_tx(&tx, now)?;
        trim_user_sessions_tx(
            &tx,
            &user.id,
            now,
            MAX_ACTIVE_SESSIONS_PER_USER.saturating_sub(1),
        )?;
        tx.execute(
            "INSERT INTO sessions(
                 token_hash, csrf_hash, user_id, role, session_epoch,
                 created_at, last_seen_at, idle_expires_at, absolute_expires_at, revoked_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6, ?7, ?8, NULL)",
            rusqlite::params![
                session_token_hash,
                csrf_hash,
                user.id,
                user.role.as_str(),
                user.session_epoch,
                now,
                idle_expires_at,
                absolute_expires_at
            ],
        )
        .map_err(|err| err.to_string())?;
        tx.commit().map_err(|err| err.to_string())?;

        Ok(SessionCredentials { token, csrf_token })
    }

    /// Resolves and slides a persistent session. Expired, revoked, disabled, or
    /// epoch-stale sessions are rejected and marked revoked.
    pub fn identify_session(&self, token: &str) -> Result<Option<Identity>, String> {
        if !is_valid_token(token) {
            return Ok(None);
        }
        let token_hash = token_hash(token);
        let now = unix_timestamp()?;
        let conn = self.connection()?;
        let found = conn
            .query_row(
                "SELECT
                     u.id, u.username, u.role, u.enabled,
                     u.session_epoch, s.session_epoch,
                     s.last_seen_at, s.idle_expires_at, s.absolute_expires_at,
                     s.revoked_at
                 FROM sessions s
                 JOIN users u ON u.id = s.user_id
                 WHERE s.token_hash = ?1",
                [&token_hash],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, i64>(3)? != 0,
                        row.get::<_, i64>(4)?,
                        row.get::<_, i64>(5)?,
                        row.get::<_, i64>(6)?,
                        row.get::<_, i64>(7)?,
                        row.get::<_, i64>(8)?,
                        row.get::<_, Option<i64>>(9)?,
                    ))
                },
            )
            .optional()
            .map_err(|err| err.to_string())?;

        let Some((
            user_id,
            username,
            role,
            enabled,
            user_epoch,
            session_epoch,
            last_seen_at,
            idle_expires_at,
            absolute_expires_at,
            revoked_at,
        )) = found
        else {
            return Ok(None);
        };

        let invalid = revoked_at.is_some()
            || !enabled
            || user_epoch != session_epoch
            || now >= idle_expires_at
            || now >= absolute_expires_at;
        if invalid {
            conn.execute(
                "UPDATE sessions SET revoked_at = COALESCE(revoked_at, ?1)
                 WHERE token_hash = ?2",
                rusqlite::params![now, token_hash],
            )
            .map_err(|err| err.to_string())?;
            return Ok(None);
        }

        if now.saturating_sub(last_seen_at) >= SESSION_TOUCH_INTERVAL.as_secs() as i64 {
            let next_idle = (now + IDLE_SESSION_TTL.as_secs() as i64).min(absolute_expires_at);
            conn.execute(
                "UPDATE sessions
                 SET last_seen_at = ?1, idle_expires_at = ?2
                 WHERE token_hash = ?3",
                rusqlite::params![now, next_idle, token_hash],
            )
            .map_err(|err| err.to_string())?;
        }
        Ok(Some(Identity {
            user_id,
            username,
            role: Role::parse(&role).unwrap_or(Role::Viewer),
        }))
    }

    pub fn revoke_session(&self, token: &str) -> Result<(), String> {
        if !is_valid_token(token) {
            return Ok(());
        }
        let now = unix_timestamp()?;
        let conn = self.connection()?;
        conn.execute(
            "UPDATE sessions SET revoked_at = COALESCE(revoked_at, ?1)
             WHERE token_hash = ?2",
            rusqlite::params![now, token_hash(token)],
        )
        .map_err(|err| err.to_string())?;
        Ok(())
    }

    #[allow(dead_code)]
    pub fn validate_csrf(&self, session_token: &str, csrf_token: &str) -> Result<bool, String> {
        if !is_valid_token(session_token)
            || !is_valid_token(csrf_token)
            || self.identify_session(session_token)?.is_none()
        {
            return Ok(false);
        }
        let conn = self.connection()?;
        conn.query_row(
            "SELECT EXISTS(
                 SELECT 1 FROM sessions
                 WHERE token_hash = ?1 AND csrf_hash = ?2 AND revoked_at IS NULL
             )",
            rusqlite::params![token_hash(session_token), token_hash(csrf_token)],
            |row| row.get::<_, i64>(0),
        )
        .map(|found| found != 0)
        .map_err(|err| err.to_string())
    }

    /// Validates the double-submit CSRF cookie/header pair against the hash
    /// stored with the persistent session. Transport middleware can call this
    /// for unsafe HTTP methods without learning any raw server-side token.
    pub fn validate_csrf_headers(&self, headers: &HeaderMap) -> Result<bool, String> {
        let Some(session_token) = token_from_headers(headers) else {
            return Ok(false);
        };
        let Some(cookie_token) = csrf_token_from_cookie(headers) else {
            return Ok(false);
        };
        let Some(header_token) = headers
            .get(CSRF_HEADER_NAME)
            .and_then(|value| value.to_str().ok())
        else {
            return Ok(false);
        };
        if cookie_token != header_token {
            return Ok(false);
        }
        self.validate_csrf(&session_token, &cookie_token)
    }
}

/// Process-local authentication state. Persistent sessions remain in
/// `AuthStore`; this value owns bounded login backoff and Argon2 admission.
pub struct Sessions {
    login_attempts: LoginRateLimiter,
    password_verifications: Arc<Semaphore>,
}

impl Default for Sessions {
    fn default() -> Self {
        Self {
            login_attempts: LoginRateLimiter::default(),
            password_verifications: Arc::new(Semaphore::new(MAX_CONCURRENT_PASSWORD_VERIFICATIONS)),
        }
    }
}

impl Sessions {
    pub fn new() -> Self {
        Self::default()
    }

    pub(crate) fn check_login(
        &self,
        peer_ip: std::net::IpAddr,
        username: &str,
    ) -> Result<(), Duration> {
        self.login_attempts.check(peer_ip, username)
    }

    pub(crate) fn record_login_failure(&self, peer_ip: std::net::IpAddr, username: &str) {
        self.login_attempts.record_failure(peer_ip, username);
    }

    pub(crate) fn clear_login_attempts(&self, peer_ip: std::net::IpAddr, username: &str) {
        self.login_attempts.clear(peer_ip, username);
    }

    pub(crate) fn try_acquire_password_verification(
        &self,
    ) -> Result<OwnedSemaphorePermit, Duration> {
        self.password_verifications
            .clone()
            .try_acquire_owned()
            .map_err(|_| Duration::from_secs(1))
    }
}

pub fn session_cookie(token: &str) -> String {
    format!(
        "{COOKIE_NAME}={token}; HttpOnly; SameSite=Strict; Path=/; Max-Age={}",
        ABSOLUTE_SESSION_TTL.as_secs()
    )
}

pub fn csrf_cookie(token: &str) -> String {
    format!(
        "{CSRF_COOKIE_NAME}={token}; SameSite=Strict; Path=/; Max-Age={}",
        ABSOLUTE_SESSION_TTL.as_secs()
    )
}

pub fn clear_cookie() -> String {
    format!("{COOKIE_NAME}=; HttpOnly; SameSite=Strict; Path=/; Max-Age=0")
}

pub fn clear_csrf_cookie() -> String {
    format!("{CSRF_COOKIE_NAME}=; SameSite=Strict; Path=/; Max-Age=0")
}

pub fn token_from_headers(headers: &HeaderMap) -> Option<String> {
    cookie_value(headers, COOKIE_NAME).filter(|token| is_valid_token(token))
}

#[allow(dead_code)]
pub fn csrf_token_from_cookie(headers: &HeaderMap) -> Option<String> {
    cookie_value(headers, CSRF_COOKIE_NAME).filter(|token| is_valid_token(token))
}

fn cookie_value(headers: &HeaderMap, name: &str) -> Option<String> {
    let cookie = headers.get(header::COOKIE)?.to_str().ok()?;
    cookie.split(';').find_map(|part| {
        let (key, value) = part.trim().split_once('=')?;
        (key == name).then(|| value.to_string())
    })
}

/// Fallible identity lookup for middleware and endpoints that need to
/// distinguish an invalid session from an authentication-store failure.
pub fn identify_request_result(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<Option<Identity>, String> {
    if !state.require_auth {
        return Ok(Some(Identity::local_owner()));
    }
    let Some(token) = token_from_headers(headers) else {
        return Ok(None);
    };
    state.auth.identify_session(&token)
}

/// Compatibility wrapper used by `/api/auth/me` and older callers.
#[allow(dead_code)]
pub fn identify_request(state: &AppState, headers: &HeaderMap) -> Option<Identity> {
    identify_request_result(state, headers).ok().flatten()
}

/// Guards API routes in LAN mode. Personal loopback mode is a synthetic owner
/// and remains password-free.
pub async fn require_auth_mw(
    State(state): State<AppState>,
    mut req: Request,
    next: Next,
) -> Response {
    if !state.require_auth {
        let identity = Identity::local_owner();
        record_observed_identity(&req, &identity);
        req.extensions_mut().insert(identity);
        return next.run(req).await;
    }
    let Some(path) = api_relative_path(req.uri().path()) else {
        return next.run(req).await;
    };
    if matches!(path, "/health" | "/auth/login" | "/auth/me" | "/me") {
        return next.run(req).await;
    }

    let identity = match identify_request_result(&state, req.headers()) {
        Ok(Some(identity)) => identity,
        Ok(None) => {
            return ApiError::new(
                StatusCode::UNAUTHORIZED,
                "unauthorized",
                "Sign in to use this workspace.",
            )
            .into_response();
        }
        Err(err) => return ApiError::internal("identify session", err).into_response(),
    };
    record_observed_identity(&req, &identity);

    if is_mutating_method(req.method()) {
        match state.auth.validate_csrf_headers(req.headers()) {
            Ok(true) => {}
            Ok(false) => {
                return ApiError::new(
                    StatusCode::FORBIDDEN,
                    "invalid_csrf",
                    "This action could not be verified. Refresh the page and try again.",
                )
                .into_response();
            }
            Err(err) => return ApiError::internal("validate request token", err).into_response(),
        }
    }

    if let Some(permission) = required_permission(req.method(), path)
        && authorize(&identity, permission).is_err()
    {
        return ApiError::new(
            StatusCode::FORBIDDEN,
            "forbidden",
            "Your account does not have permission to perform this action.",
        )
        .into_response();
    }
    req.extensions_mut().insert(identity);
    next.run(req).await
}

fn record_observed_identity(request: &Request, identity: &Identity) {
    if let Some(context) = request
        .extensions()
        .get::<crate::server::observability::RequestContext>()
    {
        context.record_identity(identity);
    }
}

fn is_mutating_method(method: &Method) -> bool {
    matches!(
        *method,
        Method::POST | Method::PUT | Method::PATCH | Method::DELETE
    )
}

fn api_relative_path(path: &str) -> Option<&str> {
    strip_api_prefix(path, "/api/v2").or_else(|| strip_api_prefix(path, "/api"))
}

fn strip_api_prefix<'a>(path: &'a str, prefix: &str) -> Option<&'a str> {
    let suffix = path.strip_prefix(prefix)?;
    if suffix.is_empty() {
        Some("/")
    } else if suffix.starts_with('/') {
        Some(suffix)
    } else {
        None
    }
}

fn required_permission(method: &Method, path: &str) -> Option<Permission> {
    if path.starts_with("/auth/users") || path.starts_with("/admin/users") {
        Some(Permission::ManageUsers)
    } else if path.starts_with("/database/") || path.starts_with("/admin/duckdb/") {
        Some(Permission::MaintainDatabase)
    } else if path.starts_with("/admin/network") {
        Some(Permission::ManageNetwork)
    } else if path.ends_with("/semantic")
        // Pinning the workspace currency or weight unit reinterprets every
        // stored number for everyone, so it is a mapping decision, not a read.
        || (path == "/schema/fixed-values" && is_mutating_method(method))
        || (path.starts_with("/imports/profiles") && is_mutating_method(method))
    {
        Some(Permission::ManageMappings)
    } else if method == Method::POST && (path == "/imports" || path == "/imports/peek") {
        Some(Permission::Import)
    } else {
        None
    }
}

#[derive(Debug)]
struct StoredUser {
    id: String,
    role: Role,
    enabled: bool,
    session_epoch: i64,
}

fn user_by_username(tx: &Transaction<'_>, username: &str) -> Result<Option<StoredUser>, String> {
    tx.query_row(
        "SELECT id, role, enabled, session_epoch
         FROM users WHERE username = ?1 COLLATE NOCASE",
        [username],
        |row| {
            let role = row.get::<_, String>(1)?;
            Ok(StoredUser {
                id: row.get(0)?,
                role: Role::parse(&role).unwrap_or(Role::Viewer),
                enabled: row.get::<_, i64>(2)? != 0,
                session_epoch: row.get(3)?,
            })
        },
    )
    .optional()
    .map_err(|err| err.to_string())
}

fn active_owner_count(tx: &Transaction<'_>) -> Result<u64, String> {
    tx.query_row(
        "SELECT COUNT(*) FROM users
         WHERE enabled = 1 AND lower(role) = 'owner'",
        [],
        |row| row.get::<_, i64>(0),
    )
    .map(|count| count as u64)
    .map_err(|err| err.to_string())
}

fn protect_owner_transition(
    tx: &Transaction<'_>,
    old_role: Role,
    old_enabled: bool,
    new_role: Role,
    new_enabled: bool,
) -> Result<(), String> {
    let removes_active_owner =
        old_enabled && old_role == Role::Owner && (!new_enabled || new_role != Role::Owner);
    if removes_active_owner && active_owner_count(tx)? <= 1 {
        return Err("Cannot disable or demote the last enabled owner.".to_string());
    }
    Ok(())
}

fn revoke_user_sessions_tx(
    tx: &Transaction<'_>,
    user_id: &str,
    revoked_at: i64,
) -> Result<(), String> {
    tx.execute(
        "UPDATE sessions SET revoked_at = COALESCE(revoked_at, ?1) WHERE user_id = ?2",
        rusqlite::params![revoked_at, user_id],
    )
    .map_err(|err| err.to_string())?;
    Ok(())
}

fn prune_sessions_tx(tx: &Transaction<'_>, now: i64) -> Result<(), String> {
    tx.execute(
        "DELETE FROM sessions
         WHERE revoked_at IS NOT NULL
            OR idle_expires_at <= ?1
            OR absolute_expires_at <= ?1",
        [now],
    )
    .map_err(|err| err.to_string())?;
    Ok(())
}

fn trim_user_sessions_tx(
    tx: &Transaction<'_>,
    user_id: &str,
    now: i64,
    keep: usize,
) -> Result<(), String> {
    tx.execute(
        "DELETE FROM sessions
         WHERE user_id = ?1
           AND id NOT IN (
               SELECT id FROM sessions
               WHERE user_id = ?1
                 AND revoked_at IS NULL
                 AND idle_expires_at > ?2
                 AND absolute_expires_at > ?2
               ORDER BY created_at DESC, id DESC
               LIMIT ?3
           )",
        rusqlite::params![user_id, now, keep as i64],
    )
    .map_err(|err| err.to_string())?;
    Ok(())
}

fn migrate_auth_schema(conn: &mut Connection) -> Result<(), String> {
    let tx = conn.transaction().map_err(|err| err.to_string())?;
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS users (
             username TEXT PRIMARY KEY COLLATE NOCASE,
             password_hash TEXT NOT NULL,
             role TEXT NOT NULL,
             created_at TEXT NOT NULL,
             id TEXT,
             enabled INTEGER NOT NULL DEFAULT 1,
             session_epoch INTEGER NOT NULL DEFAULT 0,
             updated_at TEXT NOT NULL DEFAULT ''
         );",
    )
    .map_err(|err| err.to_string())?;

    add_column_if_missing(&tx, "users", "id", "TEXT")?;
    add_column_if_missing(&tx, "users", "enabled", "INTEGER NOT NULL DEFAULT 1")?;
    add_column_if_missing(&tx, "users", "session_epoch", "INTEGER NOT NULL DEFAULT 0")?;
    add_column_if_missing(&tx, "users", "updated_at", "TEXT NOT NULL DEFAULT ''")?;

    let missing_ids = {
        let mut stmt = tx
            .prepare("SELECT rowid FROM users WHERE id IS NULL OR trim(id) = ''")
            .map_err(|err| err.to_string())?;
        let rows = stmt
            .query_map([], |row| row.get::<_, i64>(0))
            .map_err(|err| err.to_string())?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|err| err.to_string())?
    };
    for rowid in missing_ids {
        tx.execute(
            "UPDATE users SET id = ?1 WHERE rowid = ?2",
            rusqlite::params![random_identifier()?, rowid],
        )
        .map_err(|err| err.to_string())?;
    }

    tx.execute_batch(
        "UPDATE users SET role = lower(trim(role));
         UPDATE users SET updated_at = created_at WHERE updated_at = '';
         CREATE UNIQUE INDEX IF NOT EXISTS idx_users_stable_id ON users(id);",
    )
    .map_err(|err| err.to_string())?;

    let enabled_owner_count: i64 = tx
        .query_row(
            "SELECT COUNT(*) FROM users WHERE enabled = 1 AND lower(role) = 'owner'",
            [],
            |row| row.get(0),
        )
        .map_err(|err| err.to_string())?;
    if enabled_owner_count == 0 {
        let earliest_admin = tx
            .query_row(
                "SELECT rowid FROM users
                 WHERE enabled = 1 AND lower(role) = 'admin'
                 ORDER BY created_at, rowid LIMIT 1",
                [],
                |row| row.get::<_, i64>(0),
            )
            .optional()
            .map_err(|err| err.to_string())?;
        let owner_rowid = match earliest_admin {
            Some(rowid) => Some(rowid),
            None => tx
                .query_row(
                    "SELECT rowid FROM users
                     ORDER BY enabled DESC, created_at, rowid LIMIT 1",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .optional()
                .map_err(|err| err.to_string())?,
        };
        if let Some(rowid) = owner_rowid {
            tx.execute(
                "UPDATE users SET role = 'owner', enabled = 1 WHERE rowid = ?1",
                [rowid],
            )
            .map_err(|err| err.to_string())?;
        }
    }

    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS sessions (
             id INTEGER PRIMARY KEY AUTOINCREMENT,
             token_hash TEXT NOT NULL UNIQUE,
             csrf_hash TEXT NOT NULL,
             user_id TEXT NOT NULL,
             role TEXT NOT NULL,
             session_epoch INTEGER NOT NULL,
             created_at INTEGER NOT NULL,
             last_seen_at INTEGER NOT NULL,
             idle_expires_at INTEGER NOT NULL,
             absolute_expires_at INTEGER NOT NULL,
             revoked_at INTEGER,
             FOREIGN KEY(user_id) REFERENCES users(id) ON DELETE CASCADE
         );
         CREATE INDEX IF NOT EXISTS idx_sessions_user ON sessions(user_id);
         CREATE INDEX IF NOT EXISTS idx_sessions_expiry
             ON sessions(revoked_at, idle_expires_at, absolute_expires_at);
         CREATE TABLE IF NOT EXISTS auth_meta (
             key TEXT PRIMARY KEY,
             value TEXT NOT NULL
         );
         INSERT INTO auth_meta(key, value) VALUES ('schema_version', '2')
         ON CONFLICT(key) DO UPDATE SET value = excluded.value;",
    )
    .map_err(|err| err.to_string())?;

    let now = unix_timestamp()?;
    tx.execute(
        "DELETE FROM sessions
         WHERE (revoked_at IS NOT NULL OR absolute_expires_at <= ?1)
           AND created_at < ?2",
        rusqlite::params![now, now - ABSOLUTE_SESSION_TTL.as_secs() as i64],
    )
    .map_err(|err| err.to_string())?;
    tx.commit().map_err(|err| err.to_string())
}

fn add_column_if_missing(
    tx: &Transaction<'_>,
    table: &str,
    column: &str,
    definition: &str,
) -> Result<(), String> {
    let mut stmt = tx
        .prepare(&format!("PRAGMA table_info({table})"))
        .map_err(|err| err.to_string())?;
    let columns = stmt
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(|err| err.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|err| err.to_string())?;
    if !columns.iter().any(|name| name == column) {
        tx.execute_batch(&format!(
            "ALTER TABLE {table} ADD COLUMN {column} {definition};"
        ))
        .map_err(|err| err.to_string())?;
    }
    Ok(())
}

fn validate_username(username: &str) -> Result<&str, String> {
    let username = username.trim();
    if username.is_empty() {
        Err("Username cannot be empty.".to_string())
    } else if username.chars().count() > 128 {
        Err("Username cannot exceed 128 characters.".to_string())
    } else {
        Ok(username)
    }
}

fn validate_password(password: &str) -> Result<(), String> {
    if password.len() < 8 {
        Err("Password must be at least 8 characters.".to_string())
    } else if password.len() > MAX_PASSWORD_BYTES {
        Err(format!(
            "Password cannot exceed {MAX_PASSWORD_BYTES} bytes."
        ))
    } else {
        Ok(())
    }
}

fn hash_password(password: &str) -> Result<String, String> {
    use argon2::password_hash::SaltString;
    use argon2::{Argon2, PasswordHasher};

    let mut salt_bytes = [0u8; 16];
    getrandom::getrandom(&mut salt_bytes)
        .map_err(|err| format!("OS random generator failed: {err}"))?;
    let salt = SaltString::encode_b64(&salt_bytes).map_err(|err| err.to_string())?;
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|hash| hash.to_string())
        .map_err(|err| err.to_string())
}

fn verify_password(password: &str, stored: &str) -> bool {
    use argon2::password_hash::PasswordHash;
    use argon2::{Argon2, PasswordVerifier};

    match PasswordHash::new(stored) {
        Ok(parsed) => Argon2::default()
            .verify_password(password.as_bytes(), &parsed)
            .is_ok(),
        Err(_) => false,
    }
}

fn random_token() -> Result<String, String> {
    let mut bytes = [0u8; TOKEN_BYTES];
    getrandom::getrandom(&mut bytes).map_err(|err| format!("OS random generator failed: {err}"))?;
    Ok(hex_encode(&bytes))
}

fn random_identifier() -> Result<String, String> {
    let mut bytes = [0u8; 16];
    getrandom::getrandom(&mut bytes).map_err(|err| format!("OS random generator failed: {err}"))?;
    Ok(hex_encode(&bytes))
}

fn token_hash(token: &str) -> String {
    hex_encode(&Sha256::digest(token.as_bytes()))
}

fn is_valid_token(token: &str) -> bool {
    token.len() == TOKEN_HEX_LENGTH && token.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        encoded.push(HEX[(byte >> 4) as usize] as char);
        encoded.push(HEX[(byte & 0x0f) as usize] as char);
    }
    encoded
}

fn unix_timestamp() -> Result<i64, String> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .map_err(|err| format!("system clock is before Unix epoch: {err}"))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum::body::Body;
    use axum::http::Request as HttpRequest;
    use tower::ServiceExt;

    use super::*;
    use crate::db::Db;
    use crate::server::jobs::JobRegistry;
    use crate::server::state::AppStateInner;

    fn database_path(temp: &tempfile::TempDir) -> PathBuf {
        temp.path().join("workspace.db")
    }

    #[test]
    fn role_parsing_and_permissions_are_explicit() {
        assert_eq!(Role::parse(" OWNER "), Some(Role::Owner));
        assert_eq!(Role::parse("admin"), Some(Role::Admin));
        assert_eq!(Role::parse("EDITOR"), Some(Role::Editor));
        assert_eq!(Role::parse("viewer"), Some(Role::Viewer));
        assert_eq!(Role::parse("superuser"), None);

        let editor = Identity {
            user_id: "editor-id".to_string(),
            username: "editor".to_string(),
            role: Role::Editor,
        };
        assert!(authorize(&editor, Permission::Import).is_ok());
        assert!(authorize(&editor, Permission::ManageUsers).is_err());
        assert!(Role::Admin.allows(Permission::ManageUsers));
        assert!(!Role::Admin.allows(Permission::ManageOwners));
        assert!(Role::Owner.allows(Permission::ManageOwners));
    }

    #[test]
    fn migration_is_additive_idempotent_and_promotes_earliest_admin() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = database_path(&temp);
        let auth_path = auth_db_path(&db_path);
        let old_hash = hash_password("old-password").unwrap();
        {
            let conn = Connection::open(&auth_path).unwrap();
            conn.execute_batch(
                "CREATE TABLE users (
                     username TEXT PRIMARY KEY COLLATE NOCASE,
                     password_hash TEXT NOT NULL,
                     role TEXT NOT NULL,
                     created_at TEXT NOT NULL
                 );",
            )
            .unwrap();
            conn.execute(
                "INSERT INTO users VALUES (?1, ?2, 'admin', '2025-01-01T00:00:00Z')",
                rusqlite::params!["first", old_hash],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO users VALUES (?1, ?2, 'admin', '2025-02-01T00:00:00Z')",
                rusqlite::params!["second", hash_password("other-password").unwrap()],
            )
            .unwrap();
        }

        let store = AuthStore::open(&db_path).unwrap();
        let users = store.list_users().unwrap();
        assert_eq!(users.len(), 2);
        assert_eq!(users[0].role, "owner");
        assert_eq!(users[1].role, "admin");
        assert!(users.iter().all(|user| user.enabled && !user.id.is_empty()));
        assert_eq!(
            store.verify("first", "old-password").unwrap(),
            Some(Role::Owner)
        );
        drop(store);

        let reopened = AuthStore::open(&db_path).unwrap();
        assert_eq!(reopened.list_users().unwrap().len(), 2);
        let conn = reopened.connection().unwrap();
        let session_columns: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('sessions')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(session_columns, 11);
    }

    #[test]
    fn first_account_must_be_an_explicit_owner() {
        let temp = tempfile::tempdir().unwrap();
        let store = AuthStore::open(&database_path(&temp)).unwrap();

        let error = store
            .add_user("viewer", "strong-password", Role::Viewer)
            .unwrap_err();
        assert!(error.contains("first account") && error.contains("owner"));
        assert_eq!(store.user_count().unwrap(), 0);

        store
            .add_user("owner", "strong-password", Role::Owner)
            .unwrap();
        assert_eq!(store.user_role("owner").unwrap(), Some(Role::Owner));
    }

    #[test]
    fn persistent_session_survives_store_reopen_and_keeps_raw_tokens_out_of_db() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = database_path(&temp);
        let credentials = {
            let store = AuthStore::open(&db_path).unwrap();
            store
                .add_user("owner", "strong-password", Role::Owner)
                .unwrap();
            let credentials = store.create_session("owner").unwrap();
            assert_eq!(
                store
                    .identify_session(&credentials.token)
                    .unwrap()
                    .unwrap()
                    .role,
                Role::Owner
            );
            credentials
        };

        let reopened = AuthStore::open(&db_path).unwrap();
        let identity = reopened
            .identify_session(&credentials.token)
            .unwrap()
            .unwrap();
        assert_eq!(identity.username, "owner");
        assert!(
            reopened
                .validate_csrf(&credentials.token, &credentials.csrf_token)
                .unwrap()
        );
        let conn = reopened.connection().unwrap();
        let stored: (String, String) = conn
            .query_row(
                "SELECT token_hash, csrf_hash FROM sessions LIMIT 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_ne!(stored.0, credentials.token);
        assert_ne!(stored.1, credentials.csrf_token);
    }

    #[test]
    fn expired_revoked_and_epoch_stale_sessions_are_rejected() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = database_path(&temp);
        let store = AuthStore::open(&db_path).unwrap();
        store
            .add_user("owner", "strong-password", Role::Owner)
            .unwrap();

        let expired = store.create_session("owner").unwrap();
        {
            let conn = store.connection().unwrap();
            conn.execute(
                "UPDATE sessions SET idle_expires_at = 0 WHERE token_hash = ?1",
                [token_hash(&expired.token)],
            )
            .unwrap();
        }
        assert!(store.identify_session(&expired.token).unwrap().is_none());

        let revoked = store.create_session("owner").unwrap();
        store.revoke_session(&revoked.token).unwrap();
        assert!(store.identify_session(&revoked.token).unwrap().is_none());

        let stale = store.create_session("owner").unwrap();
        store.set_password("owner", "new-strong-password").unwrap();
        assert!(store.identify_session(&stale.token).unwrap().is_none());
    }

    #[test]
    fn last_enabled_owner_cannot_be_removed_disabled_or_demoted() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = database_path(&temp);
        let store = AuthStore::open(&db_path).unwrap();
        store
            .add_user("owner", "strong-password", Role::Owner)
            .unwrap();
        store
            .add_user("admin", "another-password", Role::Admin)
            .unwrap();

        assert!(store.remove_user("owner").is_err());
        assert!(store.set_user_enabled("owner", false).is_err());
        assert!(store.set_user_role("owner", Role::Editor).is_err());
        assert!(
            store
                .add_user("owner", "replacement-password", Role::Viewer)
                .is_err()
        );
        assert_eq!(
            store.verify("owner", "strong-password").unwrap(),
            Some(Role::Owner)
        );

        store
            .add_user("second-owner", "second-owner-password", Role::Owner)
            .unwrap();
        store.set_user_role("owner", Role::Editor).unwrap();
        assert!(store.remove_user("second-owner").is_err());
    }

    #[test]
    fn sessions_are_bounded_and_malformed_tokens_are_rejected() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = database_path(&temp);
        let store = AuthStore::open(&db_path).unwrap();
        store
            .add_user("owner", "strong-password", Role::Owner)
            .unwrap();

        assert!(store.identify_session("short").unwrap().is_none());
        assert!(!store.validate_csrf("short", "also-short").unwrap());

        let mut credentials = Vec::new();
        for _ in 0..(MAX_ACTIVE_SESSIONS_PER_USER + 2) {
            credentials.push(store.create_session("owner").unwrap());
        }
        let active: i64 = store
            .connection()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM sessions WHERE revoked_at IS NULL",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(active as usize, MAX_ACTIVE_SESSIONS_PER_USER);
        assert!(
            store
                .identify_session(&credentials[0].token)
                .unwrap()
                .is_none()
        );
        assert!(
            store
                .identify_session(&credentials.last().unwrap().token)
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn session_expiry_is_not_written_on_every_request() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = database_path(&temp);
        let store = AuthStore::open(&db_path).unwrap();
        store
            .add_user("owner", "strong-password", Role::Owner)
            .unwrap();
        let credentials = store.create_session("owner").unwrap();
        let token_hash = token_hash(&credentials.token);
        let now = unix_timestamp().unwrap();
        let recent = now - 30;
        store
            .connection()
            .unwrap()
            .execute(
                "UPDATE sessions SET last_seen_at = ?1 WHERE token_hash = ?2",
                rusqlite::params![recent, token_hash],
            )
            .unwrap();

        assert!(
            store
                .identify_session(&credentials.token)
                .unwrap()
                .is_some()
        );
        let last_seen: i64 = store
            .connection()
            .unwrap()
            .query_row(
                "SELECT last_seen_at FROM sessions WHERE token_hash = ?1",
                [&token_hash],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(last_seen, recent);
    }

    #[test]
    fn argon2_verification_admission_is_bounded() {
        let sessions = Sessions::new();
        let mut permits = Vec::new();
        for _ in 0..MAX_CONCURRENT_PASSWORD_VERIFICATIONS {
            permits.push(sessions.try_acquire_password_verification().unwrap());
        }
        assert!(sessions.try_acquire_password_verification().is_err());
        permits.pop();
        assert!(sessions.try_acquire_password_verification().is_ok());
    }

    #[test]
    fn cookies_and_csrf_headers_use_strict_policy() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = database_path(&temp);
        let store = AuthStore::open(&db_path).unwrap();
        store
            .add_user("owner", "strong-password", Role::Owner)
            .unwrap();
        let credentials = store.create_session("owner").unwrap();

        assert!(session_cookie(&credentials.token).contains("HttpOnly"));
        assert!(session_cookie(&credentials.token).contains("SameSite=Strict"));
        assert!(!csrf_cookie(&credentials.csrf_token).contains("HttpOnly"));

        let mut headers = HeaderMap::new();
        headers.insert(
            header::COOKIE,
            format!(
                "{COOKIE_NAME}={}; {CSRF_COOKIE_NAME}={}",
                credentials.token, credentials.csrf_token
            )
            .parse()
            .unwrap(),
        );
        headers.insert(CSRF_HEADER_NAME, credentials.csrf_token.parse().unwrap());
        assert!(store.validate_csrf_headers(&headers).unwrap());

        headers.insert(CSRF_HEADER_NAME, "wrong".parse().unwrap());
        assert!(!store.validate_csrf_headers(&headers).unwrap());
    }

    #[tokio::test]
    async fn lan_mutations_require_the_session_csrf_token() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = database_path(&temp);
        Db::open(&db_path).unwrap();
        let auth = AuthStore::open(&db_path).unwrap();
        auth.add_user("owner", "strong-password", Role::Owner)
            .unwrap();
        let missing_csrf = auth.create_session("owner").unwrap();
        let valid_csrf = auth.create_session("owner").unwrap();
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
        let app = crate::server::api::router(state.clone());

        let missing = HttpRequest::builder()
            .method(Method::POST)
            .uri("/api/auth/logout")
            .header(
                header::COOKIE,
                format!(
                    "{COOKIE_NAME}={}; {CSRF_COOKIE_NAME}={}",
                    missing_csrf.token, missing_csrf.csrf_token
                ),
            )
            .body(Body::empty())
            .unwrap();
        let missing_response = app.clone().oneshot(missing).await.unwrap();
        assert_eq!(missing_response.status(), StatusCode::FORBIDDEN);
        assert!(
            state
                .auth
                .identify_session(&missing_csrf.token)
                .unwrap()
                .is_some(),
            "a rejected request must not revoke the session"
        );

        let valid = HttpRequest::builder()
            .method(Method::POST)
            .uri("/api/auth/logout")
            .header(
                header::COOKIE,
                format!(
                    "{COOKIE_NAME}={}; {CSRF_COOKIE_NAME}={}",
                    valid_csrf.token, valid_csrf.csrf_token
                ),
            )
            .header(CSRF_HEADER_NAME, &valid_csrf.csrf_token)
            .body(Body::empty())
            .unwrap();
        let valid_response = app.oneshot(valid).await.unwrap();
        assert_eq!(valid_response.status(), StatusCode::OK);
        assert!(
            state
                .auth
                .identify_session(&valid_csrf.token)
                .unwrap()
                .is_none(),
            "a valid logout must revoke the session"
        );
    }
}
