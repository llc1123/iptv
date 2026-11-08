use crate::{
    config::Config,
    dhcp::{ClientConfig, acquire_lease, configure_interface, interface_mac},
    epg::{EpgClient, EpgConfig},
    guide::{GuideCache, GuideZone},
    hls::TokenStore,
    http::{AppState, router},
    refresh,
    renewal::{Renewal, RenewalError},
    snapshot::Snapshot,
};
use axum::Router;
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
    let zone: GuideZone = config.guide_timezone.parse()?;
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
            match result {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    tracing::error!(%error, "stopping: DHCP lease is no longer usable");
                    failure = Some(error.into());
                }
                Err(error) => {
                    tracing::error!(%error, "stopping: DHCP renewal task failed");
                    failure = Some(error.into());
                }
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
    // netifd owns the interface in managed mode: the address comes from it and
    // this process never configures the network or renews anything.
    let (source_ip, interface_mac, renewal) = if config.network_managed {
        let address = config
            .lease_address
            .ok_or("--network-managed requires --lease-address")?;
        let interface = config.interface.clone();
        let mac = tokio::task::spawn_blocking(move || interface_mac(&interface)).await??;
        (
            IpAddr::V4(address),
            mac,
            tokio::spawn(std::future::pending()),
        )
    } else {
        let interface = config.interface.clone();
        let dhcp_user = config.dhcp_user.as_str().to_owned();
        let dhcp_password = config.dhcp_password.as_str().to_owned();
        let prefix = config.stb_id_prefix.clone();
        let renewal = Renewal {
            interface: interface.clone(),
            user: dhcp_user.clone(),
            password: dhcp_password.clone(),
            stb_id_prefix: prefix.clone(),
            route_table: config.route_table,
        };
        let route_table = config.route_table;
        let (lease, mac) = tokio::task::spawn_blocking(move || {
            let client_config = ClientConfig {
                user: &dhcp_user,
                password: &dhcp_password,
                stb_id_prefix: &prefix,
            };
            let (lease, mac) = acquire_lease(&interface, &client_config)?;
            configure_interface(&interface, &lease, route_table)?;
            Ok::<_, crate::dhcp::DhcpError>((lease, mac))
        })
        .await??;
        let renewal = tokio::spawn(renewal.run(lease.clone(), shutdown_rx.clone()));
        (IpAddr::V4(lease.address), mac, renewal)
    };
    let epg = EpgClient::new(EpgConfig {
        authentication_url: config.authentication_url.clone(),
        user_id: config.epg_user.as_str().to_owned(),
        password: config.epg_password.as_str().to_owned(),
        stb_id: config.stb_id.clone(),
        stb_mac: epg_mac(config.epg_mac.as_deref(), interface_mac),
        source_ip,
        interface: Some(config.interface.clone()),
    })?;
    let guide_cache = GuideCache::new(config.guide_cache.clone());
    let cached = guide_cache.load();
    let cache_loaded = cached.is_some();
    let (channels, programmes) = match cached {
        Some(programmes) => (epg.channels().await?, programmes),
        None => refresh::refresh_once(&epg, &config, zone, None).await?,
    };
    let snapshot = Snapshot::new(
        channels,
        programmes,
        zone,
        config.guide_past_days.saturating_add(1),
    );
    if !cache_loaded {
        let programmes = snapshot.programmes_shared();
        match tokio::task::spawn_blocking(move || guide_cache.save(&programmes)).await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => tracing::warn!(%error, "guide cache write failed"),
            Err(error) => tracing::warn!(%error, "guide cache write failed"),
        }
    }
    let snapshots = Arc::new(RwLock::new(snapshot));
    let (refresh_tx, refresh_rx) = mpsc::channel(16);
    let state = Arc::new(AppState {
        epg: epg.clone(),
        snapshot: Arc::clone(&snapshots),
        tokens: TokenStore::default(),
        logo_dir: config.logo_dir.clone(),
        guide_cache: config.guide_cache.clone(),
        zone,
        refresh: refresh_tx,
    });
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
pub(crate) async fn signal(sender: watch::Sender<bool>) {
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
