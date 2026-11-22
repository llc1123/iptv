use super::*;

mod replay;

#[tokio::test(start_paused = true)]
async fn replay_activity_keeps_unread_future_segments_available() {
    let store = TokenStore::default();
    let playlist = super::super::playlist::rewrite_playlist(
        b"#EXTM3U\n#EXTINF:240,\nfirst.ts\n#EXTINF:240,\nsecond.ts\n#EXTINF:240,\nlast.ts\n#EXT-X-ENDLIST\n",
        &reqwest::Url::parse("http://upstream.test/replay.m3u8").unwrap(),
        &store,
        PlaybackContext::NewReplay,
    )
    .unwrap();
    let playlist = String::from_utf8(playlist).unwrap();
    let tokens: Vec<_> = playlist
        .lines()
        .filter_map(|line| line.strip_prefix("/_hls/"))
        .collect();

    tokio::time::advance(Duration::from_secs(240)).await;
    store.resolve(tokens[0]).unwrap();
    tokio::time::advance(Duration::from_secs(240)).await;

    assert_eq!(
        store.resolve(tokens[2]).unwrap().url,
        "http://upstream.test/last.ts"
    );
}

fn pair(url: &str, suffix: &str) -> (String, String) {
    (url.to_owned(), suffix.to_owned())
}

#[tokio::test(start_paused = true)]
async fn live_activity_does_not_renew_unread_resources() {
    let store = TokenStore::default();
    let tokens = store
        .register_batch(
            &[
                pair("http://upstream.test/live-first.ts", ".ts"),
                pair("http://upstream.test/live-last.ts", ".ts"),
            ],
            PlaybackContext::Live,
        )
        .unwrap();
    tokio::time::advance(Duration::from_secs(240)).await;
    store.resolve(&tokens[0]).unwrap();
    tokio::time::advance(Duration::from_secs(60)).await;

    assert!(store.resolve(&tokens[0]).is_ok());
    assert_eq!(store.resolve(&tokens[1]), Err(TokenStoreError::Unavailable));
}

#[test]
fn a_full_store_evicts_others_before_reregistered_batch_entries() {
    let store = TokenStore::default();
    let live = store
        .register_batch(&[pair("http://up/live.ts", ".ts")], PlaybackContext::Live)
        .unwrap();
    let filler: Vec<_> = (1..CAPACITY)
        .map(|index| pair(&format!("http://up/{index}.ts"), ".ts"))
        .collect();
    store
        .register_batch(&filler, PlaybackContext::Live)
        .unwrap();
    let again = store
        .register_batch(
            &[
                pair("http://up/live.ts", ".ts"),
                pair("http://up/new.ts", ".ts"),
            ],
            PlaybackContext::Live,
        )
        .unwrap();
    assert_eq!(again[0], live[0]);
    assert_eq!(store.entries.lock().unwrap().by_token.len(), CAPACITY);
}

#[test]
fn registered_tokens_resolve() {
    let store = TokenStore::default();
    let tokens = store
        .register_batch(
            &[pair("http://upstream.test/segment.ts", ".ts")],
            PlaybackContext::Live,
        )
        .unwrap();
    assert_eq!(&tokens[0][24..], ".ts");
    assert_eq!(
        store.resolve(&tokens[0]).unwrap().url,
        "http://upstream.test/segment.ts"
    );
    assert_eq!(store.resolve("unknown"), Err(TokenStoreError::Unavailable));
}

#[test]
fn batch_registration_is_atomic_when_one_url_is_invalid() {
    let store = TokenStore::default();
    let existing = store
        .register_batch(
            &[pair("http://upstream.test/existing.ts", ".ts")],
            PlaybackContext::Live,
        )
        .unwrap();
    let invalid = "x".repeat(URL_MAX_BYTES + 1);
    assert_eq!(
        store.register_batch(
            &[
                pair("http://upstream.test/new.ts", ".ts"),
                pair(&invalid, ".ts")
            ],
            PlaybackContext::Live
        ),
        Err(TokenStoreError::Invalid)
    );
    assert_eq!(store.entries.lock().unwrap().by_token.len(), 1);
    assert!(store.resolve(&existing[0]).is_ok());
}

#[test]
fn live_urls_keep_their_token_and_suffix_variants_do_not_collide() {
    let store = TokenStore::default();
    let first = store
        .register_batch(
            &[pair("http://upstream.test/a", ".ts")],
            PlaybackContext::Live,
        )
        .unwrap();
    let again = store
        .register_batch(
            &[pair("http://upstream.test/a", ".ts")],
            PlaybackContext::Live,
        )
        .unwrap();
    let bare = store
        .register_batch(&[pair("http://upstream.test/a", "")], PlaybackContext::Live)
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
    let old = store.register_batch(&old, PlaybackContext::Live).unwrap();
    store.resolve(&old[0]).unwrap();
    let new = store
        .register_batch(
            &[pair("http://upstream.test/new", ".ts")],
            PlaybackContext::Live,
        )
        .unwrap();
    let entries = store.entries.lock().unwrap();
    assert_eq!(entries.by_token.len(), CAPACITY);
    assert_eq!(entries.by_url.len(), CAPACITY);
    assert!(entries.by_token.contains_key(&old[0]));
    assert!(entries.by_token.contains_key(&new[0]));
}
