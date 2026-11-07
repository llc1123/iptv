#![expect(
    clippy::unwrap_used,
    reason = "loopback fixture setup uses deterministic test assertions"
)]

use axum::{Router, body::Body};
use clap::Parser;
use iptv_media_server::{
    config::Config,
    epg::{BootChannel, EpgClient, EpgConfig, Programme},
    guide::GuideZone,
    hls::TokenStore,
    http::{AppState, ServiceControl, router},
    refresh,
    snapshot::Snapshot,
};
use std::{net::Ipv4Addr, str::FromStr, sync::Arc, time::Duration};
use tokio::{
    io::AsyncWriteExt,
    net::TcpListener,
    sync::{RwLock, mpsc, watch},
};
use tower::ServiceExt;
mod provider;
use provider::{read_request, response_for};

pub struct Fixture {
    pub app: Router,
    pub snapshot: Arc<Snapshot>,
    pub host: String,
    provider: tokio::task::JoinHandle<()>,
    refresh: tokio::task::JoinHandle<()>,
}
impl Fixture {
    pub fn stop(self) {
        self.provider.abort();
        self.refresh.abort();
    }
}

pub async fn fixture() -> Fixture {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let address = listener.local_addr().unwrap();
    let provider = tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            let (path, body) = read_request(&mut stream).await.unwrap();
            let response = response_for(&path, &body, address);
            stream.write_all(response.as_bytes()).await.unwrap();
        }
    });
    let epg = EpgClient::new(EpgConfig {
        authentication_url: format!("http://{address}/EDS/jsp/AuthenticationURL"),
        user_id: "user".into(),
        password: "password".into(),
        stb_id: "stb".into(),
        stb_mac: "00:11:22:33:44:55".into(),
        source_ip: Ipv4Addr::LOCALHOST.into(),
        stb_type: "E900V21D".into(),
        area_id: "021".into(),
        user_group_id: "2025".into(),
    })
    .unwrap();
    let config = Config::try_parse_from([
        "iptv-media-server",
        "--dhcp-user",
        "d",
        "--dhcp-password",
        "p",
        "--user-id",
        "u",
        "--password",
        "p",
        "--guide-past-days",
        "0",
        "--guide-future-days",
        "0",
        "--refresh-interval-seconds",
        "3600",
    ])
    .unwrap();
    let zone = GuideZone::from_str("UTC").unwrap();
    let snapshot = Snapshot::new(
        vec![
            channel("channel", "1"),
            channel("live", "0"),
            channel("bad", "1"),
        ],
        vec![programme()],
        zone.clone(),
        1,
    );
    let snapshots = Arc::new(RwLock::new(Arc::clone(&snapshot)));
    let (refresh_tx, refresh_rx) = mpsc::channel(8);
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let refresh = tokio::spawn(refresh::run_loop(
        epg.clone(),
        config,
        zone.clone(),
        Arc::clone(&snapshots),
        refresh_rx,
        false,
        shutdown_rx,
    ));
    let state = Arc::new(AppState::new(
        epg,
        snapshots,
        Arc::new(TokenStore::default()),
        None,
        zone,
        ServiceControl {
            refresh: refresh_tx,
            shutdown: shutdown_tx,
        },
    ));
    Fixture {
        app: router(state),
        snapshot,
        host: address.to_string(),
        provider,
        refresh,
    }
}

fn channel(id: &str, time_shift: &str) -> BootChannel {
    BootChannel {
        channel_id: id.into(),
        name: id.into(),
        mix_no: "1".into(),
        channel_url: String::new(),
        time_shift_url: String::new(),
        channel_sdp: String::new(),
        time_shift: time_shift.into(),
        time_shift_length: String::new(),
        logo_url: String::new(),
        code: "code".into(),
        group_title: "News".into(),
    }
}
fn programme() -> Programme {
    Programme {
        title: "Programme".into(),
        start_time_ms: 1_700_000_000_000,
        end_time_ms: 1_700_000_001_000,
        programme_id: "programme".into(),
        channel_id: "channel".into(),
    }
}

pub async fn get(app: &Router, host: &str, uri: &str) -> axum::response::Response {
    let request = axum::http::Request::builder()
        .uri(uri)
        .header("host", host)
        .body(Body::empty())
        .unwrap();
    tokio::time::timeout(Duration::from_secs(1), app.clone().oneshot(request))
        .await
        .unwrap()
        .unwrap()
}
