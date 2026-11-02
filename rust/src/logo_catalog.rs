//! Bounded, local-only compatibility for an existing Python logo catalog.

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use thiserror::Error;

/// Default relative path used by the optional catalog configuration.
pub const DEFAULT_CATALOG_PATH: &str = "var/logo-catalog/catalog.json";
/// Maximum catalog JSON bytes accepted by the default loader.
pub const MAX_CATALOG_BYTES: usize = 4 * 1024 * 1024;
/// Maximum channel entries accepted by the default loader.
pub const MAX_CATALOG_ENTRIES: usize = 100_000;
/// Maximum bytes read from one logo file by the default loader.
pub const MAX_ENTRY_BYTES: usize = 8 * 1024 * 1024;
const CACHE_CONTROL: &str = "public, max-age=86400";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
/// Resource bounds applied while loading and serving a catalog.
pub struct LogoCatalogLimits {
    /// Maximum catalog JSON bytes.
    pub max_catalog_bytes: usize,
    /// Maximum channel entries.
    pub max_entries: usize,
    /// Maximum bytes read from one logo file.
    pub max_file_bytes: usize,
}

impl Default for LogoCatalogLimits {
    fn default() -> Self {
        Self {
            max_catalog_bytes: MAX_CATALOG_BYTES,
            max_entries: MAX_CATALOG_ENTRIES,
            max_file_bytes: MAX_ENTRY_BYTES,
        }
    }
}

#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
/// Fixed, redacted failures for catalog loading and file serving.
pub enum LogoCatalogError {
    /// The optional catalog does not exist.
    #[error("logo catalog is missing")]
    Missing,
    /// The catalog path could not be read safely.
    #[error("logo catalog cannot be read")]
    Read,
    /// The catalog exceeded its byte bound.
    #[error("logo catalog is too large")]
    CatalogTooLarge,
    /// The catalog is not valid JSON.
    #[error("logo catalog JSON is malformed")]
    MalformedJson,
    /// Required schema fields are invalid.
    #[error("logo catalog schema is invalid")]
    InvalidSchema,
    /// The catalog version is not supported.
    #[error("logo catalog version is unsupported")]
    UnsupportedVersion,
    /// The catalog exceeded its entry bound.
    #[error("logo catalog entry limit exceeded")]
    TooManyEntries,
    /// An entry has an invalid field value.
    #[error("logo catalog entry is invalid")]
    InvalidEntry,
    /// A relative logo path is unsafe.
    #[error("logo catalog path is unsafe")]
    UnsafePath,
    /// The catalog root is unsafe.
    #[error("logo catalog root is unsafe")]
    UnsafeRoot,
    /// The logo extension has no safe MIME mapping.
    #[error("logo file extension is unsupported")]
    UnsupportedExtension,
    /// The logo is not a regular available file.
    #[error("logo file is unavailable")]
    Unavailable,
    /// The logo exceeded its byte bound.
    #[error("logo file is too large")]
    FileTooLarge,
    /// The logo could not be read.
    #[error("logo file cannot be read")]
    FileRead,
    /// The requested byte range is outside the logo.
    #[error("logo range is invalid")]
    InvalidRange,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
struct CatalogFile {
    #[serde(default = "default_version")]
    version: u64,
    #[serde(default = "default_raw_m3u")]
    raw_m3u: String,
    #[serde(default = "default_matched_m3u")]
    matched_m3u: String,
    #[serde(default)]
    channels: Vec<CatalogEntry>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
struct CatalogEntry {
    pub(super) channel_id: String,
    pub(super) stream_url: String,
    pub(super) display_name: String,
    #[serde(default)]
    pub(super) tvg_id: String,
    #[serde(default)]
    pub(super) tvg_name: String,
    #[serde(default)]
    pub(super) tvg_logo: String,
    #[serde(default)]
    pub(super) group_title: String,
    #[serde(default)]
    pub(super) logo_path: String,
}

fn default_version() -> u64 {
    1
}
fn default_raw_m3u() -> String {
    "channels.raw.m3u".to_owned()
}
fn default_matched_m3u() -> String {
    "channels.matched.m3u".to_owned()
}

#[derive(Clone, Debug, PartialEq, Eq)]
/// A bounded local logo response.
pub struct LogoAsset {
    /// Bytes selected by the request.
    pub bytes: Vec<u8>,
    /// Safe MIME type derived from the extension.
    pub content_type: &'static str,
    /// Cache policy compatible with the Python file response.
    pub cache_control: &'static str,
}

/// A validated catalog rooted at its catalog directory.
#[derive(Clone, Debug)]
pub struct LogoCatalog {
    root: PathBuf,
    entries: HashMap<String, CatalogEntry>,
    limits: LogoCatalogLimits,
}

impl LogoCatalog {
    /// Loads a catalog using fixed default bounds.
    ///
    /// # Errors
    /// Returns a fixed error when the catalog is malformed, oversized, or unsafe.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, LogoCatalogError> {
        Self::load_with_limits(path, LogoCatalogLimits::default())
    }

    /// Loads a catalog when present, preserving optional startup behavior.
    ///
    /// # Errors
    /// Returns a fixed error when a present catalog is malformed, oversized, or unsafe.
    pub fn load_if_present(path: impl AsRef<Path>) -> Result<Option<Self>, LogoCatalogError> {
        match fs::symlink_metadata(path.as_ref()) {
            Ok(_) => Self::load(path).map(Some),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(_) => Err(LogoCatalogError::Read),
        }
    }

    /// Loads a catalog with caller-selected resource bounds.
    ///
    /// # Errors
    /// Returns a fixed error when the catalog violates its schema or supplied bounds.
    pub fn load_with_limits(
        path: impl AsRef<Path>,
        limits: LogoCatalogLimits,
    ) -> Result<Self, LogoCatalogError> {
        let path = path.as_ref();
        let root = safe_root(path.parent().ok_or(LogoCatalogError::UnsafeRoot)?)?;
        let metadata = regular_metadata(path).map_err(|_| LogoCatalogError::Read)?;
        if metadata.len()
            > u64::try_from(limits.max_catalog_bytes)
                .map_err(|_| LogoCatalogError::CatalogTooLarge)?
        {
            return Err(LogoCatalogError::CatalogTooLarge);
        }
        let bytes = read_bounded(path, limits.max_catalog_bytes, LogoCatalogError::Read)?;
        let value: serde_json::Value =
            serde_json::from_slice(&bytes).map_err(|_| LogoCatalogError::MalformedJson)?;
        if !value.is_object() {
            return Err(LogoCatalogError::InvalidSchema);
        }
        let file: CatalogFile =
            serde_json::from_value(value).map_err(|_| LogoCatalogError::InvalidSchema)?;
        if file.version != 1 {
            return Err(LogoCatalogError::UnsupportedVersion);
        }
        if file.raw_m3u.is_empty() || file.matched_m3u.is_empty() {
            return Err(LogoCatalogError::InvalidSchema);
        }
        if file.channels.len() > limits.max_entries {
            return Err(LogoCatalogError::TooManyEntries);
        }
        let mut entries = HashMap::with_capacity(file.channels.len());
        for entry in file.channels {
            validate_entry(&entry, &root)?;
            entries.insert(entry.channel_id.clone(), entry);
        }
        Ok(Self {
            root,
            entries,
            limits,
        })
    }

    /// Reads the complete logo for a catalog channel.
    ///
    /// # Errors
    /// Returns a fixed error when the selected file is unsafe, oversized, or unreadable.
    pub fn read(&self, channel_id: &str) -> Result<Option<LogoAsset>, LogoCatalogError> {
        self.read_range(channel_id, None)
    }

    /// Reads an inclusive byte range, or the complete logo when omitted.
    ///
    /// # Errors
    /// Returns a fixed error when the selected file or range is invalid.
    pub fn read_range(
        &self,
        channel_id: &str,
        range: Option<(u64, u64)>,
    ) -> Result<Option<LogoAsset>, LogoCatalogError> {
        let Some(entry) = self.entries.get(channel_id) else {
            return Ok(None);
        };
        if entry.logo_path.is_empty() {
            return Ok(None);
        }
        let path = self.safe_entry_path(&entry.logo_path)?;
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(LogoCatalogError::FileRead),
        };
        if !metadata.file_type().is_file() {
            return Err(LogoCatalogError::Unavailable);
        }
        if metadata.len()
            > u64::try_from(self.limits.max_file_bytes)
                .map_err(|_| LogoCatalogError::FileTooLarge)?
        {
            return Err(LogoCatalogError::FileTooLarge);
        }
        let resolved = fs::canonicalize(&path).map_err(|_| LogoCatalogError::FileRead)?;
        if !resolved.starts_with(&self.root) {
            return Err(LogoCatalogError::UnsafePath);
        }
        let bytes = read_bounded(
            &resolved,
            self.limits.max_file_bytes,
            LogoCatalogError::FileRead,
        )?;
        let selected = match range {
            None => bytes,
            Some((start, end))
                if start <= end
                    && end
                        < u64::try_from(bytes.len())
                            .map_err(|_| LogoCatalogError::InvalidRange)? =>
            {
                let start = usize::try_from(start).map_err(|_| LogoCatalogError::InvalidRange)?;
                let end = usize::try_from(end).map_err(|_| LogoCatalogError::InvalidRange)?;
                bytes[start..=end].to_vec()
            }
            Some(_) => return Err(LogoCatalogError::InvalidRange),
        };
        Ok(Some(LogoAsset {
            bytes: selected,
            content_type: content_type(&entry.logo_path)
                .ok_or(LogoCatalogError::UnsupportedExtension)?,
            cache_control: CACHE_CONTROL,
        }))
    }

    fn safe_entry_path(&self, value: &str) -> Result<PathBuf, LogoCatalogError> {
        validate_relative_path(value)?;
        reject_symlink_components(&self.root, value)?;
        let path = self.root.join(value);
        let resolved = match fs::canonicalize(&path) {
            Ok(resolved) => resolved,
            Err(error) if error.kind() == io::ErrorKind::NotFound => path,
            Err(_) => return Err(LogoCatalogError::FileRead),
        };
        if !resolved.starts_with(&self.root) {
            return Err(LogoCatalogError::UnsafePath);
        }
        Ok(resolved)
    }
}

#[path = "logo_catalog_paths.rs"]
mod paths;
use paths::{
    content_type, read_bounded, regular_metadata, reject_symlink_components, safe_root,
    validate_entry, validate_relative_path,
};

#[cfg(test)]
#[path = "logo_catalog_tests.rs"]
mod tests;
