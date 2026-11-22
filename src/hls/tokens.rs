//! Short-lived, unguessable proxy tokens for upstream HLS resources.

use std::{
    collections::{HashMap, HashSet},
    sync::Mutex,
    time::Duration,
};
use tokio::time::Instant;

/// Idle lifetime of a token; each use extends it.
const TTL: Duration = Duration::from_secs(300);
pub(crate) const CAPACITY: usize = 10_000;
const URL_MAX_BYTES: usize = 8 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenStoreError {
    Invalid,
    Unavailable,
    Capacity,
}
impl std::fmt::Display for TokenStoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("HLS token unavailable")
    }
}
impl std::error::Error for TokenStoreError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SessionId(u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaybackContext {
    Live,
    NewReplay,
    Replay(SessionId),
}

#[derive(Clone, PartialEq, Eq)]
pub struct Resource {
    pub url: String,
    pub context: PlaybackContext,
}

// Upstream URLs can contain credentials.
impl std::fmt::Debug for Resource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Resource")
            .field("context", &self.context)
            .finish_non_exhaustive()
    }
}

enum Lifetime {
    Live(Instant),
    Replay(SessionId),
}

struct Entry {
    url: String,
    lifetime: Lifetime,
}

#[derive(Default)]
struct Entries {
    by_token: HashMap<String, Entry>,
    by_url: HashMap<(Option<SessionId>, (String, String)), String>,
    sessions: HashMap<SessionId, Instant>,
    next_session: u64,
}

impl Entries {
    fn cleanup(&mut self, now: Instant) {
        self.sessions.retain(|_, deadline| *deadline > now);
        self.by_token.retain(|_, entry| match entry.lifetime {
            Lifetime::Live(deadline) => deadline > now,
            Lifetime::Replay(id) => self.sessions.contains_key(&id),
        });
        self.prune_index();
    }

    /// Published replay resources are never evicted while their session is active.
    fn evict(&mut self, incoming: usize, protected: &HashSet<&str>) -> Result<(), TokenStoreError> {
        let excess = (self.by_token.len() + incoming).saturating_sub(CAPACITY);
        if excess > 0 {
            let mut oldest: Vec<(Instant, String)> = self
                .by_token
                .iter()
                .filter(|(token, _)| !protected.contains(token.as_str()))
                .filter_map(|(token, entry)| match entry.lifetime {
                    Lifetime::Live(deadline) => Some((deadline, token.clone())),
                    Lifetime::Replay(_) => None,
                })
                .collect();
            if oldest.len() < excess {
                return Err(TokenStoreError::Capacity);
            }
            oldest.sort_unstable();
            for (_, token) in oldest.into_iter().take(excess) {
                self.by_token.remove(&token);
            }
            self.prune_index();
        }
        Ok(())
    }

    fn prune_index(&mut self) {
        let by_token = &self.by_token;
        self.by_url.retain(|_, token| by_token.contains_key(token));
    }
}

#[derive(Default)]
pub struct TokenStore {
    entries: Mutex<Entries>,
}

impl TokenStore {
    /// Reuses each `(owner, url, suffix)` token. New replay requests get distinct
    /// sessions only after successful, nonempty registration. Expired sessions
    /// cannot be revived. Capacity failures preserve published replay resources.
    pub fn register_batch(
        &self,
        values: &[(String, String)],
        context: PlaybackContext,
    ) -> Result<Vec<String>, TokenStoreError> {
        let unique: HashSet<_> = values.iter().collect();
        if unique.len() > CAPACITY {
            return Err(TokenStoreError::Capacity);
        }
        if values.iter().any(|(url, _)| url.len() > URL_MAX_BYTES) {
            return Err(TokenStoreError::Invalid);
        }
        let mut entries = self
            .entries
            .lock()
            .map_err(|_| TokenStoreError::Unavailable)?;
        let now = Instant::now();
        entries.cleanup(now);
        let owner = match context {
            PlaybackContext::Live => None,
            PlaybackContext::NewReplay => Some(SessionId(
                entries
                    .next_session
                    .checked_add(1)
                    .ok_or(TokenStoreError::Capacity)?,
            )),
            PlaybackContext::Replay(id) => {
                if !entries.sessions.contains_key(&id) {
                    return Err(TokenStoreError::Unavailable);
                }
                Some(id)
            }
        };
        if values.is_empty() {
            return Ok(Vec::new());
        }
        let mut pending = HashMap::new();
        let mut missing = 0;
        for value in unique {
            let key = (owner, value.clone());
            let token = if let Some(token) = entries.by_url.get(&key) {
                token.clone()
            } else {
                missing += 1;
                random_token(&value.1)?
            };
            pending.insert(value, token);
        }
        let protected = pending.values().map(String::as_str).collect();
        entries.evict(missing, &protected)?;
        let mut result = Vec::with_capacity(values.len());
        for value in values {
            let token = &pending[value];
            entries.by_url.insert((owner, value.clone()), token.clone());
            entries.by_token.insert(
                token.clone(),
                Entry {
                    url: value.0.clone(),
                    lifetime: match owner {
                        None => Lifetime::Live(now + TTL),
                        Some(id) => Lifetime::Replay(id),
                    },
                },
            );
            result.push(token.clone());
        }
        if let Some(id) = owner {
            entries.sessions.insert(id, now + TTL);
            entries.next_session = entries.next_session.max(id.0);
        }
        Ok(result)
    }

    /// Renews one live token or its entire replay session in constant time.
    pub fn resolve(&self, token: &str) -> Result<Resource, TokenStoreError> {
        let mut entries = self
            .entries
            .lock()
            .map_err(|_| TokenStoreError::Unavailable)?;
        let now = Instant::now();
        let Entries {
            by_token, sessions, ..
        } = &mut *entries;
        let entry = by_token
            .get_mut(token)
            .ok_or(TokenStoreError::Unavailable)?;
        let (deadline, context) = match &mut entry.lifetime {
            Lifetime::Live(deadline) => (deadline, PlaybackContext::Live),
            Lifetime::Replay(id) => (
                sessions.get_mut(id).ok_or(TokenStoreError::Unavailable)?,
                PlaybackContext::Replay(*id),
            ),
        };
        if *deadline <= now {
            return Err(TokenStoreError::Unavailable);
        }
        *deadline = now + TTL;
        Ok(Resource {
            url: entry.url.clone(),
            context,
        })
    }
}

/// 24 URL-safe base64 characters (144 random bits) followed by `suffix`.
fn random_token(suffix: &str) -> Result<String, TokenStoreError> {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut bytes = [0_u8; 18];
    getrandom::fill(&mut bytes).map_err(|_| TokenStoreError::Unavailable)?;
    let mut token = String::with_capacity(24 + suffix.len());
    for chunk in bytes.as_chunks::<3>().0 {
        let bits = u32::from(chunk[0]) << 16 | u32::from(chunk[1]) << 8 | u32::from(chunk[2]);
        for shift in [18, 12, 6, 0] {
            let index =
                usize::try_from(bits >> shift & 63).map_err(|_| TokenStoreError::Invalid)?;
            token.push(char::from(ALPHABET[index]));
        }
    }
    token.push_str(suffix);
    Ok(token)
}

#[cfg(test)]
mod tests;
