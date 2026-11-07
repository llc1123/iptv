use axum::{
    Json,
    http::{HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use serde_json::json;
use std::fmt;

#[derive(Clone, Debug)]
pub struct RequestError {
    pub operation: &'static str,
    pub detail: String,
}

impl RequestError {
    pub fn new(operation: &'static str, detail: impl Into<String>) -> Self {
        Self {
            operation,
            detail: detail.into(),
        }
    }

    fn status(&self) -> StatusCode {
        if self.operation == "hls-proxy" {
            StatusCode::BAD_GATEWAY
        } else {
            StatusCode::SERVICE_UNAVAILABLE
        }
    }
}

impl fmt::Display for RequestError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.operation, self.detail)
    }
}

impl IntoResponse for RequestError {
    fn into_response(self) -> Response {
        let mut response = (
            self.status(),
            Json(json!({"error": format!("{}: {}", self.operation, self.detail)})),
        )
            .into_response();
        response.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json; charset=utf-8"),
        );
        response
    }
}
