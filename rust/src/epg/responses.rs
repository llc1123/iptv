//! JSON envelopes of the provider's AJAX endpoints.

use super::EpgError;
use super::models::{
    ChannelListResponse, Programme, ProgrammeResponse, TvodAddress, TvodAddressResponse,
};

/// The provider's "no programmes in this window" code.
const NO_PROGRAMMES: &str = "051002";

fn rejected(status: &str, message: &str) -> EpgError {
    EpgError::Provider(if message.is_empty() {
        format!("status={status}")
    } else {
        message.to_owned()
    })
}

/// Parses a `getChannelList` response.
pub fn parse_channel_list(body: &str) -> Result<ChannelListResponse, EpgError> {
    let response: ChannelListResponse = serde_json::from_str(body)
        .map_err(|error| EpgError::Parse(format!("channel list: {error}")))?;
    if response.status != "1" {
        return Err(rejected(&response.status, &response.error_message));
    }
    Ok(response)
}

/// Parses a `getChannelProg` response.
pub fn parse_programmes(body: &str) -> Result<Vec<Programme>, EpgError> {
    let response: ProgrammeResponse = serde_json::from_str(body)
        .map_err(|error| EpgError::Parse(format!("programmes: {error}")))?;
    if response.error_code == NO_PROGRAMMES {
        return Ok(Vec::new());
    }
    if response.status != "1" {
        return Err(rejected(&response.status, &response.error_message));
    }
    Ok(response.data)
}

/// Parses a `getTvodPlayUrl` response.
pub fn parse_tvod(body: &str) -> Result<TvodAddress, EpgError> {
    let response: TvodAddressResponse =
        serde_json::from_str(body).map_err(|error| EpgError::Parse(format!("tvod: {error}")))?;
    if response.status != "1" {
        return Err(rejected(&response.status, &response.error_message));
    }
    response
        .data
        .ok_or_else(|| EpgError::Parse("tvod: missing data".to_owned()))
}
