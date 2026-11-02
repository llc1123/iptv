//! Deterministic programme ordering and replay lookup.

use std::collections::HashMap;

use crate::epg::models::Programme;
use thiserror::Error;

const MAX_PROGRAMMES: usize = 100_000;

/// Errors raised while building the programme index.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum IndexError {
    /// The input contains more programmes than the bounded guide allows.
    #[error("guide programme limit exceeded")]
    TooManyProgrammes,
    /// The same non-empty composite identity has conflicting data.
    #[error("conflicting programme identity")]
    ConflictingIdentity,
    /// A programme has an invalid half-open interval.
    #[error("invalid programme interval")]
    InvalidInterval,
}

/// A programme collection with deterministic order and replay indexes.
#[derive(Clone, Debug)]
pub struct ProgrammeIndex {
    programmes: Vec<Programme>,
    by_id: HashMap<(String, String), usize>,
    by_channel: HashMap<String, Vec<usize>>,
}

impl ProgrammeIndex {
    /// Builds an index, removing exact duplicates and rejecting conflicting IDs.
    ///
    /// # Errors
    /// Returns [`IndexError::TooManyProgrammes`] when the bounded input is too large,
    /// [`IndexError::ConflictingIdentity`] for conflicting non-empty IDs, or
    /// [`IndexError::InvalidInterval`] for an empty or reversed interval.
    pub fn new(input: Vec<Programme>) -> Result<Self, IndexError> {
        if input.len() > MAX_PROGRAMMES {
            return Err(IndexError::TooManyProgrammes);
        }
        let mut unique = Vec::with_capacity(input.len());
        let mut identities: HashMap<(String, String), Programme> = HashMap::new();
        for programme in input {
            if programme.start_time_ms >= programme.end_time_ms {
                return Err(IndexError::InvalidInterval);
            }
            if programme.programme_id.is_empty() {
                if !unique.contains(&programme) {
                    unique.push(programme);
                }
                continue;
            }
            let key = (programme.channel_id.clone(), programme.programme_id.clone());
            if let Some(existing) = identities.get(&key) {
                if existing != &programme {
                    return Err(IndexError::ConflictingIdentity);
                }
                continue;
            }
            identities.insert(key, programme.clone());
            unique.push(programme);
        }
        unique.sort_by(|left, right| {
            left.channel_id
                .cmp(&right.channel_id)
                .then(left.start_time_ms.cmp(&right.start_time_ms))
                .then(left.end_time_ms.cmp(&right.end_time_ms))
                .then(left.programme_id.cmp(&right.programme_id))
                .then(left.title.cmp(&right.title))
        });

        let mut by_id = HashMap::with_capacity(identities.len());
        let mut by_channel: HashMap<String, Vec<usize>> = HashMap::new();
        for (index, programme) in unique.iter().enumerate() {
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
        Ok(Self {
            programmes: unique,
            by_id,
            by_channel,
        })
    }

    /// Returns programmes in deterministic order.
    #[must_use]
    pub fn programmes(&self) -> &[Programme] {
        &self.programmes
    }

    /// Finds a programme by its non-empty channel and programme IDs.
    #[must_use]
    pub fn by_id(&self, channel_id: &str, programme_id: &str) -> Option<&Programme> {
        self.by_id
            .get(&(channel_id.to_owned(), programme_id.to_owned()))
            .and_then(|index| self.programmes.get(*index))
    }

    /// Finds the first programme covering a timestamp in `[start, end)`.
    #[must_use]
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

#[cfg(test)]
#[expect(clippy::unwrap_used, reason = "unit fixtures use fixed valid values")]
mod tests {
    use super::*;

    fn programme(title: &str, start: i64, end: i64, id: &str) -> Programme {
        Programme {
            title: title.to_owned(),
            start_time_ms: start,
            end_time_ms: end,
            programme_id: id.to_owned(),
            channel_id: "c1".to_owned(),
        }
    }

    #[test]
    fn index_deduplicates_sorts_and_uses_half_open_covering() {
        let items = vec![
            programme("b", 20, 30, "2"),
            programme("a", 0, 10, "1"),
            programme("a", 0, 10, "1"),
        ];
        let index = ProgrammeIndex::new(items).unwrap();
        assert_eq!(index.programmes().len(), 2);
        assert_eq!(
            index.by_id("c1", "1").map(|item| item.title.as_str()),
            Some("a")
        );
        assert!(index.covering("c1", 10).is_none());
        assert_eq!(
            index.covering("c1", 20).map(|item| item.title.as_str()),
            Some("b")
        );
    }

    #[test]
    fn conflicting_nonempty_identity_is_rejected() {
        let result =
            ProgrammeIndex::new(vec![programme("a", 0, 10, "1"), programme("b", 0, 10, "1")]);
        assert!(matches!(result, Err(IndexError::ConflictingIdentity)));
    }
}
