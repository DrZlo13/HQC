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
    /// The request conflicts with the current state (already answered, archived, ...).
    #[error("{1}")]
    Conflict(&'static str, String),
    #[error("too many requests")]
    TooManyRequests,
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
            ApiError::Conflict(code, _) => (StatusCode::CONFLICT, *code),
            ApiError::TooManyRequests => (StatusCode::TOO_MANY_REQUESTS, "too_many_requests"),
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
        (
            status,
            Json(hqc_proto::api::ApiError {
                error: code.into(),
                message,
            }),
        )
            .into_response()
    }
}

pub type ApiResult<T> = Result<T, ApiError>;
