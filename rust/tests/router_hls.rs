mod support;

use axum::{body::to_bytes, http::StatusCode};
use support::{fixture, get, get_with_headers};

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
async fn hls_forwards_selected_headers_and_passthroughs_partial_content() {
    let fixture = fixture().await;
    let playlist = get(&fixture.app, &fixture.host, "/stream/channel.m3u8").await;
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
    let segment = get_with_headers(
        &fixture.app,
        &fixture.host,
        token,
        &[
            ("accept", "video/mp2t"),
            ("range", "bytes=0-12"),
            ("user-agent", "router-test"),
        ],
    )
    .await;
    assert_eq!(segment.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(segment.headers()["content-range"], "bytes 0-12/13");
    assert_eq!(segment.headers()["accept-ranges"], "bytes");
    assert_eq!(segment.headers()["content-length"], "13");
    let request = fixture
        .requests
        .lock()
        .unwrap()
        .iter()
        .find(|request| request.path == "/segment.ts")
        .cloned()
        .unwrap();
    assert_eq!(request.method, "GET");
    assert_eq!(request.accept.as_deref(), Some("video/mp2t"));
    assert_eq!(request.range.as_deref(), Some("bytes=0-12"));
    assert_eq!(request.user_agent.as_deref(), Some("router-test"));
    fixture.stop();
}

#[tokio::test]
async fn hls_detects_playlist_content_type_without_m3u8_path() {
    let fixture = fixture().await;
    let response = get(&fixture.app, &fixture.host, "/stream/content-type.m3u8").await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), 1 << 20).await.unwrap();
    assert!(String::from_utf8_lossy(&body).contains("/_hls/"));
    assert!(
        fixture
            .requests
            .lock()
            .unwrap()
            .iter()
            .any(|request| request.path == "/playlist")
    );
    fixture.stop();
}

#[tokio::test]
async fn hls_rejects_declared_playlist_over_limit_before_reading() {
    let fixture = fixture().await;
    let response = get(&fixture.app, &fixture.host, "/stream/large.m3u8").await;
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    assert_eq!(
        to_bytes(response.into_body(), 1 << 20)
            .await
            .unwrap()
            .as_ref(),
        br#"{"error":"hls-proxy: playlist exceeds size limit"}"#
    );
    fixture.stop();
}
