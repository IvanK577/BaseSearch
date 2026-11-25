//! Authentication endpoints: login, logout, session info, and admin user
//! management. Password verification (argon2) runs on the blocking pool.

use std::net::SocketAddr;

use axum::Json;
use axum::extract::{ConnectInfo, Path, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::server::auth::{
    Permission, Role, UserInfo, authorize, clear_cookie, clear_csrf_cookie, csrf_cookie,
    identify_request_result, session_cookie, token_from_headers,
};
use crate::server::error::{ApiError, blocking};
use crate::server::state::AppState;

#[derive(Serialize)]
pub struct UserDto {
    username: String,
    role: String,
}

#[derive(Deserialize)]
pub struct LoginRequest {
    username: String,
    password: String,
}

pub async fn login(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    State(state): State<AppState>,
    Json(req): Json<LoginRequest>,
) -> Result<Response, ApiError> {
    let username = req.username.trim().to_string();
    state
        .sessions
        .check_login(peer.ip(), &username)
        .map_err(ApiError::too_many_login_attempts)?;
    let _verification_permit = state
        .sessions
        .try_acquire_password_verification()
        .map_err(ApiError::too_many_login_attempts)?;
    state
        .sessions
        .check_login(peer.ip(), &username)
        .map_err(ApiError::too_many_login_attempts)?;
    let auth_state = state.clone();
    let verify_username = username.clone();
    let password = req.password;
    let authenticated = blocking("login", move || {
        let role = auth_state
            .auth
            .verify(&verify_username, &password)
            .map_err(|err| ApiError::internal("verify credentials", err))?;
        match role {
            Some(role) => {
                let credentials = auth_state
                    .auth
                    .create_session(&verify_username)
                    .map_err(|err| ApiError::internal("create session", err))?;
                Ok(Some((role, credentials)))
            }
            None => Ok(None),
        }
    })
    .await;

    let authenticated = authenticated?;

    match authenticated {
        Some((role, credentials)) => {
            state.sessions.clear_login_attempts(peer.ip(), &username);
            let mut response = Json(UserDto {
                username,
                role: role.as_str().to_string(),
            })
            .into_response();
            set_cookie(&mut response, &session_cookie(&credentials.token));
            set_cookie(&mut response, &csrf_cookie(&credentials.csrf_token));
            Ok(response)
        }
        None => {
            state.sessions.record_login_failure(peer.ip(), &username);
            Err(ApiError::new(
                StatusCode::UNAUTHORIZED,
                "invalid_credentials",
                "Wrong username or password.",
            ))
        }
    }
}

pub async fn logout(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    if let Some(token) = token_from_headers(&headers) {
        blocking("logout", move || {
            state
                .auth
                .revoke_session(&token)
                .map_err(|err| ApiError::internal("revoke session", err))
        })
        .await?;
    }
    let mut response = Json(json!({ "ok": true })).into_response();
    set_cookie(&mut response, &clear_cookie());
    set_cookie(&mut response, &clear_csrf_cookie());
    Ok(response)
}

#[derive(Serialize)]
pub struct MeDto {
    /// True when this server enforces authentication (non-loopback bind).
    required: bool,
    authenticated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    user: Option<UserDto>,
}

pub async fn me(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<MeDto>, ApiError> {
    let required = state.require_auth;
    let identity = blocking("current session", move || {
        identify_request_result(&state, &headers)
            .map_err(|err| ApiError::internal("identify session", err))
    })
    .await?;
    Ok(Json(MeDto {
        required,
        authenticated: identity.is_some(),
        user: identity.map(|id| UserDto {
            username: id.username,
            role: id.role.as_str().to_string(),
        }),
    }))
}

pub async fn list_users(State(state): State<AppState>) -> Result<Json<Vec<UserInfo>>, ApiError> {
    let users = blocking("list users", move || {
        state
            .auth
            .list_users()
            .map_err(|err| ApiError::internal("list users", err))
    })
    .await?;
    Ok(Json(users))
}

#[derive(Deserialize)]
pub struct CreateUserRequest {
    username: String,
    password: String,
    #[serde(default)]
    role: Option<String>,
}

pub async fn create_user(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<CreateUserRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let requested_role = match req.role.as_deref() {
        Some(value) => Some(Role::parse(value).ok_or_else(|| {
            ApiError::bad_request("Role must be owner, admin, editor, or viewer.")
        })?),
        None => None,
    };
    let identity = identify_request_result(&state, &headers)
        .map_err(|err| ApiError::internal("identify session", err))?;
    let can_manage_owners = identity
        .as_ref()
        .is_some_and(|identity| authorize(identity, Permission::ManageOwners).is_ok());
    let response_username = req.username.trim().to_string();
    let mutation = blocking("create user", move || {
        let existing_role = state
            .auth
            .user_role(&req.username)
            .map_err(|err| ApiError::internal("inspect user", err))?;
        let effective_role = requested_role.or(existing_role).unwrap_or(Role::Viewer);
        if (effective_role == Role::Owner || existing_role == Some(Role::Owner))
            && !can_manage_owners
        {
            return Err(ApiError::new(
                StatusCode::FORBIDDEN,
                "forbidden",
                "Only the workspace owner can create or modify owner accounts.",
            ));
        }
        state
            .auth
            .add_user_with_result(&req.username, &req.password, effective_role)
            .map_err(ApiError::bad_request)
    })
    .await?;
    Ok(Json(json!({
        "ok": true,
        "username": response_username,
        "role": mutation.role.as_str(),
        "action": if mutation.created { "created" } else { "updated" }
    })))
}

pub async fn delete_user(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(username): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let identity = identify_request_result(&state, &headers)
        .map_err(|err| ApiError::internal("identify session", err))?;
    let can_manage_owners = identity
        .as_ref()
        .is_some_and(|identity| authorize(identity, Permission::ManageOwners).is_ok());
    let removed = blocking("delete user", move || {
        let existing_role = state
            .auth
            .user_role(&username)
            .map_err(|err| ApiError::internal("inspect user", err))?;
        if existing_role == Some(Role::Owner) && !can_manage_owners {
            return Err(ApiError::new(
                StatusCode::FORBIDDEN,
                "forbidden",
                "Only the workspace owner can remove an owner account.",
            ));
        }
        state
            .auth
            .remove_user(&username)
            .map_err(ApiError::bad_request)
    })
    .await?;
    if !removed {
        return Err(ApiError::not_found("No such user."));
    }
    Ok(Json(json!({ "ok": true })))
}

fn set_cookie(response: &mut Response, value: &str) {
    if let Ok(header_value) = HeaderValue::from_str(value) {
        response
            .headers_mut()
            .append(header::SET_COOKIE, header_value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn multiple_auth_cookies_are_appended() {
        let mut response = Json(json!({ "ok": true })).into_response();
        set_cookie(
            &mut response,
            "bs_session=session; HttpOnly; SameSite=Strict",
        );
        set_cookie(&mut response, "bs_csrf=csrf; SameSite=Strict");

        let cookies = response.headers().get_all(header::SET_COOKIE);
        assert_eq!(cookies.iter().count(), 2);
    }
}
