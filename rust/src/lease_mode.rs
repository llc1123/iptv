//! Lease ownership for the `netifd` deployment: the daemon that holds the
//! DHCP lease for an interface, and the report it hands to the protocol
//! handler.
//!
//! On `OpenWrt` netifd owns the interface, so the lease is acquired and
//! renewed by `--lease-daemon` (started through `proto_run_command`) and
//! reported to netifd through a script, mirroring `udhcpc -s`. The media
//! server then runs separately with `--network-managed`, taking its address
//! from netifd and never touching the interface itself.

use crate::{
    config::Config,
    dhcp::{ClientConfig, DhcpError, DhcpLease, acquire_lease, renew_lease},
    renewal::{MIN_RETRY, retry_delay},
};
use std::{
    path::PathBuf,
    process::Command,
    time::{Duration, Instant},
};

/// What the protocol handler needs to describe one lease to netifd.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LeaseReport {
    pub interface: String,
    pub address: String,
    pub prefix: u32,
    pub netmask: String,
    pub gateway: String,
    pub server: String,
    pub lease_seconds: u32,
    /// Half the lease: when the daemon renews, as the provider's own clients do.
    pub renew_seconds: u32,
}

/// Which event produced a report, so the handler can add, replace or withdraw
/// the address.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LeaseEvent {
    Bound,
    Renewed,
    Expired,
}

impl LeaseEvent {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Bound => "bound",
            Self::Renewed => "renew",
            Self::Expired => "expired",
        }
    }
}

impl LeaseReport {
    fn from_lease(interface: &str, lease: &DhcpLease) -> Self {
        Self {
            interface: interface.to_owned(),
            address: lease.address.to_string(),
            prefix: lease.prefix_length(),
            netmask: lease.subnet_mask.to_string(),
            gateway: lease.router.to_string(),
            server: lease.server.to_string(),
            lease_seconds: lease.lease_seconds,
            renew_seconds: lease.lease_seconds / 2,
        }
    }

    /// The environment the report script reads.
    fn environment(&self, event: LeaseEvent) -> Vec<(&'static str, String)> {
        vec![
            ("IPTV_EVENT", event.as_str().to_owned()),
            ("IPTV_INTERFACE", self.interface.clone()),
            ("IPTV_ADDRESS", self.address.clone()),
            ("IPTV_PREFIX", self.prefix.to_string()),
            ("IPTV_NETMASK", self.netmask.clone()),
            ("IPTV_GATEWAY", self.gateway.clone()),
            ("IPTV_SERVER", self.server.clone()),
            ("IPTV_LEASE_SECONDS", self.lease_seconds.to_string()),
            ("IPTV_RENEW_SECONDS", self.renew_seconds.to_string()),
        ]
    }
}

/// Runs `script` with the report in its environment. A script that fails is
/// logged and does not stop the daemon: the lease itself is still held.
fn report_lease(script: Option<&PathBuf>, lease: &LeaseReport, event: LeaseEvent) {
    if event == LeaseEvent::Expired {
        tracing::warn!(interface = %lease.interface, "DHCP lease expired; address is no longer ours");
    } else {
        tracing::info!(
            interface = %lease.interface,
            address = %lease.address,
            prefix = lease.prefix,
            gateway = %lease.gateway,
            lease_seconds = lease.lease_seconds,
            event = event.as_str(),
            "DHCP lease held"
        );
    }
    let Some(script) = script else {
        return;
    };
    let mut command = Command::new(script);
    for (key, value) in lease.environment(event) {
        command.env(key, value);
    }
    match command.status() {
        Ok(status) if status.success() => {}
        Ok(status) => {
            tracing::warn!(%status, script = %script.display(), "lease report script failed");
        }
        Err(error) => {
            tracing::warn!(%error, script = %script.display(), "lease report script could not run");
        }
    }
}

/// Acquires one lease for `config.interface`, off the async runtime because
/// the exchange sleeps on raw-socket reads.
async fn acquire(config: &Config) -> Result<DhcpLease, DhcpError> {
    let config = config.clone();
    tokio::task::spawn_blocking(move || {
        let client = ClientConfig {
            user: config.dhcp_user.as_str(),
            password: config.dhcp_password.as_str(),
            stb_id_prefix: &config.stb_id_prefix,
        };
        acquire_lease(&config.interface, &client).map(|(lease, _)| lease)
    })
    .await
    .map_err(|error| DhcpError::Platform(crate::dhcp::LinuxError::Io(error.to_string())))?
}

/// Holds the lease for `config.interface`, renewing at T1 until shutdown.
///
/// netifd owns the address, so nothing is configured here: a lease that can no
/// longer be kept ends the daemon with an error, and netifd restarts it.
///
/// # Errors
/// Returns the DHCP error that made the lease unusable: it expired, or the
/// server moved the client to an address netifd has not configured.
pub async fn daemon(config: Config, script: Option<PathBuf>) -> Result<(), DhcpError> {
    let (shutdown_tx, shutdown) = tokio::sync::watch::channel(false);
    // netifd stops the daemon with SIGTERM (`proto_kill_command`).
    let signals = tokio::spawn(crate::startup::signal(shutdown_tx));
    let result = hold(&config, script.as_ref(), shutdown).await;
    signals.abort();
    result
}

/// Renews the binding `held` describes, off the async runtime like `acquire`.
///
/// # Errors
/// Returns the exchange error; the caller falls back to a full acquisition.
async fn renew(config: &Config, held: &DhcpLease) -> Result<DhcpLease, DhcpError> {
    let config = config.clone();
    let held = held.clone();
    tokio::task::spawn_blocking(move || {
        let client = ClientConfig {
            user: config.dhcp_user.as_str(),
            password: config.dhcp_password.as_str(),
            stb_id_prefix: &config.stb_id_prefix,
        };
        renew_lease(&config.interface, &client, &held)
    })
    .await
    .map_err(|error| DhcpError::Platform(crate::dhcp::LinuxError::Io(error.to_string())))?
}

/// Holds one lease for the interface, renewing it until shutdown.
async fn hold(
    config: &Config,
    script: Option<&PathBuf>,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) -> Result<(), DhcpError> {
    let mut held: Option<DhcpLease> = None;
    let mut expires_at = Instant::now();
    let mut attempt_at = Instant::now();

    loop {
        if !sleep_until(attempt_at, &mut shutdown).await {
            return Ok(());
        }
        // Renew what we hold. A fresh DISCOVER can be answered with any free
        // address, so it is the fallback, not the renewal.
        let renewed = match &held {
            Some(lease) => match renew(config, lease).await {
                Ok(renewed) => Ok(renewed),
                Err(error) => {
                    tracing::warn!(
                        %error,
                        "DHCP renewal request was refused; acquiring a new lease"
                    );
                    acquire(config).await
                }
            },
            None => acquire(config).await,
        };
        match renewed {
            Ok(renewed) => {
                let lease = LeaseReport::from_lease(&config.interface, &renewed);
                let same_address = held
                    .as_ref()
                    .is_none_or(|previous| previous.address == renewed.address);
                report_lease(
                    script,
                    &lease,
                    if held.is_some() && same_address {
                        LeaseEvent::Renewed
                    } else {
                        LeaseEvent::Bound
                    },
                );
                if !same_address {
                    // The media server is bound to the old address; netifd has
                    // to reconfigure and restart everything.
                    return Err(DhcpError::Platform(crate::dhcp::LinuxError::Io(
                        "DHCP renewal assigned a different address".to_owned(),
                    )));
                }
                let now = Instant::now();
                expires_at = now + Duration::from_secs(u64::from(renewed.lease_seconds));
                attempt_at = now + Duration::from_secs(u64::from(renewed.lease_seconds / 2));
                held = Some(renewed);
            }
            Err(error) => match &held {
                None => {
                    tracing::warn!(%error, "DHCP lease acquisition failed");
                    attempt_at = Instant::now() + MIN_RETRY;
                }
                Some(lease) => {
                    let now = Instant::now();
                    if now >= expires_at {
                        report_lease(
                            script,
                            &LeaseReport::from_lease(&config.interface, lease),
                            LeaseEvent::Expired,
                        );
                        return Err(error);
                    }
                    tracing::warn!(%error, "DHCP renewal failed; retrying");
                    attempt_at = now + retry_delay(expires_at - now);
                }
            },
        }
    }
}

/// Sleeps until `deadline`, returning false when shutdown arrived first.
async fn sleep_until(deadline: Instant, shutdown: &mut tokio::sync::watch::Receiver<bool>) -> bool {
    let sleep = tokio::time::sleep_until(tokio::time::Instant::from_std(deadline));
    tokio::select! {
        () = sleep => true,
        _ = shutdown.wait_for(|stop| *stop) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> LeaseReport {
        LeaseReport {
            interface: "eth1".to_owned(),
            address: "10.0.0.2".to_owned(),
            prefix: 16,
            netmask: "255.255.0.0".to_owned(),
            gateway: "10.0.0.1".to_owned(),
            server: "10.0.0.1".to_owned(),
            lease_seconds: 3600,
            renew_seconds: 1800,
        }
    }

    #[test]
    fn renew_seconds_is_half_the_lease() {
        let lease = LeaseReport::from_lease("eth1", &crate::dhcp::test_lease());
        assert_eq!(lease.renew_seconds, lease.lease_seconds / 2);
    }

    #[test]
    fn events_use_the_handler_spelling() {
        assert_eq!(LeaseEvent::Bound.as_str(), "bound");
        assert_eq!(LeaseEvent::Renewed.as_str(), "renew");
        assert_eq!(LeaseEvent::Expired.as_str(), "expired");
    }

    #[test]
    fn environment_carries_the_report() {
        let environment = sample().environment(LeaseEvent::Bound);
        assert!(environment.contains(&("IPTV_EVENT", "bound".to_owned())));
        assert!(environment.contains(&("IPTV_ADDRESS", "10.0.0.2".to_owned())));
        assert!(environment.contains(&("IPTV_PREFIX", "16".to_owned())));
        assert!(environment.contains(&("IPTV_GATEWAY", "10.0.0.1".to_owned())));
        assert!(environment.contains(&("IPTV_LEASE_SECONDS", "3600".to_owned())));
    }

    #[test]
    fn retry_stays_within_bounds() {
        assert_eq!(
            retry_delay(Duration::from_secs(3600)),
            crate::renewal::MAX_RETRY
        );
        assert_eq!(retry_delay(Duration::from_secs(8)), MIN_RETRY);
    }
}
