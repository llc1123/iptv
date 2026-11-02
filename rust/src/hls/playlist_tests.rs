#![expect(
    clippy::expect_used,
    reason = "test fixtures and deterministic test doubles have asserted invariants"
)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use reqwest::Url;
use serde::Deserialize;

use crate::lifecycle::CancellationSource;

use super::{
    PLAYLIST_MAX_BYTES, PLAYLIST_MAX_LINE_BYTES, PLAYLIST_MAX_URI_REFERENCES, rewrite_playlist,
};
use crate::hls::tokens::{Clock, Resource, TokenFactory, TokenStore};

#[derive(Debug)]
struct FixedClock;

impl Clock for FixedClock {
    fn now(&self) -> Duration {
        Duration::ZERO
    }
}

#[derive(Debug)]
struct CountingFactory {
    next: Mutex<usize>,
}

impl CountingFactory {
    fn new() -> Self {
        Self {
            next: Mutex::new(1),
        }
    }
}

impl TokenFactory for CountingFactory {
    fn token(&self) -> String {
        let mut next = self.next.lock().expect("test factory lock");
        let token = format!("fixturetok{:014}", *next);
        *next += 1;
        token
    }
}

#[derive(Debug)]
struct ConstantFactory;

impl TokenFactory for ConstantFactory {
    fn token(&self) -> String {
        "fixturetok00000000000001".to_owned()
    }
}

fn store(factory: Arc<dyn TokenFactory>) -> TokenStore {
    TokenStore::new(Arc::new(FixedClock), factory)
}

fn url(value: &str) -> Url {
    Url::parse(value).expect("valid test URL")
}

#[derive(Debug, Deserialize)]
struct Fixture {
    cases: Vec<Case>,
}

#[derive(Debug, Deserialize)]
struct Case {
    input: Input,
    expected: Expected,
}

#[derive(Debug, Deserialize)]
struct Input {
    base_url: Option<String>,
    body: Option<String>,
    kind: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Expected {
    output: Option<String>,
    resources: Option<Vec<ResourceFixture>>,
}

#[derive(Debug, Deserialize)]
struct ResourceFixture {
    token: String,
    url: String,
}

#[test]
fn fixture_rewrites_every_checked_in_hls_playlist_case() {
    let fixture: Fixture =
        serde_json::from_str(include_str!("../../tests/fixtures/hls_rewrite.json"))
            .expect("checked-in fixture JSON");
    for case in fixture
        .cases
        .into_iter()
        .filter(|case| case.input.kind.as_deref() == Some("rewrite"))
    {
        let cancellation = CancellationSource::new();
        let store = store(Arc::new(CountingFactory::new()));
        let result = rewrite_playlist(
            case.input.body.as_ref().expect("rewrite body").as_bytes(),
            &url(case.input.base_url.as_ref().expect("rewrite base URL")),
            &cancellation.token(),
            &store,
        )
        .expect("fixture rewrite succeeds");
        assert_eq!(
            result.body,
            case.expected.output.expect("rewrite output").into_bytes()
        );
        let expected_resources = case
            .expected
            .resources
            .expect("rewrite resources")
            .into_iter()
            .map(|resource| Resource {
                token: resource.token,
                url: resource.url,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            result.resources, expected_resources,
            "fixture body: {:?}",
            case.input.body
        );
    }
}

#[test]
fn master_media_and_ll_hls_attributes_preserve_non_uri_bytes() {
    let body = br#"#EXTM3U
#EXT-X-STREAM-INF:BANDWIDTH=1,URI="video/main.m3u8"
  video/main.m3u8
#EXT-X-PART:DURATION=0.5,URI="part.bin",SERVER-URI="server.mp4"
#EXT-X-PRELOAD-HINT:TYPE=PART,URI="next.bin"
#EXT-X-RENDITION-REPORT:URI="../rendition.m3u8",LAST-MSN=1
"#;
    let cancellation = CancellationSource::new();
    let store = store(Arc::new(CountingFactory::new()));
    let result = rewrite_playlist(
        body,
        &url("https://origin.test/live/master.m3u8"),
        &cancellation.token(),
        &store,
    )
    .expect("playlist rewrites");
    let output = String::from_utf8(result.body).expect("UTF-8 output");
    assert!(output.contains("#EXT-X-STREAM-INF:BANDWIDTH=1,URI=\"/_hls/"));
    assert!(output.contains("  /_hls/fixturetok00000000000002.m3u8\n"));
    assert!(output.contains("#EXT-X-PART:DURATION=0.5,URI=\"/_hls/"));
    assert_eq!(result.resources.len(), 6);
    assert_eq!(
        result.resources[0].url,
        "https://origin.test/live/video/main.m3u8"
    );
    assert_eq!(
        result.resources[1].url,
        "https://origin.test/live/video/main.m3u8"
    );
    assert_eq!(result.resources[2].url, "https://origin.test/live/part.bin");
    assert_eq!(
        result.resources[3].url,
        "https://origin.test/live/server.mp4"
    );
}

#[test]
fn query_fragment_and_final_redirect_base_preserve_url_and_path_suffix() {
    let body = b"  segment.ts?token=a#part  \n?playlist=1#frag\n";
    let cancellation = CancellationSource::new();
    let store = store(Arc::new(CountingFactory::new()));
    let result = rewrite_playlist(
        body,
        &url("https://cdn.test/final/path/index.m3u8?old=1"),
        &cancellation.token(),
        &store,
    )
    .expect("playlist rewrites");
    assert_eq!(
        result.resources[0].url,
        "https://cdn.test/final/path/segment.ts?token=a#part"
    );
    assert_eq!(
        result.resources[1].url,
        "https://cdn.test/final/path/index.m3u8?playlist=1#frag"
    );
    assert_eq!(result.resources[0].token, "fixturetok00000000000001.ts");
    assert_eq!(result.resources[1].token, "fixturetok00000000000002.m3u8");
    assert_eq!(
        result.body,
        b"  /_hls/fixturetok00000000000001.ts  \n/_hls/fixturetok00000000000002.m3u8\n"
    );
}

#[test]
fn malformed_utf8_attributes_and_limits_register_nothing() {
    let cancellation = CancellationSource::new();
    let store = store(Arc::new(CountingFactory::new()));
    assert!(
        rewrite_playlist(
            &[0xff],
            &url("https://origin.test/a.m3u8"),
            &cancellation.token(),
            &store
        )
        .is_err()
    );
    assert_eq!(store.len(), 0);
    assert!(
        rewrite_playlist(
            b"#EXT-X-KEY:URI=\"key.bin\n",
            &url("https://origin.test/a.m3u8"),
            &cancellation.token(),
            &store
        )
        .is_err()
    );
    assert_eq!(store.len(), 0);
    let too_many = (0..=PLAYLIST_MAX_URI_REFERENCES)
        .map(|_| "x\n")
        .collect::<String>();
    assert!(
        rewrite_playlist(
            too_many.as_bytes(),
            &url("https://origin.test/a.m3u8"),
            &cancellation.token(),
            &store
        )
        .is_err()
    );
    assert_eq!(store.len(), 0);
    let large_repeated_uri_lines = (0..PLAYLIST_MAX_BYTES / 2)
        .map(|_| "x\n")
        .collect::<String>();
    assert!(
        rewrite_playlist(
            large_repeated_uri_lines.as_bytes(),
            &url("https://origin.test/a.m3u8"),
            &cancellation.token(),
            &store
        )
        .is_err()
    );
    assert_eq!(store.len(), 0);
    let too_long = vec![b'x'; PLAYLIST_MAX_LINE_BYTES + 1];
    assert!(
        rewrite_playlist(
            &too_long,
            &url("https://origin.test/a.m3u8"),
            &cancellation.token(),
            &store
        )
        .is_err()
    );
    assert_eq!(store.len(), 0);
    let too_large = vec![b'x'; PLAYLIST_MAX_BYTES];
    assert!(
        rewrite_playlist(
            &too_large,
            &url("https://origin.test/a.m3u8"),
            &cancellation.token(),
            &store
        )
        .is_err()
    );
    assert_eq!(store.len(), 0);
}

#[test]
fn cancellation_before_validation_and_token_collision_commit_nothing() {
    let source = CancellationSource::new();
    source.cancel();
    let cancelled_store = store(Arc::new(CountingFactory::new()));
    assert_eq!(
        rewrite_playlist(
            b"segment.ts\n",
            &url("https://origin.test/a.m3u8"),
            &source.token(),
            &cancelled_store,
        ),
        Err(crate::error::HlsError::Cancelled)
    );
    assert_eq!(cancelled_store.len(), 0);

    let collision_store = store(Arc::new(ConstantFactory));
    let active = CancellationSource::new();
    assert!(
        rewrite_playlist(
            b"one.ts\ntwo.ts\n",
            &url("https://origin.test/a.m3u8"),
            &active.token(),
            &collision_store,
        )
        .is_err()
    );
    assert_eq!(collision_store.len(), 0);
}
