//! Lease-only mode: acquire the DHCP lease, print it for the caller and stop.
//!
//! `iptv-media-server --lease-only` is what the `OpenWrt` protocol handler
//! runs. The server never serves HTTP in this mode and does not touch the
//! interface: netifd owns the address and routes, and the handler feeds it the
//! values printed here.

use crate::config::Config;
use crate::dhcp::{ClientConfig, DhcpError, acquire_lease};

/// The fields the protocol handler reads, in the order they are printed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LeaseReport {
    pub interface: String,
    pub address: String,
    pub prefix: u32,
    pub netmask: String,
    pub gateway: String,
    pub server: String,
    pub lease_seconds: u32,
    /// When the handler should ask for a renewal: half the lease, as the
    /// server's own renewal loop does.
    pub renew_seconds: u32,
}

impl LeaseReport {
    /// The shell-assignable `KEY=value` lines the protocol handler evaluates.
    pub fn lines(&self) -> String {
        let mut lines = String::new();
        for (key, value) in [
            ("IPTV_INTERFACE", self.interface.as_str()),
            ("IPTV_ADDRESS", self.address.as_str()),
            ("IPTV_NETMASK", self.netmask.as_str()),
            ("IPTV_PREFIX", &self.prefix.to_string()),
            ("IPTV_GATEWAY", self.gateway.as_str()),
            ("IPTV_SERVER", self.server.as_str()),
            ("IPTV_LEASE_SECONDS", &self.lease_seconds.to_string()),
            ("IPTV_RENEW_SECONDS", &self.renew_seconds.to_string()),
        ] {
            lines.push_str(key);
            lines.push('=');
            lines.push_str(value);
            lines.push('\n');
        }
        lines
    }
}

/// Acquires one lease for `config.interface`.
///
/// # Errors
/// Returns the DHCP error the exchange produced, so the handler can report it
/// through netifd instead of failing silently.
pub fn acquire(config: &Config) -> Result<LeaseReport, DhcpError> {
    let client = ClientConfig {
        user: config.dhcp_user.as_str(),
        password: config.dhcp_password.as_str(),
        stb_id_prefix: &config.stb_id_prefix,
    };
    let (lease, _) = acquire_lease(&config.interface, &client)?;
    Ok(LeaseReport {
        interface: config.interface.clone(),
        address: lease.address.to_string(),
        prefix: lease.prefix_length(),
        netmask: lease.subnet_mask.to_string(),
        gateway: lease.router.to_string(),
        server: lease.server.to_string(),
        lease_seconds: lease.lease_seconds,
        renew_seconds: lease.lease_seconds / 2,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_prints_shell_assignments() {
        let report = LeaseReport {
            interface: "eth1".to_owned(),
            address: "10.0.0.2".to_owned(),
            prefix: 16,
            netmask: "255.255.0.0".to_owned(),
            gateway: "10.0.0.1".to_owned(),
            server: "10.0.0.1".to_owned(),
            lease_seconds: 3600,
            renew_seconds: 1800,
        };
        assert_eq!(
            report.lines(),
            "IPTV_INTERFACE=eth1\nIPTV_ADDRESS=10.0.0.2\nIPTV_NETMASK=255.255.0.0\n\
             IPTV_PREFIX=16\nIPTV_GATEWAY=10.0.0.1\nIPTV_SERVER=10.0.0.1\n\
             IPTV_LEASE_SECONDS=3600\nIPTV_RENEW_SECONDS=1800\n"
        );
    }
}
