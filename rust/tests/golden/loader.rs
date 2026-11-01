//! Typed loader for the deterministic golden-fixture corpus.
//!
//! The loader parses the fixture manifest (schema/version), verifies each
//! fixture file against its manifest-declared SHA-256, and returns typed
//! errors for malformed, drifted, or unsafe fixture content.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use serde::Deserialize;

/// Errors raised while loading or validating the golden fixture corpus.
#[derive(Debug, thiserror::Error)]
pub(crate) enum GoldenError {
    /// Reading a fixture file failed.
    #[error("fixture io failed for {path}: {message}")]
    Io {
        /// Fixture file path.
        path: String,
        /// Operating-system error text.
        message: String,
    },
    /// Fixture bytes are not valid JSON for the declared schema.
    #[error("fixture {path} is not valid fixture JSON: {message}")]
    MalformedJson {
        /// Fixture file path.
        path: String,
        /// Serialization error text.
        message: String,
    },
    /// The manifest declares an unknown schema name.
    #[error("manifest schema {found} is not supported")]
    UnsupportedSchema {
        /// Schema name found in the manifest.
        found: String,
    },
    /// The manifest declares an unsupported schema version.
    #[error("manifest version {found} is not supported")]
    UnsupportedVersion {
        /// Version found in the manifest.
        found: u64,
    },
    /// A fixture file declares a different family than its manifest entry.
    #[error("fixture {path} declares family {found} but its manifest entry says {expected}")]
    FamilyMismatch {
        /// Fixture file path.
        path: String,
        /// Family from the manifest entry.
        expected: String,
        /// Family found inside the fixture file.
        found: String,
    },
    /// A fixture file does not match its manifest-declared SHA-256.
    #[error("fixture {path} hash mismatch: manifest declares {expected}, file hashes to {actual}")]
    HashMismatch {
        /// Fixture file path.
        path: String,
        /// SHA-256 declared in the manifest.
        expected: String,
        /// SHA-256 computed from the file bytes.
        actual: String,
    },
    /// The manifest case count disagrees with the fixture file contents.
    #[error("fixture {path} lists {expected} cases in the manifest but contains {found}")]
    CaseCountMismatch {
        /// Fixture file path.
        path: String,
        /// Case count declared in the manifest.
        expected: usize,
        /// Case count parsed from the fixture file.
        found: usize,
    },
    /// Fixture content carries a runtime-secret or host-path marker.
    #[error("fixture {path} contains forbidden marker {marker}")]
    ForbiddenMarker {
        /// Fixture file path.
        path: String,
        /// Forbidden marker found.
        marker: String,
    },
    /// Required coverage tags have no fixture case.
    #[error("fixture corpus is missing required coverage tags: {missing:?}")]
    MissingCoverage {
        /// Coverage tags with no case.
        missing: Vec<String>,
    },
    /// A fixture case contradicts a documented wire or mapping rule.
    #[error("fixture case {case} violates its documented rule: {reason}")]
    Invariant {
        /// Fixture case id.
        case: String,
        /// Rule that the case violates.
        reason: String,
    },
}

/// Manifest entry describing one fixture file.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct ManifestEntry {
    /// Fixture file name, relative to the fixture directory.
    pub path: String,
    /// Fixture family name.
    pub family: String,
    /// SHA-256 of the fixture file bytes.
    pub sha256: String,
    /// Number of cases the fixture file must contain.
    pub case_count: usize,
    /// Coverage tags contributed by the file's cases.
    pub covers: Vec<String>,
}

/// Fixture corpus manifest with schema identity and per-file goldens.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct Manifest {
    /// Manifest schema name.
    pub schema: String,
    /// Manifest schema version.
    pub version: u64,
    /// Coverage tags the corpus must provide.
    pub required_coverage: Vec<String>,
    /// SHA-256 of the newline-joined forbidden-marker contract.
    pub forbidden_markers_sha256: String,
    /// Fixture file entries with golden hashes.
    pub files: Vec<ManifestEntry>,
}

/// One golden fixture case: deterministic input plus expected outcome.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct Case {
    /// Stable case id.
    pub id: String,
    /// Coverage tags this case contributes.
    pub covers: Vec<String>,
    /// Case input document.
    pub input: serde_json::Value,
    /// Case expected-outcome document.
    pub expected: serde_json::Value,
}

/// One fixture family file.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct FamilyFile {
    /// Family name.
    pub family: String,
    /// Cases in generation order.
    pub cases: Vec<Case>,
}

/// Fully verified fixture corpus.
#[derive(Debug)]
pub(crate) struct Corpus {
    /// Parsed manifest.
    pub manifest: Manifest,
    /// Verified family files keyed by fixture file name.
    pub families: BTreeMap<String, FamilyFile>,
}

/// Expected manifest schema name.
pub(crate) const SCHEMA: &str = "iptv-poc-golden";
/// Expected manifest schema version.
pub(crate) const SCHEMA_VERSION: u64 = 1;

/// Parse and validate a fixture manifest document.
///
/// # Errors
/// Returns [`GoldenError::MalformedJson`], [`GoldenError::UnsupportedSchema`],
/// or [`GoldenError::UnsupportedVersion`] when the document is not a
/// supported manifest.
pub(crate) fn parse_manifest(bytes: &[u8]) -> Result<Manifest, GoldenError> {
    let manifest: Manifest =
        serde_json::from_slice(bytes).map_err(|error| GoldenError::MalformedJson {
            path: "manifest.json".to_string(),
            message: error.to_string(),
        })?;
    if manifest.schema != SCHEMA {
        return Err(GoldenError::UnsupportedSchema {
            found: manifest.schema,
        });
    }
    if manifest.version != SCHEMA_VERSION {
        return Err(GoldenError::UnsupportedVersion {
            found: manifest.version,
        });
    }
    Ok(manifest)
}

/// Parse one fixture family file and confirm its declared family.
///
/// # Errors
/// Returns [`GoldenError::MalformedJson`] or [`GoldenError::FamilyMismatch`]
/// when the bytes are not a fixture file of the expected family.
pub(crate) fn parse_family(
    path: &str,
    bytes: &[u8],
    expected_family: &str,
) -> Result<FamilyFile, GoldenError> {
    let family: FamilyFile =
        serde_json::from_slice(bytes).map_err(|error| GoldenError::MalformedJson {
            path: path.to_string(),
            message: error.to_string(),
        })?;
    if family.family != expected_family {
        return Err(GoldenError::FamilyMismatch {
            path: path.to_string(),
            expected: expected_family.to_string(),
            found: family.family,
        });
    }
    Ok(family)
}

/// Return the lowercase SHA-256 hex digest of ``bytes``.
#[must_use]
pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let digest = ring::digest::digest(&ring::digest::SHA256, bytes);
    let mut rendered = String::with_capacity(64);
    for byte in digest.as_ref() {
        rendered.push(char::from(HEX[usize::from(byte >> 4)]));
        rendered.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    rendered
}

/// Verify one fixture file against its manifest-declared hash.
///
/// # Errors
/// Returns [`GoldenError::HashMismatch`] when the bytes drifted from the
/// manifest golden.
pub(crate) fn verify_hash(
    path: &str,
    expected_sha256: &str,
    bytes: &[u8],
) -> Result<(), GoldenError> {
    let actual = sha256_hex(bytes);
    if actual != expected_sha256 {
        return Err(GoldenError::HashMismatch {
            path: path.to_string(),
            expected: expected_sha256.to_string(),
            actual,
        });
    }
    Ok(())
}

/// Refuse fixture content carrying runtime-secret or host-path markers.
///
/// # Errors
/// Returns [`GoldenError::ForbiddenMarker`] when a marker appears.
pub(crate) fn scan_forbidden(path: &str, bytes: &[u8]) -> Result<(), GoldenError> {
    let text = String::from_utf8_lossy(bytes);
    for marker in crate::checks::forbidden_markers() {
        if text.contains(marker) {
            return Err(GoldenError::ForbiddenMarker {
                path: path.to_string(),
                marker: (*marker).to_string(),
            });
        }
    }
    Ok(())
}

/// Verify every manifest entry against the fixture directory contents.
///
/// # Errors
/// Returns the first typed error for missing, drifted, malformed, or
/// miscounted fixture files.
pub(crate) fn verify_fixture_dir(
    directory: &Path,
    manifest: &Manifest,
) -> Result<BTreeMap<String, FamilyFile>, GoldenError> {
    let mut families = BTreeMap::new();
    for entry in &manifest.files {
        let path = directory.join(&entry.path);
        let bytes = fs::read(&path).map_err(|error| GoldenError::Io {
            path: entry.path.clone(),
            message: error.to_string(),
        })?;
        verify_hash(&entry.path, &entry.sha256, &bytes)?;
        scan_forbidden(&entry.path, &bytes)?;
        let family = parse_family(&entry.path, &bytes, &entry.family)?;
        if family.cases.len() != entry.case_count {
            return Err(GoldenError::CaseCountMismatch {
                path: entry.path.clone(),
                expected: entry.case_count,
                found: family.cases.len(),
            });
        }
        families.insert(entry.path.clone(), family);
    }
    Ok(families)
}

/// Load and fully verify the checked-in fixture corpus.
///
/// # Errors
/// Returns a [`GoldenError`] when the manifest or any fixture file is
/// malformed, drifted, or missing required coverage.
pub(crate) fn load_corpus(directory: &Path) -> Result<Corpus, GoldenError> {
    let manifest_bytes =
        fs::read(directory.join("manifest.json")).map_err(|error| GoldenError::Io {
            path: "manifest.json".to_string(),
            message: error.to_string(),
        })?;
    scan_forbidden("manifest.json", &manifest_bytes)?;
    let manifest = parse_manifest(&manifest_bytes)?;
    let families = verify_fixture_dir(directory, &manifest)?;
    let covered: std::collections::BTreeSet<&str> = families
        .values()
        .flat_map(|family| family.cases.iter())
        .flat_map(|case| case.covers.iter())
        .map(String::as_str)
        .collect();
    let missing: Vec<String> = manifest
        .required_coverage
        .iter()
        .filter(|tag| !covered.contains(tag.as_str()))
        .cloned()
        .collect();
    if !missing.is_empty() {
        return Err(GoldenError::MissingCoverage { missing });
    }
    Ok(Corpus { manifest, families })
}
