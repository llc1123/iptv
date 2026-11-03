#![expect(
    clippy::expect_used,
    reason = "integration fixture construction uses fixed valid values"
)]
#![expect(missing_docs, reason = "integration harness is test-only")]

mod support;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use axum::body::{Body, to_bytes};
use axum::http::header::{CONTENT_TYPE, HOST};
use axum::http::{Request, StatusCode};
use futures_util::stream;
use iptv_media_server::epg::{EpgClient, EpgConfig, Programme};
use iptv_media_server::guide::GuideZone;
use iptv_media_server::health::HealthState;
use iptv_media_server::hls::{Clock, TokenFactory, TokenStore};
use iptv_media_server::http::router::build_router;
use iptv_media_server::http::{
    AppState, AppStateParts, EpochClock, ForwardRequest, ForwardResponse, HlsForwarder, HlsFuture,
    HlsTarget, PlaybackError, PlaybackFuture, PlaybackResolver,
};
use iptv_media_server::lifecycle::{CancellationToken, Lifecycle};
use iptv_media_server::refresh::{EpgMetadataSource, RefreshEngine, RefreshEngineConfig};
use iptv_media_server::render::{BaseUrl, RenderKind};
use iptv_media_server::snapshot::{LogoView, Snapshot, SnapshotStore};
use reqwest::Url;
use ring::digest::{SHA256, digest};
use serde::Deserialize;
use tower::ServiceExt;

use support::mock_provider::MockProvider;

struct FixedEpoch;
impl EpochClock for FixedEpoch {
    fn now(&self) -> u64 {
        1_700_000_000
    }
}

#[derive(Deserialize)]
struct FixtureManifest {
    fixtures: Vec<FixtureRecord>,
}

#[derive(Deserialize)]
struct FixtureRecord {
    name: String,
    sha256: String,
    size: usize,
}

struct FixedTokenClock;
impl Clock for FixedTokenClock {
    fn now(&self) -> Duration {
        Duration::from_secs(42)
    }
}

struct FixedTokenFactory;
impl TokenFactory for FixedTokenFactory {
    fn token(&self) -> String {
        "fixture-token-0000000000".to_owned()
    }
}

struct Playback;
impl PlaybackResolver for Playback {
    fn resolve_live<'a>(&'a self, _channel_id: &'a str) -> PlaybackFuture<'a> {
        Box::pin(async {
            Url::parse("http://media.example.test/live.m3u8")
                .map_err(|_| PlaybackError::Unavailable)
        })
    }

    fn resolve_replay<'a>(
        &'a self,
        _programme: &'a Programme,
        _cancellation: CancellationToken,
    ) -> PlaybackFuture<'a> {
        Box::pin(async {
            Url::parse("http://media.example.test/replay.m3u8")
                .map_err(|_| PlaybackError::Unavailable)
        })
    }
}

struct Forwarder {
    calls: AtomicUsize,
}
impl HlsForwarder for Forwarder {
    fn forward(&self, request: ForwardRequest) -> HlsFuture<'_> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let body = match request.target {
            HlsTarget::Url(_) => b"#EXTM3U\nsegment.ts\n".to_vec(),
            HlsTarget::Token(_) => b"segment".to_vec(),
        };
        Box::pin(async move {
            let mut headers = axum::http::HeaderMap::new();
            headers.insert(
                CONTENT_TYPE,
                "application/vnd.apple.mpegurl"
                    .parse()
                    .map_err(|_| iptv_media_server::http::ProxyError::Unavailable)?,
            );
            Ok(ForwardResponse {
                status: StatusCode::OK,
                headers,
                body: Body::from_stream(stream::once(async move {
                    Ok::<_, std::convert::Infallible>(body)
                })),
            })
        })
    }
}

fn client(origin: &str) -> Result<EpgClient, Box<dyn std::error::Error>> {
    Ok(EpgClient::new(EpgConfig {
        authentication_url: format!("{origin}/EDS/jsp/AuthenticationURL"),
        user_id: "fixture-user".to_owned(),
        password: "fixture-password".to_owned(),
        stb_id: "fixture-stb".to_owned(),
        stb_mac: "02:00:00:00:00:01".to_owned(),
        source_ip: std::net::Ipv4Addr::LOCALHOST.into(),
        stb_type: "fixture-type".to_owned(),
        area_id: "fixture-area".to_owned(),
        user_group_id: "fixture-group".to_owned(),
        nonce: 7,
        write_timeout: Duration::from_secs(1),
        pool_timeout: Duration::from_secs(1),
        media_pool_size: 2,
    })?)
}

fn state() -> Result<(AppState, Arc<TokenStore>), Box<dyn std::error::Error>> {
    let channel = iptv_media_server::epg::models::BootChannel {
        channel_id: "c1".to_owned(),
        name: "One".to_owned(),
        mix_no: "1".to_owned(),
        channel_url: String::new(),
        time_shift_url: String::new(),
        channel_sdp: String::new(),
        time_shift: String::new(),
        time_shift_length: String::new(),
        logo_url: String::new(),
        code: "code-1".to_owned(),
        group_title: "全部".to_owned(),
    };
    let programme = Programme {
        title: "One programme".to_owned(),
        start_time_ms: 1_000_000,
        end_time_ms: 2_000_000,
        programme_id: "p1".to_owned(),
        channel_id: "c1".to_owned(),
    };
    let snapshot = Snapshot::new(
        1,
        vec![channel.clone()],
        vec![programme],
        LogoView::from_channels(&[channel]),
        GuideZone::Fixed { offset_minutes: 0 },
        1,
    )?;
    let snapshots = SnapshotStore::new();
    snapshots.publish(snapshot)?;
    let tokens = Arc::new(TokenStore::new(
        Arc::new(FixedTokenClock),
        Arc::new(FixedTokenFactory),
    ));
    Ok((
        AppState::from_parts(AppStateParts {
            lifecycle: Lifecycle::new(),
            snapshots,
            health: HealthState::new(),
            playback: Arc::new(Playback),
            hls: Arc::new(Forwarder {
                calls: AtomicUsize::new(0),
            }),
            tokens: Arc::clone(&tokens),
            logos: None,
            clock: Arc::new(FixedEpoch),
        }),
        tokens,
    ))
}

async fn request(
    app: &AppState,
    method: &str,
    uri: &str,
) -> Result<axum::response::Response, Box<dyn std::error::Error>> {
    Ok(build_router(app.clone())
        .oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .header(HOST, "fixture.example")
                .body(Body::empty())?,
        )
        .await?)
}

#[tokio::test]
async fn epg_client_auth_bootstrap_programme_tvod_and_forced_reauth_are_wire_bounded()
-> Result<(), Box<dyn std::error::Error>> {
    let provider = MockProvider::start().await?;
    let epg = client(provider.origin())?;
    let session = epg.authenticate().await?;
    let channels = epg.fetch_channels(&session).await?;
    assert_eq!(channels[0].channel_id, "c1");
    let programmes = epg
        .fetch_programmes(&session, "c1", "code-1", 1_000_000, 2_000_000)
        .await?;
    assert_eq!(programmes[0].programme_id, "p1");
    let playback = epg
        .tvod_url(&programmes[0], Lifecycle::new().cancellation_token())
        .await?;
    assert_eq!(playback.scheme(), "http");
    let events = provider.events().await;
    assert!(events.iter().all(|event| !event.route.contains("heartbeat")
        && !event.route.contains("keepalive")
        && !event.route.contains("/log/epgplay")));
    let validation = events
        .iter()
        .find(|event| event.route.ends_with("ValidAuthenticationHWCU.jsp"))
        .ok_or("missing validation")?;
    assert_eq!(
        validation.form_keys.first().map(String::as_str),
        Some("UserID")
    );
    assert_eq!(validation.form_keys.last().map(String::as_str), Some("VIP"));
    let tvod = events
        .iter()
        .find(|event| {
            event.form_keys.first().map(String::as_str) == Some("action")
                && event.form_keys.get(1).map(String::as_str) == Some("channelID")
        })
        .ok_or("missing tvod")?;
    assert_eq!(
        tvod.form_keys,
        vec!["action", "channelID", "playbillID", "startTime", "endTime"]
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| event.route.ends_with("ValidAuthenticationHWCU.jsp"))
            .count(),
        2
    );
    assert_eq!(
        events
            .iter()
            .filter(
                |event| event.form_keys.first().map(String::as_str) == Some("action")
                    && event.form_keys.get(1).map(String::as_str) == Some("channelID")
            )
            .count(),
        2
    );
    let evidence = provider.redacted_events().await;
    assert!(
        evidence
            .iter()
            .all(|event| !event.route.contains("://") && !event.route.contains("fixture-user"))
    );
    provider.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn refresh_engine_publishes_snapshot_health_and_feed_from_mock_provider()
-> Result<(), Box<dyn std::error::Error>> {
    let provider = MockProvider::start().await?;
    let source = Arc::new(EpgMetadataSource::new(client(provider.origin())?));
    let snapshots = SnapshotStore::new();
    let health = HealthState::new();
    let cancellation = iptv_media_server::lifecycle::CancellationSource::new();
    let mut engine = RefreshEngine::new(RefreshEngineConfig {
        source,
        store: snapshots.clone(),
        cache: None,
        cache_fingerprint: None,
        health: health.clone(),
        interval: Duration::from_secs(60),
        windows: vec![iptv_media_server::guide::GuideWindow {
            start_ms: 1_000_000,
            end_ms: 2_000_000,
        }],
        guide_zone: GuideZone::Fixed { offset_minutes: 0 },
        catchup_days: 1,
        cancellation: cancellation.token(),
        clock: None,
        sleeper: None,
    });
    let outcome = engine
        .refresh_once()
        .await
        .map_err(|error| format!("refresh failed: {error:?}"))?;
    assert_eq!(outcome.generation, 1);
    assert!(health.snapshot().await.ready);
    let published = snapshots.load()?.ok_or("missing refresh snapshot")?;
    assert_eq!(published.channels()[0].channel_id, "c1");
    let app = AppState::from_parts(AppStateParts {
        lifecycle: Lifecycle::new(),
        snapshots: snapshots.clone(),
        health,
        playback: Arc::new(Playback),
        hls: Arc::new(Forwarder {
            calls: AtomicUsize::new(0),
        }),
        tokens: Arc::new(TokenStore::new(
            Arc::new(FixedTokenClock),
            Arc::new(FixedTokenFactory),
        )),
        logos: None,
        clock: Arc::new(FixedEpoch),
    });
    let base = BaseUrl::parse("http://fixture.example")?;
    let expected_m3u = published
        .render_cached(
            RenderKind::M3u,
            &base,
            Some(&app.lifecycle().cancellation_token()),
        )
        .await?;
    let expected_xmltv = published
        .render_cached(
            RenderKind::Xmltv,
            &base,
            Some(&app.lifecycle().cancellation_token()),
        )
        .await?;
    let m3u = to_bytes(
        request(&app, "GET", "/channels.m3u").await?.into_body(),
        1024 * 1024,
    )
    .await?;
    let xmltv = to_bytes(
        request(&app, "GET", "/xmltv.xml").await?.into_body(),
        1024 * 1024,
    )
    .await?;
    assert_eq!(m3u.as_ref(), expected_m3u.as_ref());
    assert_eq!(xmltv.as_ref(), expected_xmltv.as_ref());
    assert!(m3u.windows(b"c1".len()).any(|window| window == b"c1"));
    assert!(xmltv.windows(b"c1".len()).any(|window| window == b"c1"));
    provider.shutdown().await?;
    Ok(())
}

#[test]
fn integration_fixture_manifest_matches_checked_in_bytes() -> Result<(), Box<dyn std::error::Error>>
{
    let manifest: FixtureManifest =
        serde_json::from_str(include_str!("integration_fixtures/manifest.json"))?;
    let record = manifest.fixtures.first().ok_or("missing fixture record")?;
    let bytes = include_bytes!("integration_fixtures/provider_responses.json");
    assert_eq!(record.name, "provider_responses.json");
    assert_eq!(record.size, bytes.len());
    assert_eq!(record.sha256, hex::encode(digest(&SHA256, bytes).as_ref()));
    Ok(())
}

#[tokio::test]
async fn public_routes_cover_feeds_playback_tokens_head_405_404_and_shutdown()
-> Result<(), Box<dyn std::error::Error>> {
    let (app, tokens) = state()?;
    let health = request(&app, "GET", "/healthz").await?;
    assert_eq!(health.status(), StatusCode::OK);
    let feed = request(&app, "GET", "/channels.m3u").await?;
    assert_eq!(feed.status(), StatusCode::OK);
    assert!(
        to_bytes(feed.into_body(), 1024 * 1024)
            .await?
            .starts_with(b"#EXTM3U")
    );
    let replay = request(&app, "GET", "/replay/c1/p1").await?;
    assert_eq!(replay.status(), StatusCode::OK);
    let catchup = request(&app, "GET", "/catchup/c1?utc=1000").await?;
    assert_eq!(catchup.status(), StatusCode::OK);
    let live = request(&app, "GET", "/stream/c1").await?;
    assert_eq!(live.status(), StatusCode::OK);
    let legacy = request(&app, "GET", "/stream/c1?utc=1000").await?;
    assert_eq!(legacy.status(), StatusCode::OK);
    let registered = tokens
        .register("http://media.example.test/a.m3u8", "")
        .map_err(|error| format!("token registration: {error:?}"))?;
    let token_response = request(&app, "GET", &format!("/_hls/{}", registered.token)).await?;
    assert_eq!(token_response.status(), StatusCode::OK);
    assert_eq!(
        request(&app, "HEAD", "/healthz").await?.status(),
        StatusCode::OK
    );
    assert_eq!(
        request(&app, "POST", "/healthz").await?.status(),
        StatusCode::METHOD_NOT_ALLOWED
    );
    assert_eq!(
        request(&app, "GET", "/missing").await?.status(),
        StatusCode::NOT_FOUND
    );
    app.lifecycle().begin_shutdown();
    assert_eq!(
        request(&app, "GET", "/channels.m3u").await?.status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
    Ok(())
}

#[tokio::test]
async fn mock_provider_shutdown_rejects_new_work_and_joins_listener()
-> Result<(), Box<dyn std::error::Error>> {
    let provider = MockProvider::start().await?;
    let origin = provider.origin().to_owned();
    provider.shutdown().await?;
    let error = reqwest::Client::builder()
        .no_proxy()
        .build()?
        .get(origin)
        .send()
        .await
        .expect_err("stopped provider rejects work");
    assert!(error.is_connect() || error.is_request());
    Ok(())
}
