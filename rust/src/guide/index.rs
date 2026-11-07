use crate::epg::Programme;
use std::collections::HashMap;

#[derive(Clone, Debug)]
pub struct ProgrammeIndex {
    programmes: Vec<Programme>,
    by_id: HashMap<(String, String), usize>,
    by_channel: HashMap<String, Vec<usize>>,
}

impl ProgrammeIndex {
    pub fn new(mut programmes: Vec<Programme>) -> Self {
        programmes.retain(|item| item.start_time_ms < item.end_time_ms);
        let mut by_id = HashMap::new();
        let mut by_channel: HashMap<String, Vec<usize>> = HashMap::new();
        for (index, programme) in programmes.iter().enumerate() {
            by_channel
                .entry(programme.channel_id.clone())
                .or_default()
                .push(index);
            if !programme.programme_id.is_empty() {
                by_id.insert(
                    (programme.channel_id.clone(), programme.programme_id.clone()),
                    index,
                );
            }
        }
        Self {
            programmes,
            by_id,
            by_channel,
        }
    }

    pub fn programmes(&self) -> &[Programme] {
        &self.programmes
    }
    pub fn by_id(&self, channel_id: &str, programme_id: &str) -> Option<&Programme> {
        self.by_id
            .get(&(channel_id.to_owned(), programme_id.to_owned()))
            .and_then(|index| self.programmes.get(*index))
    }
    pub fn covering(&self, channel_id: &str, timestamp_ms: i64) -> Option<&Programme> {
        self.by_channel.get(channel_id).and_then(|indexes| {
            indexes.iter().find_map(|index| {
                let programme = self.programmes.get(*index)?;
                (programme.start_time_ms <= timestamp_ms && timestamp_ms < programme.end_time_ms)
                    .then_some(programme)
            })
        })
    }
}
