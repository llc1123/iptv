//! Python-compatible guide cache bytes and two-file sidecar validation.

use std::{
    fs, io,
    path::{Path, PathBuf},
    time::Duration,
};

use chrono::Utc;
use ring::digest::{SHA256, digest};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use crate::epg::models::Programme;

use super::{
    cache_io::{AtomicWriteHooks, DefaultAtomicWriteHooks, write_atomic},
    fingerprint::CacheFingerprint,
};

const MAX_CACHE_BYTES: u64 = 16 * 1024 * 1024;
const MAX_METADATA_BYTES: u64 = 8 * 1024;
const SCHEMA: &str = "iptv-guide-cache";
const VERSION: u32 = 1;

/// The stable JSON sidecar schema.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct CacheMetadata {
    /// Sidecar schema identifier.
    pub schema: String,
    /// Sidecar schema version.
    pub version: u32,
    /// UTC fetch time in Unix milliseconds.
    pub fetched_at_ms: i64,
    /// SHA-256 of the exact list-file bytes.
    pub data_sha256: String,
    /// Secret-free configuration fingerprint.
    pub config_fingerprint: String,
}

/// The result of classifying a cache pair.
#[derive(Debug, PartialEq, Eq)]
pub enum CacheState {
    /// No cache list exists.
    Missing,
    /// The cache is valid and within the inclusive freshness age.
    Fresh {
        /// Parsed guide programmes.
        programmes: Vec<Programme>,
        /// Age of the cache in milliseconds.
        age_ms: u64,
    },
    /// The cache is valid but older than the freshness age.
    Stale {
        /// Age of the cache in milliseconds.
        age_ms: u64,
    },
    /// The cache cannot be served.
    Invalid(CacheInvalidReason),
}

impl CacheState {
    /// Returns cached programmes only when the complete cache is fresh.
    #[must_use]
    pub fn into_fresh_programmes(self) -> Option<Vec<Programme>> {
        match self {
            Self::Fresh { programmes, .. } => Some(programmes),
            Self::Missing | Self::Stale { .. } | Self::Invalid(_) => None,
        }
    }
}

/// Reasons a cache cannot be served.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum CacheInvalidReason {
    /// The list file is larger than the bounded cache size.
    #[error("cache oversize")]
    Oversize,
    /// The sidecar is missing while the list exists.
    #[error("missing cache companion")]
    MissingCompanion,
    /// The list is not a JSON list.
    #[error("cache is not a list")]
    NotAList,
    /// The list JSON is malformed.
    #[error("invalid cache JSON")]
    InvalidJson,
    /// One or more programme records are invalid.
    #[error("invalid cache programme")]
    InvalidProgramme,
    /// The sidecar schema or version is unsupported.
    #[error("invalid cache schema")]
    Schema,
    /// The list checksum does not match the sidecar.
    #[error("cache checksum mismatch")]
    Checksum,
    /// The current configuration does not match the sidecar.
    #[error("cache fingerprint mismatch")]
    Fingerprint,
    /// The sidecar timestamp is in the future.
    #[error("cache timestamp is in the future")]
    FutureTime,
    /// The filesystem returned an unusable cache file.
    #[error("cache filesystem error")]
    Filesystem,
}

/// Errors from cache parsing, fingerprinting, and atomic persistence.
#[derive(Debug, Error)]
pub enum CacheError {
    /// A cache fingerprint input is not a valid explicit origin.
    #[error("invalid cache fingerprint")]
    InvalidFingerprint,
    /// An atomic cache write failed.
    #[error("cache write failed")]
    Io(#[source] io::Error),
    /// Persistence was cancelled before publication.
    #[error("cache write cancelled")]
    Cancelled,
    /// Serialized cache bytes exceed the bounded format size.
    #[error("cache oversize")]
    Oversize,
}

/// Filesystem-backed guide cache.
#[derive(Clone, Debug)]
pub struct GuideCache {
    path: PathBuf,
}

/// Cancellation check used at every atomic-write boundary.
pub trait Cancellation {
    /// Returns true when publication must stop.
    fn is_cancelled(&self) -> bool;
}

/// A cancellation source that never cancels.
#[derive(Clone, Copy, Debug, Default)]
pub struct NeverCancelled;

impl Cancellation for NeverCancelled {
    fn is_cancelled(&self) -> bool {
        false
    }
}

impl GuideCache {
    /// Creates a cache handle for a list-file path.
    #[must_use]
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    /// Returns the list-file path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Returns the exact Python-compatible list bytes.
    ///
    /// # Errors
    /// Returns [`CacheError::Oversize`] when serialization exceeds the cache bound.
    pub fn serialize(programmes: &[Programme]) -> Result<Vec<u8>, CacheError> {
        let mut bytes = serde_json::to_vec_pretty(programmes).map_err(|_| CacheError::Oversize)?;
        bytes.push(b'\n');
        if u64::try_from(bytes.len()).map_err(|_| CacheError::Oversize)? > MAX_CACHE_BYTES {
            return Err(CacheError::Oversize);
        }
        Ok(bytes)
    }

    /// Classifies the cache pair without deleting invalid files.
    pub fn load(
        &self,
        now_ms: i64,
        max_age: Duration,
        fingerprint: &CacheFingerprint,
    ) -> CacheState {
        let data = match fs::read(&self.path) {
            Ok(data) => data,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return CacheState::Missing,
            Err(_) => return CacheState::Invalid(CacheInvalidReason::Filesystem),
        };
        if u64::try_from(data.len()).map_or(true, |size| size > MAX_CACHE_BYTES) {
            return CacheState::Invalid(CacheInvalidReason::Oversize);
        }
        let sidecar_path = sidecar_path(&self.path);
        let sidecar = match fs::read(&sidecar_path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return CacheState::Invalid(CacheInvalidReason::MissingCompanion);
            }
            Err(_) => return CacheState::Invalid(CacheInvalidReason::Filesystem),
        };
        if u64::try_from(sidecar.len()).map_or(true, |size| size > MAX_METADATA_BYTES) {
            return CacheState::Invalid(CacheInvalidReason::Oversize);
        }
        let Ok(metadata) = serde_json::from_slice::<CacheMetadata>(&sidecar) else {
            return CacheState::Invalid(CacheInvalidReason::Schema);
        };
        if metadata.schema != SCHEMA || metadata.version != VERSION {
            return CacheState::Invalid(CacheInvalidReason::Schema);
        }
        let actual_digest = hex::encode(digest(&SHA256, &data).as_ref());
        if metadata.data_sha256 != actual_digest {
            return CacheState::Invalid(CacheInvalidReason::Checksum);
        }
        if metadata.config_fingerprint != fingerprint.as_str() {
            return CacheState::Invalid(CacheInvalidReason::Fingerprint);
        }
        if metadata.fetched_at_ms > now_ms {
            return CacheState::Invalid(CacheInvalidReason::FutureTime);
        }
        let age_ms =
            u64::try_from(now_ms.saturating_sub(metadata.fetched_at_ms)).unwrap_or(u64::MAX);
        let max_age_ms = u64::try_from(max_age.as_millis()).unwrap_or(u64::MAX);
        let programmes = match parse_programmes(&data) {
            Ok(programmes) => programmes,
            Err(reason) => return CacheState::Invalid(reason),
        };
        if age_ms <= max_age_ms {
            CacheState::Fresh { programmes, age_ms }
        } else {
            CacheState::Stale { age_ms }
        }
    }

    /// Classifies a cache using twice the configured refresh interval.
    #[must_use]
    pub fn load_with_refresh_interval(
        &self,
        now_ms: i64,
        refresh_interval_seconds: u64,
        fingerprint: &CacheFingerprint,
    ) -> CacheState {
        let max_age = refresh_interval_seconds
            .checked_mul(2)
            .map_or(Duration::MAX, Duration::from_secs);
        self.load(now_ms, max_age, fingerprint)
    }

    /// Atomically writes the list and sidecar as separate sibling files.
    ///
    /// The two sibling renames are individually atomic, not pair-atomic. A mixed
    /// pair is rejected by checksum validation on the next load.
    ///
    /// # Errors
    /// Returns [`CacheError::Oversize`], [`CacheError::Cancelled`], or
    /// [`CacheError::Io`] when serialization or publication fails.
    pub fn save<C: Cancellation>(
        &self,
        programmes: &[Programme],
        fetched_at_ms: i64,
        fingerprint: &CacheFingerprint,
        cancellation: &C,
    ) -> Result<(), CacheError> {
        self.save_with_hooks(
            programmes,
            fetched_at_ms,
            fingerprint,
            cancellation,
            &DefaultAtomicWriteHooks,
        )
    }

    /// Atomically writes with caller-provided failure hooks.
    ///
    /// # Errors
    /// Returns [`CacheError::Oversize`], [`CacheError::Cancelled`], or
    /// [`CacheError::Io`] when serialization or publication fails.
    pub fn save_with_hooks<C: Cancellation, H: AtomicWriteHooks>(
        &self,
        programmes: &[Programme],
        fetched_at_ms: i64,
        fingerprint: &CacheFingerprint,
        cancellation: &C,
        hooks: &H,
    ) -> Result<(), CacheError> {
        let data = Self::serialize(programmes)?;
        let metadata = CacheMetadata {
            schema: SCHEMA.to_owned(),
            version: VERSION,
            fetched_at_ms,
            data_sha256: hex::encode(digest(&SHA256, &data).as_ref()),
            config_fingerprint: fingerprint.as_str().to_owned(),
        };
        let metadata_bytes =
            serde_json::to_vec_pretty(&metadata).map_err(|_| CacheError::Oversize)?;
        if u64::try_from(metadata_bytes.len()).map_or(true, |size| size > MAX_METADATA_BYTES) {
            return Err(CacheError::Oversize);
        }
        write_atomic(&self.path, &data, cancellation, hooks)?;
        write_atomic(
            &sidecar_path(&self.path),
            &metadata_bytes,
            cancellation,
            hooks,
        )
    }
}

fn parse_programmes(data: &[u8]) -> Result<Vec<Programme>, CacheInvalidReason> {
    let value =
        serde_json::from_slice::<Value>(data).map_err(|_| CacheInvalidReason::InvalidJson)?;
    let Some(items) = value.as_array() else {
        return Err(CacheInvalidReason::NotAList);
    };
    if items.len() > 100_000 {
        return Err(CacheInvalidReason::InvalidProgramme);
    }
    serde_json::from_value(value).map_err(|_| CacheInvalidReason::InvalidProgramme)
}

fn sidecar_path(path: &Path) -> PathBuf {
    PathBuf::from(format!("{}.meta.json", path.display()))
}

/// Returns the current UTC wall-clock timestamp used by cache callers.
#[must_use]
pub fn system_now_ms() -> i64 {
    Utc::now().timestamp_millis()
}

#[cfg(test)]
#[expect(clippy::unwrap_used, reason = "unit fixtures use fixed valid values")]
#[path = "cache_tests.rs"]
mod tests;
