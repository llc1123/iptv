//! Provider response models with the Python client's aliases and coercions.

use std::fmt;

use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer, Serialize};

/// Provider channel identifier from the EPG response.
pub type EpgChannelId = String;
/// Local channel code used by the provider's programme endpoint.
pub type ChannelId = String;

fn deserialize_string<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    String::deserialize(deserializer)
}

fn deserialize_number_or_string<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    struct NumberOrString;

    impl Visitor<'_> for NumberOrString {
        type Value = String;

        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("a string or JSON number")
        }

        fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            Ok(value.to_owned())
        }

        fn visit_string<E>(self, value: String) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            Ok(value)
        }

        fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            Ok(value.to_string())
        }

        fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            Ok(value.to_string())
        }

        fn visit_f64<E>(self, value: f64) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            Ok(value.to_string())
        }
    }

    deserializer.deserialize_any(NumberOrString)
}

fn deserialize_i64_from_number_or_string<'de, D>(deserializer: D) -> Result<i64, D::Error>
where
    D: Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    match value {
        serde_json::Value::Number(number) => number
            .as_i64()
            .ok_or_else(|| de::Error::custom("timestamp is not an integer")),
        serde_json::Value::String(text) => text
            .parse::<i64>()
            .map_err(|_| de::Error::custom("timestamp is not an integer")),
        _ => Err(de::Error::custom("timestamp must be a string or integer")),
    }
}

/// A channel returned by the provider's JSON endpoint.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Channel {
    /// Optional provider authorization code.
    #[serde(default, rename = "authCode", alias = "auth_code")]
    pub auth_code: String,
    /// Local channel code.
    #[serde(deserialize_with = "deserialize_string")]
    pub code: ChannelId,
    /// Display name.
    pub name: String,
    /// Charge marker.
    #[serde(default, rename = "isCharge", alias = "is_charge")]
    pub is_charge: String,
    /// EPG channel identifier.
    #[serde(rename = "ID", alias = "epg_id")]
    pub epg_id: EpgChannelId,
    /// User channel number.
    #[serde(
        rename = "mixNo",
        alias = "mix_no",
        deserialize_with = "deserialize_string"
    )]
    pub mix_no: String,
    /// Optional media identifier.
    #[serde(default, rename = "mediaID", alias = "media_id")]
    pub media_id: String,
}

/// A channel record from the boot response.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct BootChannel {
    /// EPG identifier.
    #[serde(rename = "ChannelID", alias = "channel_id")]
    pub channel_id: EpgChannelId,
    /// Display name.
    #[serde(rename = "ChannelName", alias = "name")]
    pub name: String,
    /// User channel number.
    #[serde(rename = "UserChannelID", alias = "mix_no")]
    pub mix_no: String,
    /// Live channel URL, when supplied.
    #[serde(default, rename = "ChannelURL", alias = "channel_url")]
    pub channel_url: String,
    /// Time-shift URL.
    #[serde(rename = "TimeShiftURL", alias = "time_shift_url")]
    pub time_shift_url: String,
    /// SDP payload, when supplied.
    #[serde(default, rename = "ChannelSDP", alias = "channel_sdp")]
    pub channel_sdp: String,
    /// Time-shift enable marker.
    #[serde(default, rename = "TimeShift", alias = "time_shift")]
    pub time_shift: String,
    /// Time-shift duration, when supplied.
    #[serde(default, rename = "TimeShiftLength", alias = "time_shift_length")]
    pub time_shift_length: String,
    /// Logo URL, when supplied.
    #[serde(default, rename = "ChannelLogURL", alias = "logo_url")]
    pub logo_url: String,
    /// Local channel code added after boot parsing.
    #[serde(default)]
    pub code: ChannelId,
    /// First matching provider category title.
    #[serde(default)]
    pub group_title: String,
}

/// A programme returned by the provider.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct Programme {
    /// Programme title.
    #[serde(rename = "name", alias = "title")]
    pub title: String,
    /// Start timestamp in milliseconds since Unix epoch.
    #[serde(
        rename = "startTime",
        alias = "start_time_ms",
        deserialize_with = "deserialize_i64_from_number_or_string"
    )]
    pub start_time_ms: i64,
    /// End timestamp in milliseconds since Unix epoch.
    #[serde(
        rename = "endTime",
        alias = "end_time_ms",
        deserialize_with = "deserialize_i64_from_number_or_string"
    )]
    pub end_time_ms: i64,
    /// Optional provider programme identifier.
    #[serde(
        default,
        rename = "ID",
        alias = "programme_id",
        deserialize_with = "deserialize_string"
    )]
    pub programme_id: String,
    /// EPG channel identifier.
    #[serde(rename = "channelID", alias = "channel_id")]
    pub channel_id: EpgChannelId,
}

/// A provider programme response envelope.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct ProgrammeResponse {
    /// Programme records.
    #[serde(default)]
    pub data: Vec<Programme>,
    /// Provider status marker.
    pub status: String,
    /// Optional provider error code.
    #[serde(default, rename = "errCode", alias = "error_code")]
    pub error_code: String,
    /// Optional provider error message.
    #[serde(default, rename = "errMsg", alias = "error_message")]
    pub error_message: String,
}

/// A provider channel-list response envelope.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct ChannelListResponse {
    /// Channel records.
    #[serde(default)]
    pub data: Vec<Channel>,
    /// Provider status marker.
    pub status: String,
    /// Optional provider error code.
    #[serde(default, rename = "errCode", alias = "error_code")]
    pub error_code: String,
    /// Optional provider error message.
    #[serde(default, rename = "errMsg", alias = "error_message")]
    pub error_message: String,
}

/// A TVOD playback address returned by the provider.
#[derive(Clone, Deserialize, PartialEq, Eq)]
pub struct TvodAddress {
    /// Provider programme identifier.
    #[serde(
        rename = "ID",
        alias = "programme_id",
        deserialize_with = "deserialize_number_or_string"
    )]
    pub programme_id: String,
    /// EPG channel identifier.
    #[serde(
        rename = "channelID",
        alias = "channel_id",
        deserialize_with = "deserialize_number_or_string"
    )]
    pub channel_id: EpgChannelId,
    /// Start timestamp in milliseconds.
    #[serde(
        rename = "startTime",
        alias = "start_time_ms",
        deserialize_with = "deserialize_i64_from_number_or_string"
    )]
    pub start_time_ms: i64,
    /// End timestamp in milliseconds.
    #[serde(
        rename = "endTime",
        alias = "end_time_ms",
        deserialize_with = "deserialize_i64_from_number_or_string"
    )]
    pub end_time_ms: i64,
    /// Provider playback URL.
    #[serde(
        rename = "playURL",
        alias = "play_url",
        deserialize_with = "deserialize_number_or_string"
    )]
    pub play_url: String,
}

impl fmt::Debug for TvodAddress {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TvodAddress")
            .field("programme_id", &self.programme_id)
            .field("channel_id", &self.channel_id)
            .field("start_time_ms", &self.start_time_ms)
            .field("end_time_ms", &self.end_time_ms)
            .field("play_url", &"[REDACTED]")
            .finish()
    }
}

/// A TVOD response envelope.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct TvodAddressResponse {
    /// Playback address.
    pub data: TvodAddress,
    /// Provider status marker.
    pub status: String,
    /// Optional provider error code.
    #[serde(default, rename = "errCode", alias = "error_code")]
    pub error_code: String,
    /// Optional provider error message.
    #[serde(default, rename = "errMsg", alias = "error_message")]
    pub error_message: String,
}

/// An HLS playback address returned by the provider.
#[derive(Clone, Deserialize, PartialEq, Eq)]
pub struct HlsAddress {
    /// Optional transport-stream timestamp marker.
    #[serde(default, rename = "tsTime", alias = "ts_time")]
    pub ts_time: String,
    /// Optional transport-stream marker.
    #[serde(default, rename = "isTs", alias = "is_ts")]
    pub is_ts: String,
    /// Optional status marker.
    #[serde(default)]
    pub status: String,
    /// Provider HLS URL.
    #[serde(rename = "hlsUrl", alias = "hls_url")]
    pub hls_url: String,
}

impl fmt::Debug for HlsAddress {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HlsAddress")
            .field("ts_time", &self.ts_time)
            .field("is_ts", &self.is_ts)
            .field("status", &self.status)
            .field("hls_url", &"[REDACTED]")
            .finish()
    }
}
