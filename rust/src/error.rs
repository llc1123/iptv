use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde_json::json;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum RequestError {
    #[error("unknown channel")]
    UnknownChannel,
    #[error("unknown programme")]
    UnknownProgramme,
    #[error("unknown or expired token")]
    UnknownToken,
    #[error("unknown or unavailable channel logo")]
    UnknownLogo,
    #[error("service is not ready")]
    NotReady,
    #[error("EPG request failed")]
    Epg,
    #[error("channel has no replay")]
    NoReplay,
    #[error("upstream HLS request failed")]
    Hls,
    #[error("invalid catch-up start time")]
    InvalidCatchup,
}

impl RequestError {
    const fn status(&self) -> StatusCode {
        match self {
            Self::Hls => StatusCode::BAD_GATEWAY,
            Self::UnknownChannel
            | Self::UnknownProgramme
            | Self::UnknownToken
            | Self::UnknownLogo
            | Self::InvalidCatchup => StatusCode::NOT_FOUND,
            Self::NotReady | Self::Epg | Self::NoReplay => StatusCode::SERVICE_UNAVAILABLE,
        }
    }
}

impl IntoResponse for RequestError {
    fn into_response(self) -> Response {
        (self.status(), Json(json!({"error": self.to_string()}))).into_response()
    }
}
