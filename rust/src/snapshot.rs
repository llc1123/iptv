use crate::{
    epg::{BootChannel, Programme},
    guide::{GuideZone, ProgrammeIndex},
    render::{BaseUrl, RenderKind, render_m3u, render_xmltv},
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
    ) -> Result<Arc<[u8]>, crate::render::RenderError> {
        let key = (kind, base.as_str().to_owned());
        if let Ok(cache) = self.rendered.lock() {
            if let Some(bytes) = cache.get(&key) {
                return Ok(Arc::clone(bytes));
            }
        }
        let bytes = match kind {
            RenderKind::M3u => render_m3u(&self.channels, base, self.catchup_days)?,
            RenderKind::Xmltv => {
                render_xmltv(&self.channels, &self.programmes, &self.guide_zone, base)?
            }
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
