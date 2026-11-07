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
    response::{IntoResponse, Response},
    routing::{get, post},
};
use feeds::{channels, healthz, logo, xmltv};
use proxy::{forward, query_value};
use std::sync::Arc;
use tokio::sync::{RwLock, mpsc, oneshot, watch};

#[derive(Clone)]
pub struct AppState {
    pub epg: EpgClient,
    pub snapshot: Arc<RwLock<Arc<Snapshot>>>,
    pub tokens: Arc<TokenStore>,
    pub logos: Option<Arc<LogoCatalog>>,
    pub zone: GuideZone,
    pub refresh: mpsc::Sender<crate::refresh::RefreshReply>,
    pub shutdown: watch::Sender<bool>,
}
pub struct ServiceControl {
    pub refresh: mpsc::Sender<crate::refresh::RefreshReply>,
    pub shutdown: watch::Sender<bool>,
}
impl AppState {
    pub fn new(
        epg: EpgClient,
        snapshot: Arc<RwLock<Arc<Snapshot>>>,
        tokens: Arc<TokenStore>,
        logos: Option<Arc<LogoCatalog>>,
        zone: GuideZone,
        control: ServiceControl,
    ) -> Self {
        Self {
            epg,
            snapshot,
            tokens,
            logos,
            zone,
            refresh: control.refresh,
            shutdown: control.shutdown,
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
        .route("/stream/{channel_id}", get(stream))
        .route("/replay/{channel_id}/{programme_id}", get(replay))
        .route("/catchup/{channel_id}", get(catchup))
        .route("/_hls/{token}", get(resource))
        .with_state(state)
}

async fn sync(State(state): State<Arc<AppState>>) -> Result<Response, RequestError> {
    let (reply, result) = oneshot::channel();
    state.refresh.send(reply).await.map_err(|error| {
        tracing::warn!(%error, "metadata refresh request could not be queued");
        RequestError::Epg
    })?;
    let counts = result
        .await
        .map_err(|error| {
            tracing::warn!(%error, "metadata refresh reply was dropped");
            RequestError::Epg
        })?
        .map_err(|error| {
            tracing::warn!(%error, "metadata refresh failed");
            RequestError::Epg
        })?;
    Ok(
        Json(serde_json::json!({"channels": counts.0, "guide_programmes": counts.1}))
            .into_response(),
    )
}

async fn stream(
    State(state): State<Arc<AppState>>,
    Path(channel_id): Path<String>,
    request: Request,
) -> Response {
    let channel_id = route_component(&channel_id);
    if let Some(value) = query_value(request.uri().query(), "utc") {
        return replay_at(&state, &channel_id, &value, request).await;
    }
    let snapshot = state.snapshot.read().await.clone();
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
        Err(error) => {
            tracing::warn!(%error, channel_id, "live HLS URL lookup failed");
            RequestError::Epg.into_response()
        }
    }
}
async fn replay(
    State(state): State<Arc<AppState>>,
    Path((channel_id, programme_id)): Path<(String, String)>,
    request: Request,
) -> Response {
    let channel_id = route_component(&channel_id);
    let programme_id = route_component(&programme_id);
    let snapshot = state.snapshot.read().await.clone();
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
        Err(error) => {
            tracing::warn!(%error, channel_id, "replay URL lookup failed");
            RequestError::Epg.into_response()
        }
    }
}
async fn catchup(
    State(state): State<Arc<AppState>>,
    Path(channel_id): Path<String>,
    request: Request,
) -> Response {
    let channel_id = route_component(&channel_id);
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
        Err(error) => {
            tracing::warn!(%error, channel_id, "catch-up URL lookup failed");
            RequestError::Epg.into_response()
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
        Err(_) => return RequestError::UnknownToken.into_response(),
    };
    forward(&state, request, url).await
}

fn route_component(value: &str) -> String {
    value.strip_suffix(".m3u8").unwrap_or(value).to_owned()
}
