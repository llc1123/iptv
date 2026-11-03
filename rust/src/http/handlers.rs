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
use crate::guide::parse_catchup_start;
use crate::hls::RequestHeaders;
use crate::lifecycle::RequestGuard;
use crate::render::{BaseUrl, RenderKind};

use super::{AppState, ForwardRequest, HlsTarget};

const NO_CACHE: [(&str, &str); 3] = [
    ("cache-control", "no-store, no-cache, max-age=0"),
    ("pragma", "no-cache"),
    ("expires", "0"),
];
const MAX_ROUTE_COMPONENT_BYTES: usize = 1024;
const MAX_RAW_QUERY_BYTES: usize = 4096;

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
    let Some(channel_id) = route_component(&channel_id) else {
        return error_response(AppError::Request(RequestError::EpgReplay));
    };
    if let Some(query) = request.uri().query() {
        match legacy_catchup_start(query) {
            Ok(Some(start)) => {
                return replay_at(&state, channel_id, start, admission, request).await;
            }
            Ok(None) => {}
            Err(()) => return error_response(AppError::Request(RequestError::EpgReplay)),
        }
    }
    let Ok(url) = state.playback.resolve_live(&channel_id).await else {
        return error_response(AppError::Request(RequestError::EpgHttp));
    };
    forward(&state, request, admission, HlsTarget::Url(url)).await
}

/// Resolves one exact programme and forwards its TVOD playlist.
pub async fn replay(
    State(state): State<AppState>,
    Path((channel_id, programme_id)): Path<(String, String)>,
    Extension(admission): Extension<Arc<RequestGuard>>,
    request: Request,
) -> Response {
    let Some(channel_id) = route_component(&channel_id) else {
        return error_response(AppError::Request(RequestError::EpgReplay));
    };
    let Some(programme_id) = route_component(&programme_id) else {
        return error_response(AppError::Request(RequestError::EpgReplay));
    };
    let Ok(Some(snapshot)) = state.snapshots.load() else {
        return error_response(AppError::Request(RequestError::EpgReplay));
    };
    if !snapshot
        .channels()
        .iter()
        .any(|channel| channel.channel_id == channel_id)
    {
        return error_response(AppError::Request(RequestError::EpgReplay));
    }
    let Some(programme) = snapshot
        .programme_index()
        .by_id(&channel_id, &programme_id)
        .cloned()
    else {
        return error_response(AppError::Request(RequestError::EpgReplay));
    };
    resolve_and_forward(&state, programme, admission, request).await
}

/// Resolves the programme covering a form-encoded catch-up instant.
pub async fn catchup(
    State(state): State<AppState>,
    Path(channel_id): Path<String>,
    Extension(admission): Extension<Arc<RequestGuard>>,
    request: Request,
) -> Response {
    let Some(channel_id) = route_component(&channel_id) else {
        return error_response(AppError::Request(RequestError::EpgReplay));
    };
    let query = request.uri().query().unwrap_or_default();
    let Ok(Some(value)) = catchup_query_value(query) else {
        return error_response(AppError::Request(RequestError::EpgReplay));
    };
    let Ok(Some(snapshot)) = state.snapshots.load() else {
        return error_response(AppError::Request(RequestError::EpgReplay));
    };
    if !snapshot
        .channels()
        .iter()
        .any(|channel| channel.channel_id == channel_id)
    {
        return error_response(AppError::Request(RequestError::EpgReplay));
    }
    let Ok(start) = parse_catchup_start(&value, snapshot.guide_zone()) else {
        return error_response(AppError::Request(RequestError::EpgReplay));
    };
    let Some(programme) = snapshot
        .programme_index()
        .covering(&channel_id, start)
        .cloned()
    else {
        return error_response(AppError::Request(RequestError::EpgReplay));
    };
    resolve_and_forward(&state, programme, admission, request).await
}

async fn replay_at(
    state: &AppState,
    channel_id: String,
    start: String,
    admission: Arc<RequestGuard>,
    request: Request,
) -> Response {
    let Ok(Some(snapshot)) = state.snapshots.load() else {
        return error_response(AppError::Request(RequestError::EpgReplay));
    };
    if !snapshot
        .channels()
        .iter()
        .any(|channel| channel.channel_id == channel_id)
    {
        return error_response(AppError::Request(RequestError::EpgReplay));
    }
    let Ok(timestamp) = parse_catchup_start(&start, snapshot.guide_zone()) else {
        return error_response(AppError::Request(RequestError::EpgReplay));
    };
    let Some(programme) = snapshot
        .programme_index()
        .covering(&channel_id, timestamp)
        .cloned()
    else {
        return error_response(AppError::Request(RequestError::EpgReplay));
    };
    resolve_and_forward(state, programme, admission, request).await
}

async fn resolve_and_forward(
    state: &AppState,
    programme: crate::epg::Programme,
    admission: Arc<RequestGuard>,
    request: Request,
) -> Response {
    let Ok(url) = state
        .playback
        .resolve_replay(&programme, state.lifecycle.cancellation_token())
        .await
    else {
        return error_response(AppError::Request(RequestError::EpgReplay));
    };
    forward(state, request, admission, HlsTarget::Url(url)).await
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

fn route_component(value: &str) -> Option<String> {
    let value = value.strip_suffix(".m3u8").unwrap_or(value);
    if value.is_empty()
        || value.len() > MAX_ROUTE_COMPONENT_BYTES
        || value.chars().any(char::is_control)
        || value.contains('\\')
    {
        return None;
    }
    Some(value.to_owned())
}

fn legacy_catchup_start(query: &str) -> Result<Option<String>, ()> {
    let (utc, _) = parse_query(query)?;
    Ok(utc.filter(|value| !value.is_empty()))
}

fn catchup_query_value(query: &str) -> Result<Option<String>, ()> {
    let (utc, start) = parse_query(query)?;
    Ok(utc
        .filter(|value| !value.is_empty())
        .or_else(|| start.filter(|value| !value.is_empty())))
}

fn parse_query(query: &str) -> Result<(Option<String>, Option<String>), ()> {
    if query.len() > MAX_RAW_QUERY_BYTES {
        return Err(());
    }
    let mut utc = None;
    let mut start = None;
    for pair in query.split('&') {
        let (raw_key, raw_value) = pair.split_once('=').unwrap_or((pair, ""));
        let key = form_decode(raw_key)?;
        let value = form_decode(raw_value)?;
        match key.as_str() {
            "utc" if utc.is_none() => utc = Some(value),
            "start" if start.is_none() => start = Some(value),
            _ => {}
        }
    }
    Ok((utc, start))
}

fn form_decode(value: &str) -> Result<String, ()> {
    let mut decoded = Vec::with_capacity(value.len());
    let mut bytes = value.as_bytes().iter().copied();
    while let Some(byte) = bytes.next() {
        match byte {
            b'+' => decoded.push(b' '),
            b'%' => {
                let high = bytes.next().ok_or(())?;
                let low = bytes.next().ok_or(())?;
                decoded.push((hex_digit(high)? << 4) | hex_digit(low)?);
            }
            value => decoded.push(value),
        }
    }
    let decoded = String::from_utf8(decoded).map_err(|_| ())?;
    if decoded.chars().any(char::is_control) {
        return Err(());
    }
    Ok(decoded)
}

fn hex_digit(value: u8) -> Result<u8, ()> {
    match value {
        b'0'..=b'9' => Ok(value - b'0'),
        b'a'..=b'f' => Ok(value - b'a' + 10),
        b'A'..=b'F' => Ok(value - b'A' + 10),
        _ => Err(()),
    }
}

#[cfg(test)]
mod tests {
    use super::{catchup_query_value, form_decode, legacy_catchup_start, route_component};

    #[test]
    fn query_uses_first_occurrence_and_form_semantics() {
        assert_eq!(
            catchup_query_value("utc=&utc=2&start=1").ok().flatten(),
            Some("1".to_owned())
        );
        assert_eq!(
            catchup_query_value("start=1&utc=2&start=3").ok().flatten(),
            Some("2".to_owned())
        );
        assert_eq!(
            legacy_catchup_start("utc=1&start=2").ok().flatten(),
            Some("1".to_owned())
        );
        assert_eq!(
            form_decode("2024-01-01T00%3A00%3A00%2B08%3A00"),
            Ok("2024-01-01T00:00:00+08:00".to_owned())
        );
    }

    #[test]
    fn query_and_components_are_bounded_and_reject_controls() {
        assert!(catchup_query_value(&"x".repeat(4097)).is_err());
        assert!(form_decode("bad%00").is_err());
        assert!(route_component("").is_none());
        assert_eq!(route_component("p%2F1.m3u8"), Some("p%2F1".to_owned()));
        assert_eq!(route_component("p/1.m3u8"), Some("p/1".to_owned()));
        assert_eq!(
            route_component("p/1.m3u8.m3u8"),
            Some("p/1.m3u8".to_owned())
        );
    }
}
