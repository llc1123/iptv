use crate::{
    epg::{BootChannel, Programme},
    guide::{GuideZone, ProgrammeIndex},
    logo_catalog::LogoCatalog,
    render::{BaseUrl, RenderKind, logo_url, render_m3u, render_xmltv},
};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

type RenderCache = Mutex<HashMap<(RenderKind, String), Arc<[u8]>>>;

/// Feeds embed the client's Host header, so each distinct host is a separate
/// body; past this many the snapshot renders without caching.
const RENDER_CACHE_ENTRIES: usize = 16;

pub struct Snapshot {
    channels: Vec<BootChannel>,
    programmes: Vec<Programme>,
    by_id: ProgrammeIndex,
    guide_zone: GuideZone,
    catchup_days: u16,
    rendered: RenderCache,
}

impl Snapshot {
    pub fn new(
        channels: Vec<BootChannel>,
        programmes: Vec<Programme>,
        guide_zone: GuideZone,
        catchup_days: u16,
    ) -> Arc<Self> {
        let by_id = ProgrammeIndex::new(programmes);
        let programmes = by_id.programmes().to_vec();
        Arc::new(Self {
            channels,
            programmes,
            by_id,
            guide_zone,
            catchup_days,
            rendered: Mutex::new(HashMap::new()),
        })
    }
    pub fn channels(&self) -> &[BootChannel] {
        &self.channels
    }
    pub fn programmes(&self) -> &[Programme] {
        &self.programmes
    }
    pub fn programme_index(&self) -> &ProgrammeIndex {
        &self.by_id
    }
    pub fn guide_zone(&self) -> &GuideZone {
        &self.guide_zone
    }
    pub fn render(
        &self,
        kind: RenderKind,
        base: &BaseUrl,
        logos: Option<&LogoCatalog>,
    ) -> Result<Arc<[u8]>, crate::render::RenderError> {
        // Feeds join paths onto the base without its trailing slashes, so
        // those variants render identical bytes and share an entry.
        let key = (kind, base.as_str().trim_end_matches('/').to_owned());
        if let Ok(cache) = self.rendered.lock() {
            if let Some(bytes) = cache.get(&key) {
                return Ok(Arc::clone(bytes));
            }
        }
        // Like Python, feeds advertise only logos served by `/logos`.
        let channels = self
            .channels
            .iter()
            .map(|channel| {
                let mut channel = channel.clone();
                channel.logo_url = match logos {
                    Some(logos) if logos.has_logo(&channel.channel_id) => {
                        logo_url(base, &channel.channel_id)
                    }
                    _ => String::new(),
                };
                channel
            })
            .collect::<Vec<_>>();
        let bytes = match kind {
            RenderKind::M3u => render_m3u(&channels, base, self.catchup_days)?,
            RenderKind::Xmltv => render_xmltv(&channels, &self.programmes, &self.guide_zone, base)?,
        };
        let bytes: Arc<[u8]> = Arc::from(bytes);
        if let Ok(mut cache) = self.rendered.lock() {
            if cache.len() < RENDER_CACHE_ENTRIES {
                cache.insert(key, Arc::clone(&bytes));
            }
        }
        Ok(bytes)
    }
}
