use crate::{
    epg::EpgClient,
    error::RequestError,
    guide::{GuideZone, parse_catchup_start},
    hls::TokenStore,
    logo_catalog::LogoCatalog,
    snapshot::Snapshot,
};
mod feeds;
mod proxy;
use axum::{
    Json, Router,
    extract::{Path, Request, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use feeds::{channels, healthz, logo, xmltv};
use proxy::{forward, query_value};
use std::sync::Arc;
use tokio::sync::{RwLock, mpsc, oneshot};

#[derive(Clone)]
pub struct AppState {
    pub epg: EpgClient,
    pub snapshot: Arc<RwLock<Arc<Snapshot>>>,
    pub tokens: Arc<TokenStore>,
    pub logos: Option<Arc<LogoCatalog>>,
    pub zone: GuideZone,
    pub refresh: mpsc::Sender<crate::refresh::RefreshReply>,
    pub metadata: MetadataPaths,
}
#[derive(Clone)]
pub struct MetadataPaths {
    pub catalog: String,
    pub guide_cache: String,
}
impl AppState {
    pub fn new(
        epg: EpgClient,
        snapshot: Arc<RwLock<Arc<Snapshot>>>,
        tokens: Arc<TokenStore>,
        logos: Option<Arc<LogoCatalog>>,
        zone: GuideZone,
        refresh: mpsc::Sender<crate::refresh::RefreshReply>,
        metadata: MetadataPaths,
    ) -> Self {
        Self {
            epg,
            snapshot,
            tokens,
            logos,
            zone,
            refresh,
            metadata,
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
        .route("/stream/{*channel_id}", get(stream))
        .route("/replay/{*path}", get(replay))
        .route("/catchup/{*channel_id}", get(catchup))
        .route("/_hls/{token}", get(resource))
        .with_state(state)
}

async fn sync(State(state): State<Arc<AppState>>) -> Result<Response, RequestError> {
    let (reply, result) = oneshot::channel();
    state.refresh.send(reply).await.map_err(|error| {
        tracing::warn!(%error, "metadata refresh request could not be queued");
        RequestError::new("service", "service is shutting down")
    })?;
    let counts = result
        .await
        .map_err(|error| {
            tracing::warn!(%error, "metadata refresh reply was dropped");
            RequestError::new("service", "service is shutting down")
        })?
        .map_err(|error| {
            tracing::warn!(%error, "metadata refresh failed");
            error
        })?;
    let mut response = Json(serde_json::json!({
        "catalog": state.metadata.catalog,
        "guide_cache": state.metadata.guide_cache,
        "channels": counts.channels,
        "logos": state.logos.as_ref().map_or(0, |catalog| catalog.logo_count()),
        "guide_programmes": counts.guide_programmes,
    }))
    .into_response();
    response.headers_mut().insert(
        axum::http::header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static("application/json; charset=utf-8"),
    );
    Ok(response)
}

async fn stream(
    State(state): State<Arc<AppState>>,
    Path(channel_id): Path<String>,
    request: Request,
) -> Response {
    let Some(channel_id) = route_component(&channel_id) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if let Some(value) = query_value(request.uri().query(), "utc") {
        return replay_at(&state, &channel_id, &value, request).await;
    }
    let snapshot = state.snapshot.read().await.clone();
    let Some(channel) = snapshot
        .channels()
        .iter()
        .find(|channel| channel.channel_id == channel_id)
    else {
        return RequestError::new("service", format!("unknown channel {channel_id}"))
            .into_response();
    };
    if channel.time_shift != "1" {
        return RequestError::new("epg-tvod", format!("channel {channel_id} has no replay"))
            .into_response();
    }
    match state.epg.hls_url(&channel_id).await {
        Ok(url) => forward(&state, request, url).await,
        Err(error) => {
            tracing::warn!(%error, channel_id, "live HLS URL lookup failed");
            RequestError::new("epg-tvod", error.to_string()).into_response()
        }
    }
}
async fn replay(
    State(state): State<Arc<AppState>>,
    Path(path): Path<String>,
    request: Request,
) -> Response {
    let Some(path) = path.trim_start_matches('/').strip_suffix(".m3u8") else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let Some((channel_id, programme_id)) = path.split_once('/') else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let snapshot = state.snapshot.read().await.clone();
    if !snapshot
        .channels()
        .iter()
        .any(|channel| channel.channel_id == channel_id)
    {
        return RequestError::new("service", format!("unknown channel {channel_id}"))
            .into_response();
    }
    let Some(programme) = snapshot.programme_index().by_id(channel_id, programme_id) else {
        return RequestError::new("epg-replay", format!("unknown programme {programme_id}"))
            .into_response();
    };
    match state.epg.tvod_url(programme).await {
        Ok(url) => forward(&state, request, url).await,
        Err(error) => {
            tracing::warn!(%error, channel_id, "replay URL lookup failed");
            RequestError::new("epg-replay", error.to_string()).into_response()
        }
    }
}
async fn catchup(
    State(state): State<Arc<AppState>>,
    Path(channel_id): Path<String>,
    request: Request,
) -> Response {
    let Some(channel_id) = route_component(&channel_id) else {
        return StatusCode::NOT_FOUND.into_response();
    };
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
    let snapshot = state.snapshot.read().await.clone();
    if !snapshot
        .channels()
        .iter()
        .any(|channel| channel.channel_id == channel_id)
    {
        return RequestError::new("service", format!("unknown channel {channel_id}"))
            .into_response();
    }
    let Ok(timestamp) = parse_catchup_start(value, &state.zone) else {
        return RequestError::new(
            "epg-replay",
            if value.is_empty() {
                "catch-up start time is required"
            } else {
                "invalid catch-up start time"
            },
        )
        .into_response();
    };
    let Some(programme) = snapshot.programme_index().covering(channel_id, timestamp) else {
        return RequestError::new(
            "epg-replay",
            format!("no programme covers timestamp {timestamp}"),
        )
        .into_response();
    };
    match state.epg.tvod_url(programme).await {
        Ok(url) => forward(state, request, url).await,
        Err(error) => {
            tracing::warn!(%error, channel_id, "catch-up URL lookup failed");
            RequestError::new("epg-replay", error.to_string()).into_response()
        }
    }
}

async fn resource(
    State(state): State<Arc<AppState>>,
    Path(token): Path<String>,
    request: Request,
) -> Response {
    let url = match state.tokens.resolve(&token) {
        Ok(resource) => resource.url,
        Err(_) => {
            return RequestError::new("hls-proxy", "playlist resource expired").into_response();
        }
    };
    forward(&state, request, url).await
}

fn route_component(value: &str) -> Option<String> {
    value
        .trim_start_matches('/')
        .strip_suffix(".m3u8")
        .map(str::to_owned)
}
