//! Short-lived, unguessable proxy tokens for upstream HLS resources.

use std::{
    collections::HashMap,
    sync::Mutex,
    time::{Duration, Instant},
};

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

struct Entry {
    url: String,
    expires_at: Instant,
}

#[derive(Default)]
struct Entries {
    by_token: HashMap<String, Entry>,
    /// Live token for each registered `(url, suffix)`.
    by_url: HashMap<(String, String), String>,
}

impl Entries {
    /// Makes room for `incoming` new tokens: expired entries go first, then
    /// the soonest-expiring ones.
    fn evict(&mut self, now: Instant, incoming: usize) {
        if self.by_token.len() + incoming <= CAPACITY {
            return;
        }
        self.by_token.retain(|_, entry| entry.expires_at > now);
        let excess = (self.by_token.len() + incoming).saturating_sub(CAPACITY);
        if excess > 0 {
            let mut oldest: Vec<(Instant, String)> = self
                .by_token
                .iter()
                .map(|(token, entry)| (entry.expires_at, token.clone()))
                .collect();
            oldest.sort_unstable();
            for (_, token) in oldest.into_iter().take(excess) {
                self.by_token.remove(&token);
            }
        }
        let by_token = &self.by_token;
        self.by_url.retain(|_, token| by_token.contains_key(token));
    }
}

#[derive(Default)]
pub struct TokenStore {
    entries: Mutex<Entries>,
}

impl TokenStore {
    /// Returns a token for every `(url, suffix)` pair, registering none when
    /// any URL is rejected. A pair that is still registered keeps its token,
    /// so repeated fetches of a live playlist only add its new segments.
    pub fn register_batch(
        &self,
        values: &[(String, String)],
    ) -> Result<Vec<String>, TokenStoreError> {
        if values.len() > CAPACITY {
            return Err(TokenStoreError::Capacity);
        }
        if values.iter().any(|(url, _)| url.len() > URL_MAX_BYTES) {
            return Err(TokenStoreError::Invalid);
        }
        let now = Instant::now();
        let mut entries = self
            .entries
            .lock()
            .map_err(|_| TokenStoreError::Unavailable)?;
        // Batch entries that are already registered are the live playlist's
        // current segments: extend them first so eviction takes others.
        let mut missing = 0;
        let Entries { by_url, by_token } = &mut *entries;
        for value in values {
            match by_url.get(value).and_then(|token| by_token.get_mut(token)) {
                Some(entry) => entry.expires_at = now + TTL,
                None => missing += 1,
            }
        }
        entries.evict(now, missing);
        values
            .iter()
            .map(|value| {
                let token = if let Some(token) = entries.by_url.get(value) {
                    token.clone()
                } else {
                    let token = random_token(&value.1)?;
                    entries.by_url.insert(value.clone(), token.clone());
                    token
                };
                entries.by_token.insert(
                    token.clone(),
                    Entry {
                        url: value.0.clone(),
                        expires_at: now + TTL,
                    },
                );
                Ok(token)
            })
            .collect()
    }

    /// Returns the URL behind a live token and extends its lifetime.
    pub fn resolve(&self, token: &str) -> Result<String, TokenStoreError> {
        let now = Instant::now();
        let mut entries = self
            .entries
            .lock()
            .map_err(|_| TokenStoreError::Unavailable)?;
        let entry = entries
            .by_token
            .get_mut(token)
            .filter(|entry| entry.expires_at > now)
            .ok_or(TokenStoreError::Unavailable)?;
        entry.expires_at = now + TTL;
        Ok(entry.url.clone())
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
            token.push(char::from(ALPHABET[(bits >> shift & 63) as usize]));
        }
    }
    token.push_str(suffix);
    Ok(token)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pair(url: &str, suffix: &str) -> (String, String) {
        (url.to_owned(), suffix.to_owned())
    }

    #[test]
    fn a_full_store_evicts_others_before_reregistered_batch_entries() {
        let store = TokenStore::default();
        let live = store
            .register_batch(&[pair("http://up/live.ts", ".ts")])
            .unwrap();
        let filler: Vec<_> = (1..CAPACITY)
            .map(|index| pair(&format!("http://up/{index}.ts"), ".ts"))
            .collect();
        store.register_batch(&filler).unwrap();
        let again = store
            .register_batch(&[
                pair("http://up/live.ts", ".ts"),
                pair("http://up/new.ts", ".ts"),
            ])
            .unwrap();
        assert_eq!(again[0], live[0]);
        assert_eq!(store.entries.lock().unwrap().by_token.len(), CAPACITY);
    }

    #[test]
    fn registered_tokens_resolve() {
        let store = TokenStore::default();
        let tokens = store
            .register_batch(&[pair("http://upstream.test/segment.ts", ".ts")])
            .unwrap();
        assert_eq!(&tokens[0][24..], ".ts");
        assert_eq!(
            store.resolve(&tokens[0]).unwrap(),
            "http://upstream.test/segment.ts"
        );
        assert_eq!(store.resolve("unknown"), Err(TokenStoreError::Unavailable));
    }

    #[test]
    fn batch_registration_is_atomic_when_one_url_is_invalid() {
        let store = TokenStore::default();
        let existing = store
            .register_batch(&[pair("http://upstream.test/existing.ts", ".ts")])
            .unwrap();
        let invalid = "x".repeat(URL_MAX_BYTES + 1);
        assert_eq!(
            store.register_batch(&[
                pair("http://upstream.test/new.ts", ".ts"),
                pair(&invalid, ".ts")
            ]),
            Err(TokenStoreError::Invalid)
        );
        assert_eq!(store.entries.lock().unwrap().by_token.len(), 1);
        assert!(store.resolve(&existing[0]).is_ok());
    }

    #[test]
    fn live_urls_keep_their_token_and_suffix_variants_do_not_collide() {
        let store = TokenStore::default();
        let first = store
            .register_batch(&[pair("http://upstream.test/a", ".ts")])
            .unwrap();
        let again = store
            .register_batch(&[pair("http://upstream.test/a", ".ts")])
            .unwrap();
        let bare = store
            .register_batch(&[pair("http://upstream.test/a", "")])
            .unwrap();
        assert_eq!(first, again);
        assert_ne!(first, bare);
        assert_eq!(store.entries.lock().unwrap().by_token.len(), 2);
    }

    #[test]
    fn full_store_evicts_the_soonest_expiring_entries() {
        let store = TokenStore::default();
        let old: Vec<_> = (0..CAPACITY)
            .map(|index| pair(&format!("http://upstream.test/old{index}"), ""))
            .collect();
        let old = store.register_batch(&old).unwrap();
        store.resolve(&old[0]).unwrap();
        let new = store
            .register_batch(&[pair("http://upstream.test/new", ".ts")])
            .unwrap();
        let entries = store.entries.lock().unwrap();
        assert_eq!(entries.by_token.len(), CAPACITY);
        assert_eq!(entries.by_url.len(), CAPACITY);
        assert!(entries.by_token.contains_key(&old[0]));
        assert!(entries.by_token.contains_key(&new[0]));
    }
}
