use crate::{
    config::Config,
    dhcp::{ClientConfig, acquire_lease, configure_interface},
    epg::{EpgClient, EpgConfig},
    guide::{GuideCache, GuideZone},
    hls::TokenStore,
    http::{AppState, ServiceControl, router},
    logo_catalog::LogoCatalog,
    refresh,
    snapshot::Snapshot,
};
use chrono_tz::Tz;
use std::{net::IpAddr, sync::Arc};
use tokio::sync::{RwLock, mpsc, watch};

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
    let stb_mac = epg_mac(config.epg_mac.as_deref(), interface_mac);
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
    let (channels, programmes, cache_loaded) = if let Some(programmes) = guide_cache.load() {
        (channels, programmes, true)
    } else {
        let (channels, programmes) = refresh::refresh_once(&epg, &config, &zone).await?;
        (channels, programmes, false)
    };
    let snapshots = Arc::new(RwLock::new(Snapshot::new(
        channels,
        programmes,
        zone.clone(),
        config.guide_past_days.saturating_add(1),
    )));
    let logos = match LogoCatalog::load_if_present(&config.logo_catalog) {
        Ok(value) => value.map(Arc::new),
        Err(error) => {
            tracing::warn!(%error, "logo catalog unavailable");
            None
        }
    };
    let tokens = Arc::new(TokenStore::default());
    let (refresh_tx, refresh_rx) = mpsc::channel(16);
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let state = Arc::new(AppState::new(
        epg.clone(),
        Arc::clone(&snapshots),
        tokens,
        logos,
        zone.clone(),
        ServiceControl {
            refresh: refresh_tx,
            shutdown: shutdown_tx.clone(),
        },
    ));
    let refresh_state = Arc::clone(&snapshots);
    let refresh_config = config.clone();
    let guide_zone_for_task = zone.clone();
    tokio::spawn(refresh::run_loop(
        epg,
        refresh_config,
        guide_zone_for_task,
        refresh_state,
        refresh_rx,
        cache_loaded,
        shutdown_rx,
    ));
    let listener = tokio::net::TcpListener::bind((config.host.as_str(), config.port)).await?;
    let shutdown = signal(shutdown_tx);
    axum::serve(listener, router(state))
        .with_graceful_shutdown(shutdown)
        .await?;
    Ok(())
}

fn epg_mac(configured: Option<&str>, interface: crate::dhcp::MacAddress) -> String {
    configured.map_or_else(|| interface.to_string().to_uppercase(), str::to_owned)
}

async fn signal(sender: watch::Sender<bool>) {
    #[cfg(unix)]
    let mut terminate =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).ok();
    #[cfg(unix)]
    tokio::select! {
        result = tokio::signal::ctrl_c() => {
            if let Err(error) = result {
                tracing::error!(%error, "ctrl-c signal failed");
            }
        }
        () = async {
            if let Some(signal) = &mut terminate {
                signal.recv().await;
            } else {
                std::future::pending::<()>().await;
            }
        } => {}
    }
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
    #[test]
    fn epg_mac_uses_uppercase_interface_mac_only_as_default() {
        let mac = MacAddress::new([2, 0xaa, 0xbb, 0xcc, 0xdd, 0xee]);
        assert_eq!(super::epg_mac(None, mac), "02:AA:BB:CC:DD:EE");
        assert_eq!(
            super::epg_mac(Some("configured-mac"), mac),
            "configured-mac"
        );
    }
}
