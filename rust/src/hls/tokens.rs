use ring::rand::{SecureRandom, SystemRandom};
use std::{
    collections::HashMap,
    sync::Mutex,
    time::{Duration, Instant},
};

const TTL: Duration = Duration::from_secs(300);
pub(crate) const CAPACITY: usize = 10_000;
const URL_MAX_BYTES: usize = 8 * 1024;
const STORED_BYTES: usize = 64 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Resource {
    pub token: String,
    pub url: String,
}

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
    suffix: String,
    expires_at: Instant,
}

#[derive(Default)]
struct Entries {
    by_token: HashMap<String, Entry>,
    by_url: HashMap<(String, String), String>,
    stored_bytes: usize,
}

impl Entries {
    fn remove(&mut self, token: &str) {
        if let Some(entry) = self.by_token.remove(token) {
            self.stored_bytes -= entry.url.len();
            self.by_url.remove(&(entry.url, entry.suffix));
        }
    }

    fn prune(&mut self, now: Instant) {
        let expired = self
            .by_token
            .iter()
            .filter(|(_, entry)| entry.expires_at <= now)
            .map(|(token, _)| token.clone())
            .collect::<Vec<_>>();
        for token in expired {
            self.remove(&token);
        }
    }

    /// Evicts the soonest-expiring entries until `entries` more entries holding
    /// `bytes` more URL bytes fit.
    fn make_room(&mut self, entries: usize, bytes: usize) {
        let over = |store: &Self| {
            store.by_token.len() + entries > CAPACITY || store.stored_bytes + bytes > STORED_BYTES
        };
        if !over(self) {
            return;
        }
        let mut oldest = self
            .by_token
            .iter()
            .map(|(token, entry)| (entry.expires_at, token.clone()))
            .collect::<Vec<_>>();
        oldest.sort_unstable();
        for (_, token) in oldest {
            if !over(self) {
                break;
            }
            self.remove(&token);
        }
    }
}

#[derive(Default)]
pub struct TokenStore {
    entries: Mutex<Entries>,
}

impl TokenStore {
    pub fn register(
        &self,
        url: impl Into<String>,
        suffix: &str,
    ) -> Result<Resource, TokenStoreError> {
        let url = url.into();
        let resources = self.register_batch(&[(url, suffix.to_owned())])?;
        resources
            .into_iter()
            .next()
            .ok_or(TokenStoreError::Unavailable)
    }

    /// Registers every `(url, suffix)` pair or none of them. A URL that is
    /// still registered with the same suffix keeps its token, so repeated
    /// fetches of a live playlist only add its new segments.
    pub fn register_batch(
        &self,
        values: &[(String, String)],
    ) -> Result<Vec<Resource>, TokenStoreError> {
        if values.iter().any(|(url, suffix)| {
            url.is_empty()
                || url.len() > URL_MAX_BYTES
                || suffix.len() > 6
                || suffix
                    .bytes()
                    .any(|byte| byte.is_ascii_whitespace() || byte.is_ascii_control())
        }) {
            return Err(TokenStoreError::Invalid);
        }
        let new_bytes = values.iter().map(|(url, _)| url.len()).sum::<usize>();
        if values.len() > CAPACITY || new_bytes > STORED_BYTES {
            return Err(TokenStoreError::Capacity);
        }
        let fresh = values
            .iter()
            .map(|(_, suffix)| Ok(format!("{}{}", random_token()?, suffix)))
            .collect::<Result<Vec<_>, TokenStoreError>>()?;
        let now = Instant::now();
        let mut entries = self
            .entries
            .lock()
            .map_err(|_| TokenStoreError::Unavailable)?;
        entries.prune(now);
        let missing = values
            .iter()
            .filter(|(url, suffix)| !entries.by_url.contains_key(&(url.clone(), suffix.clone())))
            .map(|(url, _)| url.len())
            .collect::<Vec<_>>();
        entries.make_room(missing.len(), missing.iter().sum());
        let mut resources = Vec::with_capacity(values.len());
        for ((url, suffix), fresh) in values.iter().zip(fresh) {
            let key = (url.clone(), suffix.clone());
            let token = if let Some(token) = entries.by_url.get(&key) {
                token.clone()
            } else {
                entries.stored_bytes += url.len();
                entries.by_url.insert(key, fresh.clone());
                fresh
            };
            entries.by_token.insert(
                token.clone(),
                Entry {
                    url: url.clone(),
                    suffix: suffix.clone(),
                    expires_at: now + TTL,
                },
            );
            resources.push(Resource {
                token,
                url: url.clone(),
            });
        }
        Ok(resources)
    }

    pub fn resolve(&self, token: &str) -> Result<Resource, TokenStoreError> {
        let now = Instant::now();
        let mut entries = self
            .entries
            .lock()
            .map_err(|_| TokenStoreError::Unavailable)?;
        entries.prune(now);
        let entry = entries
            .by_token
            .get_mut(token)
            .ok_or(TokenStoreError::Unavailable)?;
        entry.expires_at = now + TTL;
        Ok(Resource {
            token: token.to_owned(),
            url: entry.url.clone(),
        })
    }
}

fn random_token() -> Result<String, TokenStoreError> {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut bytes = [0_u8; 18];
    SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| TokenStoreError::Unavailable)?;
    let mut output = String::with_capacity(24);
    let mut value = 0_u32;
    let mut bits = 0_u8;
    for byte in bytes {
        value = (value << 8) | u32::from(byte);
        bits += 8;
        while bits >= 6 {
            bits -= 6;
            let index =
                usize::try_from((value >> bits) & 63).map_err(|_| TokenStoreError::Unavailable)?;
            output.push(char::from(ALPHABET[index]));
        }
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registered_tokens_resolve_and_slide() {
        let store = TokenStore::default();
        let resource = store
            .register("http://upstream.test/segment.ts", ".ts")
            .unwrap();
        assert_eq!(store.resolve(&resource.token).unwrap().url, resource.url);
    }

    #[test]
    fn batch_registration_is_atomic_when_one_url_is_invalid() {
        let store = TokenStore::default();
        let existing = store
            .register("http://upstream.test/existing.ts", ".ts")
            .unwrap();
        let invalid = "x".repeat(URL_MAX_BYTES + 1);
        assert_eq!(
            store.register_batch(&[
                ("http://upstream.test/new.ts".to_owned(), ".ts".to_owned()),
                (invalid, ".ts".to_owned()),
            ]),
            Err(TokenStoreError::Invalid)
        );
        assert_eq!(store.entries.lock().unwrap().by_token.len(), 1);
        assert_eq!(store.resolve(&existing.token).unwrap().url, existing.url);
    }

    #[test]
    fn live_urls_keep_their_token_and_suffix_variants_do_not_collide() {
        let store = TokenStore::default();
        let first = store.register("http://upstream.test/a", ".ts").unwrap();
        let again = store.register("http://upstream.test/a", ".ts").unwrap();
        let bare = store.register("http://upstream.test/a", "").unwrap();
        assert_eq!(first.token, again.token);
        assert_ne!(first.token, bare.token);
        assert_eq!(store.entries.lock().unwrap().by_token.len(), 2);
    }

    #[test]
    fn full_store_evicts_the_soonest_expiring_entries() {
        let store = TokenStore::default();
        let batch = |prefix: &str| {
            (0..CAPACITY)
                .map(|index| {
                    (
                        format!("http://upstream.test/{prefix}{index}"),
                        String::new(),
                    )
                })
                .collect::<Vec<_>>()
        };
        let old = store.register_batch(&batch("old")).unwrap();
        let kept = store.resolve(&old[0].token).unwrap();
        let new = store.register("http://upstream.test/new", ".ts").unwrap();
        let entries = store.entries.lock().unwrap();
        assert_eq!(entries.by_token.len(), CAPACITY);
        assert_eq!(entries.by_url.len(), CAPACITY);
        assert!(entries.by_token.contains_key(&kept.token));
        assert!(entries.by_token.contains_key(&new.token));
    }
}
