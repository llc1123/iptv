//! Non-persistent, bounded HLS resource tokens.

use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::Duration;

const TTL: Duration = Duration::from_secs(300);
const MAX_ENTRIES: usize = 500_000;
const MAX_BYTES: usize = 256 * 1024 * 1024;
const MAX_FIELD_BYTES: usize = 4096;
const TOKEN_BYTES: usize = 24;
const QUEUE_METADATA_MULTIPLE: usize = 4;
const QUEUE_METADATA_HEADROOM: usize = 16;

/// Monotonic time source used by the store.
pub trait Clock: Send + Sync {
    /// Returns elapsed monotonic time.
    fn now(&self) -> Duration;
}

/// Token generator used by the store.
pub trait TokenFactory: Send + Sync {
    /// Returns an opaque URL-safe token of exactly 24 characters.
    fn token(&self) -> String;
}

#[derive(Debug)]
struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Duration {
        static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
        std::time::Instant::now().duration_since(*START.get_or_init(std::time::Instant::now))
    }
}

#[derive(Debug)]
struct SystemTokenFactory;

impl TokenFactory for SystemTokenFactory {
    fn token(&self) -> String {
        const ALPHABET: &[u8; 64] =
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
        use ring::rand::{SecureRandom, SystemRandom};
        let mut bytes = [0_u8; 18];
        let _ = SystemRandom::new().fill(&mut bytes);
        let mut out = String::with_capacity(TOKEN_BYTES);
        let mut accumulator = 0_u32;
        let mut bits = 0_u8;
        for byte in bytes {
            accumulator = (accumulator << 8) | u32::from(byte);
            bits += 8;
            while bits >= 6 {
                bits -= 6;
                let index = ((accumulator >> bits) & 0x3f) as usize;
                out.push(ALPHABET[index] as char);
            }
        }
        out
    }
}

/// Opaque token and upstream URL pair.
#[derive(Clone, PartialEq, Eq)]
pub struct Resource {
    /// Full opaque token, including its optional suffix.
    pub token: String,
    /// Validated upstream URL retained for the later proxy layer.
    pub url: String,
}

impl fmt::Debug for Resource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Resource")
            .field("token", &"[redacted]")
            .field("url", &"[redacted]")
            .finish()
    }
}

/// Fixed public token-store failures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenStoreError {
    /// The token or URL exceeds a bounded field or has an invalid suffix.
    Invalid,
    /// The token is absent or expired.
    Unavailable,
    /// A batch could not fit within the bounded store.
    Capacity,
}

impl fmt::Display for TokenStoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("request unavailable")
    }
}

impl std::error::Error for TokenStoreError {}

struct Entry {
    url: String,
    expires_at: Duration,
    generation: u64,
    charge: usize,
}

struct State {
    entries: HashMap<String, Entry>,
    queue: VecDeque<(String, u64)>,
    bytes: usize,
    generation: u64,
}

/// A non-persistent, bounded sliding-TTL token store.
pub struct TokenStore {
    state: Mutex<State>,
    clock: Arc<dyn Clock>,
    factory: Arc<dyn TokenFactory>,
}

impl fmt::Debug for TokenStore {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("TokenStore { entries: [redacted] }")
    }
}

impl Default for TokenStore {
    fn default() -> Self {
        Self::new(Arc::new(SystemClock), Arc::new(SystemTokenFactory))
    }
}

impl TokenStore {
    /// Creates a store with injected monotonic time and token generation.
    #[must_use]
    pub fn new(clock: Arc<dyn Clock>, factory: Arc<dyn TokenFactory>) -> Self {
        Self {
            state: Mutex::new(State {
                entries: HashMap::new(),
                queue: VecDeque::new(),
                bytes: 0,
                generation: 0,
            }),
            clock,
            factory,
        }
    }

    /// Registers one URL with an optional approved suffix.
    ///
    /// # Errors
    /// Returns a fixed error when a field or suffix is invalid or capacity is exhausted.
    pub fn register(
        &self,
        url: impl Into<String>,
        suffix: &str,
    ) -> Result<Resource, TokenStoreError> {
        let url = url.into();
        validate_field(&url)?;
        validate_suffix(suffix)?;
        let token = self.factory.token();
        if token.len() != TOKEN_BYTES || !token.bytes().all(is_url_safe) {
            return Err(TokenStoreError::Invalid);
        }
        let resource = Resource {
            token: format!("{token}{suffix}"),
            url,
        };
        self.insert(resource.clone())?;
        Ok(resource)
    }

    fn insert(&self, resource: Resource) -> Result<(), TokenStoreError> {
        let mut state = self.state.lock().map_err(|_| TokenStoreError::Capacity)?;
        let now = self.clock.now();
        prune(&mut state, now);
        if state.entries.contains_key(&resource.token) {
            return Err(TokenStoreError::Capacity);
        }
        let needed = charge(&resource);
        if needed > MAX_BYTES {
            return Err(TokenStoreError::Capacity);
        }
        while state.entries.len() >= MAX_ENTRIES || state.bytes + needed > MAX_BYTES {
            if !evict_one(&mut state) {
                return Err(TokenStoreError::Capacity);
            }
        }
        state.generation = state.generation.wrapping_add(1);
        let generation = state.generation;
        let entry = Entry {
            url: resource.url,
            expires_at: now + TTL,
            generation,
            charge: needed,
        };
        state.bytes += needed;
        state.queue.push_back((resource.token.clone(), generation));
        state.entries.insert(resource.token, entry);
        Ok(())
    }

    /// Atomically registers a batch, rolling back all entries on failure.
    ///
    /// # Errors
    /// Returns a fixed error and leaves the store unchanged when validation or capacity fails.
    pub fn register_batch<I, U>(&self, values: I) -> Result<Vec<Resource>, TokenStoreError>
    where
        I: IntoIterator<Item = (U, String)>,
        U: Into<String>,
    {
        let mut state = self.state.lock().map_err(|_| TokenStoreError::Capacity)?;
        let now = self.clock.now();
        prune(&mut state, now);
        let mut resources = Vec::new();
        let mut unique_tokens = std::collections::HashSet::new();
        let mut needed = 0_usize;
        for (url, suffix) in values {
            if resources.len() >= MAX_ENTRIES {
                return Err(TokenStoreError::Capacity);
            }
            let url = url.into();
            validate_field(&url)?;
            validate_suffix(&suffix)?;
            let token = self.factory.token();
            if token.len() != TOKEN_BYTES || !token.bytes().all(is_url_safe) {
                return Err(TokenStoreError::Invalid);
            }
            let resource = Resource {
                token: format!("{token}{suffix}"),
                url,
            };
            if !unique_tokens.insert(resource.token.clone())
                || state.entries.contains_key(&resource.token)
            {
                return Err(TokenStoreError::Capacity);
            }
            let resource_charge = charge(&resource);
            needed = needed
                .checked_add(resource_charge)
                .ok_or(TokenStoreError::Capacity)?;
            if needed > MAX_BYTES {
                return Err(TokenStoreError::Capacity);
            }
            resources.push(resource);
        }
        if needed > MAX_BYTES || resources.len() > MAX_ENTRIES {
            return Err(TokenStoreError::Capacity);
        }
        while state.entries.len() + resources.len() > MAX_ENTRIES
            || state.bytes + needed > MAX_BYTES
        {
            if !evict_one(&mut state) {
                return Err(TokenStoreError::Capacity);
            }
        }
        for resource in &resources {
            state.generation = state.generation.wrapping_add(1);
            let generation = state.generation;
            let entry = Entry {
                url: resource.url.clone(),
                expires_at: now + TTL,
                generation,
                charge: charge(resource),
            };
            state.bytes += entry.charge;
            state.queue.push_back((resource.token.clone(), generation));
            state.entries.insert(resource.token.clone(), entry);
        }
        Ok(resources)
    }

    /// Resolves a token and slides its expiry by 300 seconds.
    ///
    /// # Errors
    /// Returns the same fixed unavailable error for malformed, absent, or expired tokens.
    pub fn resolve(&self, token: &str) -> Result<Resource, TokenStoreError> {
        validate_field(token)?;
        let mut state = self
            .state
            .lock()
            .map_err(|_| TokenStoreError::Unavailable)?;
        let now = self.clock.now();
        prune(&mut state, now);
        state.generation = state.generation.wrapping_add(1);
        let generation = state.generation;
        let url = match state.entries.get_mut(token) {
            Some(entry) => {
                entry.expires_at = now + TTL;
                entry.generation = generation;
                entry.url.clone()
            }
            None => return Err(TokenStoreError::Unavailable),
        };
        state.queue.push_back((token.to_owned(), generation));
        compact_queue(&mut state);
        Ok(Resource {
            token: token.to_owned(),
            url,
        })
    }

    /// Returns the current entry count, for boundedness assertions.
    #[must_use]
    pub fn len(&self) -> usize {
        self.state
            .lock()
            .map(|state| state.entries.len())
            .unwrap_or(0)
    }

    /// Returns whether the store has no entries.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

fn is_url_safe(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_')
}

fn validate_field(value: &str) -> Result<(), TokenStoreError> {
    if value.is_empty()
        || value.len() > MAX_FIELD_BYTES
        || value
            .bytes()
            .any(|byte| byte.is_ascii_control() || byte.is_ascii_whitespace())
    {
        Err(TokenStoreError::Invalid)
    } else {
        Ok(())
    }
}

fn validate_suffix(suffix: &str) -> Result<(), TokenStoreError> {
    if suffix.is_empty() || matches!(suffix, ".ts" | ".m3u8" | ".bin" | ".mp4") {
        Ok(())
    } else {
        Err(TokenStoreError::Invalid)
    }
}

fn charge(resource: &Resource) -> usize {
    resource.token.len()
        + resource.url.len()
        + std::mem::size_of::<Entry>()
        + std::mem::size_of::<(String, u64)>()
}

fn prune(state: &mut State, now: Duration) {
    while let Some((token, generation)) = state.queue.front().cloned() {
        let remove = match state.entries.get(&token) {
            Some(entry) if entry.generation == generation => entry.expires_at <= now,
            Some(_) | None => true,
        };
        if !remove {
            break;
        }
        let _ = state.queue.pop_front();
        if let Some(entry) = state.entries.get(&token) {
            if entry.generation != generation {
                continue;
            }
        }
        if let Some(entry) = state.entries.remove(&token) {
            state.bytes = state.bytes.saturating_sub(entry.charge);
        }
    }
}

fn compact_queue(state: &mut State) {
    let limit = state
        .entries
        .len()
        .saturating_mul(QUEUE_METADATA_MULTIPLE)
        .saturating_add(QUEUE_METADATA_HEADROOM);
    if state.queue.len() <= limit {
        return;
    }
    let mut generations = state
        .entries
        .iter()
        .map(|(token, entry)| (token.clone(), entry.generation))
        .collect::<Vec<_>>();
    generations.sort_unstable_by_key(|(_, generation)| *generation);
    state.queue = generations.into_iter().collect();
}

fn evict_one(state: &mut State) -> bool {
    while let Some((token, generation)) = state.queue.pop_front() {
        let matches = state
            .entries
            .get(&token)
            .is_some_and(|entry| entry.generation == generation);
        if matches {
            if let Some(entry) = state.entries.remove(&token) {
                state.bytes = state.bytes.saturating_sub(entry.charge);
                return true;
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    struct TestClock(AtomicU64);
    impl Clock for TestClock {
        fn now(&self) -> Duration {
            Duration::from_millis(self.0.load(Ordering::Relaxed))
        }
    }

    struct TestFactory(AtomicU64);
    impl TokenFactory for TestFactory {
        fn token(&self) -> String {
            format!("testtokens{:014}", self.0.fetch_add(1, Ordering::Relaxed))
        }
    }

    fn store() -> (Arc<TestClock>, TokenStore) {
        let clock = Arc::new(TestClock(AtomicU64::new(0)));
        let factory = Arc::new(TestFactory(AtomicU64::new(1)));
        (Arc::clone(&clock), TokenStore::new(clock, factory))
    }

    #[test]
    fn sliding_expiry_and_fixed_failure_are_bounded() {
        let (clock, store) = store();
        let resource = store.register("https://example.invalid/seg.ts", ".ts");
        assert!(resource.is_ok());
        let resource = resource.ok();
        assert!(resource.as_ref().is_some_and(|value| {
            value
                .token
                .as_bytes()
                .windows(3)
                .any(|window| window == b".ts")
        }));
        let token = resource.map(|value| value.token).unwrap_or_default();
        clock.0.store(299_000, Ordering::Relaxed);
        assert!(store.resolve(&token).is_ok());
        clock.0.store(598_999, Ordering::Relaxed);
        assert!(store.resolve(&token).is_ok());
        clock.0.store(899_000, Ordering::Relaxed);
        assert_eq!(store.resolve(&token), Err(TokenStoreError::Unavailable));
        assert!(!format!("{store:?}").contains(&token));
    }

    #[test]
    fn batch_failure_does_not_partially_register() {
        let (_, store) = store();
        let result = store.register_batch([
            ("https://example.invalid/a", ".ts".to_owned()),
            ("https://example.invalid/b", ".bad".to_owned()),
        ]);
        assert_eq!(result, Err(TokenStoreError::Invalid));
        assert!(store.is_empty());
    }

    #[test]
    fn repeated_resolves_bound_queue_metadata_and_keep_fresh_token_live() {
        let (clock, store) = store();
        let resource = store.register("https://example.invalid/hot.ts", ".ts").ok();
        let Some(resource) = resource else {
            return;
        };

        for now in 1..=1_000 {
            clock.0.store(now, Ordering::Relaxed);
            assert!(store.resolve(&resource.token).is_ok());
        }

        {
            let state = store.state.lock().ok();
            assert!(state.as_ref().is_some_and(|state| {
                state.queue.len()
                    <= state
                        .entries
                        .len()
                        .saturating_mul(QUEUE_METADATA_MULTIPLE)
                        .saturating_add(QUEUE_METADATA_HEADROOM)
            }));
        }
        assert_eq!(store.len(), 1);
        assert!(store.resolve(&resource.token).is_ok());
    }

    #[test]
    fn oversized_batch_stops_before_consuming_unbounded_input() {
        let (_, store) = store();
        let mut consumed = 0_usize;
        let result = store.register_batch(std::iter::from_fn(|| {
            consumed = consumed.saturating_add(1);
            Some(("https://example.invalid/item.ts", ".ts".to_owned()))
        }));

        assert_eq!(result, Err(TokenStoreError::Capacity));
        assert_eq!(consumed, MAX_ENTRIES + 1);
        assert!(store.is_empty());
    }
}
