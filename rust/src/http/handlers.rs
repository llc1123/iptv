//! Redacted route handlers for feeds, health, logos, and live HLS entrypoints.

use std::sync::Arc;

use axum::Json;
use axum::body::Body;
use axum::extract::{Extension, Path, Request, State};
use axum::http::header::{
    ACCEPT, ACCEPT_RANGES, CACHE_CONTROL, CONTENT_LENGTH, CONTENT_RANGE, CONTENT_TYPE, HOST, RANGE,
};
use axum::http::{HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};

use crate::error::{AppError, RequestError};
use crate::hls::RequestHeaders;
use crate::lifecycle::RequestGuard;
use crate::render::{BaseUrl, RenderKind};

use super::{AppState, ForwardRequest, HlsTarget};

const NO_CACHE: [(&str, &str); 3] = [
    ("cache-control", "no-store, no-cache, max-age=0"),
    ("pragma", "no-cache"),
    ("expires", "0"),
];

/// Returns the health projection at the injected clock instant.
pub async fn healthz(State(state): State<AppState>) -> Response {
    let (status, view) = state.health.response_at(state.clock.now()).await;
    let status = if status == 200 {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    (status, Json(view)).into_response()
}

/// Renders the current M3U feed from one immutable snapshot.
pub async fn channels(State(state): State<AppState>, request: Request) -> Response {
    let base = request_base(&request);
    feed(&state, base, RenderKind::M3u, "audio/x-mpegurl").await
}

/// Renders the current XMLTV feed from one immutable snapshot.
pub async fn xmltv(State(state): State<AppState>, request: Request) -> Response {
    let base = request_base(&request);
    feed(&state, base, RenderKind::Xmltv, "application/xml").await
}

async fn feed(
    state: &AppState,
    base: Result<BaseUrl, AppError>,
    kind: RenderKind,
    content_type: &str,
) -> Response {
    let result = async {
        let base = base?;
        let snapshot = state
            .snapshots
            .load()
            .map_err(|_| AppError::Request(RequestError::GuideCache))?
            .ok_or(AppError::Request(RequestError::GuideCache))?;
        let bytes = snapshot
            .render_cached(kind, &base, Some(&state.lifecycle.cancellation_token()))
            .await
            .map_err(|_| AppError::Request(RequestError::GuideCache))?;
        Ok::<Arc<[u8]>, AppError>(bytes)
    }
    .await;
    let bytes = match result {
        Ok(bytes) => bytes,
        Err(error) => return error_response(error),
    };
    let mut response = Response::new(Body::from(bytes.to_vec()));
    *response.status_mut() = StatusCode::OK;
    let headers = response.headers_mut();
    if let Ok(value) = HeaderValue::from_str(content_type) {
        headers.insert(CONTENT_TYPE, value);
    }
    let length = bytes.len().to_string();
    if let Ok(value) = HeaderValue::from_str(&length) {
        headers.insert(CONTENT_LENGTH, value);
    }
    for (name, value) in NO_CACHE {
        if let Ok(header_value) = HeaderValue::from_str(value) {
            if let Ok(header_name) = name.parse::<axum::http::HeaderName>() {
                headers.insert(header_name, header_value);
            }
        }
    }
    response
}

/// Returns a bounded local logo, with optional exact byte-range support.
pub async fn logo(
    State(state): State<AppState>,
    Path(channel_id): Path<String>,
    request: Request,
) -> Response {
    let Some(catalog) = state.logos.as_ref() else {
        return error_response(AppError::Request(RequestError::Logo));
    };
    let range = match parse_range(request.headers().get(RANGE)) {
        Ok(range) => range,
        Err(error) => return error_response(error),
    };
    let Ok(Some(asset)) = catalog.read_range(&channel_id, range) else {
        return error_response(AppError::Request(RequestError::Logo));
    };
    let mut response = Response::new(Body::from(asset.bytes.clone()));
    *response.status_mut() = if range.is_some() {
        StatusCode::PARTIAL_CONTENT
    } else {
        StatusCode::OK
    };
    let headers = response.headers_mut();
    if let Ok(value) = HeaderValue::from_str(asset.content_type) {
        headers.insert(CONTENT_TYPE, value);
    }
    if let Ok(value) = HeaderValue::from_str(asset.cache_control) {
        headers.insert(CACHE_CONTROL, value);
    }
    headers.insert(ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    if let Ok(value) = HeaderValue::from_str(&asset.bytes.len().to_string()) {
        headers.insert(CONTENT_LENGTH, value);
    }
    if let Some((start, end)) = range {
        let value = format!("bytes {start}-{end}/*");
        if let Ok(value) = HeaderValue::from_str(&value) {
            headers.insert(CONTENT_RANGE, value);
        }
    }
    response
}

/// Resolves a live channel and forwards its HLS response.
pub async fn stream(
    State(state): State<AppState>,
    Path(channel_id): Path<String>,
    Extension(admission): Extension<Arc<RequestGuard>>,
    request: Request,
) -> Response {
    let channel_id = match channel_id.strip_suffix(".m3u8") {
        Some(value) => value,
        None => channel_id.as_str(),
    };
    let Ok(url) = state.playback.resolve_live(channel_id).await else {
        return error_response(AppError::Request(RequestError::EpgHttp));
    };
    forward(&state, request, admission, HlsTarget::Url(url)).await
}

/// Resolves a full captured token and forwards its HLS response.
pub async fn hls_resource(
    State(state): State<AppState>,
    Path(rest): Path<String>,
    Extension(admission): Extension<Arc<RequestGuard>>,
    request: Request,
) -> Response {
    forward(&state, request, admission, HlsTarget::Token(rest)).await
}

async fn forward(
    state: &AppState,
    request: Request,
    admission: Arc<RequestGuard>,
    target: HlsTarget,
) -> Response {
    let headers = RequestHeaders {
        accept: header_text(request.headers().get(ACCEPT)),
        range: header_text(request.headers().get(RANGE)),
        user_agent: header_text(request.headers().get(axum::http::header::USER_AGENT)),
    };
    let request = ForwardRequest {
        target,
        headers,
        cancellation: state.lifecycle.cancellation_token(),
        admission,
    };
    match state.hls.forward(request).await {
        Ok(response) => {
            let mut output = Response::new(response.body);
            *output.status_mut() = response.status;
            *output.headers_mut() = response.headers;
            output
        }
        Err(_) => error_response(AppError::Request(RequestError::HlsProxy)),
    }
}

/// Supplies the fixed fallback for unknown paths.
pub async fn not_found() -> Response {
    StatusCode::NOT_FOUND.into_response()
}

/// Converts an internal error into the fixed public JSON shape.
pub fn error_response(error: AppError) -> Response {
    let status = match error.http_status() {
        400 => StatusCode::BAD_REQUEST,
        502 => StatusCode::BAD_GATEWAY,
        _ => StatusCode::SERVICE_UNAVAILABLE,
    };
    (status, Json(error.response_body())).into_response()
}

fn request_base(request: &Request) -> Result<BaseUrl, AppError> {
    let scheme = request.uri().scheme_str().unwrap_or("http");
    let host = request
        .headers()
        .get(HOST)
        .and_then(|value| value.to_str().ok())
        .ok_or(AppError::Request(RequestError::GuideCache))?;
    BaseUrl::parse(&format!("{scheme}://{host}"))
        .map_err(|_| AppError::Request(RequestError::GuideCache))
}

fn parse_range(value: Option<&HeaderValue>) -> Result<Option<(u64, u64)>, AppError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let text = value
        .to_str()
        .map_err(|_| AppError::Request(RequestError::Logo))?;
    let Some(range) = text.strip_prefix("bytes=") else {
        return Err(AppError::Request(RequestError::Logo));
    };
    let Some((start, end)) = range.split_once('-') else {
        return Err(AppError::Request(RequestError::Logo));
    };
    let start = start
        .parse::<u64>()
        .map_err(|_| AppError::Request(RequestError::Logo))?;
    let end = end
        .parse::<u64>()
        .map_err(|_| AppError::Request(RequestError::Logo))?;
    if start > end {
        return Err(AppError::Request(RequestError::Logo));
    }
    Ok(Some((start, end)))
}

fn header_text(value: Option<&HeaderValue>) -> Option<String> {
    value
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
}
