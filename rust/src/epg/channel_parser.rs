//! JSON channel and category response parsing.

use std::collections::BTreeMap;
use std::fmt;

use serde_json::Error as JsonError;

use super::models::{Channel, ChannelListResponse};

/// One ordered category response used to derive first-wins channel groups.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChannelGroupSource {
    /// Display title assigned to this category.
    pub title: String,
    /// Parsed category response.
    pub response: Result<ChannelListResponse, ChannelParserError>,
}

/// Typed channel parser error.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChannelParserError {
    /// Stable operation name.
    pub operation: &'static str,
    /// Stable failure class.
    pub kind: ChannelParserErrorKind,
}

/// Channel parser failure classes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChannelParserErrorKind {
    /// JSON was malformed or did not match the provider model.
    InvalidJson,
    /// The provider returned a non-success status.
    ProviderStatus,
}

impl fmt::Display for ChannelParserError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let detail = match self.kind {
            ChannelParserErrorKind::InvalidJson => "invalid channel response",
            ChannelParserErrorKind::ProviderStatus => "provider rejected channel category",
        };
        write!(formatter, "{}: {detail}", self.operation)
    }
}

impl std::error::Error for ChannelParserError {}

fn invalid_json(operation: &'static str, _error: JsonError) -> ChannelParserError {
    ChannelParserError {
        operation,
        kind: ChannelParserErrorKind::InvalidJson,
    }
}

/// Parses and validates one channel-list JSON response.
///
/// # Errors
/// Returns a typed invalid-json or provider-status error. Unknown JSON fields
/// are ignored, matching the Pydantic model configuration.
pub fn parse_channel_list(body: &str) -> Result<ChannelListResponse, ChannelParserError> {
    let response = serde_json::from_str::<ChannelListResponse>(body)
        .map_err(|error| invalid_json("epg-channel-codes", error))?;
    if response.status != "1" {
        return Err(ChannelParserError {
            operation: "epg-channel-codes",
            kind: ChannelParserErrorKind::ProviderStatus,
        });
    }
    Ok(response)
}

/// Assigns each channel to the first successful category containing it.
#[must_use]
pub fn merge_category_groups(sources: &[ChannelGroupSource]) -> BTreeMap<String, String> {
    let mut groups = BTreeMap::new();
    for source in sources {
        if let Ok(response) = &source.response {
            for channel in &response.data {
                groups
                    .entry(channel.epg_id.clone())
                    .or_insert_with(|| source.title.clone());
            }
        }
    }
    groups
}

/// Returns the channels whose provider code is non-empty.
#[must_use]
pub fn non_empty_codes(channels: &[Channel]) -> BTreeMap<String, String> {
    channels
        .iter()
        .filter(|channel| !channel.code.is_empty())
        .map(|channel| (channel.epg_id.clone(), channel.code.clone()))
        .collect()
}

#[cfg(test)]
#[expect(
    clippy::expect_used,
    reason = "parser tests use fixed valid fixture inputs"
)]
mod tests {
    use super::{
        ChannelGroupSource, ChannelParserErrorKind, merge_category_groups, parse_channel_list,
    };

    #[test]
    fn channel_json_ignores_optional_unknown_fields_and_accepts_success() {
        let response = parse_channel_list(
            r#"{"status":"1","data":[{"ID":"epg-1","code":"cc1","name":"News","mixNo":"7","ignored":true}]}"#,
        )
        .expect("fixture channel response is valid");
        assert_eq!(response.data[0].code, "cc1");
    }

    #[test]
    fn channel_json_rejects_malformed_and_non_success_status() {
        assert_eq!(
            parse_channel_list("not json")
                .expect_err("malformed JSON")
                .kind,
            ChannelParserErrorKind::InvalidJson
        );
        assert_eq!(
            parse_channel_list(r#"{"status":"0","data":[],"errMsg":"boom"}"#)
                .expect_err("provider failure")
                .kind,
            ChannelParserErrorKind::ProviderStatus
        );
    }

    #[test]
    fn category_groups_are_ordered_and_first_wins_while_failures_are_skipped() {
        let first = parse_channel_list(
            r#"{"status":"1","data":[{"ID":"epg-1","code":"a","name":"A","mixNo":"1"}]}"#,
        );
        let second = parse_channel_list(
            r#"{"status":"1","data":[{"ID":"epg-1","code":"b","name":"A","mixNo":"1"},{"ID":"epg-2","code":"c","name":"B","mixNo":"2"}]}"#,
        );
        let groups = merge_category_groups(&[
            ChannelGroupSource {
                title: "first".into(),
                response: first,
            },
            ChannelGroupSource {
                title: "failed".into(),
                response: Err(super::ChannelParserError {
                    operation: "epg-channel-codes",
                    kind: ChannelParserErrorKind::ProviderStatus,
                }),
            },
            ChannelGroupSource {
                title: "second".into(),
                response: second,
            },
        ]);
        assert_eq!(groups.get("epg-1"), Some(&"first".to_owned()));
        assert_eq!(groups.get("epg-2"), Some(&"second".to_owned()));
    }
}
