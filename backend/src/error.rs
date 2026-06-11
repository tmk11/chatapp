use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::Serialize;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum AppError {
    #[error("authentication failed")]
    Unauthorized,
    #[error("forbidden")]
    Forbidden,
    #[error("not found")]
    NotFound,
    #[error("resource conflict")]
    Conflict,
    #[error("invalid request: {0}")]
    BadRequest(String),
    #[error("payload too large")]
    PayloadTooLarge,
    #[error("internal server error")]
    Internal,
}

#[derive(Serialize)]
struct ErrorBody<'a> {
    error: &'a str,
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let status = match self {
            AppError::Unauthorized => StatusCode::UNAUTHORIZED,
            AppError::Forbidden => StatusCode::FORBIDDEN,
            AppError::NotFound => StatusCode::NOT_FOUND,
            AppError::Conflict => StatusCode::CONFLICT,
            AppError::BadRequest(_) => StatusCode::BAD_REQUEST,
            AppError::PayloadTooLarge => StatusCode::PAYLOAD_TOO_LARGE,
            AppError::Internal => StatusCode::INTERNAL_SERVER_ERROR,
        };
        let body = Json(ErrorBody {
            error: self.safe_message(),
        });
        (status, body).into_response()
    }
}

impl AppError {
    fn safe_message(&self) -> &'static str {
        match self {
            AppError::Unauthorized => "authentication failed",
            AppError::Forbidden => "forbidden",
            AppError::NotFound => "not found",
            AppError::Conflict => "resource conflict",
            AppError::BadRequest(_) => "invalid request",
            AppError::PayloadTooLarge => "payload too large",
            AppError::Internal => "internal server error",
        }
    }
}
