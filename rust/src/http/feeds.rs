use super::AppState;
use super::proxy::request_base;
use crate::{error::RequestError, render::RenderKind};
use axum::{
    Json,
    body::Body,
    extract::{Path, State},
    http::{HeaderMap, HeaderValue, StatusCode, Uri, header},
    response::{IntoResponse, Response},
};
use std::sync::Arc;

const NO_CACHE: [(&str, &str); 3] = [
    ("cache-control", "no-store, no-cache, max-age=0"),
    ("pragma", "no-cache"),
    ("expires", "0"),
];

pub(crate) async fn healthz(State(state): State<Arc<AppState>>) -> Response {
    let snapshot = state.snapshot.read().await;
    let mut response = (
        StatusCode::OK,
        Json(serde_json::json!({"status":"ok", "channels":snapshot.channels().len()})),
    )
        .into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json; charset=utf-8"),
    );
    response
}

async fn feed(
    state: &AppState,
    uri: &Uri,
    headers: &HeaderMap,
    kind: RenderKind,
    content_type: &'static str,
) -> Result<Response, RequestError> {
    let snapshot = state.snapshot.read().await.clone();
    let base = request_base(uri, headers).map_err(|()| {
        tracing::warn!("feed request base URL was invalid");
        RequestError::new("epg-guide", "request base URL was invalid")
    })?;
    let bytes = snapshot.render(kind, &base).map_err(|error| {
        tracing::warn!(%error, "feed render failed");
        RequestError::new("epg-guide", error.to_string())
    })?;
    let mut response = Response::new(Body::from(bytes.to_vec()));
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    for (name, value) in NO_CACHE {
        response
            .headers_mut()
            .insert(name, HeaderValue::from_static(value));
    }
    Ok(response)
}

pub(crate) async fn channels(
    State(state): State<Arc<AppState>>,
    uri: Uri,
    headers: HeaderMap,
) -> Result<Response, RequestError> {
    feed(
        &state,
        &uri,
        &headers,
        RenderKind::M3u,
        "audio/x-mpegurl; charset=utf-8",
    )
    .await
}
pub(crate) async fn xmltv(
    State(state): State<Arc<AppState>>,
    uri: Uri,
    headers: HeaderMap,
) -> Result<Response, RequestError> {
    feed(&state, &uri, &headers, RenderKind::Xmltv, "application/xml").await
}

pub(crate) async fn logo(
    State(state): State<Arc<AppState>>,
    Path(channel_id): Path<String>,
) -> Result<Response, RequestError> {
    let catalog = state
        .logos
        .as_ref()
        .ok_or_else(|| RequestError::new("logo", "unknown or unavailable channel logo"))?;
    let asset = catalog
        .read(&channel_id)
        .map_err(|_| RequestError::new("logo", "unknown or unavailable channel logo"))?
        .ok_or_else(|| RequestError::new("logo", "unknown or unavailable channel logo"))?;
    let mut response = Response::new(Body::from(asset.bytes));
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(asset.content_type),
    );
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static(asset.cache_control),
    );
    Ok(response)
}
