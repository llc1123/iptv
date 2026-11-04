//! Production startup composition for the media server.

use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use chrono_tz::Tz;

use crate::config::Config;
use crate::dhcp::linux::{
    ClientConfig, DhcpClient, LinuxError, LinuxPlatform, Platform, configure_interface,
};
use crate::dhcp::packet::{MacAddress, PacketError};
use crate::dhcp::{DhcpError, LifecycleResult};
use crate::epg::{EpgClient, EpgConfig};
use crate::error::{AppError, StartupError};
use crate::guide::{
    CacheFingerprint, GuideCache, GuideZone, SystemGuideClock, build_windows_from_clock,
};
use crate::health::SnapshotCounts;
use crate::hls::{HlsPolicy, TokenStore};
use crate::http::{
    AppState, AppStateParts, EpgPlaybackResolver, PolicyForwarder, ServeState, SystemEpochClock,
    SystemResolver, build_router, serve_with_gate,
};
use crate::lifecycle::{CancellationSource, CancellationToken, Lifecycle};
use crate::logo_catalog::LogoCatalog;
use crate::preflight::{PythonPreflight, StartupRefusal, acquire_startup_gate};
use crate::refresh::{EpgMetadataSource, RefreshEngine, RefreshEngineConfig, RefreshSupervisor};
use crate::snapshot::{Snapshot, SnapshotStore};

const REFRESH_STOP_TIMEOUT: Duration = Duration::from_secs(5);

/// Runs admission, network setup, metadata bootstrap, readiness, and serving.
///
/// # Errors
/// Returns a fixed operational error when any startup stage or serving is refused.
pub async fn run(config: Config) -> Result<(), AppError> {
    let zone = config
        .guide_timezone
        .parse::<Tz>()
        .map(GuideZone::Iana)
        .map_err(|_| {
            report_startup_stage("guide-window");
            startup_error()
        })?;
    let windows = build_windows_from_clock(
        &SystemGuideClock,
        &zone,
        config.guide_past_days,
        config.guide_future_days,
    )
    .map_err(|_| {
        report_startup_stage("guide-window");
        startup_error()
    })?;
    let cancellation = CancellationSource::new();
    let signal_task = tokio::spawn(signal_watcher(cancellation.clone()));
    let state_dir = state_dir(&config);
    let preflight = PythonPreflight::for_config(PathBuf::from("/proc"), &config);
    let gate = match acquire_startup_gate(&config, &state_dir, &preflight) {
        Ok(gate) => gate,
        Err(refusal) => {
            report_startup_stage("preflight");
            cancellation.cancel();
            let _ = signal_task.await;
            return Err(map_refusal(refusal));
        }
    };
    if cancellation.token().is_cancelled() {
        drop(gate);
        let _ = signal_task.await;
        report_startup_stage("preflight");
        return Err(startup_error());
    }

    let startup = compose_after_gate(&config, zone, windows, gate, cancellation.clone()).await;
    cancellation.cancel();
    let _ = signal_task.await;
    startup
}

struct Prepared {
    client: EpgClient,
    store: SnapshotStore,
    health: crate::health::HealthState,
    engine: RefreshEngine,
}

async fn compose_after_gate(
    config: &Config,
    zone: GuideZone,
    windows: Vec<crate::guide::GuideWindow>,
    gate: crate::startup_gate::StartupGate,
    cancellation: CancellationSource,
) -> Result<(), AppError> {
    let token = cancellation.token();
    let (lease, interface_mac) = match acquire_dhcp(config, token.clone()).await {
        Ok(value) => value,
        Err(error) => {
            report_startup_stage("dhcp");
            return Err(error);
        }
    };
    let prepared =
        prepare_engine(config, zone, windows, lease, interface_mac, token.clone()).await?;
    let tokens = Arc::new(TokenStore::default());
    let policy = Arc::new(
        HlsPolicy::new(
            config.upstream_allow_hosts.clone(),
            config.upstream_allow_cidrs.clone(),
            Arc::new(SystemResolver),
        )
        .map_err(|_| {
            report_startup_stage("hls-policy");
            startup_error()
        })?,
    );
    let hls = Arc::new(PolicyForwarder::new(policy, tokens.clone()));
    let playback = Arc::new(EpgPlaybackResolver::with_runtime(
        prepared.client,
        prepared.store.clone(),
        token.clone(),
    ));
    let lifecycle = Lifecycle::with_cancellation(cancellation.clone());
    let logos = LogoCatalog::load_if_present(&config.logo_catalog)
        .ok()
        .flatten()
        .map(Arc::new);
    let state = AppState::from_parts(AppStateParts {
        lifecycle: lifecycle.clone(),
        snapshots: prepared.store.clone(),
        health: prepared.health.clone(),
        playback,
        hls,
        tokens,
        logos,
        clock: Arc::new(SystemEpochClock),
    });
    let router = build_router(state);
    let supervisor = RefreshSupervisor::start_with_cancellation(prepared.engine, cancellation);
    if !prepared.store.wait_for_snapshot(&token).await {
        report_startup_stage("first-snapshot");
        let _ = supervisor.stop(REFRESH_STOP_TIMEOUT).await;
        return Err(startup_error());
    }
    let result = serve_with_gate(
        gate,
        router,
        ServeState {
            lifecycle,
            health: prepared.health,
            grace: Duration::from_secs(config.shutdown_grace_seconds),
        },
        async move { token.cancelled().await },
    )
    .await
    .map_err(|_| {
        report_startup_stage("serve");
        startup_error()
    });
    let _ = supervisor.stop(REFRESH_STOP_TIMEOUT).await;
    result
}

async fn prepare_engine(
    config: &Config,
    zone: GuideZone,
    windows: Vec<crate::guide::GuideWindow>,
    lease: crate::dhcp::lease::DhcpLease,
    interface_mac: MacAddress,
    token: CancellationToken,
) -> Result<Prepared, AppError> {
    let (stb_id, effective_mac) =
        epg_identity(&config.stb_id, config.epg_mac.as_deref(), interface_mac);
    let client = EpgClient::with_shutdown(
        EpgConfig {
            authentication_url: config.authentication_url.clone(),
            user_id: config.epg_user.as_str().to_owned(),
            password: config.epg_password.as_str().to_owned(),
            stb_id,
            stb_mac: effective_mac.clone(),
            source_ip: IpAddr::V4(lease.address),
            stb_type: "E900V21D".to_owned(),
            area_id: "021".to_owned(),
            user_group_id: "2025".to_owned(),
            nonce: fresh_epg_nonce().map_err(|_| {
                report_startup_stage("epg-client");
                startup_error()
            })?,
            write_timeout: Duration::from_secs(30),
            pool_timeout: Duration::from_secs(5),
            media_pool_size: 8,
        },
        token.clone(),
    )
    .map_err(|_| {
        report_startup_stage("epg-client");
        startup_error()
    })?;
    let source = Arc::new(EpgMetadataSource::new(client.clone()));
    let channels = source.bootstrap(token.clone()).await.map_err(|_| {
        report_startup_stage("epg-bootstrap");
        startup_error()
    })?;
    let fingerprint =
        CacheFingerprint::from_config(config, Some(&effective_mac)).map_err(|_| {
            report_startup_stage("cache");
            startup_error()
        })?;
    let cache = GuideCache::new(config.guide_cache.clone());
    let cache_state = cache.load_with_refresh_interval(
        Utc::now().timestamp_millis(),
        config.refresh_interval_seconds,
        &fingerprint,
    );
    let store = SnapshotStore::new();
    let health = crate::health::HealthState::new();
    let mut engine = RefreshEngine::new(RefreshEngineConfig {
        source: source.clone(),
        store: store.clone(),
        cache: Some(cache),
        cache_fingerprint: Some(fingerprint),
        health: health.clone(),
        interval: Duration::from_secs(config.refresh_interval_seconds),
        windows,
        guide_zone: zone.clone(),
        catchup_days: config.guide_past_days.saturating_add(1),
        cancellation: token.clone(),
        clock: None,
        sleeper: None,
    });
    if let Some(programmes) = cache_state.into_fresh_programmes() {
        seed_warm_cache(
            config,
            &mut engine,
            &store,
            &health,
            channels,
            programmes,
            zone,
        )
        .await
        .inspect_err(|_| {
            report_startup_stage("warm-cache");
        })?;
    }
    Ok(Prepared {
        client,
        store,
        health,
        engine,
    })
}

async fn seed_warm_cache(
    config: &Config,
    engine: &mut RefreshEngine,
    store: &SnapshotStore,
    health: &crate::health::HealthState,
    channels: Vec<crate::epg::BootChannel>,
    programmes: Vec<crate::epg::Programme>,
    zone: GuideZone,
) -> Result<(), AppError> {
    let snapshot = Snapshot::new(
        1,
        channels.clone(),
        programmes,
        crate::snapshot::LogoView::from_channels(&channels),
        zone,
        config.guide_past_days.saturating_add(1),
    )
    .map_err(|_| startup_error())?;
    engine
        .seed_snapshot(snapshot)
        .map_err(|_| startup_error())?;
    if let Some(snapshot) = store.load().map_err(|_| startup_error())? {
        let now = u64::try_from(Utc::now().timestamp()).unwrap_or(0);
        health
            .record_refresh_success(
                now,
                now.saturating_add(config.refresh_interval_seconds),
                SnapshotCounts::new(
                    u64::try_from(snapshot.channels().len()).unwrap_or(u64::MAX),
                    u64::try_from(snapshot.programmes().len()).unwrap_or(u64::MAX),
                ),
            )
            .await;
    }
    Ok(())
}

async fn acquire_dhcp(
    config: &Config,
    cancellation: CancellationToken,
) -> Result<(crate::dhcp::lease::DhcpLease, MacAddress), AppError> {
    let interface = config.interface.clone();
    let dhcp_user = config.dhcp_user.as_str().to_owned();
    let dhcp_password = config.dhcp_password.as_str().to_owned();
    let stb_id_prefix = config.stb_id_prefix.clone();
    let routes = config.routes.clone();
    let worker_cancellation = cancellation.clone();
    let mut task = tokio::task::spawn_blocking(move || {
        if worker_cancellation.is_cancelled() {
            return Err(startup_error());
        }
        let mut platform = LinuxPlatform::new(&interface).map_err(|_| {
            report_startup_stage("dhcp-platform");
            startup_error()
        })?;
        platform.preflight().map_err(|_| {
            report_startup_stage("dhcp-preflight");
            startup_error()
        })?;
        let mac = platform.interface_mac().map_err(|_| {
            report_startup_stage("dhcp-interface");
            startup_error()
        })?;
        let hostname = dhcp_hostname(&stb_id_prefix, mac);
        let client_config = ClientConfig {
            mac,
            user: &dhcp_user,
            password: &dhcp_password,
            hostname: hostname.as_slice(),
        };
        let mut client = DhcpClient::new(platform, client_config);
        let lease = client.acquire().map_err(|error| {
            report_dhcp_stage(&error);
            startup_error()
        })?;
        let mut platform = client.into_platform();
        if worker_cancellation.is_cancelled() {
            return Err(startup_error());
        }
        match configure_interface(&mut platform, &interface, &lease, &routes) {
            Ok(LifecycleResult::Applied(lease)) => Ok((lease, mac)),
            Ok(LifecycleResult::Preserved { .. }) | Err(_) => {
                report_startup_stage("dhcp-interface-setup");
                Err(startup_error())
            }
        }
    });
    tokio::select! {
        result = &mut task => result.map_err(|_| startup_error())?,
        () = cancellation.cancelled() => {
            let _ = task.await;
            Err(startup_error())
        }
    }
}

fn dhcp_hostname(prefix: &str, mac: MacAddress) -> Vec<u8> {
    format!("{prefix}{}", mac.compact_upper()).into_bytes()
}

fn epg_identity(
    stb_id: &str,
    epg_mac: Option<&str>,
    interface_mac: MacAddress,
) -> (String, String) {
    (
        stb_id.to_owned(),
        epg_mac.map_or_else(|| interface_mac.to_string().to_uppercase(), str::to_owned),
    )
}

fn fresh_epg_nonce() -> Result<u32, AppError> {
    use ring::rand::{SecureRandom, SystemRandom};

    let mut bytes = [0_u8; 4];
    SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| startup_error())?;
    Ok(u32::from_ne_bytes(bytes) % 100_000_000)
}

async fn signal_watcher(source: CancellationSource) {
    let token = source.token();
    #[cfg(unix)]
    {
        let Ok(mut terminate) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        else {
            source.cancel();
            return;
        };
        tokio::select! {
            result = tokio::signal::ctrl_c() => { let _ = result; source.cancel(); },
            result = terminate.recv() => { let _ = result; source.cancel(); },
            () = token.cancelled() => {},
        }
    }
    #[cfg(not(unix))]
    {
        tokio::select! {
            result = tokio::signal::ctrl_c() => { let _ = result; source.cancel(); },
            () = token.cancelled() => {},
        }
    }
}

fn state_dir(config: &Config) -> PathBuf {
    config
        .guide_cache
        .parent()
        .map_or_else(|| Path::new(".").to_path_buf(), Path::to_path_buf)
}

fn startup_error() -> AppError {
    AppError::Startup(StartupError::Startup)
}

fn report_startup_stage(stage: &'static str) {
    eprintln!("startup_stage={stage}");
}

fn report_dhcp_stage(error: &DhcpError) {
    let stage = match error {
        DhcpError::Crypto(_) => "dhcp-option60",
        DhcpError::Packet(error) => {
            report_packet_stage(error);
            return;
        }
        DhcpError::Lease(_) => "dhcp-lease",
        DhcpError::StageTimeout | DhcpError::Platform(LinuxError::Timeout) => "dhcp-timeout",
        DhcpError::Platform(LinuxError::Io) => "dhcp-socket",
        DhcpError::Platform(_) => "dhcp-platform",
    };
    report_startup_stage(stage);
}

fn report_packet_stage(error: &PacketError) {
    let stage = match error {
        PacketError::MalformedFrame => "dhcp-frame",
        PacketError::InvalidMac => "dhcp-mac",
        PacketError::InvalidCookie => "dhcp-cookie",
        PacketError::MalformedOptions => "dhcp-options",
        PacketError::DecodeFailed => "dhcp-message",
        PacketError::InvalidUdpPorts => "dhcp-ports",
        PacketError::WrongTransaction => "dhcp-transaction",
        PacketError::NotBootReply => "dhcp-bootp",
        PacketError::UnexpectedMessageType => "dhcp-message-type",
        PacketError::Nak => "dhcp-nak",
        PacketError::MissingOption => "dhcp-missing-option",
        PacketError::MissingMessageType => "dhcp-message-type-missing",
        PacketError::MissingAckOption(code) => match code {
            1 => "dhcp-ack-subnet-mask-missing",
            3 => "dhcp-ack-router-missing",
            43 => "dhcp-ack-vendor-missing",
            51 => "dhcp-ack-lease-time-missing",
            54 => "dhcp-ack-server-id-missing",
            _ => "dhcp-ack-option-missing",
        },
        PacketError::InvalidOptionLength(code) => match code {
            1 => "dhcp-subnet-mask",
            3 => "dhcp-router",
            51 => "dhcp-lease-time",
            54 => "dhcp-server-id",
            _ => "dhcp-option-length",
        },
        PacketError::PacketTooLarge => "dhcp-size",
    };
    report_startup_stage(stage);
}

fn map_refusal(_refusal: StartupRefusal) -> AppError {
    startup_error()
}

#[cfg(test)]
mod tests {
    use super::{dhcp_hostname, epg_identity, fresh_epg_nonce, signal_watcher};
    use crate::dhcp::MacAddress;
    use crate::lifecycle::CancellationSource;

    #[tokio::test]
    async fn signal_watcher_joins_after_shared_cancellation() {
        let source = CancellationSource::new();
        let task = tokio::spawn(signal_watcher(source.clone()));
        source.cancel();
        assert!(matches!(
            tokio::time::timeout(std::time::Duration::from_secs(1), task).await,
            Ok(Ok(()))
        ));
    }

    #[test]
    fn dhcp_hostname_matches_python_identity_format() {
        let mac = MacAddress::new([0x02, 0x0a, 0xb0, 0xc0, 0xd0, 0xe0]);
        assert_eq!(dhcp_hostname("STB-", mac), b"STB-020AB0C0D0E0");
    }

    #[test]
    fn epg_identity_uses_uppercase_interface_mac_only_as_default() {
        let mac = MacAddress::new([0x02, 0xaa, 0xbb, 0xcc, 0xdd, 0xee]);
        assert_eq!(
            epg_identity("", None, mac),
            (String::new(), "02:AA:BB:CC:DD:EE".to_owned())
        );
        assert_eq!(
            epg_identity("configured", Some("configured-mac"), mac),
            ("configured".to_owned(), "configured-mac".to_owned())
        );
    }

    #[test]
    fn fresh_epg_nonce_is_always_provider_valid() {
        for _ in 0..8 {
            assert!(matches!(fresh_epg_nonce(), Ok(value) if value <= 99_999_999));
        }
    }
}
