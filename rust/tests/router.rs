mod support;

use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use iptv_media_server::render::{BaseUrl, RenderKind};
use std::time::Duration;
use support::{fixture, get};
use tower::ServiceExt;

#[tokio::test]
async fn health_and_feeds_match_snapshot_rendering() {
    let fixture = fixture().await;
    let health = get(&fixture.app, &fixture.host, "/healthz").await;
    assert_eq!(health.status(), StatusCode::OK);
    let health_body: serde_json::Value =
        serde_json::from_slice(&to_bytes(health.into_body(), 1 << 20).await.unwrap()).unwrap();
    assert_eq!(health_body["status"], "ok");
    assert_eq!(health_body["channels"], 3);
    for (path, kind, content_type) in [
        (
            "/channels.m3u",
            RenderKind::M3u,
            "audio/x-mpegurl; charset=utf-8",
        ),
        ("/xmltv.xml", RenderKind::Xmltv, "application/xml"),
    ] {
        let response = get(&fixture.app, &fixture.host, path).await;
        assert_eq!(response.headers()["content-type"], content_type);
        assert_eq!(
            response.headers()["cache-control"],
            "no-store, no-cache, max-age=0"
        );
        let expected = fixture
            .snapshot
            .render(
                kind,
                &BaseUrl::parse(&format!("http://{}", fixture.host)).unwrap(),
            )
            .unwrap();
        assert_eq!(
            to_bytes(response.into_body(), 1 << 20)
                .await
                .unwrap()
                .as_ref(),
            expected.as_ref()
        );
    }
    fixture.stop();
}

#[tokio::test]
async fn live_playlist_rewrites_and_streams_segment_bytes() {
    let fixture = fixture().await;
    let playlist = get(&fixture.app, &fixture.host, "/stream/channel.m3u8").await;
    assert_eq!(playlist.status(), StatusCode::OK);
    let body = String::from_utf8(
        to_bytes(playlist.into_body(), 1 << 20)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap();
    let token = body
        .lines()
        .find(|line| line.starts_with("/_hls/"))
        .unwrap();
    let segment = get(&fixture.app, &fixture.host, token).await;
    assert_eq!(segment.status(), StatusCode::OK);
    assert_eq!(segment.headers()["content-type"], "video/mp2t");
    assert_eq!(
        to_bytes(segment.into_body(), 1 << 20)
            .await
            .unwrap()
            .as_ref(),
        b"segment-bytes"
    );
    assert_eq!(
        get(&fixture.app, &fixture.host, "/stream/live.m3u8")
            .await
            .status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
    fixture.stop();
}

#[tokio::test]
async fn replay_and_catchup_accept_seconds_milliseconds_and_encoded_iso() {
    let fixture = fixture().await;
    for uri in [
        "/replay/channel/programme.m3u8",
        "/catchup/channel.m3u8?utc=1700000000",
        "/catchup/channel.m3u8?utc=1700000000000",
        "/catchup/channel.m3u8?utc=2023-11-14T22:13:20%2B00:00",
    ] {
        assert_eq!(
            get(&fixture.app, &fixture.host, uri).await.status(),
            StatusCode::OK,
            "{uri}"
        );
    }
    assert_eq!(
        get(
            &fixture.app,
            &fixture.host,
            "/stream/channel?utc=1700000000"
        )
        .await
        .status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        get(&fixture.app, &fixture.host, "/stream/channel.m3u8?utc=")
            .await
            .status(),
        StatusCode::OK
    );
    fixture.stop();
}

#[tokio::test]
async fn errors_and_metadata_sync_are_reported_at_http_boundary() {
    let fixture = fixture().await;
    let unknown = get(&fixture.app, &fixture.host, "/stream/unknown.m3u8").await;
    assert_eq!(unknown.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        to_bytes(unknown.into_body(), 1 << 20)
            .await
            .unwrap()
            .as_ref(),
        br#"{"error":"service: unknown channel unknown"}"#
    );
    assert_eq!(
        get(&fixture.app, &fixture.host, "/_hls/unknown.ts")
            .await
            .status(),
        StatusCode::BAD_GATEWAY
    );
    let no_replay = get(&fixture.app, &fixture.host, "/stream/live.m3u8").await;
    assert_eq!(no_replay.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        to_bytes(no_replay.into_body(), 1 << 20)
            .await
            .unwrap()
            .as_ref(),
        br#"{"error":"epg-tvod: channel live has no replay"}"#
    );
    let unknown_programme = get(&fixture.app, &fixture.host, "/replay/channel/missing.m3u8").await;
    assert_eq!(unknown_programme.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        to_bytes(unknown_programme.into_body(), 1 << 20)
            .await
            .unwrap()
            .as_ref(),
        br#"{"error":"epg-replay: unknown programme missing"}"#
    );
    assert_eq!(
        get(&fixture.app, &fixture.host, "/stream/bad.m3u8")
            .await
            .status(),
        StatusCode::BAD_GATEWAY
    );
    let sync = Request::builder()
        .method("POST")
        .uri("/metadata/sync")
        .header("host", &fixture.host)
        .body(Body::empty())
        .unwrap();
    let response = tokio::time::timeout(Duration::from_secs(1), fixture.app.clone().oneshot(sync))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let sync_body: serde_json::Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 1 << 20).await.unwrap()).unwrap();
    assert_eq!(sync_body["channels"], 1);
    assert_eq!(sync_body["guide_programmes"], 1);
    assert_eq!(sync_body["logos"], 0);
    assert!(sync_body["catalog"].is_string());
    assert!(sync_body["guide_cache"].is_string());
    fixture.stop();
}
