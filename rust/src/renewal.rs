//! Keeps the DHCP lease alive for the lifetime of the server.
//!
//! The provider authenticates every exchange with Option 60, so renewal
//! repeats the full DISCOVER/REQUEST exchange used at startup instead of a
//! bare RFC 2131 RENEWING request. The first attempt runs at T1 (half the
//! lease); failures are retried until the lease expires.

use crate::dhcp::{ClientConfig, DhcpError, DhcpLease, acquire_lease, remove_address};
use std::time::Duration;
use thiserror::Error;
use tokio::{
    sync::watch,
    time::{Instant, sleep_until},
};

const MIN_RETRY: Duration = Duration::from_secs(5);
const MAX_RETRY: Duration = Duration::from_secs(60);

#[derive(Debug, Error)]
pub enum RenewalError {
    #[error("DHCP lease expired without a successful renewal")]
    Expired,
    #[error("DHCP renewal assigned a different address; restart required")]
    AddressChanged,
}

pub struct Renewal {
    pub interface: String,
    pub user: String,
    pub password: String,
    pub stb_id_prefix: String,
}

impl Renewal {
    /// Renews `lease` until shutdown. Returns an error when the lease can no
    /// longer be used: it expired, or the server moved the client to another
    /// address that the running EPG session is not bound to.
    pub async fn run(
        self,
        mut lease: DhcpLease,
        mut shutdown: watch::Receiver<bool>,
    ) -> Result<(), RenewalError> {
        let mut attempt_at = renew_at(&lease);
        loop {
            tokio::select! {
                () = sleep_until(attempt_at) => {}
                _ = shutdown.wait_for(|stop| *stop) => return Ok(()),
            }
            match self.acquire().await {
                Ok(renewed) if renewed.address == lease.address => {
                    tracing::info!(lease_seconds = renewed.lease_seconds, "DHCP lease renewed");
                    lease = renewed;
                    attempt_at = renew_at(&lease);
                }
                Ok(_) => {
                    self.release(&lease);
                    return Err(RenewalError::AddressChanged);
                }
                Err(error) => {
                    let now = Instant::now();
                    let expires_at = Instant::from_std(lease.expires_at());
                    if now >= expires_at {
                        self.release(&lease);
                        return Err(RenewalError::Expired);
                    }
                    attempt_at = now + retry_delay(expires_at - now);
                    tracing::warn!(%error, "DHCP renewal failed; retrying");
                }
            }
        }
    }

    /// Drops the address of a lease that is no longer ours, best effort: the
    /// restarted service configures the interface again either way.
    fn release(&self, lease: &DhcpLease) {
        if let Err(error) = remove_address(&self.interface, lease) {
            tracing::warn!(%error, "previous DHCP address could not be removed");
        }
    }

    async fn acquire(&self) -> Result<DhcpLease, DhcpError> {
        let interface = self.interface.clone();
        let user = self.user.clone();
        let password = self.password.clone();
        let prefix = self.stb_id_prefix.clone();
        tokio::task::spawn_blocking(move || {
            let config = ClientConfig {
                user: &user,
                password: &password,
                stb_id_prefix: &prefix,
            };
            acquire_lease(&interface, &config).map(|(lease, _)| lease)
        })
        .await
        .unwrap_or_else(|error| {
            Err(DhcpError::Platform(crate::dhcp::LinuxError::Io(
                error.to_string(),
            )))
        })
    }
}

/// T1: half of the lease duration before expiry.
fn renew_at(lease: &DhcpLease) -> Instant {
    Instant::from_std(lease.expires_at()) - Duration::from_secs(u64::from(lease.lease_seconds)) / 2
}

/// Retries at half the remaining lifetime, bounded so a short lease still
/// gets several attempts and a long one is not hammered.
fn retry_delay(remaining: Duration) -> Duration {
    (remaining / 2).clamp(MIN_RETRY, MAX_RETRY).min(remaining)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_renewal_runs_at_half_the_lease() {
        let lease = crate::dhcp::test_lease();
        let expires_at = Instant::from_std(lease.expires_at());
        assert_eq!(expires_at - renew_at(&lease), Duration::from_secs(30));
    }

    #[test]
    fn retries_are_bounded_and_never_pass_expiry() {
        assert_eq!(retry_delay(Duration::from_secs(3600)), MAX_RETRY);
        assert_eq!(
            retry_delay(Duration::from_secs(40)),
            Duration::from_secs(20)
        );
        assert_eq!(retry_delay(Duration::from_secs(8)), MIN_RETRY);
        assert_eq!(retry_delay(Duration::from_secs(2)), Duration::from_secs(2));
    }
}
