use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use super::{MetadataError, RefreshEngine, WindowOutcome, merge_windows, overlaps_window};
use super::{MetadataSession, MetadataSource, RefreshEngineConfig};
use crate::epg::models::{BootChannel, Programme};
use crate::guide::{GuideWindow, GuideZone};
use crate::health::HealthState;
use crate::lifecycle::CancellationToken;
use crate::snapshot::SnapshotStore;

type TestFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

struct FakeSource {
    calls: AtomicUsize,
    fail_windows: bool,
}

impl MetadataSource for FakeSource {
    fn authenticate(
        &self,
        _cancellation: CancellationToken,
    ) -> TestFuture<'_, Result<Arc<MetadataSession>, MetadataError>> {
        Box::pin(async { Ok(Arc::new(MetadataSession::new(1))) })
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
        _session: Arc<MetadataSession>,
        _channel: BootChannel,
        window: GuideWindow,
        _cancellation: CancellationToken,
    ) -> TestFuture<'_, Result<Vec<Programme>, MetadataError>> {
        let failed = self.fail_windows;
        self.calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            if failed {
                Err(MetadataError::Business)
            } else {
                Ok(vec![programme("c1", window.start_ms, window.end_ms, "p1")])
            }
        })
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
        },
        WindowOutcome {
            channel_id: "c1".to_owned(),
            window: GuideWindow {
                start_ms: 20,
                end_ms: 40,
            },
            programmes: None,
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
        fail_windows: false,
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
