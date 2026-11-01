//! Shared health snapshot and its additive JSON representation.

use std::sync::Arc;

use serde::Serialize;
use tokio::sync::RwLock;

use crate::error::OperationCode;

/// Runtime state reported by the health endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum HealthStatus {
    /// The service has not completed its first successful refresh.
    Starting,
    /// The service can serve its retained snapshot.
    Ok,
    /// The service is winding down and rejects new work.
    Draining,
}

/// The channel and programme counts retained across refresh failures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SnapshotCounts {
    /// Number of channels in the last successful snapshot.
    pub channels: u64,
    /// Number of programmes in the last successful snapshot.
    pub programmes: u64,
}

impl SnapshotCounts {
    /// Creates a pair of snapshot counts.
    #[must_use]
    pub const fn new(channels: u64, programmes: u64) -> Self {
        Self {
            channels,
            programmes,
        }
    }
}

/// Complete in-memory health state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HealthSnapshot {
    /// Current process state.
    pub status: HealthStatus,
    /// Whether the service may serve the retained snapshot.
    pub ready: bool,
    /// Whether the retained snapshot needs refresh.
    pub stale: bool,
    /// Number of retained channels.
    pub channels: u64,
    /// Number of retained programmes.
    pub programmes: u64,
    /// Epoch seconds of the last successful refresh.
    pub refreshed_at: Option<u64>,
    /// Fixed code for the most recent refresh or persistence failure.
    pub last_refresh_error: Option<OperationCode>,
    /// Number of consecutive failed refresh windows.
    pub failed_windows: u64,
    /// Epoch seconds until which the current snapshot lease is valid.
    pub lease_expires_at: Option<u64>,
}

impl Default for HealthSnapshot {
    fn default() -> Self {
        Self {
            status: HealthStatus::Starting,
            ready: false,
            stale: false,
            channels: 0,
            programmes: 0,
            refreshed_at: None,
            last_refresh_error: None,
            failed_windows: 0,
            lease_expires_at: None,
        }
    }
}

impl HealthSnapshot {
    fn view_at(&self, now: Option<u64>) -> HealthView {
        HealthView {
            status: self.status,
            ready: self.ready,
            stale: self.stale
                || now.is_some_and(|value| {
                    self.lease_expires_at
                        .is_some_and(|expires_at| value >= expires_at)
                }),
            channels: self.channels,
            programmes: self.programmes,
            refreshed_at: self.refreshed_at,
            last_refresh_error: self.last_refresh_error,
            failed_windows: self.failed_windows,
            lease_expires_at: self.lease_expires_at,
        }
    }
}

/// Additive JSON shape for health responses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct HealthView {
    /// Current process state.
    pub status: HealthStatus,
    /// Whether the retained snapshot may be served.
    pub ready: bool,
    /// Whether the retained snapshot needs refresh.
    pub stale: bool,
    /// Number of retained channels.
    pub channels: u64,
    /// Number of retained programmes.
    pub programmes: u64,
    /// Epoch seconds of the last successful refresh.
    pub refreshed_at: Option<u64>,
    /// Fixed code for the most recent refresh or persistence failure.
    pub last_refresh_error: Option<OperationCode>,
    /// Number of consecutive failed refresh windows.
    pub failed_windows: u64,
    /// Epoch seconds until which the current snapshot lease is valid.
    pub lease_expires_at: Option<u64>,
}

impl HealthView {
    /// Returns the HTTP status for this health view.
    #[must_use]
    pub const fn http_status(self) -> u16 {
        match self.status {
            HealthStatus::Draining => 503,
            HealthStatus::Starting | HealthStatus::Ok => 200,
        }
    }
}

/// Thread-safe health state shared by request and refresh tasks.
#[derive(Debug, Clone)]
pub struct HealthState {
    snapshot: Arc<RwLock<HealthSnapshot>>,
}

impl Default for HealthState {
    fn default() -> Self {
        Self::new()
    }
}

impl HealthState {
    /// Creates a starting health state.
    #[must_use]
    pub fn new() -> Self {
        Self {
            snapshot: Arc::new(RwLock::new(HealthSnapshot::default())),
        }
    }

    /// Returns a clone of the current health snapshot.
    pub async fn snapshot(&self) -> HealthSnapshot {
        self.snapshot.read().await.clone()
    }

    /// Returns the additive JSON view and its HTTP status.
    pub async fn response(&self) -> (u16, HealthView) {
        let view = self.snapshot.read().await.view_at(None);
        (view.http_status(), view)
    }

    /// Returns the health view projected at a caller-provided epoch second.
    pub async fn response_at(&self, now: u64) -> (u16, HealthView) {
        let view = self.snapshot.read().await.view_at(Some(now));
        (view.http_status(), view)
    }

    /// Marks the service ready with a fresh retained snapshot.
    pub async fn record_refresh_success(
        &self,
        refreshed_at: u64,
        lease_expires_at: u64,
        counts: SnapshotCounts,
    ) {
        let mut snapshot = self.snapshot.write().await;
        snapshot.status = HealthStatus::Ok;
        snapshot.ready = true;
        snapshot.stale = false;
        snapshot.channels = counts.channels;
        snapshot.programmes = counts.programmes;
        snapshot.refreshed_at = Some(refreshed_at);
        snapshot.last_refresh_error = None;
        snapshot.failed_windows = 0;
        snapshot.lease_expires_at = Some(lease_expires_at);
    }

    /// Records a failed refresh while retaining the last successful snapshot.
    pub async fn record_refresh_failure(&self, code: OperationCode) {
        let mut snapshot = self.snapshot.write().await;
        snapshot.status = HealthStatus::Ok;
        snapshot.ready = true;
        snapshot.stale = true;
        snapshot.last_refresh_error = Some(code);
        snapshot.failed_windows = snapshot.failed_windows.saturating_add(1);
    }

    /// Records persistence failure while retaining the last successful snapshot.
    pub async fn record_persistence_failure(&self) {
        self.record_refresh_failure(OperationCode::Persistence)
            .await;
    }

    /// Marks the service as draining without discarding snapshot data.
    pub async fn mark_draining(&self) {
        self.snapshot.write().await.status = HealthStatus::Draining;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn initial_state_is_starting_and_not_ready() {
        let state = HealthState::new();
        let (status, view) = state.response().await;

        assert_eq!(status, 200);
        assert_eq!(view.status, HealthStatus::Starting);
        assert!(!view.ready);
        assert!(!view.stale);
        assert_eq!(view.channels, 0);
        assert_eq!(view.programmes, 0);
    }

    #[tokio::test]
    async fn ready_state_serializes_epoch_fields_and_snapshot_counts() {
        let state = HealthState::new();
        state
            .record_refresh_success(1_700_000_000, 1_700_003_600, SnapshotCounts::new(12, 34))
            .await;
        let (status, view) = state.response().await;
        let json = serde_json::to_value(view);

        assert_eq!(status, 200);
        assert!(json.is_ok());
        let json = json.unwrap_or_default();
        assert_eq!(json["status"], "ok");
        assert_eq!(json["ready"], true);
        assert_eq!(json["stale"], false);
        assert_eq!(json["channels"], 12);
        assert_eq!(json["programmes"], 34);
        assert_eq!(json["refreshed_at"], 1_700_000_000);
        assert_eq!(json["lease_expires_at"], 1_700_003_600);
    }

    #[tokio::test]
    async fn stale_ready_keeps_http_success_and_snapshot_counts() {
        let state = HealthState::new();
        state
            .record_refresh_success(100, 200, SnapshotCounts::new(4, 5))
            .await;
        state.record_refresh_failure(OperationCode::Refresh).await;
        let (status, view) = state.response().await;

        assert_eq!(status, 200);
        assert!(view.ready);
        assert!(view.stale);
        assert_eq!(view.channels, 4);
        assert_eq!(view.programmes, 5);
        assert_eq!(view.last_refresh_error, Some(OperationCode::Refresh));
        assert_eq!(view.failed_windows, 1);
    }

    #[tokio::test]
    async fn expired_lease_projects_stale_without_changing_http_success() {
        let state = HealthState::new();
        state
            .record_refresh_success(100, 200, SnapshotCounts::new(4, 5))
            .await;
        let (status, view) = state.response_at(200).await;

        assert_eq!(status, 200);
        assert!(view.ready);
        assert!(view.stale);
        assert_eq!(view.lease_expires_at, Some(200));
    }

    #[tokio::test]
    async fn draining_returns_503_without_discarding_snapshot() {
        let state = HealthState::new();
        state
            .record_refresh_success(100, 200, SnapshotCounts::new(4, 5))
            .await;
        state.mark_draining().await;
        let (status, view) = state.response().await;

        assert_eq!(status, 503);
        assert_eq!(view.status, HealthStatus::Draining);
        assert_eq!(view.channels, 4);
        assert_eq!(view.programmes, 5);
        assert!(view.ready);
    }

    #[tokio::test]
    async fn persistence_failure_uses_fixed_code_and_increments_failed_windows() {
        let state = HealthState::new();
        state
            .record_refresh_success(100, 200, SnapshotCounts::new(4, 5))
            .await;
        state.record_persistence_failure().await;
        let view = state.snapshot().await.view_at(None);

        assert_eq!(view.last_refresh_error, Some(OperationCode::Persistence));
        assert_eq!(view.failed_windows, 1);
        assert_eq!(view.channels, 4);
        assert_eq!(view.programmes, 5);
    }

    #[tokio::test]
    async fn failed_window_codes_cannot_contain_sensitive_values() {
        let state = HealthState::new();
        state
            .record_refresh_success(100, 200, SnapshotCounts::new(4, 5))
            .await;
        state.record_refresh_failure(OperationCode::EpgHttp).await;
        let json = serde_json::to_string(&state.snapshot().await.view_at(None));

        assert!(json.is_ok());
        let json = json.unwrap_or_default();
        assert!(!json.contains("signed-url"));
        assert!(!json.contains("Authorization"));
        assert!(!json.contains("password"));
        assert!(!json.contains("token"));
        assert!(!json.contains("aa:bb:cc:dd:ee:ff"));
        assert!(json.contains("epg-http"));
    }
}
