use super::models::{Programme, ProgrammeResponse, TvodAddress};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProgrammeParserError {
    pub operation: &'static str,
    pub kind: ProgrammeParserErrorKind,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProgrammeParserErrorKind {
    InvalidJson,
    ProviderStatus,
}

impl std::fmt::Display for ProgrammeParserError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let detail = match self.kind {
            ProgrammeParserErrorKind::InvalidJson => "invalid programme response",
            ProgrammeParserErrorKind::ProviderStatus => "provider rejected programme request",
        };
        write!(f, "{}: {detail}", self.operation)
    }
}
impl std::error::Error for ProgrammeParserError {}

pub fn parse_programmes(body: &str) -> Result<Vec<Programme>, ProgrammeParserError> {
    let response: ProgrammeResponse =
        serde_json::from_str(body).map_err(|_| ProgrammeParserError {
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

pub fn tvod_times_seconds(programme: &Programme) -> Result<(i64, i64), ProgrammeParserError> {
    programme
        .start_time_ms
        .checked_div(1_000)
        .zip(programme.end_time_ms.checked_div(1_000))
        .ok_or(ProgrammeParserError {
            operation: "epg-tvod",
            kind: ProgrammeParserErrorKind::InvalidJson,
        })
}

pub fn parse_tvod(body: &str) -> Result<TvodAddress, ProgrammeParserError> {
    let value: serde_json::Value =
        serde_json::from_str(body).map_err(|_| ProgrammeParserError {
            operation: "epg-tvod",
            kind: ProgrammeParserErrorKind::InvalidJson,
        })?;
    if value.get("status").and_then(serde_json::Value::as_str) != Some("1") {
        return Err(ProgrammeParserError {
            operation: "epg-tvod",
            kind: ProgrammeParserErrorKind::ProviderStatus,
        });
    }
    serde_json::from_value(value.get("data").cloned().ok_or(ProgrammeParserError {
        operation: "epg-tvod",
        kind: ProgrammeParserErrorKind::InvalidJson,
    })?)
    .map_err(|_| ProgrammeParserError {
        operation: "epg-tvod",
        kind: ProgrammeParserErrorKind::InvalidJson,
    })
}
