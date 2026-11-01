//! Mapping invariants for catch-up time, failure status shapes, and HLS
//! rewrite fixture cases.

use serde_json::Value;

use super::{error_kind, field_str, field_u64, invariant};
use crate::loader::{Case, GoldenError};

/// Check numeric catch-up cases against the documented threshold rule.
///
/// # Errors
/// Returns [`GoldenError::Invariant`] when a numeric case breaks the rule.
pub(crate) fn catchup_numeric_rule(case: &Case) -> Result<(), GoldenError> {
    let utc = field_str(&case.input, "utc", &case.id)?;
    if utc.is_empty() || !utc.bytes().all(|byte| byte.is_ascii_digit()) {
        return Ok(());
    }
    let reason = |text: &str| invariant(&case.id, text);
    let Ok(timestamp) = utc.parse::<i64>() else {
        return Err(reason("numeric catch-up value does not parse"));
    };
    let expected = if timestamp > 10_000_000_000 {
        timestamp
    } else {
        timestamp * 1000
    };
    let pinned = field_u64(&case.expected, "start_time_ms", &case.id)?;
    let expected_u64 =
        u64::try_from(expected).map_err(|_| reason("numeric catch-up value is negative"))?;
    if pinned != expected_u64 {
        return Err(reason("numeric catch-up value breaks the threshold rule"));
    }
    Ok(())
}

/// Check failure-status cases against the documented status/body mapping.
///
/// # Errors
/// Returns [`GoldenError::Invariant`] when the case breaks the mapping.
pub(crate) fn status_mapping(case: &Case) -> Result<(), GoldenError> {
    let kind = field_str(&case.input, "kind", &case.id)?;
    let reason = |text: &str| invariant(&case.id, text);
    match kind.as_str() {
        "http-error" => {
            let operation = field_str(&case.input, "operation", &case.id)?;
            let detail = field_str(&case.input, "detail", &case.id)?;
            let want_status = if operation == "hls-proxy" { 502 } else { 503 };
            if field_u64(&case.expected, "status", &case.id)? != want_status {
                return Err(reason(
                    "http error status does not match the operation mapping",
                ));
            }
            let body = case
                .expected
                .get("body")
                .and_then(|body| body.get("error"))
                .and_then(Value::as_str)
                .ok_or_else(|| reason("http error body is not an error string"))?;
            if body != format!("{operation}: {detail}") {
                return Err(reason("http error body does not match the string format"));
            }
        }
        "provider-status" => {
            let operation = field_str(&case.input, "operation", &case.id)?;
            if let Some(error) = case.expected.get("error") {
                if error_kind(&case.expected) != Some("provider-status-failure") {
                    return Err(reason("provider failure claims the wrong error kind"));
                }
                if error.get("operation").and_then(Value::as_str) != Some(operation.as_str()) {
                    return Err(reason("provider failure claims the wrong operation"));
                }
            } else if case.expected.get("result").and_then(Value::as_str) != Some("ok") {
                return Err(reason("provider success is not result=ok"));
            }
        }
        other => return Err(reason(&format!("unknown status case kind {other}"))),
    }
    Ok(())
}

/// Check HLS rewrite cases only reference registered resource tokens.
///
/// # Errors
/// Returns [`GoldenError::Invariant`] when output and resource map disagree.
pub(crate) fn hls_rewrite_tokens(case: &Case) -> Result<(), GoldenError> {
    let kind = field_str(&case.input, "kind", &case.id)?;
    let reason = |text: &str| invariant(&case.id, text);
    match kind.as_str() {
        "rewrite" => {
            let output = field_str(&case.expected, "output", &case.id)?;
            let resources = case
                .expected
                .get("resources")
                .and_then(Value::as_array)
                .ok_or_else(|| reason("rewrite case has no resource map"))?;
            if output.matches("/_hls/").count() != resources.len() {
                return Err(reason("rewritten output does not match the resource map"));
            }
            for resource in resources {
                let token = resource
                    .get("token")
                    .and_then(Value::as_str)
                    .ok_or_else(|| reason("resource has no token"))?;
                if !token.starts_with("fixturetok") || !output.contains(&format!("/_hls/{token}")) {
                    return Err(reason("resource token is not a deterministic output token"));
                }
                if resource
                    .get("url")
                    .and_then(Value::as_str)
                    .is_none_or(str::is_empty)
                {
                    return Err(reason("resource has no upstream url"));
                }
            }
        }
        "resource" => {
            if let Some(error) = error_kind(&case.expected) {
                if error != "hls-resource-expired" {
                    return Err(reason("resource failure claims the wrong error kind"));
                }
            } else {
                let token = field_str(&case.expected, "token", &case.id)?;
                if !token.starts_with("fixturetok") {
                    return Err(reason("resource token is not a deterministic token"));
                }
                if field_str(&case.expected, "url", &case.id)?.is_empty() {
                    return Err(reason("resource case has no upstream url"));
                }
            }
        }
        other => return Err(reason(&format!("unknown hls case kind {other}"))),
    }
    Ok(())
}
