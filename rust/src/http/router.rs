//! HTTP route composition and graceful serving boundary.

use std::future::Future;
use std::io;
use std::time::Duration;

use axum::extract::{Request, State};
use axum::middleware::{self, Next};
use axum::response::Response;
use axum::{Router, routing::get};

use crate::error::{AppError, StartupError};
use crate::lifecycle::Lifecycle;
use crate::startup_gate::StartupGate;

use super::AppState;
use super::handlers;

/// State used by the graceful serving boundary.
#[derive(Clone)]
pub struct ServeState {
    /// Shared request lifecycle.
    pub lifecycle: Lifecycle,
    /// Shared health state.
    pub health: crate::health::HealthState,
    /// Maximum configured drain grace.
    pub grace: Duration,
}

/// Builds exactly the Todo 15 public route set.
pub fn build_router(state: AppState) -> Router {
    let health = Router::new().route("/healthz", get(handlers::healthz));
    let admitted = Router::new()
        .route("/channels.m3u", get(handlers::channels))
        .route("/xmltv.xml", get(handlers::xmltv))
        .route("/logos/{channel_id}", get(handlers::logo))
        .route("/stream/{channel_id}", get(handlers::stream))
        .route("/replay/{channel_id}/{programme_id}", get(handlers::replay))
        .route("/catchup/{channel_id}", get(handlers::catchup))
        .route("/_hls/{*rest}", get(handlers::hls_resource))
        .layer(middleware::from_fn_with_state(state.clone(), admit))
        .with_state(state.clone());
    health
        .merge(admitted)
        .fallback(handlers::not_found)
        .with_state(state)
}

async fn admit(State(state): State<AppState>, mut request: Request, next: Next) -> Response {
    match state.lifecycle.try_enter_request() {
        Ok(guard) => {
            request.extensions_mut().insert(std::sync::Arc::new(guard));
            next.run(request).await
        }
        Err(_) => handlers::error_response(AppError::Startup(StartupError::Startup)),
    }
}

/// Serves a router while retaining the startup lock and listener ownership.
///
/// # Errors
/// Returns an I/O error when the retained listener cannot be transferred to Tokio or Axum.
pub async fn serve_with_gate<F>(
    gate: StartupGate,
    router: Router,
    serve_state: ServeState,
    shutdown: F,
) -> Result<(), io::Error>
where
    F: Future<Output = ()> + Send + 'static,
{
    let (lock, port) = gate.into_parts();
    let listener = port.into_listener();
    listener.set_nonblocking(true)?;
    let listener = tokio::net::TcpListener::from_std(listener)?;
    let lifecycle = serve_state.lifecycle;
    let health = serve_state.health;
    let grace = serve_state.grace;
    let result = axum::serve(listener, router).with_graceful_shutdown(async move {
        shutdown.await;
        health.mark_draining().await;
        let _ = lifecycle.drain(grace).await;
    });
    let result = result.await;
    drop(lock);
    result
}
