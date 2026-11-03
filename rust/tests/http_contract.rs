//! Deterministic route and response contract tests.
#![expect(
    clippy::expect_used,
    reason = "contract fixtures use fixed valid values"
)]

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use axum::body::{Body, to_bytes};
use axum::http::header::{
    ACCEPT_RANGES, CACHE_CONTROL, CONTENT_ENCODING, CONTENT_RANGE, CONTENT_TYPE, HOST,
};
use axum::http::{Request, StatusCode};
use futures_util::stream;
use iptv_media_server::epg::models::{BootChannel, Programme};
use iptv_media_server::guide::GuideZone;
use iptv_media_server::health::HealthState;
use iptv_media_server::hls::TokenStore;
use iptv_media_server::http::router::build_router;
use iptv_media_server::http::{
    AppState, AppStateParts, EpochClock, ForwardRequest, ForwardResponse, HlsForwarder, HlsFuture,
    HlsTarget, PlaybackError, PlaybackFuture, PlaybackResolver,
};
use iptv_media_server::lifecycle::Lifecycle;
use iptv_media_server::logo_catalog::LogoCatalog;
use iptv_media_server::snapshot::{LogoView, Snapshot, SnapshotStore};
use reqwest::Url;
use serde_json::Value;
use tower::ServiceExt;

struct FixedClock(u64);

static NEXT_FIXTURE: AtomicUsize = AtomicUsize::new(0);
impl EpochClock for FixedClock {
    fn now(&self) -> u64 {
        self.0
    }
}

struct FakePlayback {
    calls: AtomicUsize,
    replay_calls: AtomicUsize,
}

impl PlaybackResolver for FakePlayback {
    fn resolve_live<'a>(&'a self, _channel_id: &'a str) -> PlaybackFuture<'a> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async {
            Url::parse("http://upstream.invalid/live.m3u8").map_err(|_| PlaybackError::Unavailable)
        })
    }

    fn resolve_replay<'a>(
        &'a self,
        _programme: &'a Programme,
        _cancellation: iptv_media_server::lifecycle::CancellationToken,
    ) -> PlaybackFuture<'a> {
        self.replay_calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async {
            Url::parse("http://upstream.invalid/replay.m3u8")
                .map_err(|_| PlaybackError::Unavailable)
        })
    }
}

struct FakeForwarder {
    calls: AtomicUsize,
}

impl HlsForwarder for FakeForwarder {
    fn forward(&self, request: ForwardRequest) -> HlsFuture<'_> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let (body, status, headers) = match request.target {
            HlsTarget::Url(_) => (
                b"#EXTM3U\nsegment.ts\n".to_vec(),
                StatusCode::OK,
                [(CONTENT_TYPE, "application/vnd.apple.mpegurl")]
                    .into_iter()
                    .map(|(name, value)| (name, value.parse().expect("header")))
                    .collect(),
            ),
            HlsTarget::Token(token) if token == "range.ts" => (
                b"segment".to_vec(),
                StatusCode::PARTIAL_CONTENT,
                [
                    (CONTENT_TYPE, "video/mpeg"),
                    (CONTENT_RANGE, "bytes 0-6/7"),
                    (ACCEPT_RANGES, "bytes"),
                    (CONTENT_ENCODING, "identity"),
                ]
                .into_iter()
                .map(|(name, value)| (name, value.parse().expect("header")))
                .collect(),
            ),
            HlsTarget::Token(_) => (
                b"segment".to_vec(),
                StatusCode::OK,
                [(CONTENT_TYPE, "video/mp2t")]
                    .into_iter()
                    .map(|(name, value)| (name, value.parse().expect("header")))
                    .collect(),
            ),
        };
        let admission = request.admission;
        Box::pin(async move {
            Ok(ForwardResponse {
                status,
                headers,
                body: Body::from_stream(stream::once(async move {
                    drop(admission);
                    Ok::<Vec<u8>, std::convert::Infallible>(body)
                })),
            })
        })
    }
}

struct Fixture {
    root: PathBuf,
    state: AppState,
    playback: Arc<FakePlayback>,
    forwarder: Arc<FakeForwarder>,
}

impl Fixture {
    fn new(with_logo: bool) -> Self {
        let channel = BootChannel {
            channel_id: "c1".to_owned(),
            name: "One".to_owned(),
            mix_no: String::new(),
            channel_url: String::new(),
            time_shift_url: String::new(),
            channel_sdp: String::new(),
            time_shift: String::new(),
            time_shift_length: String::new(),
            logo_url: String::new(),
            code: String::new(),
            group_title: String::new(),
        };
        let snapshot = Snapshot::new(
            1,
            vec![channel.clone()],
            vec![Programme {
                title: "One programme".to_owned(),
                start_time_ms: 0,
                end_time_ms: 10_000,
                programme_id: "p/1".to_owned(),
                channel_id: "c1".to_owned(),
            }],
            LogoView::from_channels(&[channel]),
            GuideZone::Fixed { offset_minutes: 0 },
            0,
        )
        .expect("snapshot");
        let snapshots = SnapshotStore::new();
        snapshots.publish(snapshot).expect("publish");

        let id = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
        let root =
            std::env::temp_dir().join(format!("iptv-http-contract-{}-{id}", std::process::id()));
        fs::create_dir_all(root.join("logos")).expect("logo root");
        let logos = if with_logo {
            fs::write(root.join("logos/one.png"), b"png").expect("logo");
            let catalog = root.join("catalog.json");
            fs::write(
                &catalog,
                r#"{"version":1,"channels":[{"channel_id":"c1","stream_url":"s","display_name":"One","logo_path":"logos/one.png"}]}"#,
            )
            .expect("catalog");
            Some(Arc::new(LogoCatalog::load(catalog).expect("catalog load")))
        } else {
            None
        };
        let playback = Arc::new(FakePlayback {
            calls: AtomicUsize::new(0),
            replay_calls: AtomicUsize::new(0),
        });
        let forwarder = Arc::new(FakeForwarder {
            calls: AtomicUsize::new(0),
        });
        let state = AppState::from_parts(AppStateParts {
            lifecycle: Lifecycle::new(),
            snapshots,
            health: HealthState::new(),
            playback: playback.clone(),
            hls: forwarder.clone(),
            tokens: Arc::new(TokenStore::default()),
            logos,
            clock: Arc::new(FixedClock(1_700_000_000)),
        });
        Self {
            root,
            state,
            playback,
            forwarder,
        }
    }

    async fn request(&self, method: &str, uri: &str) -> axum::response::Response {
        let request = Request::builder()
            .method(method)
            .uri(uri)
            .header(HOST, "direct.example")
            .body(Body::empty())
            .expect("request");
        build_router(self.state.clone())
            .oneshot(request)
            .await
            .expect("response")
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).ok();
    }
}

#[tokio::test]
async fn fixture_declares_only_scoped_routes() {
    let contract: Value =
        serde_json::from_str(include_str!("integration_fixtures/http_contract.json"))
            .expect("contract json");
    let routes = contract["routes"].as_array().expect("routes");
    let paths = routes
        .iter()
        .map(|route| route["path"].as_str().expect("path"))
        .collect::<Vec<_>>();
    assert_eq!(
        paths,
        vec![
            "/healthz",
            "/channels.m3u",
            "/xmltv.xml",
            "/logos/{channel_id}",
            "/stream/{channel_id}",
            "/replay/{channel_id}/{programme_id}",
            "/catchup/{channel_id}",
            "/_hls/{*rest}"
        ]
    );
    assert!(paths.iter().any(|path| path.contains("replay")));
    assert!(paths.iter().any(|path| path.contains("catchup")));
}

#[tokio::test]
async fn health_feed_and_direct_host_contracts_are_fixed() {
    let fixture = Fixture::new(false);
    let health = fixture.request("GET", "/healthz").await;
    assert_eq!(health.status(), StatusCode::OK);
    let health_body = to_bytes(health.into_body(), 4096)
        .await
        .expect("health body");
    assert!(String::from_utf8_lossy(&health_body).contains("\"status\":\"starting\""));

    let feed = fixture.request("GET", "/channels.m3u").await;
    assert_eq!(feed.status(), StatusCode::OK);
    assert_eq!(feed.headers()[CONTENT_TYPE], "audio/x-mpegurl");
    assert_eq!(
        feed.headers()[CACHE_CONTROL],
        "no-store, no-cache, max-age=0"
    );
    let body = to_bytes(feed.into_body(), 16 * 1024 * 1024)
        .await
        .expect("feed body");
    assert!(body.starts_with(b"#EXTM3U "));
    assert_eq!(fixture.playback.calls.load(Ordering::SeqCst), 0);

    let request = Request::builder()
        .method("GET")
        .uri("/channels.m3u")
        .header(HOST, "direct.example")
        .header("Forwarded", "host=evil.example")
        .body(Body::empty())
        .expect("request");
    let response = build_router(fixture.state.clone())
        .oneshot(request)
        .await
        .expect("response");
    let body = to_bytes(response.into_body(), 16 * 1024 * 1024)
        .await
        .expect("body");
    assert!(!String::from_utf8_lossy(&body).contains("evil.example"));
}

#[tokio::test]
async fn logo_live_head_and_method_contracts_are_fixed() {
    let fixture = Fixture::new(true);
    let logo = fixture.request("GET", "/logos/c1").await;
    assert_eq!(logo.status(), StatusCode::OK);
    assert_eq!(logo.headers()[CONTENT_TYPE], "image/png");
    assert_eq!(
        to_bytes(logo.into_body(), 1024).await.expect("logo body"),
        &b"png"[..]
    );

    let range_request = Request::builder()
        .method("GET")
        .uri("/logos/c1")
        .header(HOST, "direct.example")
        .header("Range", "bytes=0-1")
        .body(Body::empty())
        .expect("range request");
    let range = build_router(fixture.state.clone())
        .oneshot(range_request)
        .await
        .expect("range response");
    assert_eq!(range.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(
        to_bytes(range.into_body(), 1024).await.expect("range body"),
        &b"pn"[..]
    );

    let live = fixture.request("GET", "/stream/c1.m3u8").await;
    assert_eq!(live.status(), StatusCode::OK);
    assert_eq!(fixture.playback.calls.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.forwarder.calls.load(Ordering::SeqCst), 1);

    let head = fixture.request("HEAD", "/channels.m3u").await;
    assert_eq!(head.status(), StatusCode::OK);
    assert!(
        to_bytes(head.into_body(), 1024)
            .await
            .expect("head body")
            .is_empty()
    );

    let wrong_method = fixture.request("POST", "/channels.m3u").await;
    assert_eq!(wrong_method.status(), StatusCode::METHOD_NOT_ALLOWED);
    assert!(wrong_method.headers().contains_key("allow"));
    assert_eq!(
        fixture.request("GET", "/missing").await.status(),
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn replay_and_catchup_use_one_snapshot_and_forward_only_valid_matches() {
    let fixture = Fixture::new(false);

    let replay = fixture.request("GET", "/replay/c1/p%2F1.m3u8").await;
    assert_eq!(replay.status(), StatusCode::OK);
    assert_eq!(fixture.playback.replay_calls.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.forwarder.calls.load(Ordering::SeqCst), 1);

    let fallback = fixture.request("GET", "/catchup/c1?utc=&start=1").await;
    assert_eq!(fallback.status(), StatusCode::OK);
    assert_eq!(fixture.playback.replay_calls.load(Ordering::SeqCst), 2);

    let end = fixture.request("GET", "/catchup/c1?utc=10").await;
    assert_eq!(end.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(fixture.playback.replay_calls.load(Ordering::SeqCst), 2);

    let legacy = fixture.request("GET", "/stream/c1.m3u8?utc=1").await;
    assert_eq!(legacy.status(), StatusCode::OK);
    assert_eq!(fixture.playback.replay_calls.load(Ordering::SeqCst), 3);
    assert_eq!(fixture.playback.calls.load(Ordering::SeqCst), 0);

    let unknown = fixture.request("GET", "/catchup/unknown?utc=1").await;
    assert_eq!(unknown.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(fixture.playback.replay_calls.load(Ordering::SeqCst), 3);

    assert_eq!(
        fixture
            .request("HEAD", "/replay/c1/p%2F1.m3u8")
            .await
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        fixture.request("POST", "/catchup/c1?utc=1").await.status(),
        StatusCode::METHOD_NOT_ALLOWED
    );
}

#[tokio::test]
async fn health_drains_and_admission_rejects_new_streams() {
    let fixture = Fixture::new(false);
    fixture.state.lifecycle().begin_shutdown();
    fixture.state.health().mark_draining().await;
    let health = fixture.request("GET", "/healthz").await;
    assert_eq!(health.status(), StatusCode::SERVICE_UNAVAILABLE);
    let stream = fixture.request("GET", "/stream/c1.m3u8").await;
    assert_eq!(stream.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(fixture.playback.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn hls_token_suffix_is_preserved_by_route() {
    let fixture = Fixture::new(false);
    let response = fixture.request("GET", "/_hls/range.ts").await;
    assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(response.headers()[CONTENT_RANGE], "bytes 0-6/7");
    assert_eq!(response.headers()[ACCEPT_RANGES], "bytes");
    assert_eq!(response.headers()[CONTENT_ENCODING], "identity");
    assert_eq!(
        to_bytes(response.into_body(), 1024)
            .await
            .expect("hls body"),
        &b"segment"[..]
    );
}

#[tokio::test]
async fn streamed_body_retains_request_admission_until_drop() {
    let fixture = Fixture::new(false);
    let response = fixture.request("GET", "/_hls/range.ts").await;
    assert_eq!(fixture.state.lifecycle().active_requests(), 1);
    drop(response);
    assert_eq!(fixture.state.lifecycle().active_requests(), 0);
}

#[test]
fn contract_state_debug_is_redacted() {
    let fixture = Fixture::new(false);
    let debug = format!("{:?}", fixture.state);
    assert!(!debug.contains("upstream.invalid"));
    assert!(!debug.contains("upstream"));
}
