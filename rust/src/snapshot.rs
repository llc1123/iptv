//! Immutable channel and guide snapshots with bounded feed memoization.

use std::collections::HashSet;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use thiserror::Error;
use tokio::sync::Notify;

use crate::epg::models::{BootChannel, Programme};
use crate::guide::{GuideZone, IndexError, ProgrammeIndex};
use crate::lifecycle::CancellationToken;
use crate::render::{BaseUrl, RenderError, RenderKind, render_m3u, render_xmltv};

#[path = "snapshot_cache.rs"]
mod snapshot_cache;
use snapshot_cache::{CacheAcquire, RenderCache, wait_for_flight};

const MAX_CHANNELS: usize = 100_000;

/// Immutable logos aligned one-for-one with a snapshot's provider-order channels.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogoView {
    entries: Arc<[Option<String>]>,
}

impl LogoView {
    /// Builds a logo view from channel-aligned optional logo URLs.
    #[must_use]
    pub fn new(entries: Vec<Option<String>>) -> Self {
        Self {
            entries: Arc::from(entries.into_boxed_slice()),
        }
    }

    /// Builds a logo view from the logo values already present on channels.
    #[must_use]
    pub fn from_channels(channels: &[BootChannel]) -> Self {
        Self::new(
            channels
                .iter()
                .map(|channel| (!channel.logo_url.is_empty()).then(|| channel.logo_url.clone()))
                .collect(),
        )
    }

    /// Returns the immutable channel-aligned logo entries.
    #[must_use]
    pub fn entries(&self) -> &[Option<String>] {
        &self.entries
    }
}

/// Errors raised while constructing or publishing a snapshot.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum SnapshotError {
    /// The channel collection exceeded the fixed snapshot bound.
    #[error("snapshot channel limit exceeded")]
    TooManyChannels,
    /// A channel identifier was empty or repeated.
    #[error("invalid snapshot channel identifiers")]
    InvalidChannelIdentifiers,
    /// The logo view did not align with the channel collection.
    #[error("snapshot logo alignment invalid")]
    LogoAlignment,
    /// The programme input exceeded the immutable guide bound.
    #[error("snapshot programme index invalid")]
    ProgrammeLimit,
    /// The programme index contained conflicting data for one identity.
    #[error("snapshot programme identity conflict")]
    ProgrammeConflict,
    /// The programme index contained an invalid interval.
    #[error("snapshot programme interval invalid")]
    ProgrammeInterval,
    /// A publication attempted to move backwards or repeat a generation.
    #[error("snapshot generation is not newer")]
    GenerationNotNewer,
    /// The store lock was poisoned by a prior panic.
    #[error("snapshot store unavailable")]
    StoreUnavailable,
}

impl From<IndexError> for SnapshotError {
    fn from(error: IndexError) -> Self {
        match error {
            IndexError::TooManyProgrammes => Self::ProgrammeLimit,
            IndexError::ConflictingIdentity => Self::ProgrammeConflict,
            IndexError::InvalidInterval => Self::ProgrammeInterval,
        }
    }
}

/// One atomically publishable, read-only metadata generation.
pub struct Snapshot {
    generation: u64,
    channels: Arc<[BootChannel]>,
    programmes: Arc<[Programme]>,
    xmltv_programmes: Arc<[Programme]>,
    programme_index: ProgrammeIndex,
    logos: LogoView,
    guide_zone: GuideZone,
    catchup_days: u16,
    render_cache: Mutex<RenderCache>,
    render_invocations: AtomicUsize,
}

impl std::fmt::Debug for Snapshot {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Snapshot")
            .field("generation", &self.generation)
            .field("channel_count", &self.channels.len())
            .field("programme_count", &self.programmes.len())
            .field("catchup_days", &self.catchup_days)
            .finish_non_exhaustive()
    }
}

impl Snapshot {
    /// Creates a validated immutable snapshot.
    ///
    /// # Errors
    /// Returns a fixed [`SnapshotError`] when channels, logos, generations, or guide data
    /// violate their bounds or uniqueness invariants.
    pub fn new(
        generation: u64,
        channels: Vec<BootChannel>,
        programmes: Vec<Programme>,
        logos: LogoView,
        guide_zone: GuideZone,
        catchup_days: u16,
    ) -> Result<Arc<Self>, SnapshotError> {
        if channels.len() > MAX_CHANNELS {
            return Err(SnapshotError::TooManyChannels);
        }
        let mut ids = HashSet::with_capacity(channels.len());
        if channels
            .iter()
            .any(|channel| channel.channel_id.is_empty() || !ids.insert(channel.channel_id.clone()))
        {
            return Err(SnapshotError::InvalidChannelIdentifiers);
        }
        if logos.entries().len() != channels.len() {
            return Err(SnapshotError::LogoAlignment);
        }
        let xmltv_programmes = stable_xmltv_programmes(&programmes);
        let programme_index = ProgrammeIndex::new(programmes)?;
        Ok(Arc::new(Self {
            generation,
            channels: Arc::from(channels.into_boxed_slice()),
            programmes: Arc::from(programme_index.programmes().to_vec().into_boxed_slice()),
            xmltv_programmes: Arc::from(xmltv_programmes.into_boxed_slice()),
            programme_index,
            logos,
            guide_zone,
            catchup_days,
            render_cache: Mutex::new(RenderCache::default()),
            render_invocations: AtomicUsize::new(0),
        }))
    }

    /// Returns the generation number owned by this snapshot.
    #[must_use]
    pub const fn generation(&self) -> u64 {
        self.generation
    }

    /// Returns channels in the provider's original order.
    #[must_use]
    pub fn channels(&self) -> &[BootChannel] {
        &self.channels
    }

    /// Returns programmes in programme-index order.
    #[must_use]
    pub fn programmes(&self) -> &[Programme] {
        &self.programmes
    }

    /// Returns the replay lookup index.
    #[must_use]
    pub fn programme_index(&self) -> &ProgrammeIndex {
        &self.programme_index
    }

    /// Returns the aligned immutable logo view.
    #[must_use]
    pub fn logos(&self) -> &LogoView {
        &self.logos
    }

    /// Returns the configured guide timezone.
    #[must_use]
    pub const fn guide_zone(&self) -> &GuideZone {
        &self.guide_zone
    }

    /// Returns the configured catch-up day count.
    #[must_use]
    pub const fn catchup_days(&self) -> u16 {
        self.catchup_days
    }

    /// Returns the number of owner renders performed by this snapshot.
    #[must_use]
    pub fn render_invocations(&self) -> usize {
        self.render_invocations.load(Ordering::Relaxed)
    }

    /// Renders and memoizes one feed for this snapshot and request base.
    ///
    /// # Errors
    /// Returns a fixed [`RenderError`] when rendering, cancellation, cache capacity, or
    /// ownership checks fail.
    pub async fn render_cached(
        &self,
        kind: RenderKind,
        base_url: &BaseUrl,
        cancellation: Option<&CancellationToken>,
    ) -> Result<Arc<[u8]>, RenderError> {
        let flight = loop {
            match self.acquire_flight(kind, base_url.as_str())? {
                CacheAcquire::Ready(bytes) => return Ok(bytes),
                CacheAcquire::Flight {
                    flight,
                    owner: true,
                } => break flight,
                CacheAcquire::Flight {
                    flight,
                    owner: false,
                } => {
                    wait_for_flight(&flight, cancellation).await?;
                }
            }
        };
        if cancellation.is_some_and(CancellationToken::is_cancelled) {
            self.finish_flight(
                kind,
                base_url.as_str(),
                &flight,
                &Err(RenderError::Cancelled),
            )?;
            return Err(RenderError::Cancelled);
        }
        self.render_invocations.fetch_add(1, Ordering::Relaxed);
        let result = match kind {
            RenderKind::M3u => render_m3u(&self.render_channels(), base_url, self.catchup_days),
            RenderKind::Xmltv => render_xmltv(
                &self.render_channels(),
                &self.xmltv_programmes,
                &self.guide_zone,
                base_url,
            ),
        }
        .map(|bytes| Arc::from(bytes.into_boxed_slice()));
        if cancellation.is_some_and(CancellationToken::is_cancelled) {
            self.finish_flight(
                kind,
                base_url.as_str(),
                &flight,
                &Err(RenderError::Cancelled),
            )?;
            return Err(RenderError::Cancelled);
        }
        self.finish_flight(kind, base_url.as_str(), &flight, &result)?;
        result
    }

    /// Returns the number of currently tracked request bases.
    ///
    /// # Errors
    /// Returns [`RenderError::CacheUnavailable`] when the cache lock is poisoned.
    pub fn cached_base_count(&self) -> Result<usize, RenderError> {
        self.render_cache
            .lock()
            .map(|cache| cache.bases.len())
            .map_err(|_| RenderError::CacheUnavailable)
    }

    fn render_channels(&self) -> Vec<BootChannel> {
        self.channels
            .iter()
            .zip(self.logos.entries())
            .map(|(channel, logo)| {
                let mut channel = channel.clone();
                channel.logo_url = logo.clone().unwrap_or_default();
                channel
            })
            .collect()
    }
}

/// Atomically holds the current immutable snapshot.
#[derive(Clone, Debug)]
pub struct SnapshotStore {
    current: Arc<RwLock<Option<Arc<Snapshot>>>>,
    changed: Arc<Notify>,
}

impl Default for SnapshotStore {
    fn default() -> Self {
        Self {
            current: Arc::new(RwLock::new(None)),
            changed: Arc::new(Notify::new()),
        }
    }
}

impl SnapshotStore {
    /// Creates an empty snapshot store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Loads the current snapshot and releases the read lock before returning.
    ///
    /// # Errors
    /// Returns [`SnapshotError::StoreUnavailable`] when the store lock is poisoned.
    pub fn load(&self) -> Result<Option<Arc<Snapshot>>, SnapshotError> {
        self.current
            .read()
            .map(|current| current.clone())
            .map_err(|_| SnapshotError::StoreUnavailable)
    }

    /// Publishes a newer generation as one immutable value.
    ///
    /// # Errors
    /// Returns [`SnapshotError::GenerationNotNewer`] for an old generation or
    /// [`SnapshotError::StoreUnavailable`] when the store lock is poisoned.
    pub fn publish(&self, snapshot: Arc<Snapshot>) -> Result<(), SnapshotError> {
        let mut current = self
            .current
            .write()
            .map_err(|_| SnapshotError::StoreUnavailable)?;
        if current
            .as_ref()
            .is_some_and(|existing| snapshot.generation <= existing.generation)
        {
            return Err(SnapshotError::GenerationNotNewer);
        }
        *current = Some(snapshot);
        self.changed.notify_waiters();
        Ok(())
    }

    /// Waits for the first published snapshot without polling.
    pub async fn wait_for_snapshot(&self, cancellation: &CancellationToken) -> bool {
        loop {
            let notified = self.changed.notified();
            if self.load().ok().flatten().is_some() {
                return true;
            }
            tokio::select! {
                () = notified => {},
                () = cancellation.cancelled() => return false,
            }
        }
    }
}

fn stable_xmltv_programmes(input: &[Programme]) -> Vec<Programme> {
    let mut seen = HashSet::new();
    let mut programmes: Vec<Programme> = input
        .iter()
        .filter(|programme| {
            seen.insert((
                programme.channel_id.clone(),
                programme.start_time_ms,
                programme.end_time_ms,
                programme.title.clone(),
            ))
        })
        .cloned()
        .collect();
    programmes.sort_by(|left, right| {
        left.channel_id
            .cmp(&right.channel_id)
            .then(left.start_time_ms.cmp(&right.start_time_ms))
    });
    programmes
}

#[cfg(test)]
#[path = "snapshot_regression_tests.rs"]
mod regression_tests;
#[cfg(test)]
#[path = "snapshot_tests.rs"]
mod tests;
