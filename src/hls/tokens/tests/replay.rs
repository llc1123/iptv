use super::*;

#[tokio::test(start_paused = true)]
async fn replay_owners_isolate_identical_urls_and_reuse_only_within_session() {
    let store = TokenStore::default();
    let values = [pair("http://up/media.m3u8", ".m3u8")];
    let first = store
        .register_batch(&values, PlaybackContext::NewReplay)
        .unwrap();
    let second = store
        .register_batch(&values, PlaybackContext::NewReplay)
        .unwrap();
    let live = store
        .register_batch(&values, PlaybackContext::Live)
        .unwrap();
    let context = store.resolve(&first[0]).unwrap().context;
    assert_ne!(first, second);
    assert_ne!(first, live);
    assert_eq!(store.register_batch(&values, context).unwrap(), first);

    tokio::time::advance(Duration::from_secs(240)).await;
    store.resolve(&first[0]).unwrap();
    tokio::time::advance(Duration::from_secs(60)).await;

    assert!(store.resolve(&first[0]).is_ok());
    assert_eq!(store.resolve(&second[0]), Err(TokenStoreError::Unavailable));
    assert_eq!(store.resolve(&live[0]), Err(TokenStoreError::Unavailable));
}

#[tokio::test(start_paused = true)]
async fn idle_session_expires_at_boundary_and_registration_cannot_revive_it() {
    let store = TokenStore::default();
    let values = [pair("http://up/media.m3u8", ".m3u8")];
    let old = store
        .register_batch(&values, PlaybackContext::NewReplay)
        .unwrap();
    let context = store.resolve(&old[0]).unwrap().context;
    tokio::time::advance(TTL).await;

    assert_eq!(store.resolve(&old[0]), Err(TokenStoreError::Unavailable));
    assert_eq!(
        store.register_batch(&values, context),
        Err(TokenStoreError::Unavailable)
    );
    let fresh = store
        .register_batch(&values, PlaybackContext::NewReplay)
        .unwrap();
    assert_ne!(old, fresh);
    assert!(store.resolve(&fresh[0]).is_ok());
    assert_eq!(store.resolve(&old[0]), Err(TokenStoreError::Unavailable));
    let entries = store.entries.lock().unwrap();
    assert_eq!(entries.by_token.len(), 1);
    assert_eq!(entries.by_url.len(), 1);
    assert_eq!(entries.sessions.len(), 1);
}

#[tokio::test(start_paused = true)]
async fn registration_reclaims_expired_sessions_below_capacity() {
    let store = TokenStore::default();
    let values = [
        pair("http://up/first.ts", ".ts"),
        pair("http://up/last.ts", ".ts"),
    ];
    store
        .register_batch(&values, PlaybackContext::NewReplay)
        .unwrap();
    store
        .register_batch(&values, PlaybackContext::NewReplay)
        .unwrap();
    tokio::time::advance(TTL).await;
    store
        .register_batch(&values[..1], PlaybackContext::Live)
        .unwrap();

    let entries = store.entries.lock().unwrap();
    assert_eq!(entries.by_token.len(), 1);
    assert_eq!(entries.by_url.len(), 1);
    assert!(entries.sessions.is_empty());
}

#[test]
fn empty_and_invalid_replays_allocate_no_session() {
    let store = TokenStore::default();
    assert_eq!(
        store
            .register_batch(&[], PlaybackContext::NewReplay)
            .unwrap(),
        Vec::<String>::new()
    );
    assert_eq!(
        store.register_batch(
            &[pair(&"x".repeat(URL_MAX_BYTES + 1), "")],
            PlaybackContext::NewReplay
        ),
        Err(TokenStoreError::Invalid)
    );
    let entries = store.entries.lock().unwrap();
    assert!(entries.by_token.is_empty());
    assert!(entries.by_url.is_empty());
    assert!(entries.sessions.is_empty());
}

#[test]
fn full_replay_store_preserves_published_resources_and_rejects_new_owners() {
    let store = TokenStore::default();
    let values: Vec<_> = (0..CAPACITY)
        .map(|i| pair(&format!("http://up/{i}.ts"), ".ts"))
        .collect();
    let published = store
        .register_batch(&values, PlaybackContext::NewReplay)
        .unwrap();
    let context = store.resolve(&published[0]).unwrap().context;
    for owner in [PlaybackContext::Live, PlaybackContext::NewReplay, context] {
        assert_eq!(
            store.register_batch(&[pair("http://up/new.ts", ".ts")], owner),
            Err(TokenStoreError::Capacity)
        );
    }
    assert_eq!(store.register_batch(&values, context).unwrap(), published);
    for token in published {
        assert!(store.resolve(&token).is_ok());
    }
    let entries = store.entries.lock().unwrap();
    assert_eq!(entries.by_token.len(), CAPACITY);
    assert_eq!(entries.by_url.len(), CAPACITY);
    assert_eq!(entries.sessions.len(), 1);
}

#[tokio::test(start_paused = true)]
async fn capacity_evicts_live_resources_even_when_replay_session_is_older() {
    let store = TokenStore::default();
    let replay = store
        .register_batch(
            &[pair("http://up/replay.ts", ".ts")],
            PlaybackContext::NewReplay,
        )
        .unwrap();
    tokio::time::advance(Duration::from_secs(1)).await;
    let filler: Vec<_> = (1..CAPACITY)
        .map(|i| pair(&format!("http://up/{i}.ts"), ".ts"))
        .collect();
    let live = store
        .register_batch(&filler, PlaybackContext::Live)
        .unwrap();

    let added = store
        .register_batch(
            &[pair("http://up/new.ts", ".ts")],
            PlaybackContext::NewReplay,
        )
        .unwrap();
    assert!(store.resolve(&replay[0]).is_ok());
    assert!(store.resolve(&added[0]).is_ok());
    assert_eq!(
        live.iter()
            .filter(|token| store.resolve(token).is_err())
            .count(),
        1
    );
    let entries = store.entries.lock().unwrap();
    assert_eq!(entries.by_token.len(), CAPACITY);
    assert_eq!(entries.by_url.len(), CAPACITY);
    assert_eq!(entries.sessions.len(), 2);
}
