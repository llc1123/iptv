use super::models::{Channel, ChannelListResponse};
use serde_json::Error as JsonError;
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChannelGroupSource {
    pub title: String,
    pub response: Result<ChannelListResponse, ChannelParserError>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChannelParserError {
    pub operation: &'static str,
    pub kind: ChannelParserErrorKind,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChannelParserErrorKind {
    InvalidJson,
    ProviderStatus,
}

impl std::fmt::Display for ChannelParserError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let detail = match self.kind {
            ChannelParserErrorKind::InvalidJson => "invalid channel response",
            ChannelParserErrorKind::ProviderStatus => "provider rejected channel category",
        };
        write!(f, "{}: {detail}", self.operation)
    }
}
impl std::error::Error for ChannelParserError {}

fn invalid(operation: &'static str, _: JsonError) -> ChannelParserError {
    ChannelParserError {
        operation,
        kind: ChannelParserErrorKind::InvalidJson,
    }
}

pub fn parse_channel_list(body: &str) -> Result<ChannelListResponse, ChannelParserError> {
    let response: ChannelListResponse =
        serde_json::from_str(body).map_err(|error| invalid("epg-channel-codes", error))?;
    if response.status != "1" {
        return Err(ChannelParserError {
            operation: "epg-channel-codes",
            kind: ChannelParserErrorKind::ProviderStatus,
        });
    }
    Ok(response)
}

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

#[must_use]
pub fn non_empty_codes(channels: &[Channel]) -> BTreeMap<String, String> {
    channels
        .iter()
        .filter(|channel| !channel.code.is_empty())
        .map(|channel| (channel.epg_id.clone(), channel.code.clone()))
        .collect()
}
