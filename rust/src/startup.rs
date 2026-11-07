use crate::{
    config::Config,
    dhcp::{ClientConfig, acquire_lease, configure_interface},
    epg::{EpgClient, EpgConfig},
    guide::{GuideCache, GuideZone},
    hls::TokenStore,
    http::{AppState, MetadataPaths, router},
    logo_catalog::LogoCatalog,
    refresh,
    renewal::{Renewal, RenewalError},
    snapshot::Snapshot,
};
use axum::Router;
use chrono_tz::Tz;
use std::{future::IntoFuture, net::IpAddr, sync::Arc};
use tokio::{
    net::TcpListener,
    sync::{RwLock, mpsc, watch},
    task::JoinHandle,
    time::{Duration, timeout},
};

type StartupError = Box<dyn std::error::Error + Send + Sync>;

const DRAIN_TIMEOUT: Duration = Duration::from_secs(10);

struct Service {
    listener: TcpListener,
    router: Router,
    refresh: JoinHandle<()>,
    renewal: JoinHandle<Result<(), RenewalError>>,
}

pub async fn run(config: Config) -> Result<(), StartupError> {
    let zone = config.guide_timezone.parse::<Tz>().map(GuideZone::Iana)?;
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let signal_task = tokio::spawn(signal(shutdown_tx.clone()));
    let mut shutdown = shutdown_rx.clone();
    let service = tokio::select! {
        result = start(config, zone, shutdown_rx) => result?,
        () = wait_shutdown(&mut shutdown) => {
            tracing::info!("shutdown requested during startup");
            signal_task.abort();
            return Ok(());
        }
    };
    let mut drain = shutdown.clone();
    let server = axum::serve(service.listener, service.router)
        .with_graceful_shutdown(async move { wait_shutdown(&mut drain).await })
        .into_future();
    let mut server = std::pin::pin!(server);
    let mut renewal = service.renewal;
    let mut failure: Option<StartupError> = None;
    let drain = tokio::select! {
        result = &mut server => {
            result?;
            false
        }
        () = wait_shutdown(&mut shutdown) => true,
        result = &mut renewal => {
            if let Ok(Err(error)) = result {
                tracing::error!(%error, "stopping: DHCP lease is no longer usable");
                failure = Some(error.into());
            }
            let _ = shutdown_tx.send(true);
            true
        }
    };
    if drain {
        // Proxied HLS responses can stay open indefinitely; stop waiting for
        // them after a bounded drain.
        if let Ok(result) = timeout(DRAIN_TIMEOUT, &mut server).await {
            result?;
        } else {
            tracing::warn!("open connections did not drain; shutting down");
        }
    }
    let _ = shutdown_tx.send(true);
    let _ = timeout(Duration::from_secs(5), service.refresh).await;
    renewal.abort();
    signal_task.abort();
    failure.map_or(Ok(()), Err)
}

async fn start(
    config: Config,
    zone: GuideZone,
    shutdown_rx: watch::Receiver<bool>,
) -> Result<Service, StartupError> {
    let interface = config.interface.clone();
    let dhcp_user = config.dhcp_user.as_str().to_owned();
    let dhcp_password = config.dhcp_password.as_str().to_owned();
    let prefix = config.stb_id_prefix.clone();
    let routes = config.routes.clone();
    let renewal = Renewal {
        interface: interface.clone(),
        user: dhcp_user.clone(),
        password: dhcp_password.clone(),
        stb_id_prefix: prefix.clone(),
    };
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
    let renewal = tokio::spawn(renewal.run(lease.clone(), shutdown_rx.clone()));
    let epg = EpgClient::new(EpgConfig {
        authentication_url: config.authentication_url.clone(),
        user_id: config.epg_user.as_str().to_owned(),
        password: config.epg_password.as_str().to_owned(),
        stb_id: config.stb_id.clone(),
        stb_mac: epg_mac(config.epg_mac.as_deref(), interface_mac),
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
        let (channels, programmes) = refresh::refresh_once(&epg, &config, &zone, None).await?;
        let saved = programmes.clone();
        match tokio::task::spawn_blocking(move || guide_cache.save(&saved)).await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => tracing::warn!(%error, "guide cache write failed"),
            Err(error) => tracing::warn!(%error, "guide cache write failed"),
        }
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
    let (refresh_tx, refresh_rx) = mpsc::channel(16);
    let state = Arc::new(AppState::new(
        epg.clone(),
        Arc::clone(&snapshots),
        Arc::new(TokenStore::default()),
        logos,
        zone.clone(),
        refresh_tx,
        MetadataPaths {
            catalog: config.logo_catalog.display().to_string(),
            guide_cache: config.guide_cache.display().to_string(),
        },
    ));
    let refresh = tokio::spawn(refresh::run_loop(
        epg,
        config.clone(),
        zone,
        snapshots,
        refresh_rx,
        cache_loaded,
        shutdown_rx,
    ));
    let listener = TcpListener::bind((config.host.as_str(), config.port)).await?;
    Ok(Service {
        listener,
        router: router(state),
        refresh,
        renewal,
    })
}

fn epg_mac(configured: Option<&str>, interface: crate::dhcp::MacAddress) -> String {
    configured.map_or_else(|| interface.to_string().to_uppercase(), str::to_owned)
}

async fn wait_shutdown(shutdown: &mut watch::Receiver<bool>) {
    let _ = shutdown.wait_for(|stop| *stop).await;
}

/// Requests shutdown on Ctrl-C or SIGTERM. A handler that cannot be installed
/// is logged and never fires, so it cannot stop a healthy server.
async fn signal(sender: watch::Sender<bool>) {
    let ctrl_c = async {
        if let Err(error) = tokio::signal::ctrl_c().await {
            tracing::error!(%error, "ctrl-c handler unavailable");
            std::future::pending::<()>().await;
        }
    };
    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut terminate) => {
                terminate.recv().await;
            }
            Err(error) => {
                tracing::error!(%error, "SIGTERM handler unavailable");
                std::future::pending::<()>().await;
            }
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        () = ctrl_c => {}
        () = terminate => {}
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
