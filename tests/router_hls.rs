mod support;

use axum::{body::to_bytes, http::StatusCode};
use support::{fixture, get, get_with_headers};

async fn playlist_text(
    fixture: &support::Fixture,
    uri: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    let response = get(&fixture.app, &fixture.host, uri).await;
    assert_eq!(response.status(), StatusCode::OK);
    Ok(String::from_utf8(
        to_bytes(response.into_body(), 1 << 20).await?.to_vec(),
    )?)
}

fn resources(playlist: &str) -> Vec<String> {
    playlist
        .split("/_hls/")
        .skip(1)
        .flat_map(|rest| rest.split(['"', '\n']).take(1))
        .map(|token| format!("/_hls/{token}"))
        .collect()
}

#[tokio::test]
async fn replay_entrypoints_share_nested_resource_lifetime_but_isolate_playbacks() {
    for entrypoint in [
        "/replay/channel/programme.m3u8",
        "/catchup/channel.m3u8?utc=1700000000",
        "/stream/channel.m3u8?utc=1700000000",
    ] {
        let fixture = fixture().await;
        let root = playlist_text(&fixture, entrypoint).await.unwrap();
        assert!(!root.contains("#EXT-X-ENDLIST"));
        let master = resources(&root);
        assert_eq!(master.len(), 2);
        let media = playlist_text(&fixture, &master[1]).await.unwrap();
        assert!(media.contains("#EXT-X-ENDLIST"));
        let segments = resources(&media);
        assert_eq!(segments.len(), 5);
        let second = resources(&playlist_text(&fixture, entrypoint).await.unwrap());
        let second_media = resources(&playlist_text(&fixture, &second[1]).await.unwrap());
        assert_ne!(master, second);
        assert_ne!(segments, second_media);

        for (uri, expected) in [
            (&master[0], "root-key"),
            (&segments[1], "init-bytes"),
            (&segments[0], "media-key"),
            (&segments[2], "first-bytes"),
            (&segments[4], "last-bytes"),
        ] {
            tokio::time::pause();
            tokio::time::advance(std::time::Duration::from_secs(240)).await;
            tokio::time::resume();
            let response = get(&fixture.app, &fixture.host, uri).await;
            assert_eq!(
                response.status(),
                StatusCode::OK,
                "{entrypoint}: {expected}"
            );
            assert_eq!(
                to_bytes(response.into_body(), 1 << 20)
                    .await
                    .unwrap()
                    .as_ref(),
                expected.as_bytes()
            );
        }

        assert_eq!(playlist_text(&fixture, &master[1]).await.unwrap(), media);
        for uri in [&second[1], &second_media[4]] {
            assert_eq!(
                get(&fixture.app, &fixture.host, uri).await.status(),
                StatusCode::BAD_GATEWAY
            );
        }
        tokio::time::pause();
        tokio::time::advance(std::time::Duration::from_secs(300)).await;
        tokio::time::resume();
        for uri in [&master[1], &segments[3]] {
            assert_eq!(
                get(&fixture.app, &fixture.host, uri).await.status(),
                StatusCode::BAD_GATEWAY
            );
        }
        fixture.stop();
    }
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
    let repeated = playlist_text(&fixture, "/stream/channel.m3u8")
        .await
        .unwrap();
    assert_eq!(resources(&repeated), vec![token]);
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
