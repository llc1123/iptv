use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant};

use reqwest::{Client, Url};
use tokio::sync::{Mutex, Notify, OwnedSemaphorePermit, Semaphore};

use super::client::{EpgError, EpgErrorStage};

/// Freshness window for authenticated generations.
pub const SESSION_TTL: Duration = Duration::from_secs(600);

/// Monotonic time source used by session freshness checks.
pub trait Clock: Send + Sync {
    /// Returns the current monotonic instant.
    fn now(&self) -> Instant;
}

#[derive(Debug, Clone, Copy, Default)]
/// System monotonic clock implementation.
pub struct SessionClock;

impl Clock for SessionClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
}

#[cfg(test)]
#[expect(
    clippy::expect_used,
    reason = "session fixtures use explicit construction failures"
)]
#[path = "session_tests.rs"]
mod tests;

/// Injectable asynchronous backoff sleeper.
pub trait Sleeper: Send + Sync {
    /// Sleeps without blocking the runtime thread.
    fn sleep(&self, duration: Duration) -> Pin<Box<dyn Future<Output = ()> + Send + '_>>;
}

#[derive(Debug, Clone, Copy, Default)]
pub struct TokioSleeper;

impl Sleeper for TokioSleeper {
    fn sleep(&self, duration: Duration) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(tokio::time::sleep(duration))
    }
}

#[derive(Clone)]
/// Immutable authenticated state for one generation.
pub struct SessionBundle {
    generation: u64,
    created_at: Instant,
    origin: Url,
    user_token: String,
    session_id: String,
    temp_key: String,
    control_client: Client,
    media_client: Client,
    media_permits: Arc<Semaphore>,
    media_pool_timeout: Duration,
    cookie_snapshot: Vec<String>,
}

impl std::fmt::Debug for SessionBundle {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SessionBundle")
            .field("generation", &self.generation)
            .field("origin_scheme", &self.origin.scheme())
            .field("origin_has_explicit_port", &self.origin.port().is_some())
            .field("user_token_present", &!self.user_token.is_empty())
            .field("session_id_present", &!self.session_id.is_empty())
            .field("temp_key_present", &!self.temp_key.is_empty())
            .field("cookie_snapshot_present", &!self.cookie_snapshot.is_empty())
            .field("control_client_role", &"control")
            .field("media_client_role", &"media")
            .finish_non_exhaustive()
    }
}

impl SessionBundle {
    pub(crate) fn stamped(&self, created_at: Instant) -> Self {
        Self {
            created_at,
            ..self.clone()
        }
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "the immutable bundle binds both clients and all protocol state atomically"
    )]
    pub(crate) fn new(
        generation: u64,
        created_at: Instant,
        origin: Url,
        user_token: String,
        session_id: String,
        temp_key: String,
        control_client: Client,
        media_client: Client,
        media_permits: Arc<Semaphore>,
        media_pool_timeout: Duration,
        cookie_snapshot: Vec<String>,
    ) -> Self {
        Self {
            generation,
            created_at,
            origin,
            user_token,
            session_id,
            temp_key,
            control_client,
            media_client,
            media_permits,
            media_pool_timeout,
            cookie_snapshot,
        }
    }

    #[must_use]
    /// Returns the generation number.
    pub fn generation(&self) -> u64 {
        self.generation
    }
    #[must_use]
    /// Returns the validated redirect origin.
    pub fn origin(&self) -> &Url {
        &self.origin
    }
    #[must_use]
    /// Returns the provider user token.
    pub fn user_token(&self) -> &str {
        &self.user_token
    }
    #[must_use]
    /// Returns the provider session identifier.
    pub fn session_id(&self) -> &str {
        &self.session_id
    }
    #[must_use]
    /// Returns the derived identity key.
    pub fn temp_key(&self) -> &str {
        &self.temp_key
    }
    #[must_use]
    /// Returns the control client handle.
    pub fn control_client(&self) -> &Client {
        &self.control_client
    }
    #[must_use]
    /// Returns the media client handle.
    pub fn media_client(&self) -> &Client {
        &self.media_client
    }
    #[must_use]
    /// Returns whether the bundle is within its freshness window.
    pub fn is_fresh(&self, now: Instant) -> bool {
        now.duration_since(self.created_at) < SESSION_TTL
    }

    /// Acquires one bounded media-plane request permit.
    ///
    /// # Errors
    /// Returns a redacted pool-timeout error when admission exceeds the configured deadline.
    pub async fn acquire_media_permit(&self) -> Result<OwnedSemaphorePermit, EpgError> {
        tokio::time::timeout(
            self.media_pool_timeout,
            Arc::clone(&self.media_permits).acquire_owned(),
        )
        .await
        .map_err(|_| EpgError::pool_timeout(EpgErrorStage::Request, 1))?
        .map_err(|_| EpgError::pool_timeout(EpgErrorStage::Request, 1))
    }
}

type FlightResult = Result<Arc<SessionBundle>, EpgError>;

struct Flight {
    generation: u64,
    result: Arc<Mutex<Option<FlightResult>>>,
    published: Arc<Notify>,
}

struct CoordinatorState {
    current: Option<Arc<SessionBundle>>,
    flight: Option<Arc<Flight>>,
}

#[derive(Clone)]
/// Generation-keyed authentication flight coordinator.
pub struct AuthCoordinator {
    state: Arc<Mutex<CoordinatorState>>,
    clock: Arc<dyn Clock>,
    shutdown: crate::lifecycle::CancellationToken,
    _shutdown_source: Option<crate::lifecycle::CancellationSource>,
}

impl std::fmt::Debug for AuthCoordinator {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("AuthCoordinator { state: [REDACTED], flight: [REDACTED] }")
    }
}

impl Default for AuthCoordinator {
    fn default() -> Self {
        Self::new(Arc::new(SessionClock))
    }
}

impl AuthCoordinator {
    #[must_use]
    /// Creates a coordinator with an independent shutdown token.
    pub fn new(clock: Arc<dyn Clock>) -> Self {
        let source = crate::lifecycle::CancellationSource::new();
        Self {
            state: Arc::new(Mutex::new(CoordinatorState {
                current: None,
                flight: None,
            })),
            clock,
            shutdown: source.token(),
            _shutdown_source: Some(source),
        }
    }

    #[must_use]
    /// Creates a coordinator bound to the service shutdown token.
    pub fn with_shutdown(
        clock: Arc<dyn Clock>,
        shutdown: crate::lifecycle::CancellationToken,
    ) -> Self {
        Self {
            state: Arc::new(Mutex::new(CoordinatorState {
                current: None,
                flight: None,
            })),
            clock,
            shutdown,
            _shutdown_source: None,
        }
    }

    /// Returns the latest committed bundle, if any.
    pub async fn current(&self) -> Option<Arc<SessionBundle>> {
        self.state.lock().await.current.clone()
    }

    /// Runs or joins the current detached authentication flight.
    ///
    /// # Errors
    /// Returns the shared redacted authentication result.
    pub async fn ensure_authenticated<F, Fut>(
        &self,
        authenticate: F,
    ) -> Result<Arc<SessionBundle>, EpgError>
    where
        F: FnOnce(u64) -> Fut + Send + 'static,
        Fut: Future<Output = FlightResult> + Send + 'static,
    {
        self.ensure_authenticated_with_shutdown(authenticate, self.shutdown.clone())
            .await
    }

    /// Runs or joins authentication while binding the flight to a caller-owned shutdown token.
    ///
    /// # Errors
    /// Returns the shared redacted authentication result or cancellation from either owner.
    pub async fn ensure_authenticated_with_shutdown<F, Fut>(
        &self,
        authenticate: F,
        shutdown: crate::lifecycle::CancellationToken,
    ) -> Result<Arc<SessionBundle>, EpgError>
    where
        F: FnOnce(u64) -> Fut + Send + 'static,
        Fut: Future<Output = FlightResult> + Send + 'static,
    {
        let (flight, owner) = {
            let mut state = self.state.lock().await;
            if let Some(bundle) = state.current.as_ref() {
                if bundle.is_fresh(self.clock.now()) {
                    return Ok(Arc::clone(bundle));
                }
            }
            if let Some(flight) = state.flight.as_ref() {
                (Arc::clone(flight), false)
            } else {
                let next_generation = state
                    .current
                    .as_ref()
                    .map_or(0, |bundle| bundle.generation() + 1);
                let flight = Arc::new(Flight {
                    generation: next_generation,
                    result: Arc::new(Mutex::new(None)),
                    published: Arc::new(Notify::new()),
                });
                state.flight = Some(Arc::clone(&flight));
                (flight, true)
            }
        };
        if owner {
            let state = Arc::clone(&self.state);
            let owner_flight = Arc::clone(&flight);
            let coordinator_shutdown = self.shutdown.clone();
            let flight_shutdown = shutdown.clone();
            let clock = Arc::clone(&self.clock);
            tokio::spawn(async move {
                let result = tokio::select! {
                    result = authenticate(owner_flight.generation) => result,
                    () = coordinator_shutdown.cancelled() => Err(EpgError::cancelled(EpgErrorStage::Authentication)),
                    () = flight_shutdown.cancelled() => Err(EpgError::cancelled(EpgErrorStage::Authentication)),
                };
                let result = result.map(|bundle| Arc::new(bundle.stamped(clock.now())));
                let mut state = state.lock().await;
                let owns_flight = state
                    .flight
                    .as_ref()
                    .is_some_and(|current| Arc::ptr_eq(current, &owner_flight));
                if owns_flight {
                    if let Ok(bundle) = &result {
                        let generation_matches = bundle.generation() == owner_flight.generation;
                        let current_is_older = state
                            .current
                            .as_ref()
                            .is_none_or(|old| old.generation() < owner_flight.generation);
                        if generation_matches && current_is_older {
                            state.current = Some(Arc::clone(bundle));
                        }
                    }
                    *owner_flight.result.lock().await = Some(result);
                    owner_flight.published.notify_waiters();
                    state.flight = None;
                }
            });
        }
        loop {
            let notified = flight.published.notified();
            if let Some(result) = flight.result.lock().await.as_ref() {
                return result.clone();
            }
            tokio::select! {
                () = notified => {},
                () = shutdown.cancelled() => return Err(EpgError::cancelled(EpgErrorStage::Authentication)),
            }
        }
    }
}
