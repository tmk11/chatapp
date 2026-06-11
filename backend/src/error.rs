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
    #[error("resource conflict")]
    Conflict,
    #[error("invalid request: {0}")]
    BadRequest(String),
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
            AppError::Conflict => StatusCode::CONFLICT,
            AppError::BadRequest(_) => StatusCode::BAD_REQUEST,
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
            AppError::Conflict => "resource conflict",
            AppError::BadRequest(_) => "invalid request",
            AppError::Internal => "internal server error",
        }
    }
}
