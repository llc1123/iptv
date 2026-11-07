use std::{future::Future, io, time::Duration};

use axum::{Router, routing::get};

use crate::health::HealthState;
use crate::lifecycle::Lifecycle;

use super::{AppState, handlers};

#[derive(Clone)]
pub struct ServeState {
    pub lifecycle: Lifecycle,
    pub health: HealthState,
    pub grace: Duration,
}

pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(handlers::healthz))
        .route("/channels.m3u", get(handlers::channels))
        .route("/xmltv.xml", get(handlers::xmltv))
        .route("/logos/{channel_id}", get(handlers::logo))
        .route("/stream/{channel_id}", get(handlers::stream))
        .route("/replay/{channel_id}/{programme_id}", get(handlers::replay))
        .route("/catchup/{channel_id}", get(handlers::catchup))
        .route("/_hls/{*rest}", get(handlers::hls_resource))
        .fallback(handlers::not_found)
        .with_state(state)
}

pub async fn serve<F>(
    router: Router,
    address: String,
    state: ServeState,
    shutdown: F,
) -> Result<(), io::Error>
where
    F: Future<Output = ()> + Send + 'static,
{
    let listener = tokio::net::TcpListener::bind(address).await?;
    axum::serve(listener, router)
        .with_graceful_shutdown(async move {
            shutdown.await;
            state.health.mark_draining().await;
            let _ = state.lifecycle.drain(state.grace).await;
        })
        .await
}
