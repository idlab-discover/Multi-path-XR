use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
};
use thiserror::Error;

/// Application error type (typed, no panics in hot paths).
#[derive(Debug, Error)]
pub enum AppError {
    #[error("bad request: {0}")]
    BadRequest(String),

    #[error("method not allowed")]
    MethodNotAllowed,

    #[error("configuration error: {0}")]
    Config(String),

    #[error("origin error: {0}")]
    Origin(String),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("internal error: {0}")]
    Internal(String),
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, msg) = match &self {
            AppError::BadRequest(_) => (StatusCode::BAD_REQUEST, self.to_string()),
            AppError::MethodNotAllowed => (StatusCode::METHOD_NOT_ALLOWED, self.to_string()),
            AppError::Config(_) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "config error".to_string(),
            ),
            AppError::Origin(_) => (StatusCode::BAD_GATEWAY, "upstream error".to_string()),
            AppError::Io(_) => (StatusCode::INTERNAL_SERVER_ERROR, "io error".to_string()),
            AppError::Internal(_) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal error".to_string(),
            ),
        };
        (status, msg).into_response()
    }
}
