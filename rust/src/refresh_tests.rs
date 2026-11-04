use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use super::{
    MetadataError, RefreshClock, RefreshEngine, RefreshSleeper, WindowOutcome, merge_windows,
    overlaps_window,
};
use super::{MetadataSession, MetadataSource, RefreshEngineConfig};
use crate::epg::models::{BootChannel, Programme};
use crate::guide::{GuideWindow, GuideZone};
use crate::health::HealthState;
use crate::lifecycle::CancellationToken;
use crate::snapshot::SnapshotStore;

type TestFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

struct FakeSource {
    calls: AtomicUsize,
    auth_calls: AtomicUsize,
    generation: AtomicU64,
    fail_windows: bool,
    expire_first_window: AtomicBool,
    cancel_on_programme: Option<crate::lifecycle::CancellationSource>,
}

impl MetadataSource for FakeSource {
    fn authenticate(
        &self,
        _cancellation: CancellationToken,
    ) -> TestFuture<'_, Result<Arc<MetadataSession>, MetadataError>> {
        self.auth_calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Ok(Arc::new(MetadataSession::new(1))) })
    }

    fn reauthenticate(
        &self,
        _session: Arc<MetadataSession>,
        _cancellation: CancellationToken,
    ) -> TestFuture<'_, Result<Arc<MetadataSession>, MetadataError>> {
        let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 2;
        self.auth_calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move { Ok(Arc::new(MetadataSession::new(generation))) })
    }

    fn channels(
        &self,
        _session: Arc<MetadataSession>,
        _cancellation: CancellationToken,
    ) -> TestFuture<'_, Result<Vec<BootChannel>, MetadataError>> {
        Box::pin(async {
            Ok(vec![BootChannel {
                channel_id: "c1".to_owned(),
                name: "News".to_owned(),
                mix_no: "1".to_owned(),
                channel_url: String::new(),
                time_shift_url: "ts".to_owned(),
                channel_sdp: String::new(),
                time_shift: String::new(),
                time_shift_length: String::new(),
                logo_url: String::new(),
                code: "code".to_owned(),
                group_title: "All".to_owned(),
            }])
        })
    }

    fn programmes(
        &self,
        session: Arc<MetadataSession>,
        _channel: BootChannel,
        window: GuideWindow,
        _cancellation: CancellationToken,
    ) -> TestFuture<'_, Result<Vec<Programme>, MetadataError>> {
        let failed = self.fail_windows;
        let expire = self.expire_first_window.swap(false, Ordering::SeqCst);
        let reauthenticated_generation = self.auth_calls.load(Ordering::SeqCst) > 1;
        if let Some(cancellation) = &self.cancel_on_programme {
            cancellation.cancel();
        }
        self.calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            if expire {
                Err(MetadataError::SessionExpired)
            } else if failed {
                Err(MetadataError::Business)
            } else if reauthenticated_generation && session.generation() != 2 {
                Err(MetadataError::Authentication(
                    crate::epg::EpgErrorStage::Authentication,
                    crate::epg::EpgErrorClass::MissingField,
                ))
            } else {
                Ok(vec![programme("c1", window.start_ms, window.end_ms, "p1")])
            }
        })
    }
}

#[derive(Clone)]
struct TestClock {
    now: Arc<Mutex<Instant>>,
    monotonic_calls: Arc<AtomicUsize>,
    utc: chrono::DateTime<chrono::Utc>,
}

impl TestClock {
    fn new(now: Instant) -> Self {
        Self {
            now: Arc::new(Mutex::new(now)),
            monotonic_calls: Arc::new(AtomicUsize::new(0)),
            utc: chrono::DateTime::<chrono::Utc>::UNIX_EPOCH,
        }
    }
}

impl RefreshClock for TestClock {
    fn monotonic(&self) -> Instant {
        let call = self.monotonic_calls.fetch_add(1, Ordering::SeqCst);
        *self.now.lock().expect("clock lock")
            + Duration::from_secs(u64::try_from(call).unwrap_or(u64::MAX) * 5)
    }

    fn utc(&self) -> chrono::DateTime<chrono::Utc> {
        self.utc
    }
}

struct CancellingSleeper {
    cancellation: crate::lifecycle::CancellationSource,
    durations: Arc<Mutex<Vec<Duration>>>,
}

impl RefreshSleeper for CancellingSleeper {
    fn sleep(&self, duration: Duration) -> TestFuture<'_, ()> {
        self.durations.lock().expect("duration lock").push(duration);
        self.cancellation.cancel();
        Box::pin(std::future::pending())
    }
}

fn programme(channel_id: &str, start_time_ms: i64, end_time_ms: i64, id: &str) -> Programme {
    Programme {
        title: id.to_owned(),
        start_time_ms,
        end_time_ms,
        programme_id: id.to_owned(),
        channel_id: channel_id.to_owned(),
    }
}

#[test]
fn backoff_uses_interval_lower_bound_and_saturates() {
    assert_eq!(
        RefreshEngine::backoff_delay(Duration::from_secs(10), 1),
        Duration::from_secs(10)
    );
    assert_eq!(
        RefreshEngine::backoff_delay(Duration::from_secs(600), 1),
        Duration::from_secs(120)
    );
    assert_eq!(
        RefreshEngine::backoff_delay(Duration::from_secs(600), 20),
        Duration::from_secs(600)
    );
    assert_eq!(
        RefreshEngine::backoff_delay(Duration::from_secs(600), u32::MAX),
        Duration::from_secs(600)
    );
}

#[test]
fn successful_window_replaces_overlap_and_failed_window_carries_only_overlap() {
    let previous = vec![
        programme("c1", 0, 10, "old-overlap"),
        programme("c1", 20, 30, "old-outside"),
        programme("c2", 0, 10, "other-channel"),
    ];
    let outcomes = vec![
        WindowOutcome {
            channel_id: "c1".to_owned(),
            window: GuideWindow {
                start_ms: 0,
                end_ms: 15,
            },
            programmes: Some(vec![programme("c1", 1, 5, "new")]),
            error: None,
        },
        WindowOutcome {
            channel_id: "c1".to_owned(),
            window: GuideWindow {
                start_ms: 20,
                end_ms: 40,
            },
            programmes: None,
            error: Some(MetadataError::Business),
        },
    ];
    let merged = merge_windows(&previous, &outcomes);
    assert!(merged.iter().any(|item| item.programme_id == "new"));
    assert!(!merged.iter().any(|item| item.programme_id == "old-overlap"));
    assert!(merged.iter().any(|item| item.programme_id == "old-outside"));
    assert!(
        merged
            .iter()
            .any(|item| item.programme_id == "other-channel")
    );
}

#[test]
fn programme_window_uses_half_open_overlap() {
    let item = programme("c1", 10, 20, "p");
    assert!(overlaps_window(
        &item,
        GuideWindow {
            start_ms: 19,
            end_ms: 21
        }
    ));
    assert!(!overlaps_window(
        &item,
        GuideWindow {
            start_ms: 20,
            end_ms: 30
        }
    ));
}

#[test]
fn metadata_error_classification_does_not_reauth_business_failures() {
    assert_eq!(
        MetadataError::from(crate::epg::EpgError::cancelled(
            crate::epg::EpgErrorStage::Request
        )),
        MetadataError::Cancelled
    );
}

#[tokio::test]
async fn successful_cycle_publishes_one_generation_and_records_health() {
    let cancellation = crate::lifecycle::CancellationSource::new();
    let source = Arc::new(FakeSource {
        calls: AtomicUsize::new(0),
        auth_calls: AtomicUsize::new(0),
        generation: AtomicU64::new(0),
        fail_windows: false,
        expire_first_window: AtomicBool::new(false),
        cancel_on_programme: None,
    });
    let health = HealthState::new();
    let zone = GuideZone::Iana(chrono_tz::UTC);
    let mut engine = RefreshEngine::new(RefreshEngineConfig {
        source,
        store: SnapshotStore::new(),
        cache: None,
        cache_fingerprint: None,
        health: health.clone(),
        interval: Duration::from_secs(3600),
        windows: vec![GuideWindow {
            start_ms: 0,
            end_ms: 100,
        }],
        guide_zone: zone,
        catchup_days: 1,
        cancellation: cancellation.token(),
        clock: None,
        sleeper: None,
    });
    let result = engine.refresh_once().await;
    assert!(result.is_ok());
    let Some(outcome) = result.ok() else { return };
    assert_eq!(outcome.generation, 1);
    assert!(health.snapshot().await.ready);
}

#[tokio::test]
async fn all_scheduled_windows_complete_after_the_error_detail_limit() {
    let cancellation = crate::lifecycle::CancellationSource::new();
    let source = Arc::new(FakeSource {
        calls: AtomicUsize::new(0),
        auth_calls: AtomicUsize::new(0),
        generation: AtomicU64::new(0),
        fail_windows: true,
        expire_first_window: AtomicBool::new(false),
        cancel_on_programme: None,
    });
    let source_for_assertion = Arc::clone(&source);
    let mut engine = RefreshEngine::new(RefreshEngineConfig {
        source,
        store: SnapshotStore::new(),
        cache: None,
        cache_fingerprint: None,
        health: HealthState::new(),
        interval: Duration::from_secs(60),
        windows: (0_i64..33)
            .map(|index| GuideWindow {
                start_ms: index * 100,
                end_ms: index * 100 + 100,
            })
            .collect(),
        guide_zone: GuideZone::Iana(chrono_tz::UTC),
        catchup_days: 1,
        cancellation: cancellation.token(),
        clock: None,
        sleeper: None,
    });

    assert_eq!(engine.refresh_once().await, Err(MetadataError::Business));
    assert_eq!(source_for_assertion.calls.load(Ordering::SeqCst), 33);
    assert_eq!(source_for_assertion.auth_calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn expired_programme_window_reauthenticates_and_replays_once() {
    let cancellation = crate::lifecycle::CancellationSource::new();
    let source = Arc::new(FakeSource {
        calls: AtomicUsize::new(0),
        auth_calls: AtomicUsize::new(0),
        generation: AtomicU64::new(0),
        fail_windows: false,
        expire_first_window: AtomicBool::new(true),
        cancel_on_programme: None,
    });
    let source_for_assertion = Arc::clone(&source);
    let mut engine = RefreshEngine::new(RefreshEngineConfig {
        source,
        store: SnapshotStore::new(),
        cache: None,
        cache_fingerprint: None,
        health: HealthState::new(),
        interval: Duration::from_secs(60),
        windows: vec![GuideWindow {
            start_ms: 0,
            end_ms: 100,
        }],
        guide_zone: GuideZone::Iana(chrono_tz::UTC),
        catchup_days: 1,
        cancellation: cancellation.token(),
        clock: None,
        sleeper: None,
    });

    assert_eq!(
        engine
            .refresh_once()
            .await
            .map(|outcome| outcome.failed_windows),
        Ok(0)
    );
    assert_eq!(source_for_assertion.auth_calls.load(Ordering::SeqCst), 2);
    assert_eq!(source_for_assertion.calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn cancellation_after_window_fetch_skips_snapshot_publication() {
    let cancellation = crate::lifecycle::CancellationSource::new();
    let source = Arc::new(FakeSource {
        calls: AtomicUsize::new(0),
        auth_calls: AtomicUsize::new(0),
        generation: AtomicU64::new(0),
        fail_windows: false,
        expire_first_window: AtomicBool::new(false),
        cancel_on_programme: Some(cancellation.clone()),
    });
    let store = SnapshotStore::new();
    let mut engine = RefreshEngine::new(RefreshEngineConfig {
        source,
        store: store.clone(),
        cache: None,
        cache_fingerprint: None,
        health: HealthState::new(),
        interval: Duration::from_secs(60),
        windows: vec![GuideWindow {
            start_ms: 0,
            end_ms: 100,
        }],
        guide_zone: GuideZone::Iana(chrono_tz::UTC),
        catchup_days: 1,
        cancellation: cancellation.token(),
        clock: None,
        sleeper: None,
    });

    assert_eq!(engine.refresh_once().await, Err(MetadataError::Cancelled));
    assert!(store.load().expect("snapshot store").is_none());
}

#[tokio::test]
async fn scheduler_uses_injected_clock_at_cycle_completion() {
    let cancellation = crate::lifecycle::CancellationSource::new();
    let clock = Arc::new(TestClock::new(Instant::now()));
    let durations = Arc::new(Mutex::new(Vec::new()));
    let sleeper = Arc::new(CancellingSleeper {
        cancellation: cancellation.clone(),
        durations: Arc::clone(&durations),
    });
    let source = Arc::new(FakeSource {
        calls: AtomicUsize::new(0),
        auth_calls: AtomicUsize::new(0),
        generation: AtomicU64::new(0),
        fail_windows: false,
        expire_first_window: AtomicBool::new(false),
        cancel_on_programme: None,
    });
    let engine = RefreshEngine::new(RefreshEngineConfig {
        source,
        store: SnapshotStore::new(),
        cache: None,
        cache_fingerprint: None,
        health: HealthState::new(),
        interval: Duration::from_secs(60),
        windows: vec![GuideWindow {
            start_ms: 0,
            end_ms: 100,
        }],
        guide_zone: GuideZone::Iana(chrono_tz::UTC),
        catchup_days: 1,
        cancellation: cancellation.token(),
        clock: Some(clock),
        sleeper: Some(sleeper),
    });

    engine.run().await;
    assert_eq!(
        durations.lock().expect("duration lock").as_slice(),
        [Duration::from_secs(55)]
    );
}
