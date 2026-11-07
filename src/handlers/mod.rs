//! HTTP request handlers.

pub mod admin_bucket;
pub mod admin_maintenance;
pub mod admin_object;
pub mod admin_trash;
pub mod app;
pub mod bucket;
pub mod object;

use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use serde::Serialize;

/// Standard error response format.
#[derive(Serialize)]
pub struct ErrorResponse {
    pub code: u16,
    pub message: String,
}

/// Create a JSON error response.
pub fn error_response(status: StatusCode, message: impl Into<String>) -> Response {
    let body = Json(ErrorResponse {
        code: status.as_u16(),
        message: message.into(),
    });
    (status, body).into_response()
}

/// Standard paginated response.
#[derive(Serialize)]
pub struct PaginatedResponse<T> {
    pub items: Vec<T>,
    pub total: i64,
    pub offset: i64,
    pub limit: i64,
}
