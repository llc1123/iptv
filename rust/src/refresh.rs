//! Cancellable, fixed-delay guide refresh and atomic snapshot publication.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use tokio::sync::Semaphore;
use tokio::task::{JoinHandle, JoinSet};

use crate::epg::{BootChannel, EpgClient, EpgError, Programme, ProgrammeQuery};
use crate::error::OperationCode;
use crate::guide::cache::CacheError;
use crate::guide::{CacheFingerprint, GuideCache, GuideWindow};
use crate::health::{HealthState, SnapshotCounts};
use crate::lifecycle::{CancellationSource, CancellationToken};
use crate::snapshot::{LogoView, Snapshot, SnapshotError, SnapshotStore};

const MAX_RETAINED_WINDOW_ERRORS: usize = 32;
const MAX_WINDOW_CONCURRENCY: usize = 8;
const BACKOFF_BASE: Duration = Duration::from_secs(60);
const MAX_BACKOFF_EXPONENT: u32 = 31;

type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

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
    client: EpgClient,
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
    /// Provider EPG client.
    pub client: EpgClient,
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
            client: config.client,
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
    pub fn seed_snapshot(&mut self, snapshot: Arc<Snapshot>) -> Result<(), EpgError> {
        self.store
            .publish(Arc::clone(&snapshot))
            .map_err(|_| EpgError::Provider("snapshot unavailable".to_owned()))?;
        self.prior = Some(snapshot);
        Ok(())
    }

    /// Publishes a validated warm snapshot before the first scheduled cycle.
    ///
    /// # Errors
    /// Returns [`EpgError::Provider`] when the store rejects the generation.
    pub async fn seed_warm_snapshot(
        &mut self,
        snapshot: Arc<Snapshot>,
        refreshed_at: u64,
    ) -> Result<(), EpgError> {
        self.store
            .publish(Arc::clone(&snapshot))
            .map_err(|_| EpgError::Provider("snapshot unavailable".to_owned()))?;
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
    pub async fn refresh_once(&mut self) -> Result<RefreshOutcome, EpgError> {
        if self.cancellation.is_cancelled() {
            return Err(EpgError::Provider("cancelled".to_owned()));
        }
        let channels = match self.client.channels().await {
            Ok(channels) => channels,
            Err(error) => {
                self.record_cycle_failure().await;
                return Err(error);
            }
        };
        if self.cancellation.is_cancelled() {
            return Err(EpgError::Provider("cancelled".to_owned()));
        }
        let previous = self
            .prior
            .as_ref()
            .map(|snapshot| snapshot.programmes().to_vec());
        let outcomes = self.fetch_windows(channels.clone()).await;
        if self.cancellation.is_cancelled() {
            return Err(EpgError::Provider("cancelled".to_owned()));
        }
        let valid = outcomes
            .iter()
            .filter_map(|outcome| outcome.programmes.as_ref())
            .count();
        if valid == 0 {
            if let Some(error) = outcomes.iter().find_map(|outcome| outcome.error.as_ref()) {
                crate::startup::report_startup_stage(match error {
                    EpgError::Parse(_) => "refresh-window-parse",
                    EpgError::Status(_) => "refresh-window-status",
                    EpgError::Http(_) => "refresh-window-request",
                    EpgError::Discovery | EpgError::MissingField(_) | EpgError::Provider(_) => {
                        "refresh-window-auth"
                    }
                });
            }
            self.record_cycle_failure().await;
            return Err(EpgError::Provider("refresh windows failed".to_owned()));
        }
        let failed_windows = outcomes
            .iter()
            .filter(|outcome| outcome.programmes.is_none())
            .count();
        let fetched_invalid_interval = outcomes
            .iter()
            .filter_map(|outcome| outcome.programmes.as_ref())
            .flatten()
            .any(|programme| programme.start_time_ms >= programme.end_time_ms);
        let prior_invalid_interval = previous
            .as_deref()
            .unwrap_or(&[])
            .iter()
            .any(|programme| programme.start_time_ms >= programme.end_time_ms);
        let programmes = merge_windows(previous.as_deref().unwrap_or(&[]), &outcomes);
        let channels = carry_logos(self.prior.as_ref(), channels);
        let logos = LogoView::from_channels(&channels);
        let generation = self
            .prior
            .as_ref()
            .map_or(1, |snapshot| snapshot.generation().saturating_add(1));
        let snapshot_result = Snapshot::new(
            generation,
            channels,
            programmes,
            logos,
            self.guide_zone.clone(),
            self.catchup_days,
        );
        let snapshot = match snapshot_result {
            Ok(snapshot) => snapshot,
            Err(error) => {
                crate::startup::report_startup_stage(match error {
                    SnapshotError::ProgrammeLimit => "refresh-programme-limit",
                    SnapshotError::ProgrammeConflict => "refresh-programme-conflict",
                    SnapshotError::ProgrammeInterval if fetched_invalid_interval => {
                        "refresh-window-interval"
                    }
                    SnapshotError::ProgrammeInterval if prior_invalid_interval => {
                        "refresh-prior-interval"
                    }
                    SnapshotError::ProgrammeInterval => "refresh-programme-interval",
                    SnapshotError::TooManyChannels
                    | SnapshotError::InvalidChannelIdentifiers
                    | SnapshotError::LogoAlignment
                    | SnapshotError::GenerationNotNewer
                    | SnapshotError::StoreUnavailable => "refresh-snapshot",
                });
                self.record_cycle_failure().await;
                return Err(EpgError::Provider("snapshot validation failed".to_owned()));
            }
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
                    Err(CacheError::Cancelled) => {
                        return Err(EpgError::Provider("cancelled".to_owned()));
                    }
                    Err(_) => {
                        crate::startup::report_startup_stage("refresh-cache");
                        true
                    }
                }
            } else {
                false
            };
        if self.cancellation.is_cancelled() {
            return Err(EpgError::Provider("cancelled".to_owned()));
        }
        if self.store.publish(Arc::clone(&snapshot)).is_err() {
            crate::startup::report_startup_stage("refresh-publish");
            self.record_cycle_failure().await;
            return Err(EpgError::Provider("snapshot publication failed".to_owned()));
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

    async fn fetch_windows(&self, channels: Vec<BootChannel>) -> Vec<WindowOutcome> {
        let semaphore = Arc::new(Semaphore::new(MAX_WINDOW_CONCURRENCY));
        let mut set = JoinSet::new();
        for channel in channels {
            for window in &self.windows {
                let semaphore = Arc::clone(&semaphore);
                let client = self.client.clone();
                let cancellation = self.cancellation.clone();
                let window = *window;
                let channel = channel.clone();
                set.spawn(async move {
                    let Ok(_permit) = semaphore.acquire_owned().await else {
                        return WindowOutcome::from_result(
                            window,
                            channel.channel_id,
                            Err(EpgError::Provider("cancelled".to_owned())),
                        );
                    };
                    let channel_id = channel.channel_id.clone();
                    let result = if cancellation.is_cancelled() {
                        Err(EpgError::Provider("cancelled".to_owned()))
                    } else {
                        client
                            .programmes(&ProgrammeQuery {
                                channel_id: channel.channel_id.clone(),
                                channel_code: channel.code.clone(),
                                start_time_ms: window.start_ms,
                                end_time_ms: window.end_ms.saturating_sub(1),
                            })
                            .await
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

#[derive(Debug)]
struct WindowOutcome {
    channel_id: String,
    window: GuideWindow,
    programmes: Option<Vec<Programme>>,
    error: Option<EpgError>,
}

impl WindowOutcome {
    fn from_result(
        window: GuideWindow,
        channel_id: String,
        result: Result<Vec<Programme>, EpgError>,
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
    let mut merged = previous
        .iter()
        .filter(|programme| programme.start_time_ms < programme.end_time_ms)
        .cloned()
        .collect::<Vec<_>>();
    for outcome in outcomes {
        let Some(programmes) = &outcome.programmes else {
            continue;
        };
        let valid_programmes = programmes
            .iter()
            .filter(|programme| programme.start_time_ms < programme.end_time_ms)
            .cloned()
            .collect::<Vec<_>>();
        if valid_programmes.is_empty() {
            continue;
        }
        merged.retain(|programme| {
            programme.channel_id != outcome.channel_id
                || !overlaps_window(programme, outcome.window)
        });
        merged.extend(valid_programmes);
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
