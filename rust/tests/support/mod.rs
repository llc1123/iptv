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
    http::{AppState, router},
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
use provider::{ProviderState, RequestRecord, read_request, response_for};

pub struct Fixture {
    pub app: Router,
    pub snapshot: Arc<Snapshot>,
    pub snapshots: Arc<RwLock<Arc<Snapshot>>>,
    pub host: String,
    pub requests: Arc<std::sync::Mutex<Vec<RequestRecord>>>,
    pub provider_state: Arc<ProviderState>,
    pub guide_cache_path: std::path::PathBuf,
    provider: tokio::task::JoinHandle<()>,
    refresh: tokio::task::JoinHandle<()>,
    shutdown: watch::Sender<bool>,
}
impl Fixture {
    pub fn stop(self) {
        let Fixture {
            snapshot,
            snapshots,
            provider_state,
            guide_cache_path,
            provider,
            refresh,
            shutdown,
            ..
        } = self;
        drop((snapshot, snapshots, provider_state, guide_cache_path));
        let _ = shutdown.send(true);
        provider.abort();
        refresh.abort();
    }
}

pub async fn fixture() -> Fixture {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let address = listener.local_addr().unwrap();
    let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
    let provider_state = Arc::new(ProviderState::default());
    let provider_requests = Arc::clone(&requests);
    let provider_state_for_task = Arc::clone(&provider_state);
    let provider = tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            let request = read_request(&mut stream).await.unwrap();
            provider_requests.lock().unwrap().push(request.clone());
            let response = response_for(&request, address, &provider_state_for_task);
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
        interface: None,
    })
    .unwrap();
    let guide_cache_path = std::env::temp_dir().join(format!("iptv-guide-{}.json", address.port()));
    let config = test_config(&guide_cache_path);
    let zone = GuideZone::from_str("UTC").unwrap();
    let snapshot = Snapshot::new(
        vec![
            channel("channel", "1"),
            channel("live", "0"),
            channel("bad", "1"),
            channel("content-type", "1"),
            channel("large", "1"),
        ],
        vec![
            programme(),
            programme_for_now("bad", "bad-programme", "Bad"),
        ],
        zone,
        1,
    );
    let snapshots = Arc::new(RwLock::new(Arc::clone(&snapshot)));
    let (refresh_tx, refresh_rx) = mpsc::channel(8);
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let refresh = tokio::spawn(refresh::run_loop(
        epg.clone(),
        config,
        zone,
        Arc::clone(&snapshots),
        refresh_rx,
        false,
        shutdown_rx,
    ));
    let state = Arc::new(AppState {
        epg,
        snapshot: Arc::clone(&snapshots),
        tokens: TokenStore::default(),
        logo_dir: std::env::temp_dir().join("iptv-test-no-logos"),
        guide_cache: guide_cache_path.clone(),
        zone,
        refresh: refresh_tx,
    });
    Fixture {
        app: router(state),
        snapshot,
        snapshots,
        host: address.to_string(),
        requests,
        provider_state,
        guide_cache_path,
        provider,
        refresh,
        shutdown: shutdown_tx,
    }
}

fn test_config(guide_cache_path: &std::path::Path) -> Config {
    Config::try_parse_from([
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
        "--guide-cache",
        guide_cache_path.to_str().unwrap(),
    ])
    .unwrap()
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
    programme_for("channel", "programme", "Programme")
}

fn programme_for(channel_id: &str, programme_id: &str, title: &str) -> Programme {
    Programme {
        title: title.into(),
        start_time_ms: 1_700_000_000_000,
        end_time_ms: 1_700_000_001_000,
        programme_id: programme_id.into(),
        channel_id: channel_id.into(),
    }
}

fn programme_for_now(channel_id: &str, programme_id: &str, title: &str) -> Programme {
    let start_time_ms = chrono::Utc::now().timestamp_millis();
    Programme {
        title: title.into(),
        start_time_ms,
        end_time_ms: start_time_ms + 1_000,
        programme_id: programme_id.into(),
        channel_id: channel_id.into(),
    }
}

pub async fn get(app: &Router, host: &str, uri: &str) -> axum::response::Response {
    get_with_headers(app, host, uri, &[]).await
}

pub async fn get_with_headers(
    app: &Router,
    host: &str,
    uri: &str,
    headers: &[(&str, &str)],
) -> axum::response::Response {
    let mut builder = axum::http::Request::builder().uri(uri).header("host", host);
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let request = builder.body(Body::empty()).unwrap();
    tokio::time::timeout(Duration::from_secs(1), app.clone().oneshot(request))
        .await
        .unwrap()
        .unwrap()
}
