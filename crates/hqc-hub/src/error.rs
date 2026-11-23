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
    /// `422` with its own code: a well-formed body whose content is refused (a project icon
    /// that is not a small square PNG, JPEG or WebP, T49).
    #[error("{1}")]
    Unprocessable(&'static str, String),
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
    /// `502`: the worker answered a request with an `error` of its own, whose code and message
    /// the owner gets as they are (`session.withdraw`, `session.input_now`, SPEC 0.219).
    #[error("{1}")]
    WorkerError(String, String),
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
        let (status, code): (_, std::borrow::Cow<'static, str>) = match &self {
            ApiError::Unauthorized(_) => (StatusCode::UNAUTHORIZED, "unauthorized".into()),
            ApiError::Forbidden(_) => (StatusCode::FORBIDDEN, "forbidden".into()),
            ApiError::NotFound => (StatusCode::NOT_FOUND, "not_found".into()),
            ApiError::BadRequest(_) => (StatusCode::BAD_REQUEST, "bad_request".into()),
            ApiError::Invalid(code, _) => (StatusCode::BAD_REQUEST, (*code).into()),
            ApiError::Unprocessable(code, _) => (StatusCode::UNPROCESSABLE_ENTITY, (*code).into()),
            ApiError::Conflict(code, _) | ApiError::ConflictWith(code, _, _) => {
                (StatusCode::CONFLICT, (*code).into())
            }
            ApiError::WorkerTimeout => (StatusCode::GATEWAY_TIMEOUT, "worker_timeout".into()),
            ApiError::TooManyRequests => {
                (StatusCode::TOO_MANY_REQUESTS, "too_many_requests".into())
            }
            ApiError::Limited(code, _) => (StatusCode::TOO_MANY_REQUESTS, (*code).into()),
            ApiError::TooLarge(_) => (StatusCode::PAYLOAD_TOO_LARGE, "too_large".into()),
            ApiError::Busy => (StatusCode::SERVICE_UNAVAILABLE, "busy".into()),
            ApiError::WorkerError(code, _) => (StatusCode::BAD_GATEWAY, code.clone().into()),
            ApiError::Internal(e) => {
                tracing::error!(error = %e, "internal error");
                (StatusCode::INTERNAL_SERVER_ERROR, "internal".into())
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
        body.insert("error".into(), code.as_ref().into());
        body.insert("message".into(), message.into());
        (status, Json(serde_json::Value::Object(body))).into_response()
    }
}

pub type ApiResult<T> = Result<T, ApiError>;
