//! Exact, bounded M3U and XMLTV response rendering.

use thiserror::Error;

use crate::epg::models::{BootChannel, Programme};
use crate::guide::{GuideTimeError, GuideZone};

#[path = "render_writer.rs"]
mod render_writer;
use render_writer::{
    Writer, validate_line_fields, validate_xml_value, write_component, xml_attr, xml_text,
};

/// Maximum accepted base URL length and rendered response size.
pub const MAX_RENDER_BYTES: usize = 16 * 1024 * 1024;

#[path = "render_base.rs"]
mod render_base;
pub use render_base::BaseUrl;

/// The two independently cached feed formats.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RenderKind {
    /// M3U channel feed.
    M3u,
    /// XMLTV guide feed.
    Xmltv,
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
        write_xml_text_element(writer, "display-name", &channel.name)?;
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
    let mut ordered = programmes
        .iter()
        .filter(|programme| ids.contains(programme.channel_id.as_str()))
        .filter(|programme| {
            seen.insert((
                programme.channel_id.as_str(),
                programme.start_time_ms,
                programme.end_time_ms,
                programme.title.as_str(),
            ))
        })
        .collect::<Vec<_>>();
    ordered.sort_by(|left, right| {
        left.channel_id
            .cmp(&right.channel_id)
            .then(left.start_time_ms.cmp(&right.start_time_ms))
    });
    for programme in ordered {
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
        write_xml_text_element(writer, "title", &programme.title)?;
        if !programme.programme_id.is_empty() {
            validate_xml_value(&programme.programme_id)?;
            writer.text("    <episode-num system=\"iptv-id\">", false)?;
            xml_text(writer, &programme.programme_id)?;
            writer.line("</episode-num>", true)?;
            writer.text("    <url>", false)?;
            xml_text(writer, base.joined())?;
            writer.text("/replay/", false)?;
            write_component(writer, &programme.channel_id)?;
            writer.bytes(b"/")?;
            write_component(writer, &programme.programme_id)?;
            writer.line(".m3u8</url>", true)?;
        }
        writer.line("  </programme>", true)?;
    }
    writer.line("</tv>", false)?;
    Ok(())
}

fn write_xml_text_element(writer: &mut Writer, name: &str, value: &str) -> Result<(), RenderError> {
    if value.is_empty() {
        writer.line(&format!("    <{name} />"), true)?;
    } else {
        writer.text(&format!("    <{name}>"), false)?;
        xml_text(writer, value)?;
        writer.line(&format!("</{name}>"), true)?;
    }
    Ok(())
}

impl From<GuideTimeError> for RenderError {
    fn from(_: GuideTimeError) -> Self {
        Self::InvalidTimestamp
    }
}

#[cfg(test)]
#[path = "render_regression_tests.rs"]
mod regression_tests;
#[cfg(test)]
#[path = "render_tests.rs"]
mod tests;
