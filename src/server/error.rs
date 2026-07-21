//! API error type. Every failure returned to the browser is a small JSON
//! `{ "error": { "code", "message" } }` object. Internal/unexpected errors are
//! logged to stderr and reported to the client with a generic message so raw
//! database or panic details never leak into the UI.

use axum::Json;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Serialize;

use crate::db::STATEMENT_DEADLINE_EXCEEDED;

pub struct ApiError {
    status: StatusCode,
    code: &'static str,
    message: String,
    retry_after_seconds: Option<u64>,
    log_context: Option<&'static str>,
}

#[derive(Clone, Copy)]
pub(crate) struct ApiErrorMetadata {
    pub code: &'static str,
    pub context: Option<&'static str>,
}

impl ApiError {
    pub fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
            retry_after_seconds: None,
            log_context: None,
        }
    }

    pub fn too_many_login_attempts(retry_after: std::time::Duration) -> Self {
        let seconds = retry_after
            .as_secs()
            .saturating_add(u64::from(retry_after.subsec_nanos() > 0))
            .max(1);
        Self {
            status: StatusCode::TOO_MANY_REQUESTS,
            code: "login_rate_limited",
            message: format!("Too many sign-in attempts. Wait {seconds} seconds and try again."),
            retry_after_seconds: Some(seconds),
            log_context: None,
        }
    }

    pub fn server_busy() -> Self {
        Self {
            status: StatusCode::SERVICE_UNAVAILABLE,
            code: "server_busy",
            message: "The server is busy. Try again shortly.".to_string(),
            retry_after_seconds: Some(1),
            log_context: None,
        }
    }

    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "bad_request", message)
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, "not_found", message)
    }

    pub fn conflict(message: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, "database_busy", message)
    }

    pub fn unsupported(message: impl Into<String>) -> Self {
        Self::new(StatusCode::UNPROCESSABLE_ENTITY, "unsupported", message)
    }

    pub fn payload_too_large(message: impl Into<String>) -> Self {
        Self::new(StatusCode::PAYLOAD_TOO_LARGE, "payload_too_large", message)
    }

    pub fn insufficient_storage(message: impl Into<String>) -> Self {
        Self::new(
            StatusCode::INSUFFICIENT_STORAGE,
            "insufficient_storage",
            message,
        )
    }

    /// A broad query interrupted by the statement deadline.
    pub fn query_timeout() -> Self {
        Self::new(
            StatusCode::BAD_REQUEST,
            "query_timeout",
            "The query is too broad and timed out. Add filters or a search term and try again.",
        )
    }

    /// An unexpected internal failure. The real detail is logged, not returned.
    pub fn internal(context: &'static str, _detail: impl std::fmt::Display) -> Self {
        let mut error = Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal",
            "The server hit an unexpected error. Use the request ID to find it in the application log.",
        );
        error.log_context = Some(context);
        error
    }

    /// Maps a database-layer `String` error into the right API error. Two kinds
    /// of failure are actually the caller's fault, not ours, and get a clean
    /// 400 instead of a scary "unexpected server error":
    ///   * the statement-deadline sentinel (query too broad);
    ///   * a search-input validation error (our compiler encodes these as
    ///     rusqlite `InvalidParameterName`, which stringifies with this prefix).
    pub fn from_db(context: &'static str, err: String) -> Self {
        if err == STATEMENT_DEADLINE_EXCEEDED {
            Self::query_timeout()
        } else if let Some(detail) = err.strip_prefix("Invalid parameter name: ") {
            Self::bad_request(sentence_case(detail))
        } else {
            Self::internal(context, err)
        }
    }
}

/// Upper-cases the first character so a lowercase compiler message reads as a
/// sentence in the UI ("range is only valid…" → "Range is only valid…").
fn sentence_case(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

#[derive(Serialize)]
struct ErrorBody<'a> {
    error: ErrorDetail<'a>,
}

#[derive(Serialize)]
struct ErrorDetail<'a> {
    code: &'a str,
    message: &'a str,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let metadata = ApiErrorMetadata {
            code: self.code,
            context: self.log_context,
        };
        let body = ErrorBody {
            error: ErrorDetail {
                code: self.code,
                message: &self.message,
            },
        };
        let mut response = (self.status, Json(body)).into_response();
        if let Some(seconds) = self.retry_after_seconds
            && let Ok(value) = HeaderValue::from_str(&seconds.to_string())
        {
            response.headers_mut().insert(header::RETRY_AFTER, value);
        }
        response.extensions_mut().insert(metadata);
        response
    }
}

/// Runs a blocking (SQLite) closure on the Tokio blocking pool and maps a
/// panic/join failure into a clean internal error.
pub async fn blocking<T, F>(context: &'static str, f: F) -> Result<T, ApiError>
where
    F: FnOnce() -> Result<T, ApiError> + Send + 'static,
    T: Send + 'static,
{
    match tokio::task::spawn_blocking(f).await {
        Ok(result) => result,
        Err(join) => Err(ApiError::internal(context, join)),
    }
}
