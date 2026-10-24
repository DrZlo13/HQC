//! HTTP error type: `{error: <code>, message}` with a matching status.

use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("unauthorized: {0}")]
    Unauthorized(String),
    #[error("forbidden: {0}")]
    Forbidden(String),
    #[error("not found")]
    NotFound,
    #[error("{0}")]
    BadRequest(String),
    /// `400` with its own code (`path_not_found`, `outside_allowed_roots`, ...).
    #[error("{1}")]
    Invalid(&'static str, String),
    /// The request conflicts with the current state (already answered, archived, ...).
    #[error("{1}")]
    Conflict(&'static str, String),
    /// `409` whose body carries more fields than `{error, message}`
    /// (`workspace_conflict`, SPEC §6).
    #[error("{1}")]
    ConflictWith(
        &'static str,
        String,
        serde_json::Map<String, serde_json::Value>,
    ),
    /// The worker did not answer a request in time.
    #[error("the worker did not answer in time")]
    WorkerTimeout,
    #[error("too many requests")]
    TooManyRequests,
    /// `429` with its own code (`worker_busy`, SPEC §20).
    #[error("{1}")]
    Limited(&'static str, String),
    /// The body is over a size limit (an artifact over 100 MB, SPEC §12).
    #[error("{0}")]
    TooLarge(String),
    /// Too many requests of this kind in flight; retry later.
    #[error("busy, retry later")]
    Busy,
    #[error(transparent)]
    Internal(#[from] anyhow::Error),
}

impl From<sqlx::Error> for ApiError {
    fn from(e: sqlx::Error) -> Self {
        ApiError::Internal(e.into())
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, code) = match &self {
            ApiError::Unauthorized(_) => (StatusCode::UNAUTHORIZED, "unauthorized"),
            ApiError::Forbidden(_) => (StatusCode::FORBIDDEN, "forbidden"),
            ApiError::NotFound => (StatusCode::NOT_FOUND, "not_found"),
            ApiError::BadRequest(_) => (StatusCode::BAD_REQUEST, "bad_request"),
            ApiError::Invalid(code, _) => (StatusCode::BAD_REQUEST, *code),
            ApiError::Conflict(code, _) | ApiError::ConflictWith(code, _, _) => {
                (StatusCode::CONFLICT, *code)
            }
            ApiError::WorkerTimeout => (StatusCode::GATEWAY_TIMEOUT, "worker_timeout"),
            ApiError::TooManyRequests => (StatusCode::TOO_MANY_REQUESTS, "too_many_requests"),
            ApiError::Limited(code, _) => (StatusCode::TOO_MANY_REQUESTS, *code),
            ApiError::TooLarge(_) => (StatusCode::PAYLOAD_TOO_LARGE, "too_large"),
            ApiError::Busy => (StatusCode::SERVICE_UNAVAILABLE, "busy"),
            ApiError::Internal(e) => {
                tracing::error!(error = %e, "internal error");
                (StatusCode::INTERNAL_SERVER_ERROR, "internal")
            }
        };
        let message = match &self {
            ApiError::Internal(_) => "internal error".to_string(),
            other => other.to_string(),
        };
        let mut body = serde_json::Map::new();
        if let ApiError::ConflictWith(_, _, extra) = self {
            body = extra;
        }
        body.insert("error".into(), code.into());
        body.insert("message".into(), message.into());
        (status, Json(serde_json::Value::Object(body))).into_response()
    }
}

pub type ApiResult<T> = Result<T, ApiError>;
