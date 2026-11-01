//! Fixture-content invariant checks for the golden corpus.
//!
//! These checks validate that each fixture case is internally consistent with
//! the documented wire layout, padding, mapping, and token rules it claims to
//! freeze. They guard the fixtures against generator corruption; they are not
//! a second implementation of the POC.

mod flow;
mod wire;

pub(crate) use flow::{catchup_numeric_rule, hls_rewrite_tokens, status_mapping};
pub(crate) use wire::{
    epg_auth_shape, option60_encode_layout, option60_reject_violation, padding_rule,
};

use serde_json::Value;

use crate::loader::{Case, GoldenError};

/// Coverage tags the corpus must provide, matching the generator contract.
pub(crate) const REQUIRED_COVERAGE: &[&str] = &[
    "aligned-padding",
    "non-aligned-padding",
    "malformed-lengths",
    "duplicate-attributes",
    "html-entities",
    "crlf-playlists",
    "cr-playlists",
    "bom-playlists",
    "relative-urls",
    "protocol-relative-urls",
    "query-urls",
    "xml-escaping",
    "numeric-catchup",
    "iso-catchup",
    "failure-status-shapes",
];

/// Markers that must never appear in fixture content.
pub(crate) const FORBIDDEN_MARKERS: &[&str] = &[
    "iptv-e2e",
    "/root/",
    "var/",
    "IPTV_PASSWORD",
    "IPTV_DHCP_PASSWORD",
    "IPTV_USER_ID",
    "IPTV_DHCP_USER",
];

/// Return the forbidden-marker list shared with the generator.
#[must_use]
pub(crate) fn forbidden_markers() -> &'static [&'static str] {
    FORBIDDEN_MARKERS
}

fn invariant(case: &str, reason: &str) -> GoldenError {
    GoldenError::Invariant {
        case: case.to_string(),
        reason: reason.to_string(),
    }
}

fn field_str(value: &Value, key: &str, case: &str) -> Result<String, GoldenError> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| invariant(case, &format!("missing string field {key}")))
}

fn field_u64(value: &Value, key: &str, case: &str) -> Result<u64, GoldenError> {
    value
        .get(key)
        .and_then(Value::as_u64)
        .ok_or_else(|| invariant(case, &format!("missing integer field {key}")))
}

fn field_hex(value: &Value, key: &str, case: &str) -> Result<Vec<u8>, GoldenError> {
    let text = field_str(value, key, case)?;
    decode_hex(&text).ok_or_else(|| invariant(case, &format!("field {key} is not hex")))
}

fn decode_hex(text: &str) -> Option<Vec<u8>> {
    if text.len() % 2 != 0 {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&text[index..index + 2], 16).ok())
        .collect()
}

fn error_kind(expected: &Value) -> Option<&str> {
    expected.get("error")?.get("kind")?.as_str()
}

fn is_error_case(case: &Case) -> bool {
    case.expected.get("error").is_some()
}
