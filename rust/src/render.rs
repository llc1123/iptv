//! Exact, bounded M3U and XMLTV response rendering.

use thiserror::Error;

use crate::epg::models::{BootChannel, Programme};
use crate::guide::{GuideTimeError, GuideZone};

#[path = "render_writer.rs"]
mod render_writer;
use render_writer::{
    Writer, append_component, validate_line_fields, validate_xml_value, write_component, xml_attr,
    xml_text,
};

/// Maximum accepted base URL length and rendered response size.
pub const MAX_RENDER_BYTES: usize = 16 * 1024 * 1024;
const MAX_BASE_URL_BYTES: usize = 2048;

/// The two independently cached feed formats.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RenderKind {
    /// M3U channel feed.
    M3u,
    /// XMLTV guide feed.
    Xmltv,
}

/// A validated direct request origin retained exactly for cache identity.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct BaseUrl(String);

impl BaseUrl {
    /// Validates a direct `http` or `https` scheme and authority without headers or paths.
    ///
    /// # Errors
    /// Returns [`RenderError::InvalidBaseUrl`] for malformed, oversized, or control-bearing input.
    pub fn parse(value: &str) -> Result<Self, RenderError> {
        if value.is_empty()
            || value.len() > MAX_BASE_URL_BYTES
            || value.chars().any(char::is_control)
            || value.to_ascii_lowercase().contains("forwarded")
        {
            return Err(RenderError::InvalidBaseUrl);
        }
        let Some((scheme, remainder)) = value.split_once("://") else {
            return Err(RenderError::InvalidBaseUrl);
        };
        if !matches!(scheme, "http" | "https") || remainder.is_empty() {
            return Err(RenderError::InvalidBaseUrl);
        }
        let authority_end = remainder.find('/').unwrap_or(remainder.len());
        let authority = &remainder[..authority_end];
        let path = &remainder[authority_end..];
        if authority.is_empty() || path.chars().any(|character| character != '/') {
            return Err(RenderError::InvalidBaseUrl);
        }
        if authority.contains('@') || authority.contains(' ') || authority.contains('\\') {
            return Err(RenderError::InvalidBaseUrl);
        }
        let host = if let Some(end) = authority.find(']') {
            if !authority.starts_with('[') {
                return Err(RenderError::InvalidBaseUrl);
            }
            let suffix = &authority[end + 1..];
            if !suffix.is_empty() {
                let Some(port) = suffix.strip_prefix(':') else {
                    return Err(RenderError::InvalidBaseUrl);
                };
                valid_port(port)?;
            }
            &authority[..=end]
        } else if authority.matches(':').count() == 1 {
            let (host, port) = authority
                .rsplit_once(':')
                .ok_or(RenderError::InvalidBaseUrl)?;
            if host.is_empty() {
                return Err(RenderError::InvalidBaseUrl);
            }
            valid_port(port)?;
            host
        } else if authority.contains(':') {
            return Err(RenderError::InvalidBaseUrl);
        } else {
            authority
        };
        if host.is_empty() {
            return Err(RenderError::InvalidBaseUrl);
        }
        Ok(Self(value.to_owned()))
    }

    /// Returns the exact input used as the cache key.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    fn joined(&self) -> &str {
        self.0.trim_end_matches('/')
    }
}

fn valid_port(port: &str) -> Result<(), RenderError> {
    if port.is_empty() || port.parse::<u16>().ok().is_none_or(|value| value == 0) {
        Err(RenderError::InvalidBaseUrl)
    } else {
        Ok(())
    }
}

/// Fixed, redacted rendering and cache errors.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum RenderError {
    /// The request base was not a direct origin.
    #[error("invalid render base URL")]
    InvalidBaseUrl,
    /// A feed line contained a forbidden control character.
    #[error("invalid render field")]
    InvalidField,
    /// A timestamp could not be represented in the configured zone.
    #[error("invalid render timestamp")]
    InvalidTimestamp,
    /// The measured output exceeded the fixed response bound.
    #[error("render output limit exceeded")]
    Oversize,
    /// The per-snapshot base cache has no inactive entry to evict.
    #[error("render cache capacity exhausted")]
    CacheCapacity,
    /// The cache lock could not be acquired.
    #[error("render cache unavailable")]
    CacheUnavailable,
    /// The caller's lifecycle cancellation was observed.
    #[error("render cancelled")]
    Cancelled,
    /// A completed flight no longer owned its cache slot.
    #[error("render ownership lost")]
    OwnershipLost,
}

/// Renders the exact Python-compatible M3U bytes.
///
/// # Errors
/// Returns a fixed [`RenderError`] when a field, URL, allocation, or size bound is invalid.
pub fn render_m3u(
    channels: &[BootChannel],
    base_url: &BaseUrl,
    catchup_days: u16,
) -> Result<Vec<u8>, RenderError> {
    let mut measure = Writer::measure(MAX_RENDER_BYTES);
    write_m3u(&mut measure, channels, base_url, catchup_days)?;
    let mut writer = Writer::output(MAX_RENDER_BYTES, measure.count)?;
    write_m3u(&mut writer, channels, base_url, catchup_days)?;
    writer.finish()
}

/// Renders the exact Python-compatible XMLTV bytes.
///
/// # Errors
/// Returns a fixed [`RenderError`] when a field, timestamp, URL, allocation, or size bound is invalid.
pub fn render_xmltv(
    channels: &[BootChannel],
    programmes: &[Programme],
    zone: &GuideZone,
    base_url: &BaseUrl,
) -> Result<Vec<u8>, RenderError> {
    let mut measure = Writer::measure(MAX_RENDER_BYTES);
    write_xmltv(&mut measure, channels, programmes, zone, base_url)?;
    let mut writer = Writer::output(MAX_RENDER_BYTES, measure.count)?;
    write_xmltv(&mut writer, channels, programmes, zone, base_url)?;
    writer.finish()
}

fn write_m3u(
    writer: &mut Writer,
    channels: &[BootChannel],
    base: &BaseUrl,
    catchup_days: u16,
) -> Result<(), RenderError> {
    writer.line("#EXTM3U x-tvg-url=\"", false)?;
    writer.text(base.joined(), false)?;
    writer.line("/xmltv.xml\"", true)?;
    for channel in channels {
        validate_line_fields(channel)?;
        writer.text("#EXTINF:-1 tvg-id=\"", false)?;
        writer.text(&channel.channel_id, false)?;
        writer.text("\" tvg-name=\"", false)?;
        writer.text(&channel.name, false)?;
        if !channel.group_title.is_empty() {
            writer.text("\" group-title=\"", false)?;
            writer.text(&channel.group_title, false)?;
        }
        let catchup_enabled = channel.time_shift == "1" && catchup_days > 0;
        if catchup_enabled {
            writer.text("\" catchup=\"append\" catchup-days=\"", false)?;
            writer.text(&catchup_days.to_string(), false)?;
            writer.text("\" catchup-source=\"", false)?;
            writer.text(base.joined(), false)?;
            writer.text("/catchup/", false)?;
            write_component(writer, &channel.channel_id)?;
            writer.text(".m3u8?utc={utc}&lutc={lutc}\"", false)?;
        }
        if !channel.logo_url.is_empty() {
            if !catchup_enabled {
                writer.text("\"", false)?;
            }
            writer.text(" tvg-logo=\"", false)?;
            writer.text(&channel.logo_url, false)?;
        }
        writer.text("\",", false)?;
        writer.text(&channel.name, false)?;
        writer.line("", true)?;
        writer.text(base.as_str(), false)?;
        writer.text("/stream/", false)?;
        write_component(writer, &channel.channel_id)?;
        writer.line(".m3u8", true)?;
    }
    Ok(())
}

fn write_xmltv(
    writer: &mut Writer,
    channels: &[BootChannel],
    programmes: &[Programme],
    zone: &GuideZone,
    base: &BaseUrl,
) -> Result<(), RenderError> {
    writer.line("<?xml version='1.0' encoding='utf-8'?>", true)?;
    if channels.is_empty() && programmes.is_empty() {
        writer.line("<tv generator-info-name=\"iptv-media-server\" />", false)?;
        return Ok(());
    }
    writer.line("<tv generator-info-name=\"iptv-media-server\">", true)?;
    for channel in channels {
        validate_xml_value(&channel.channel_id)?;
        validate_xml_value(&channel.name)?;
        writer.text("  <channel id=\"", false)?;
        xml_attr(writer, &channel.channel_id)?;
        writer.line("\">", true)?;
        writer.text("    <display-name>", false)?;
        xml_text(writer, &channel.name)?;
        writer.line("</display-name>", true)?;
        if !channel.logo_url.is_empty() {
            validate_xml_value(&channel.logo_url)?;
            writer.text("    <icon src=\"", false)?;
            xml_attr(writer, &channel.logo_url)?;
            writer.line("\" />", true)?;
        }
        writer.line("  </channel>", true)?;
    }
    let ids: std::collections::HashSet<&str> = channels
        .iter()
        .map(|item| item.channel_id.as_str())
        .collect();
    let mut seen = std::collections::HashSet::new();
    for programme in programmes {
        if !ids.contains(programme.channel_id.as_str()) {
            continue;
        }
        let key = (
            &programme.channel_id,
            programme.start_time_ms,
            programme.end_time_ms,
            &programme.title,
        );
        if !seen.insert(key) {
            continue;
        }
        validate_xml_value(&programme.channel_id)?;
        validate_xml_value(&programme.title)?;
        let start = zone.format_timestamp_ms(programme.start_time_ms)?;
        let stop = zone.format_timestamp_ms(programme.end_time_ms)?;
        writer.text("  <programme start=\"", false)?;
        xml_attr(writer, &start)?;
        writer.text("\" stop=\"", false)?;
        xml_attr(writer, &stop)?;
        writer.text("\" channel=\"", false)?;
        xml_attr(writer, &programme.channel_id)?;
        writer.line("\">", true)?;
        writer.text("    <title>", false)?;
        xml_text(writer, &programme.title)?;
        writer.line("</title>", true)?;
        if !programme.programme_id.is_empty() {
            validate_xml_value(&programme.programme_id)?;
            writer.text("    <episode-num system=\"iptv-id\">", false)?;
            xml_text(writer, &programme.programme_id)?;
            writer.line("</episode-num>", true)?;
            writer.text("    <url>", false)?;
            let mut path = Vec::new();
            append_component(&mut path, &programme.channel_id)?;
            path.push(b'/');
            append_component(&mut path, &programme.programme_id)?;
            writer.text(base.joined(), false)?;
            writer.text("/replay/", false)?;
            writer.bytes(&path)?;
            writer.line(".m3u8</url>", true)?;
        }
        writer.line("  </programme>", true)?;
    }
    writer.line("</tv>", false)?;
    Ok(())
}

impl From<GuideTimeError> for RenderError {
    fn from(_: GuideTimeError) -> Self {
        Self::InvalidTimestamp
    }
}

#[cfg(test)]
#[path = "render_tests.rs"]
mod tests;
