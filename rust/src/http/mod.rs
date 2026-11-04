//! Axum routes and bounded HLS serving boundaries.

use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::body::Body;
use axum::http::{HeaderMap, StatusCode};
use reqwest::Url;

use crate::health::HealthState;
use crate::hls::{RequestHeaders, TokenStore};
use crate::lifecycle::{CancellationSource, CancellationToken, Lifecycle, RequestGuard};
use crate::logo_catalog::LogoCatalog;
use crate::snapshot::SnapshotStore;
use crate::{epg::EpgClient, epg::Programme};

pub mod handlers;
pub mod proxy;
pub mod router;

pub use proxy::{PolicyForwarder, SystemResolver};
pub use router::{ServeState, build_router, serve_with_gate};

/// A playback resolution failure without provider details.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaybackError {
    /// The provider did not return a usable live URL.
    Unavailable,
}

/// An HLS forwarding failure without upstream details.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProxyError {
    /// The request could not be admitted or forwarded.
    Unavailable,
}

/// Boxed playback-resolution future used by the injected resolver seam.
pub type PlaybackFuture<'a> = Pin<Box<dyn Future<Output = Result<Url, PlaybackError>> + Send + 'a>>;

/// Resolves an internal channel identifier to an authenticated playback URL.
pub trait PlaybackResolver: Send + Sync {
    /// Resolves one live channel without exposing provider details.
    fn resolve_live<'a>(&'a self, channel_id: &'a str) -> PlaybackFuture<'a>;

    /// Resolves one snapshot programme through authenticated TVOD.
    ///
    /// The default keeps existing live-only adapters source-compatible.
    fn resolve_replay<'a>(
        &'a self,
        _programme: &'a Programme,
        _cancellation: CancellationToken,
    ) -> PlaybackFuture<'a> {
        Box::pin(async { Err(PlaybackError::Unavailable) })
    }
}

/// Playback resolver backed by the authenticated EPG TVOD operation.
#[derive(Clone, Debug)]
pub struct EpgPlaybackResolver {
    client: EpgClient,
    snapshots: SnapshotStore,
    cancellation: CancellationToken,
}

impl EpgPlaybackResolver {
    /// Creates a resolver for authenticated TVOD playback.
    #[must_use]
    pub fn new(client: EpgClient) -> Self {
        Self::with_runtime(
            client,
            SnapshotStore::new(),
            CancellationSource::new().token(),
        )
    }

    /// Creates a resolver bound to the live snapshot and process cancellation.
    #[must_use]
    pub fn with_runtime(
        client: EpgClient,
        snapshots: SnapshotStore,
        cancellation: CancellationToken,
    ) -> Self {
        Self {
            client,
            snapshots,
            cancellation,
        }
    }
}

impl PlaybackResolver for EpgPlaybackResolver {
    fn resolve_live<'a>(&'a self, channel_id: &'a str) -> PlaybackFuture<'a> {
        Box::pin(async move {
            let allowed = self
                .snapshots
                .load()
                .ok()
                .flatten()
                .is_some_and(|snapshot| {
                    snapshot.channels().iter().any(|channel| {
                        channel.channel_id == channel_id && channel.time_shift == "1"
                    })
                });
            if !allowed {
                return Err(PlaybackError::Unavailable);
            }
            self.client
                .hls_url(channel_id, self.cancellation.clone())
                .await
                .map_err(|_| PlaybackError::Unavailable)
        })
    }

    fn resolve_replay<'a>(
        &'a self,
        programme: &'a Programme,
        cancellation: CancellationToken,
    ) -> PlaybackFuture<'a> {
        Box::pin(async move {
            self.client
                .tvod_url(programme, cancellation)
                .await
                .map_err(|_| PlaybackError::Unavailable)
        })
    }
}

/// A direct upstream URL or a full opaque token captured from a playlist.
pub enum HlsTarget {
    /// A URL returned by the playback resolver.
    Url(Url),
    /// A complete token, including any media suffix.
    Token(String),
}

/// A request passed to the HLS forwarding seam.
pub struct ForwardRequest {
    /// Upstream target selected by the route.
    pub target: HlsTarget,
    /// Safe request headers copied from the client request.
    pub headers: RequestHeaders,
    /// Lifecycle cancellation shared with the body.
    pub cancellation: CancellationToken,
    /// Admission held until a streaming body is dropped.
    pub admission: Arc<RequestGuard>,
}

/// A response produced by an HLS forwarder.
pub struct ForwardResponse {
    /// Upstream-compatible status code.
    pub status: StatusCode,
    /// Narrowed response headers.
    pub headers: HeaderMap,
    /// Bounded bytes or a backpressure-aware stream.
    pub body: Body,
}

/// Boxed HLS-forwarding future.
pub type HlsFuture<'a> =
    Pin<Box<dyn Future<Output = Result<ForwardResponse, ProxyError>> + Send + 'a>>;

/// Forwards a live URL or captured HLS token through policy-controlled I/O.
pub trait HlsForwarder: Send + Sync {
    /// Performs one bounded upstream request.
    fn forward(&self, request: ForwardRequest) -> HlsFuture<'_>;
}

/// Epoch clock used by health responses and deterministic contract tests.
pub trait EpochClock: Send + Sync {
    /// Returns current Unix epoch seconds.
    fn now(&self) -> u64;
}

/// Wall-clock implementation used by production startup.
#[derive(Debug, Default)]
pub struct SystemEpochClock;

impl EpochClock for SystemEpochClock {
    fn now(&self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| duration.as_secs())
    }
}

/// Shared immutable HTTP state. Its debug output intentionally omits all values.
#[derive(Clone)]
pub struct AppState {
    pub(crate) lifecycle: Lifecycle,
    pub(crate) snapshots: SnapshotStore,
    pub(crate) health: HealthState,
    pub(crate) playback: Arc<dyn PlaybackResolver>,
    pub(crate) hls: Arc<dyn HlsForwarder>,
    pub(crate) tokens: Arc<TokenStore>,
    pub(crate) logos: Option<Arc<LogoCatalog>>,
    pub(crate) clock: Arc<dyn EpochClock>,
}

impl fmt::Debug for AppState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AppState")
            .field("has_logo_catalog", &self.logos.is_some())
            .field("token_store_empty", &self.tokens.is_empty())
            .finish_non_exhaustive()
    }
}

/// Constructor values for [`AppState`].
pub struct AppStateParts {
    /// Request admission and shutdown lifecycle.
    pub lifecycle: Lifecycle,
    /// Immutable metadata snapshot store.
    pub snapshots: SnapshotStore,
    /// Health projection shared with refresh.
    pub health: HealthState,
    /// Provider playback resolver.
    pub playback: Arc<dyn PlaybackResolver>,
    /// Policy-controlled HLS forwarder.
    pub hls: Arc<dyn HlsForwarder>,
    /// Opaque playlist token store.
    pub tokens: Arc<TokenStore>,
    /// Optional bounded local logo catalog.
    pub logos: Option<Arc<LogoCatalog>>,
    /// Injected clock for health projection.
    pub clock: Arc<dyn EpochClock>,
}

impl AppState {
    /// Builds shared state from explicit production or test seams.
    #[must_use]
    pub fn from_parts(parts: AppStateParts) -> Self {
        Self {
            lifecycle: parts.lifecycle,
            snapshots: parts.snapshots,
            health: parts.health,
            playback: parts.playback,
            hls: parts.hls,
            tokens: parts.tokens,
            logos: parts.logos,
            clock: parts.clock,
        }
    }

    /// Returns the shared lifecycle for shutdown orchestration.
    #[must_use]
    pub fn lifecycle(&self) -> Lifecycle {
        self.lifecycle.clone()
    }

    /// Returns the shared health projection for shutdown orchestration.
    #[must_use]
    pub fn health(&self) -> HealthState {
        self.health.clone()
    }
}
