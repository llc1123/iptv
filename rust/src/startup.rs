use crate::{
    config::Config,
    dhcp::{ClientConfig, acquire_lease, configure_interface},
    epg::{EpgClient, EpgConfig},
    guide::{GuideCache, GuideZone},
    hls::TokenStore,
    http::{AppSignals, AppState, router},
    logo_catalog::LogoCatalog,
    refresh,
    snapshot::Snapshot,
};
use chrono_tz::Tz;
use std::{net::IpAddr, sync::Arc};
use tokio::sync::{Notify, RwLock, watch};

pub async fn run(config: Config) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let zone = config.guide_timezone.parse::<Tz>().map(GuideZone::Iana)?;
    let interface = config.interface.clone();
    let dhcp_user = config.dhcp_user.as_str().to_owned();
    let dhcp_password = config.dhcp_password.as_str().to_owned();
    let prefix = config.stb_id_prefix.clone();
    let routes = config.routes.clone();
    let (lease, interface_mac) = tokio::task::spawn_blocking(move || {
        let client_config = ClientConfig {
            user: &dhcp_user,
            password: &dhcp_password,
            stb_id_prefix: &prefix,
        };
        let (lease, mac) = acquire_lease(&interface, &client_config)?;
        configure_interface(&interface, &lease, &routes)?;
        Ok::<_, crate::dhcp::DhcpError>((lease, mac))
    })
    .await??;
    let stb_mac = config
        .epg_mac
        .clone()
        .unwrap_or_else(|| interface_mac.to_string().to_uppercase());
    let epg = EpgClient::new(EpgConfig {
        authentication_url: config.authentication_url.clone(),
        user_id: config.epg_user.as_str().to_owned(),
        password: config.epg_password.as_str().to_owned(),
        stb_id: config.stb_id.clone(),
        stb_mac,
        source_ip: IpAddr::V4(lease.address),
        stb_type: "E900V21D".to_owned(),
        area_id: "021".to_owned(),
        user_group_id: "2025".to_owned(),
    })?;
    epg.authenticate().await?;
    let channels = epg.channels().await?;
    let guide_cache = GuideCache::new(config.guide_cache.clone());
    let (channels, programmes) = match guide_cache.load() {
        Some(programmes) => (channels, programmes),
        None => refresh::refresh_once(&epg, &channels, &config, &zone).await,
    };
    let snapshots = Arc::new(RwLock::new(Some(Snapshot::new(
        channels,
        programmes,
        zone.clone(),
        config.guide_past_days.saturating_add(1),
    ))));
    let logos = match LogoCatalog::load_if_present(&config.logo_catalog) {
        Ok(value) => value.map(Arc::new),
        Err(error) => {
            tracing::warn!(%error, "logo catalog unavailable");
            None
        }
    };
    let notify = Arc::new(Notify::new());
    let refresh_done = Arc::new(Notify::new());
    let tokens = Arc::new(TokenStore::default());
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let state = Arc::new(AppState::new(
        epg.clone(),
        Arc::clone(&snapshots),
        tokens,
        logos,
        zone.clone(),
        config.clone(),
        AppSignals {
            refresh: Arc::clone(&notify),
            refresh_done: Arc::clone(&refresh_done),
            shutdown: shutdown_tx.clone(),
        },
    ));
    let refresh_state = Arc::clone(&snapshots);
    let refresh_notify = Arc::clone(&notify);
    let refresh_done_task = Arc::clone(&refresh_done);
    let refresh_config = config.clone();
    let guide_zone_for_task = zone.clone();
    tokio::spawn(refresh::run_loop(
        epg,
        refresh_config,
        guide_zone_for_task,
        refresh_state,
        refresh_notify,
        refresh_done_task,
        shutdown_rx,
    ));
    let listener = tokio::net::TcpListener::bind((config.host.as_str(), config.port)).await?;
    let shutdown = signal(shutdown_tx);
    axum::serve(listener, router(state))
        .with_graceful_shutdown(shutdown)
        .await?;
    Ok(())
}

async fn signal(sender: watch::Sender<bool>) {
    #[cfg(unix)]
    let mut terminate =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).ok();
    #[cfg(unix)]
    tokio::select! { result = tokio::signal::ctrl_c() => { if let Err(error) = result { tracing::error!(%error, "ctrl-c signal failed"); } }, () = async { if let Some(signal) = &mut terminate { signal.recv().await; } else { std::future::pending().await } } => {} }
    #[cfg(not(unix))]
    if let Err(error) = tokio::signal::ctrl_c().await {
        tracing::error!(%error, "ctrl-c signal failed");
    }
    if let Err(error) = sender.send(true) {
        tracing::debug!(%error, "shutdown already complete");
    }
}

#[cfg(test)]
mod tests {
    use crate::dhcp::MacAddress;
    fn epg_identity(stb_id: &str, epg_mac: Option<&str>, mac: MacAddress) -> (String, String) {
        (
            stb_id.to_owned(),
            epg_mac.map_or_else(|| mac.to_string().to_uppercase(), str::to_owned),
        )
    }
    #[test]
    fn epg_identity_uses_uppercase_interface_mac_only_as_default() {
        let mac = MacAddress::new([2, 0xaa, 0xbb, 0xcc, 0xdd, 0xee]);
        assert_eq!(
            epg_identity("", None, mac),
            (String::new(), "02:AA:BB:CC:DD:EE".to_owned())
        );
        assert_eq!(
            epg_identity("configured", Some("configured-mac"), mac),
            ("configured".to_owned(), "configured-mac".to_owned())
        );
    }
}
