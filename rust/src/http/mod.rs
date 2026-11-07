use crate::{
    config::Config,
    epg::EpgClient,
    error::RequestError,
    guide::{GuideZone, parse_catchup_start},
    hls::{TokenStore, playlist::rewrite_playlist},
    logo_catalog::LogoCatalog,
    render::{BaseUrl, RenderKind},
    snapshot::Snapshot,
};
use axum::{
    Json, Router,
    body::Body,
    extract::{Path, Request, State},
    http::{
        HeaderMap, HeaderValue, StatusCode, Uri,
        header::{self, HOST},
    },
    response::{IntoResponse, Response},
    routing::{get, post},
};
use futures_util::stream;
use reqwest::Url;
use std::sync::Arc;
use tokio::sync::{Notify, RwLock, watch};

const NO_CACHE: [(&str, &str); 3] = [
    ("cache-control", "no-store, no-cache, max-age=0"),
    ("pragma", "no-cache"),
    ("expires", "0"),
];

#[derive(Clone)]
pub struct AppState {
    pub epg: EpgClient,
    pub snapshot: Arc<RwLock<Option<Arc<Snapshot>>>>,
    pub tokens: Arc<TokenStore>,
    pub logos: Option<Arc<LogoCatalog>>,
    pub zone: GuideZone,
    pub config: Config,
    pub refresh: Arc<Notify>,
    pub refresh_done: Arc<Notify>,
    pub shutdown: watch::Sender<bool>,
}
pub struct AppSignals {
    pub refresh: Arc<Notify>,
    pub refresh_done: Arc<Notify>,
    pub shutdown: watch::Sender<bool>,
}
impl AppState {
    pub fn new(
        epg: EpgClient,
        snapshot: Arc<RwLock<Option<Arc<Snapshot>>>>,
        tokens: Arc<TokenStore>,
        logos: Option<Arc<LogoCatalog>>,
        zone: GuideZone,
        config: Config,
        signals: AppSignals,
    ) -> Self {
        Self {
            epg,
            snapshot,
            tokens,
            logos,
            zone,
            config,
            refresh: signals.refresh,
            refresh_done: signals.refresh_done,
            shutdown: signals.shutdown,
        }
    }
}

pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/channels.m3u", get(channels))
        .route("/xmltv.xml", get(xmltv))
        .route("/logos/{channel_id}", get(logo))
        .route("/metadata/sync", post(sync))
        .route("/stream/{channel_id}.m3u8", get(stream))
        .route("/replay/{channel_id}/{programme_id}.m3u8", get(replay))
        .route("/catchup/{channel_id}.m3u8", get(catchup))
        .route("/_hls/{token}", get(resource))
        .with_state(state)
}

async fn healthz(State(state): State<Arc<AppState>>) -> Response {
    let snapshot = state.snapshot.read().await;
    match snapshot.as_ref() {
        Some(snapshot) => (
            StatusCode::OK,
            Json(serde_json::json!({"status":"ok", "channels":snapshot.channels().len()})),
        )
            .into_response(),
        None => RequestError::NotReady.into_response(),
    }
}
async fn feed(
    state: &AppState,
    uri: &Uri,
    headers: &HeaderMap,
    kind: RenderKind,
    content_type: &'static str,
) -> Result<Response, RequestError> {
    let snapshot = state
        .snapshot
        .read()
        .await
        .clone()
        .ok_or(RequestError::NotReady)?;
    let base = request_base(uri, headers).map_err(|()| RequestError::Epg)?;
    let bytes = snapshot
        .render(kind, &base)
        .map_err(|_| RequestError::Epg)?;
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
async fn channels(
    State(state): State<Arc<AppState>>,
    uri: Uri,
    headers: HeaderMap,
) -> Result<Response, RequestError> {
    feed(&state, &uri, &headers, RenderKind::M3u, "audio/x-mpegurl").await
}
async fn xmltv(
    State(state): State<Arc<AppState>>,
    uri: Uri,
    headers: HeaderMap,
) -> Result<Response, RequestError> {
    feed(&state, &uri, &headers, RenderKind::Xmltv, "application/xml").await
}

async fn logo(
    State(state): State<Arc<AppState>>,
    Path(channel_id): Path<String>,
) -> Result<Response, RequestError> {
    let catalog = state.logos.as_ref().ok_or(RequestError::UnknownLogo)?;
    let asset = catalog
        .read(&channel_id)
        .map_err(|_| RequestError::UnknownLogo)?
        .ok_or(RequestError::UnknownLogo)?;
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

async fn sync(State(state): State<Arc<AppState>>) -> Result<Response, RequestError> {
    let done = state.refresh_done.notified();
    state.refresh.notify_one();
    done.await;
    let snapshot = state
        .snapshot
        .read()
        .await
        .clone()
        .ok_or(RequestError::NotReady)?;
    Ok(Json(serde_json::json!({"channels": snapshot.channels().len(), "guide_programmes": snapshot.programmes().len()})).into_response())
}

async fn stream(
    State(state): State<Arc<AppState>>,
    Path(channel_id): Path<String>,
    request: Request,
) -> Response {
    if let Some(value) = query_value(request.uri().query(), "utc") {
        return replay_at(&state, &channel_id, &value, request).await;
    }
    let Some(snapshot) = state.snapshot.read().await.clone() else {
        return RequestError::NotReady.into_response();
    };
    let Some(channel) = snapshot
        .channels()
        .iter()
        .find(|channel| channel.channel_id == channel_id)
    else {
        return RequestError::UnknownChannel.into_response();
    };
    if channel.time_shift != "1" {
        return RequestError::NoReplay.into_response();
    }
    match state.epg.hls_url(&channel_id).await {
        Ok(url) => forward(&state, request, url).await,
        Err(_) => RequestError::Epg.into_response(),
    }
}
async fn replay(
    State(state): State<Arc<AppState>>,
    Path((channel_id, programme_id)): Path<(String, String)>,
    request: Request,
) -> Response {
    let Some(snapshot) = state.snapshot.read().await.clone() else {
        return RequestError::NotReady.into_response();
    };
    if !snapshot
        .channels()
        .iter()
        .any(|channel| channel.channel_id == channel_id)
    {
        return RequestError::UnknownChannel.into_response();
    }
    let Some(programme) = snapshot.programme_index().by_id(&channel_id, &programme_id) else {
        return RequestError::UnknownProgramme.into_response();
    };
    match state.epg.tvod_url(programme).await {
        Ok(url) => forward(&state, request, url).await,
        Err(_) => RequestError::Epg.into_response(),
    }
}
async fn catchup(
    State(state): State<Arc<AppState>>,
    Path(channel_id): Path<String>,
    request: Request,
) -> Response {
    let value = query_value(request.uri().query(), "utc")
        .or_else(|| query_value(request.uri().query(), "start"));
    replay_at(
        &state,
        &channel_id,
        value.as_deref().unwrap_or_default(),
        request,
    )
    .await
}
async fn replay_at(state: &AppState, channel_id: &str, value: &str, request: Request) -> Response {
    let Some(snapshot) = state.snapshot.read().await.clone() else {
        return RequestError::NotReady.into_response();
    };
    if !snapshot
        .channels()
        .iter()
        .any(|channel| channel.channel_id == channel_id)
    {
        return RequestError::UnknownChannel.into_response();
    }
    let Ok(timestamp) = parse_catchup_start(value, &state.zone) else {
        return RequestError::InvalidCatchup.into_response();
    };
    let Some(programme) = snapshot.programme_index().covering(channel_id, timestamp) else {
        return RequestError::UnknownProgramme.into_response();
    };
    match state.epg.tvod_url(programme).await {
        Ok(url) => forward(state, request, url).await,
        Err(_) => RequestError::Epg.into_response(),
    }
}

async fn resource(
    State(state): State<Arc<AppState>>,
    Path(token): Path<String>,
    request: Request,
) -> Response {
    let url = match state.tokens.resolve(&token) {
        Ok(resource) => resource.url,
        Err(_) => return RequestError::UnknownToken.into_response(),
    };
    forward(&state, request, url).await
}

async fn forward(state: &AppState, request: Request, url: String) -> Response {
    let Ok(url) = Url::parse(&url) else {
        return RequestError::Hls.into_response();
    };
    let mut builder = state.epg.http_client().get(url);
    for name in [header::ACCEPT, header::RANGE, header::USER_AGENT] {
        if let Some(value) = request.headers().get(&name) {
            builder = builder.header(name, value);
        }
    }
    let Ok(response) = builder.send().await else {
        return RequestError::Hls.into_response();
    };
    if !response.status().is_success() {
        return RequestError::Hls.into_response();
    }
    let final_url = response.url().clone();
    let content_type = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    let playlist = final_url.path().to_ascii_lowercase().ends_with(".m3u8")
        || content_type.to_ascii_lowercase().contains("mpegurl");
    if playlist {
        let bytes = match response.bytes().await {
            Ok(bytes) if bytes.len() <= 4 * 1024 * 1024 => bytes,
            _ => return RequestError::Hls.into_response(),
        };
        let rewritten = match rewrite_playlist(&bytes, &final_url, &state.tokens) {
            Ok(value) => value.body,
            Err(_) => return RequestError::Hls.into_response(),
        };
        return (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "application/vnd.apple.mpegurl")],
            rewritten,
        )
            .into_response();
    }
    let status = response.status();
    let mut headers = HeaderMap::new();
    for name in [
        header::CONTENT_TYPE,
        header::CONTENT_LENGTH,
        header::CONTENT_RANGE,
        header::ACCEPT_RANGES,
    ] {
        if let Some(value) = response.headers().get(&name) {
            headers.insert(name, value.clone());
        }
    }
    if content_type
        .split(';')
        .next()
        .is_some_and(|value| value.trim().eq_ignore_ascii_case("video/mpeg"))
    {
        headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("video/mp2t"));
    }
    let body = Body::from_stream(stream::unfold(response, |mut response| async {
        match response.chunk().await {
            Ok(Some(bytes)) => Some((Ok::<_, reqwest::Error>(bytes), response)),
            Ok(None) => None,
            Err(error) => Some((Err(error), response)),
        }
    }));
    (status, headers, body).into_response()
}

fn request_base(uri: &Uri, headers: &HeaderMap) -> Result<BaseUrl, ()> {
    let scheme = uri.scheme_str().unwrap_or("http");
    let host = headers
        .get(HOST)
        .and_then(|value| value.to_str().ok())
        .ok_or(())?;
    BaseUrl::parse(&format!("{scheme}://{host}")).map_err(|_| ())
}
fn query_value(query: Option<&str>, wanted: &str) -> Option<String> {
    query?
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .find_map(|(key, value)| (key == wanted).then(|| value.to_owned()))
}
