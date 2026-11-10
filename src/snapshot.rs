//! One immutable view of channels and guide, with its rendered feeds.

use crate::{
    epg::{BootChannel, Programme},
    guide::GuideZone,
    render::{BaseUrl, RenderError, RenderKind, render_m3u_with_logos, render_xmltv_with_logos},
};
use axum::body::Bytes;
use std::{
    collections::HashMap,
    path::Path,
    sync::{Arc, Mutex},
};

/// Feeds embed the client's Host header, so each distinct host is a separate
/// body; past this many the snapshot renders without caching.
const RENDER_CACHE_ENTRIES: usize = 16;

pub struct Snapshot {
    channels: Vec<BootChannel>,
    programmes: Arc<[Programme]>,
    /// Indexes into `programmes` per channel, in guide order.
    by_channel: HashMap<String, Vec<usize>>,
    zone: GuideZone,
    catchup_days: u16,
    rendered: Mutex<HashMap<(RenderKind, String), Bytes>>,
}

impl Snapshot {
    /// Programmes whose interval is empty or inverted are dropped.
    pub fn new(
        channels: Vec<BootChannel>,
        mut programmes: Vec<Programme>,
        zone: GuideZone,
        catchup_days: u16,
    ) -> Arc<Self> {
        programmes.retain(|programme| programme.start_time_ms < programme.end_time_ms);
        let mut by_channel: HashMap<String, Vec<usize>> = HashMap::new();
        for (index, programme) in programmes.iter().enumerate() {
            by_channel
                .entry(programme.channel_id.clone())
                .or_default()
                .push(index);
        }
        Arc::new(Self {
            channels,
            programmes: programmes.into(),
            by_channel,
            zone,
            catchup_days,
            rendered: Mutex::new(HashMap::new()),
        })
    }

    pub fn channels(&self) -> &[BootChannel] {
        &self.channels
    }

    pub fn has_channel(&self, channel_id: &str) -> bool {
        self.channels
            .iter()
            .any(|channel| channel.channel_id == channel_id)
    }

    pub fn programmes(&self) -> &[Programme] {
        &self.programmes
    }

    pub(crate) fn programmes_shared(&self) -> Arc<[Programme]> {
        Arc::clone(&self.programmes)
    }

    fn channel_programmes(&self, channel_id: &str) -> impl Iterator<Item = &Programme> {
        self.by_channel
            .get(channel_id)
            .into_iter()
            .flatten()
            .map(|&index| &self.programmes[index])
    }

    /// The first programme of a channel with this ID.
    pub fn programme_by_id(&self, channel_id: &str, programme_id: &str) -> Option<&Programme> {
        self.channel_programmes(channel_id)
            .find(|programme| !programme_id.is_empty() && programme.programme_id == programme_id)
    }

    /// The first programme of a channel airing at `timestamp_ms`.
    pub fn programme_at(&self, channel_id: &str, timestamp_ms: i64) -> Option<&Programme> {
        self.channel_programmes(channel_id).find(|programme| {
            programme.start_time_ms <= timestamp_ms && timestamp_ms < programme.end_time_ms
        })
    }

    /// The feed for `base`, rendered once per distinct base.
    pub fn render(
        &self,
        kind: RenderKind,
        base: &BaseUrl,
        logos: Option<&Path>,
    ) -> Result<Bytes, RenderError> {
        // Keyed on the exact base: M3U stream lines keep a trailing slash
        // (as Python does), so `http://h` and `http://h/` render differently.
        let key = (kind, base.as_str().to_owned());
        if let Some(bytes) = self
            .rendered
            .lock()
            .ok()
            .and_then(|cache| cache.get(&key).cloned())
        {
            return Ok(bytes);
        }
        let bytes = Bytes::from(match kind {
            RenderKind::M3u => {
                render_m3u_with_logos(&self.channels, base, self.catchup_days, logos)
            }
            RenderKind::Xmltv => {
                render_xmltv_with_logos(&self.channels, &self.programmes, self.zone, base, logos)?
            }
        });
        if let Ok(mut cache) = self.rendered.lock()
            && cache.len() < RENDER_CACHE_ENTRIES
        {
            cache.insert(key, bytes.clone());
        }
        Ok(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn programme(id: &str, title: &str, start: i64, end: i64) -> Programme {
        Programme {
            title: title.to_owned(),
            start_time_ms: start,
            end_time_ms: end,
            programme_id: id.to_owned(),
            channel_id: "c".to_owned(),
        }
    }

    #[test]
    fn lookups_take_the_first_match_and_skip_empty_intervals() {
        let snapshot = Snapshot::new(
            Vec::new(),
            vec![
                programme("dup", "empty", 5, 5),
                programme("dup", "first", 1, 3),
                programme("dup", "second", 2, 4),
            ],
            GuideZone(chrono_tz::UTC),
            1,
        );
        let title = |programme: Option<&Programme>| programme.map(|item| item.title.clone());
        assert_eq!(
            title(snapshot.programme_by_id("c", "dup")),
            Some("first".to_owned())
        );
        assert_eq!(
            title(snapshot.programme_at("c", 2)),
            Some("first".to_owned())
        );
        assert_eq!(title(snapshot.programme_at("c", 5)), None);
        assert_eq!(title(snapshot.programme_by_id("other", "dup")), None);
    }
}
