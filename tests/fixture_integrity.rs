//! Integrity of the frozen golden fixtures.
//!
//! The fixtures were generated from the Python reference implementation before
//! it was removed, and are now immutable: every expected value is a record of
//! what Python produced. This test replaces the generator's `--check` drift
//! detection, so an accidental edit to a fixture fails the gate instead of
//! silently changing what "parity" means.

#![doc = "Integrity of the frozen golden fixtures."]
#![expect(
    clippy::expect_used,
    reason = "a malformed or missing checked-in fixture should fail the test"
)]

use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{collections::BTreeSet, fmt::Write as _, fs, path::Path};

const SCHEMA: &str = "iptv-poc-golden";
const VERSION: u64 = 1;
const FIXTURES: &str = "tests/fixtures";

fn manifest() -> Value {
    let path = Path::new(FIXTURES).join("manifest.json");
    let text = fs::read_to_string(&path).expect("manifest is readable");
    serde_json::from_str(&text).expect("manifest is valid JSON")
}

fn sha256(path: &Path) -> String {
    let bytes = fs::read(path).expect("fixture is readable");
    hex_upper(&Sha256::digest(&bytes))
}

fn hex_upper(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let () = write!(out, "{byte:02x}").expect("writing to a String cannot fail");
    }
    out
}

#[test]
fn fixtures_match_the_manifest_hashes() {
    let manifest = manifest();
    let files = manifest["files"].as_array().expect("files is an array");
    assert!(!files.is_empty(), "the manifest lists no fixtures");

    for entry in files {
        let path = entry["path"].as_str().expect("path is a string");
        let expected = entry["sha256"].as_str().expect("sha256 is a string");
        let full = Path::new(FIXTURES).join(path);
        assert!(
            full.is_file(),
            "{path} is listed in the manifest but missing; the fixtures are frozen, so restore it"
        );
        assert_eq!(
            sha256(&full),
            expected,
            "{path} no longer matches its recorded hash; the fixtures are frozen and \
             must not be edited by hand"
        );
    }
}

#[test]
fn no_unlisted_fixture_is_present() {
    let manifest = manifest();
    let listed: BTreeSet<&str> = manifest["files"]
        .as_array()
        .expect("files is an array")
        .iter()
        .map(|entry| entry["path"].as_str().expect("path is a string"))
        .collect();
    let mut unlisted = Vec::new();
    for entry in fs::read_dir(FIXTURES).expect("fixtures directory is readable") {
        let path = entry.expect("directory entry is readable").path();
        if path.extension().is_some_and(|ext| ext == "json")
            && path.file_name() != Some("manifest.json".as_ref())
        {
            let name = path.file_name().expect("has a name").to_string_lossy();
            if !listed.contains(name.as_ref()) {
                unlisted.push(name.into_owned());
            }
        }
    }
    assert!(
        unlisted.is_empty(),
        "fixtures missing from the manifest: {unlisted:?}"
    );
}

#[test]
fn coverage_required_by_the_manifest_is_present() {
    let manifest = manifest();
    let required: BTreeSet<&str> = manifest["required_coverage"]
        .as_array()
        .expect("required_coverage is an array")
        .iter()
        .map(|case| case.as_str().expect("a case name"))
        .collect();
    let covered: BTreeSet<&str> = manifest["files"]
        .as_array()
        .expect("files is an array")
        .iter()
        .flat_map(|entry| entry["covers"].as_array().expect("covers is an array"))
        .map(|case| case.as_str().expect("a case name"))
        .collect();
    let missing: Vec<_> = required.difference(&covered).collect();
    assert!(missing.is_empty(), "fixtures no longer cover: {missing:?}");
}

#[test]
fn manifest_schema_is_the_frozen_one() {
    let manifest = manifest();
    assert_eq!(manifest["schema"].as_str(), Some(SCHEMA));
    assert_eq!(manifest["version"].as_u64(), Some(VERSION));
    // The recorded hash of the E2E credentials marker proves no fixture ever
    // contained a live value from var/iptv-e2e.env.
    assert!(
        manifest["forbidden_markers_sha256"].is_string(),
        "the manifest must keep the forbidden-markers hash"
    );
}
