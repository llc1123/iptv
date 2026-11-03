//! Programme and TVOD JSON response parsing.

use std::fmt;

use super::models::{Programme, ProgrammeResponse, TvodAddress};

/// Typed programme parser error.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProgrammeParserError {
    /// Stable operation name.
    pub operation: &'static str,
    /// Stable failure class.
    pub kind: ProgrammeParserErrorKind,
}

/// Programme parser failure classes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProgrammeParserErrorKind {
    /// JSON was malformed or did not match the provider model.
    InvalidJson,
    /// The provider returned a non-success status.
    ProviderStatus,
}

impl fmt::Display for ProgrammeParserError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let detail = match self.kind {
            ProgrammeParserErrorKind::InvalidJson => "invalid programme response",
            ProgrammeParserErrorKind::ProviderStatus => "provider rejected programme request",
        };
        write!(formatter, "{}: {detail}", self.operation)
    }
}

impl std::error::Error for ProgrammeParserError {}

/// Parses a programme response, including the provider's empty `051002` case.
///
/// # Errors
/// Returns a typed error for malformed JSON/model data or a non-success status.
pub fn parse_programmes(body: &str) -> Result<Vec<Programme>, ProgrammeParserError> {
    let response =
        serde_json::from_str::<ProgrammeResponse>(body).map_err(|_| ProgrammeParserError {
            operation: "epg-programmes",
            kind: ProgrammeParserErrorKind::InvalidJson,
        })?;
    if response.error_code == "051002" {
        return Ok(Vec::new());
    }
    if response.status != "1" {
        return Err(ProgrammeParserError {
            operation: "epg-programmes",
            kind: ProgrammeParserErrorKind::ProviderStatus,
        });
    }
    Ok(response.data)
}

/// Converts programme millisecond timestamps to the provider's TVOD seconds.
///
/// # Errors
/// Returns [`ProgrammeParserErrorKind::InvalidJson`] when a timestamp cannot be
/// represented as whole seconds without overflow.
pub fn tvod_times_seconds(programme: &Programme) -> Result<(i64, i64), ProgrammeParserError> {
    let start = programme
        .start_time_ms
        .checked_div(1_000)
        .ok_or(ProgrammeParserError {
            operation: "epg-tvod",
            kind: ProgrammeParserErrorKind::InvalidJson,
        })?;
    let end = programme
        .end_time_ms
        .checked_div(1_000)
        .ok_or(ProgrammeParserError {
            operation: "epg-tvod",
            kind: ProgrammeParserErrorKind::InvalidJson,
        })?;
    Ok((start, end))
}

/// Parses a provider TVOD response and returns its playback address.
///
/// # Errors
/// Returns [`ProgrammeParserErrorKind::InvalidJson`] for malformed response data or
/// [`ProgrammeParserErrorKind::ProviderStatus`] when the provider did not succeed.
pub fn parse_tvod(body: &str) -> Result<TvodAddress, ProgrammeParserError> {
    let response =
        serde_json::from_str::<serde_json::Value>(body).map_err(|_| ProgrammeParserError {
            operation: "epg-tvod",
            kind: ProgrammeParserErrorKind::InvalidJson,
        })?;
    if response.get("status").and_then(serde_json::Value::as_str) != Some("1") {
        return Err(ProgrammeParserError {
            operation: "epg-tvod",
            kind: ProgrammeParserErrorKind::ProviderStatus,
        });
    }
    serde_json::from_value(response.get("data").cloned().ok_or(ProgrammeParserError {
        operation: "epg-tvod",
        kind: ProgrammeParserErrorKind::InvalidJson,
    })?)
    .map_err(|_| ProgrammeParserError {
        operation: "epg-tvod",
        kind: ProgrammeParserErrorKind::InvalidJson,
    })
}

#[cfg(test)]
#[expect(
    clippy::expect_used,
    reason = "parser tests use fixed valid fixture inputs"
)]
mod tests {
    use super::{ProgrammeParserErrorKind, parse_programmes, parse_tvod, tvod_times_seconds};

    #[test]
    fn empty_programme_code_returns_empty_without_status_failure() {
        let programmes = parse_programmes(r#"{"status":"0","errCode":"051002","data":[]}"#)
            .expect("051002 is the provider empty result");
        assert!(programmes.is_empty());
    }

    #[test]
    fn programme_parser_preserves_millisecond_values_and_tvod_seconds() {
        let programmes = parse_programmes(
            r#"{"status":"1","data":[{"ID":"p1","channelID":"c1","name":"News","startTime":"1699971200123","endTime":1699974800999}],"optional":true}"#,
        )
        .expect("fixture programme response is valid");
        assert_eq!(
            tvod_times_seconds(&programmes[0]).expect("whole seconds"),
            (1_699_971_200, 1_699_974_800)
        );
    }

    #[test]
    fn programme_parser_rejects_invalid_json_and_status() {
        assert_eq!(
            parse_programmes("not json")
                .expect_err("malformed JSON")
                .kind,
            ProgrammeParserErrorKind::InvalidJson
        );
        assert_eq!(
            parse_programmes(
                r#"{"status":"2","errCode":"1002","errMsg":"window rejected","data":[]}"#,
            )
            .expect_err("provider failure")
            .kind,
            ProgrammeParserErrorKind::ProviderStatus
        );
        assert_eq!(
            parse_tvod("not json").expect_err("malformed TVOD").kind,
            ProgrammeParserErrorKind::InvalidJson
        );
        assert_eq!(
            parse_tvod(r#"{"status":"0","data":{}}"#)
                .expect_err("provider TVOD failure")
                .kind,
            ProgrammeParserErrorKind::ProviderStatus
        );
    }
}
