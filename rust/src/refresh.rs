//! Cancellable, fixed-delay guide refresh and atomic snapshot publication.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use tokio::sync::Semaphore;
use tokio::task::{JoinHandle, JoinSet};

use crate::epg::{
    BootChannel, EpgClient, EpgError, EpgErrorClass, EpgErrorStage, Programme, SessionBundle,
};
use crate::error::OperationCode;
use crate::guide::cache::CacheError;
use crate::guide::{CacheFingerprint, GuideCache, GuideWindow};
use crate::health::{HealthState, SnapshotCounts};
use crate::lifecycle::{CancellationSource, CancellationToken};
use crate::snapshot::{LogoView, Snapshot, SnapshotStore};

const MAX_RETAINED_WINDOW_ERRORS: usize = 32;
const MAX_WINDOW_CONCURRENCY: usize = 8;
const BACKOFF_BASE: Duration = Duration::from_secs(60);
const MAX_BACKOFF_EXPONENT: u32 = 31;

type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Safe classification of provider failures used by refresh policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MetadataError {
    /// The current session expired and one reauthentication/replay is allowed.
    SessionExpired,
    /// Authentication itself failed and the cycle must stop.
    Authentication(crate::epg::EpgErrorStage, crate::epg::EpgErrorClass),
    /// A non-authentication provider or parser failure.
    Business,
    /// The caller cancelled the operation.
    Cancelled,
}

/// Opaque session handle passed from the shared EPG coordinator to metadata calls.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MetadataSession {
    generation: u64,
}

impl MetadataSession {
    /// Creates a handle for a coordinator generation.
    #[must_use]
    pub const fn new(generation: u64) -> Self {
        Self { generation }
    }

    /// Returns the coordinator generation represented by this handle.
    #[must_use]
    pub const fn generation(&self) -> u64 {
        self.generation
    }
}

impl From<EpgError> for MetadataError {
    fn from(error: EpgError) -> Self {
        match (error.stage(), error.class_code(), error.status_code()) {
            (EpgErrorStage::Request, EpgErrorClass::HttpStatus, Some(401 | 403)) => {
                Self::SessionExpired
            }
            (
                stage @ (EpgErrorStage::Discovery
                | EpgErrorStage::Login
                | EpgErrorStage::Authentication),
                _,
                _,
            ) => Self::Authentication(stage, error.class_code()),
            (_, EpgErrorClass::Cancelled, _) => Self::Cancelled,
            _ => Self::Business,
        }
    }
}

/// Provider-facing metadata source. Implementations must redact their own errors.
pub trait MetadataSource: Send + Sync {
    /// Authenticates or reuses the shared generation-keyed session.
    fn authenticate(
        &self,
        cancellation: CancellationToken,
    ) -> BoxFuture<'_, Result<Arc<MetadataSession>, MetadataError>>;
    /// Reauthenticates only when the observed session generation is still current.
    fn reauthenticate(
        &self,
        session: Arc<MetadataSession>,
        cancellation: CancellationToken,
    ) -> BoxFuture<'_, Result<Arc<MetadataSession>, MetadataError>>;
    /// Fetches boot channels, codes, and groups as one publishable collection.
    fn channels(
        &self,
        session: Arc<MetadataSession>,
        cancellation: CancellationToken,
    ) -> BoxFuture<'_, Result<Vec<BootChannel>, MetadataError>>;
    /// Fetches one programme window.
    fn programmes(
        &self,
        session: Arc<MetadataSession>,
        channel: BootChannel,
        window: GuideWindow,
        cancellation: CancellationToken,
    ) -> BoxFuture<'_, Result<Vec<Programme>, MetadataError>>;
}

/// Adapter that routes refresh metadata through the existing authenticated EPG client.
pub struct EpgMetadataSource {
    client: EpgClient,
    session: Mutex<Option<Arc<SessionBundle>>>,
}

impl EpgMetadataSource {
    /// Creates the refresh adapter without authenticating or contacting the provider.
    #[must_use]
    pub fn new(client: EpgClient) -> Self {
        Self {
            client,
            session: Mutex::new(None),
        }
    }

    /// Authenticates and fetches the initial channel bootstrap.
    ///
    /// # Errors
    /// Returns the fixed metadata error produced by authentication or channel parsing.
    pub async fn bootstrap(
        &self,
        cancellation: CancellationToken,
    ) -> Result<Vec<BootChannel>, MetadataError> {
        let session = self.authenticate(cancellation.clone()).await?;
        self.channels(session, cancellation).await
    }
}

impl MetadataSource for EpgMetadataSource {
    fn authenticate(
        &self,
        cancellation: CancellationToken,
    ) -> BoxFuture<'_, Result<Arc<MetadataSession>, MetadataError>> {
        Box::pin(async move {
            let session = tokio::select! {
                result = self.client.authenticate_with_shutdown(cancellation.clone()) => result.map_err(MetadataError::from)?,
                () = cancellation.cancelled() => return Err(MetadataError::Cancelled),
            };
            let generation = session.generation();
            let mut current = self.session.lock().map_err(|_| MetadataError::Business)?;
            *current = Some(session);
            Ok(Arc::new(MetadataSession::new(generation)))
        })
    }

    fn reauthenticate(
        &self,
        session: Arc<MetadataSession>,
        cancellation: CancellationToken,
    ) -> BoxFuture<'_, Result<Arc<MetadataSession>, MetadataError>> {
        Box::pin(async move {
            let current = tokio::select! {
                result = self.client.force_reauthenticate_with_shutdown(
                    session.generation(),
                    cancellation.clone(),
                ) => result.map_err(MetadataError::from)?,
                () = cancellation.cancelled() => return Err(MetadataError::Cancelled),
            };
            let generation = current.generation();
            let mut stored = self.session.lock().map_err(|_| MetadataError::Business)?;
            *stored = Some(current);
            Ok(Arc::new(MetadataSession::new(generation)))
        })
    }

    fn channels(
        &self,
        session: Arc<MetadataSession>,
        cancellation: CancellationToken,
    ) -> BoxFuture<'_, Result<Vec<BootChannel>, MetadataError>> {
        Box::pin(async move {
            let current = self
                .session
                .lock()
                .map_err(|_| MetadataError::Business)?
                .clone();
            let Some(current) = current.filter(|value| value.generation() == session.generation())
            else {
                return Err(MetadataError::Authentication(
                    crate::epg::EpgErrorStage::Authentication,
                    crate::epg::EpgErrorClass::MissingField,
                ));
            };
            tokio::select! {
                result = self.client.fetch_channels(&current) => result.map_err(MetadataError::from),
                () = cancellation.cancelled() => Err(MetadataError::Cancelled),
            }
        })
    }

    fn programmes(
        &self,
        session: Arc<MetadataSession>,
        channel: BootChannel,
        window: GuideWindow,
        cancellation: CancellationToken,
    ) -> BoxFuture<'_, Result<Vec<Programme>, MetadataError>> {
        Box::pin(async move {
            let current = self
                .session
                .lock()
                .map_err(|_| MetadataError::Business)?
                .clone();
            let Some(current) = current.filter(|value| value.generation() == session.generation())
            else {
                return Err(MetadataError::Authentication(
                    crate::epg::EpgErrorStage::Authentication,
                    crate::epg::EpgErrorClass::MissingField,
                ));
            };
            tokio::select! {
                result = self.client.fetch_programmes(&current, &channel.channel_id, &channel.code, window.start_ms, window.end_ms.saturating_sub(1)) => result.map_err(MetadataError::from),
                () = cancellation.cancelled() => Err(MetadataError::Cancelled),
            }
        })
    }
}

/// Monotonic and UTC time used by one refresh engine.
pub trait RefreshClock: Send + Sync {
    /// Returns monotonic time for scheduling and session freshness.
    fn monotonic(&self) -> Instant;
    /// Returns UTC time for guide windows and health timestamps.
    fn utc(&self) -> DateTime<Utc>;
}

/// Production clock backed by `Instant` and `Utc`.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemRefreshClock;

impl RefreshClock for SystemRefreshClock {
    fn monotonic(&self) -> Instant {
        Instant::now()
    }
    fn utc(&self) -> DateTime<Utc> {
        Utc::now()
    }
}

/// Injectable async sleeper used by scheduling and tests.
pub trait RefreshSleeper: Send + Sync {
    /// Sleeps for the requested fixed delay.
    fn sleep(&self, duration: Duration) -> BoxFuture<'_, ()>;
}

/// Tokio production sleeper.
#[derive(Clone, Copy, Debug, Default)]
pub struct TokioRefreshSleeper;

impl RefreshSleeper for TokioRefreshSleeper {
    fn sleep(&self, duration: Duration) -> BoxFuture<'_, ()> {
        Box::pin(tokio::time::sleep(duration))
    }
}

/// Guide refresh parameters and publication dependencies.
pub struct RefreshEngine {
    source: Arc<dyn MetadataSource>,
    store: SnapshotStore,
    cache: Option<GuideCache>,
    cache_fingerprint: Option<CacheFingerprint>,
    health: HealthState,
    clock: Arc<dyn RefreshClock>,
    sleeper: Arc<dyn RefreshSleeper>,
    interval: Duration,
    windows: Vec<GuideWindow>,
    guide_zone: crate::guide::GuideZone,
    catchup_days: u16,
    cancellation: CancellationToken,
    failure_count: u32,
    prior: Option<Arc<Snapshot>>,
}

/// Configuration for constructing a refresh engine.
pub struct RefreshEngineConfig {
    /// Provider metadata source.
    pub source: Arc<dyn MetadataSource>,
    /// Atomic in-memory snapshot store.
    pub store: SnapshotStore,
    /// Optional best-effort persistent cache.
    pub cache: Option<GuideCache>,
    /// Fingerprint used by the persistent cache.
    pub cache_fingerprint: Option<CacheFingerprint>,
    /// Shared health state.
    pub health: HealthState,
    /// Refresh cadence.
    pub interval: Duration,
    /// Guide windows to request.
    pub windows: Vec<GuideWindow>,
    /// Guide zone stored in each snapshot.
    pub guide_zone: crate::guide::GuideZone,
    /// Catch-up retention stored in each snapshot.
    pub catchup_days: u16,
    /// Shutdown token shared with lifecycle and auth.
    pub cancellation: CancellationToken,
    /// Optional test clock.
    pub clock: Option<Arc<dyn RefreshClock>>,
    /// Optional test sleeper.
    pub sleeper: Option<Arc<dyn RefreshSleeper>>,
}

impl RefreshEngine {
    /// Creates a refresh engine without performing I/O.
    #[must_use]
    pub fn new(config: RefreshEngineConfig) -> Self {
        Self {
            source: config.source,
            store: config.store,
            cache: config.cache,
            cache_fingerprint: config.cache_fingerprint,
            health: config.health,
            clock: config.clock.unwrap_or_else(|| Arc::new(SystemRefreshClock)),
            sleeper: config
                .sleeper
                .unwrap_or_else(|| Arc::new(TokioRefreshSleeper)),
            interval: config.interval,
            windows: config.windows,
            guide_zone: config.guide_zone,
            catchup_days: config.catchup_days,
            cancellation: config.cancellation,
            failure_count: 0,
            prior: None,
        }
    }

    /// Seeds a validated warm snapshot before the first scheduled cycle.
    ///
    /// # Errors
    /// Returns the snapshot publication error when the generation is not newer.
    pub fn seed_snapshot(&mut self, snapshot: Arc<Snapshot>) -> Result<(), MetadataError> {
        self.store
            .publish(Arc::clone(&snapshot))
            .map_err(|_| MetadataError::Business)?;
        self.prior = Some(snapshot);
        Ok(())
    }

    /// Publishes a validated warm snapshot before the first scheduled cycle.
    ///
    /// # Errors
    /// Returns [`MetadataError::Business`] when the store rejects the generation.
    pub async fn seed_warm_snapshot(
        &mut self,
        snapshot: Arc<Snapshot>,
        refreshed_at: u64,
    ) -> Result<(), MetadataError> {
        self.store
            .publish(Arc::clone(&snapshot))
            .map_err(|_| MetadataError::Business)?;
        self.health
            .record_refresh_success(
                refreshed_at,
                refreshed_at.saturating_add(self.interval.as_secs()),
                SnapshotCounts::new(
                    u64::try_from(snapshot.channels().len()).unwrap_or(u64::MAX),
                    u64::try_from(snapshot.programmes().len()).unwrap_or(u64::MAX),
                ),
            )
            .await;
        self.prior = Some(snapshot);
        Ok(())
    }

    /// Returns the exact saturated fixed-delay backoff for a failure count.
    #[must_use]
    pub fn backoff_delay(interval: Duration, consecutive_failures: u32) -> Duration {
        let shift = consecutive_failures.min(MAX_BACKOFF_EXPONENT);
        let multiplier = 1_u32 << shift;
        let exponential = BACKOFF_BASE
            .checked_mul(multiplier)
            .unwrap_or(Duration::MAX);
        if interval < BACKOFF_BASE {
            interval
        } else {
            exponential.min(interval)
        }
    }

    /// Runs one complete refresh cycle. No cycle is spawned by this method.
    ///
    /// # Errors
    /// Returns a fixed metadata error when authentication, channel bootstrap, all programme
    /// windows, snapshot validation, or atomic publication fails.
    #[expect(
        clippy::too_many_lines,
        reason = "the cycle keeps fetch, validate, persist, publish, and health ordering visible"
    )]
    pub async fn refresh_once(&mut self) -> Result<RefreshOutcome, MetadataError> {
        if self.cancellation.is_cancelled() {
            return Err(MetadataError::Cancelled);
        }
        let session = match self.source.authenticate(self.cancellation.clone()).await {
            Ok(session) => session,
            Err(error) => {
                self.record_cycle_failure().await;
                return Err(error);
            }
        };
        let channel_result = self
            .source
            .channels(Arc::clone(&session), self.cancellation.clone())
            .await;
        let (session, channels) = match channel_result {
            Ok(channels) => (session, channels),
            Err(MetadataError::SessionExpired) => {
                let session = match self
                    .source
                    .reauthenticate(Arc::clone(&session), self.cancellation.clone())
                    .await
                {
                    Ok(session) => session,
                    Err(error) => {
                        self.record_cycle_failure().await;
                        return Err(error);
                    }
                };
                let channels = match self
                    .source
                    .channels(Arc::clone(&session), self.cancellation.clone())
                    .await
                {
                    Ok(channels) => channels,
                    Err(error) => {
                        self.record_cycle_failure().await;
                        return Err(error);
                    }
                };
                (session, channels)
            }
            Err(error) => {
                self.record_cycle_failure().await;
                return Err(error);
            }
        };
        if self.cancellation.is_cancelled() {
            return Err(MetadataError::Cancelled);
        }
        let previous = self
            .prior
            .as_ref()
            .map(|snapshot| snapshot.programmes().to_vec());
        let outcomes = self.fetch_windows(session, channels.clone()).await;
        if self.cancellation.is_cancelled() {
            return Err(MetadataError::Cancelled);
        }
        let valid = outcomes
            .iter()
            .filter_map(|outcome| outcome.programmes.as_ref())
            .count();
        if valid == 0 {
            if let Some(error) = outcomes.iter().find_map(|outcome| outcome.error.as_ref()) {
                crate::startup::report_startup_stage(match error {
                    MetadataError::Authentication(_, class) => match class {
                        EpgErrorClass::Parse => "refresh-window-parse",
                        EpgErrorClass::HttpStatus => "refresh-window-status",
                        EpgErrorClass::Transport
                        | EpgErrorClass::Timeout
                        | EpgErrorClass::PoolTimeout => "refresh-window-request",
                        EpgErrorClass::Cancelled => "refresh-window-cancelled",
                        EpgErrorClass::InvalidOrigin
                        | EpgErrorClass::MissingField
                        | EpgErrorClass::Crypto => "refresh-window-auth",
                    },
                    MetadataError::SessionExpired => "refresh-window-auth",
                    MetadataError::Business => "refresh-window-other",
                    MetadataError::Cancelled => "refresh-window-cancelled",
                });
            }
            self.record_cycle_failure().await;
            return Err(MetadataError::Business);
        }
        let failed_windows = outcomes
            .iter()
            .filter(|outcome| outcome.programmes.is_none())
            .count();
        let programmes = merge_windows(previous.as_deref().unwrap_or(&[]), &outcomes);
        let channels = carry_logos(self.prior.as_ref(), channels);
        let logos = LogoView::from_channels(&channels);
        let generation = self
            .prior
            .as_ref()
            .map_or(1, |snapshot| snapshot.generation().saturating_add(1));
        let Ok(snapshot) = Snapshot::new(
            generation,
            channels,
            programmes,
            logos,
            self.guide_zone.clone(),
            self.catchup_days,
        ) else {
            crate::startup::report_startup_stage("refresh-snapshot");
            self.record_cycle_failure().await;
            return Err(MetadataError::Business);
        };
        let persistence_failed =
            if let (Some(cache), Some(fingerprint)) = (&self.cache, &self.cache_fingerprint) {
                match cache.save(
                    snapshot.programmes(),
                    self.clock.utc().timestamp_millis(),
                    fingerprint,
                    &self.cancellation,
                ) {
                    Ok(()) => false,
                    Err(CacheError::Cancelled) => return Err(MetadataError::Cancelled),
                    Err(_) => {
                        crate::startup::report_startup_stage("refresh-cache");
                        true
                    }
                }
            } else {
                false
            };
        if self.cancellation.is_cancelled() {
            return Err(MetadataError::Cancelled);
        }
        if self.store.publish(Arc::clone(&snapshot)).is_err() {
            crate::startup::report_startup_stage("refresh-publish");
            self.record_cycle_failure().await;
            return Err(MetadataError::Business);
        }
        self.prior = Some(Arc::clone(&snapshot));
        self.failure_count = 0;
        let now = u64::try_from(self.clock.utc().timestamp()).unwrap_or(0);
        let channel_count = u64::try_from(snapshot.channels().len()).unwrap_or(u64::MAX);
        let programme_count = u64::try_from(snapshot.programmes().len()).unwrap_or(u64::MAX);
        if failed_windows == 0 {
            self.health
                .record_refresh_success(
                    now,
                    now.saturating_add(self.interval.as_secs()),
                    SnapshotCounts::new(channel_count, programme_count),
                )
                .await;
        } else {
            self.health
                .record_refresh_partial(
                    now,
                    now.saturating_add(self.interval.as_secs()),
                    SnapshotCounts::new(channel_count, programme_count),
                    u64::try_from(failed_windows).unwrap_or(u64::MAX),
                )
                .await;
        }
        if persistence_failed {
            self.health.record_persistence_failure().await;
        }
        Ok(RefreshOutcome {
            generation,
            failed_windows,
        })
    }

    async fn record_cycle_failure(&self) {
        self.health
            .record_refresh_failure_with_snapshot(OperationCode::Refresh, self.prior.is_some())
            .await;
    }

    async fn fetch_windows(
        &self,
        session: Arc<MetadataSession>,
        channels: Vec<BootChannel>,
    ) -> Vec<WindowOutcome> {
        let semaphore = Arc::new(Semaphore::new(MAX_WINDOW_CONCURRENCY));
        let mut set = JoinSet::new();
        for channel in channels {
            for window in &self.windows {
                let semaphore = Arc::clone(&semaphore);
                let source = Arc::clone(&self.source);
                let session = Arc::clone(&session);
                let cancellation = self.cancellation.clone();
                let window = *window;
                let channel = channel.clone();
                set.spawn(async move {
                    let Ok(_permit) = semaphore.acquire_owned().await else {
                        return WindowOutcome::from_result(
                            window,
                            channel.channel_id,
                            Err(MetadataError::Cancelled),
                        );
                    };
                    let channel_id = channel.channel_id.clone();
                    let replay_channel = channel.clone();
                    let result = source
                        .programmes(Arc::clone(&session), channel, window, cancellation.clone())
                        .await;
                    let result = match result {
                        Err(MetadataError::SessionExpired) => {
                            match source
                                .reauthenticate(Arc::clone(&session), cancellation.clone())
                                .await
                            {
                                Ok(replay_session) => {
                                    source
                                        .programmes(
                                            replay_session,
                                            replay_channel,
                                            window,
                                            cancellation,
                                        )
                                        .await
                                }
                                Err(error) => Err(error),
                            }
                        }
                        result => result,
                    };
                    WindowOutcome::from_result(window, channel_id, result)
                });
            }
        }
        let mut outcomes = Vec::new();
        while let Some(joined) = set.join_next().await {
            if let Ok(outcome) = joined {
                outcomes.push(outcome);
            }
        }
        let mut retained_errors = 0_usize;
        for outcome in &mut outcomes {
            if outcome.error.is_some() {
                retained_errors = retained_errors.saturating_add(1);
                if retained_errors > MAX_RETAINED_WINDOW_ERRORS {
                    outcome.error = None;
                }
            }
        }
        outcomes
    }

    /// Returns the delay after the last cycle completion and advances failure state.
    pub fn next_delay(&mut self, published: bool) -> Duration {
        if published {
            self.failure_count = 0;
            self.interval
        } else {
            self.failure_count = self.failure_count.saturating_add(1);
            Self::backoff_delay(self.interval, self.failure_count)
        }
    }

    /// Runs cycles sequentially until shutdown. The sleep starts after each cycle.
    pub async fn run(mut self) {
        loop {
            let published = (self.refresh_once().await).is_ok();
            let completion = self.clock.monotonic();
            let delay = self.next_delay(published);
            let deadline = completion
                .checked_add(delay)
                .unwrap_or_else(|| self.clock.monotonic());
            let remaining = deadline.saturating_duration_since(self.clock.monotonic());
            tokio::select! {
                () = self.cancellation.cancelled() => return,
                () = self.sleeper.sleep(remaining) => {},
            }
        }
    }

    fn with_cancellation(mut self, cancellation: CancellationToken) -> Self {
        self.cancellation = cancellation;
        self
    }
}

/// One non-Clone supervisor owning its task and cancellation source.
pub struct RefreshSupervisor {
    cancellation: CancellationSource,
    handle: Option<JoinHandle<()>>,
}

impl RefreshSupervisor {
    /// Starts one owned, non-overlapping refresh task.
    #[must_use]
    pub fn start(engine: RefreshEngine) -> Self {
        let cancellation = CancellationSource::new();
        Self::start_with_cancellation(engine, cancellation)
    }

    /// Starts a refresh task owned by the process cancellation source.
    #[must_use]
    pub fn start_with_cancellation(
        engine: RefreshEngine,
        cancellation: CancellationSource,
    ) -> Self {
        let token = cancellation.token();
        let handle = tokio::spawn(async move { engine.with_cancellation(token).run().await });
        Self {
            cancellation,
            handle: Some(handle),
        }
    }

    /// Cancels and awaits the task, aborting and awaiting once the deadline expires.
    pub async fn stop(mut self, deadline: Duration) -> SupervisorStop {
        self.cancellation.cancel();
        let Some(mut handle) = self.handle.take() else {
            return SupervisorStop::Stopped;
        };
        if tokio::time::timeout(deadline, &mut handle).await.is_ok() {
            SupervisorStop::Stopped
        } else {
            handle.abort();
            drop(handle.await);
            SupervisorStop::Aborted
        }
    }
}

/// Shutdown result for a refresh supervisor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SupervisorStop {
    /// The refresh task completed cooperatively.
    Stopped,
    /// The deadline expired and the task was aborted and awaited.
    Aborted,
}

/// Result of a publishable cycle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RefreshOutcome {
    /// Published snapshot generation.
    pub generation: u64,
    /// Number of failed windows carried forward.
    pub failed_windows: usize,
}

#[derive(Clone, Debug)]
struct WindowOutcome {
    channel_id: String,
    window: GuideWindow,
    programmes: Option<Vec<Programme>>,
    error: Option<MetadataError>,
}

impl WindowOutcome {
    fn from_result(
        window: GuideWindow,
        channel_id: String,
        result: Result<Vec<Programme>, MetadataError>,
    ) -> Self {
        let (programmes, error) = match result {
            Ok(programmes) => (Some(programmes), None),
            Err(error) => (None, Some(error)),
        };
        Self {
            channel_id,
            window,
            programmes,
            error,
        }
    }
}

fn merge_windows(previous: &[Programme], outcomes: &[WindowOutcome]) -> Vec<Programme> {
    let mut merged = previous.to_vec();
    for outcome in outcomes {
        let Some(programmes) = &outcome.programmes else {
            continue;
        };
        merged.retain(|programme| {
            programme.channel_id != outcome.channel_id
                || !overlaps_window(programme, outcome.window)
        });
        merged.extend(programmes.iter().cloned());
    }
    merged.sort_by_key(|programme| {
        (
            programme.channel_id.clone(),
            programme.start_time_ms,
            programme.end_time_ms,
            programme.programme_id.clone(),
        )
    });
    merged
}

fn overlaps_window(programme: &Programme, window: GuideWindow) -> bool {
    programme.start_time_ms < window.end_ms && programme.end_time_ms > window.start_ms
}

fn carry_logos(
    previous: Option<&Arc<Snapshot>>,
    mut channels: Vec<BootChannel>,
) -> Vec<BootChannel> {
    let Some(previous) = previous else {
        return channels;
    };
    for channel in &mut channels {
        if channel.logo_url.is_empty() {
            if let Some(old) = previous
                .channels()
                .iter()
                .find(|old| old.channel_id == channel.channel_id)
            {
                channel.logo_url.clone_from(&old.logo_url);
            }
        }
    }
    channels
}

impl crate::guide::cache::Cancellation for CancellationToken {
    fn is_cancelled(&self) -> bool {
        self.is_cancelled()
    }
}

#[cfg(test)]
#[expect(
    clippy::expect_used,
    reason = "refresh fixtures use explicit construction failures"
)]
#[path = "refresh_tests.rs"]
mod tests;
