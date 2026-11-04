//! Validated DHCP lease values and expiry semantics.

#![allow(clippy::missing_errors_doc)]

use std::net::Ipv4Addr;
use std::time::{Duration, Instant};

use thiserror::Error;

use super::packet::{DhcpReply, PacketError};

/// A validated address lease returned by a DHCPACK.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DhcpLease {
    /// Address assigned to the interface.
    pub address: Ipv4Addr,
    /// Router used for provider routes.
    pub router: Ipv4Addr,
    /// DHCP server identifier.
    pub server: Ipv4Addr,
    /// Contiguous subnet mask.
    pub subnet_mask: Ipv4Addr,
    /// Lease duration in seconds.
    pub lease_seconds: u32,
    /// Vendor-specific-information DHCP option 125 bytes.
    pub vendor_specific: Vec<u8>,
    expires_at: Instant,
}

impl DhcpLease {
    /// Returns the CIDR prefix length represented by the subnet mask.
    #[must_use]
    pub fn prefix_length(&self) -> u8 {
        prefix_length(self.subnet_mask).map_or(0, |prefix| prefix)
    }

    /// Returns whether the lease is expired at a monotonic instant.
    #[must_use]
    pub fn is_expired(&self, now: Instant) -> bool {
        now >= self.expires_at
    }

    /// Returns the monotonic expiry instant.
    #[must_use]
    pub const fn expires_at(&self) -> Instant {
        self.expires_at
    }
}

/// Lease validation failures.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum LeaseError {
    /// The ACK did not contain one of the required fields.
    #[error("DHCP ACK lacks required lease data")]
    MissingField,
    /// The subnet mask is not contiguous.
    #[error("DHCP subnet mask is not contiguous")]
    InvalidPrefix,
    /// The router is not on the leased subnet.
    #[error("DHCP router is outside the leased subnet")]
    RouterOutsideSubnet,
    /// The server returned a zero-duration lease.
    #[error("DHCP lease duration is invalid")]
    InvalidDuration,
    /// The lease expiry could not be represented.
    #[error("DHCP lease expiry is invalid")]
    InvalidExpiry,
    /// Packet decoding failed before lease validation.
    #[error(transparent)]
    Packet(#[from] PacketError),
}

/// Converts a validated ACK into a lease with an injected acquisition instant.
pub fn from_ack(reply: DhcpReply, acquired_at: Instant) -> Result<DhcpLease, LeaseError> {
    let lease_seconds = reply.lease_seconds.ok_or(LeaseError::MissingField)?;
    let vendor_specific = reply.vendor_specific.ok_or(LeaseError::MissingField)?;
    let subnet_mask = reply.subnet_mask.ok_or(LeaseError::MissingField)?;
    let router = reply.router.ok_or(LeaseError::MissingField)?;
    let server = reply.server.ok_or(LeaseError::MissingField)?;
    let _prefix = prefix_length(subnet_mask).ok_or(LeaseError::InvalidPrefix)?;
    let mask = u32::from_be_bytes(subnet_mask.octets());
    let address = u32::from_be_bytes(reply.address.octets());
    let router_value = u32::from_be_bytes(router.octets());
    if (address & mask) != (router_value & mask) {
        return Err(LeaseError::RouterOutsideSubnet);
    }
    if lease_seconds == 0 {
        return Err(LeaseError::InvalidDuration);
    }
    let expires_at = acquired_at
        .checked_add(Duration::from_secs(u64::from(lease_seconds)))
        .ok_or(LeaseError::InvalidExpiry)?;
    Ok(DhcpLease {
        address: reply.address,
        router,
        server,
        subnet_mask,
        lease_seconds,
        vendor_specific,
        expires_at,
    })
}

fn prefix_length(mask: Ipv4Addr) -> Option<u8> {
    let bits = u32::from_be_bytes(mask.octets());
    let prefix = u8::try_from(bits.leading_ones()).ok()?;
    let expected = match prefix {
        0 => 0,
        32 => u32::MAX,
        value => u32::MAX << (32 - u32::from(value)),
    };
    (bits == expected).then_some(prefix)
}

#[cfg(test)]
pub(crate) fn test_lease() -> DhcpLease {
    let acquired_at = Instant::now();
    DhcpLease {
        address: Ipv4Addr::new(192, 0, 2, 44),
        router: Ipv4Addr::new(192, 0, 2, 1),
        server: Ipv4Addr::new(192, 0, 2, 1),
        subnet_mask: Ipv4Addr::new(255, 255, 255, 0),
        lease_seconds: 60,
        vendor_specific: vec![0x10],
        expires_at: acquired_at + Duration::from_secs(60),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dhcp::packet::ReplyKind;

    fn reply(mask: Ipv4Addr, router: Ipv4Addr, seconds: u32) -> DhcpReply {
        DhcpReply {
            xid: 1,
            kind: ReplyKind::Ack,
            address: Ipv4Addr::new(192, 0, 2, 44),
            source: Ipv4Addr::new(192, 0, 2, 1),
            server: Some(Ipv4Addr::new(192, 0, 2, 1)),
            lease_seconds: Some(seconds),
            vendor_specific: Some(vec![1, 2, 3]),
            subnet_mask: Some(mask),
            router: Some(router),
        }
    }

    #[test]
    fn lease_has_prefix_and_expiry() {
        let acquired = Instant::now();
        let result = from_ack(
            reply(
                Ipv4Addr::new(255, 255, 255, 0),
                Ipv4Addr::new(192, 0, 2, 1),
                60,
            ),
            acquired,
        );
        assert!(result.is_ok(), "fixture lease must validate");
        let Ok(lease) = result else { return };
        assert_eq!(lease.prefix_length(), 24);
        assert!(!lease.is_expired(acquired));
        assert!(lease.is_expired(acquired + Duration::from_secs(60)));
    }

    #[test]
    fn lease_rejects_invalid_prefix_router_and_duration() {
        let now = Instant::now();
        assert_eq!(
            from_ack(
                reply(
                    Ipv4Addr::new(255, 0, 255, 0),
                    Ipv4Addr::new(192, 0, 2, 1),
                    60
                ),
                now
            ),
            Err(LeaseError::InvalidPrefix)
        );
        assert_eq!(
            from_ack(
                reply(
                    Ipv4Addr::new(255, 255, 255, 0),
                    Ipv4Addr::new(192, 0, 3, 1),
                    60
                ),
                now
            ),
            Err(LeaseError::RouterOutsideSubnet)
        );
        assert_eq!(
            from_ack(
                reply(
                    Ipv4Addr::new(255, 255, 255, 0),
                    Ipv4Addr::new(192, 0, 2, 1),
                    0
                ),
                now
            ),
            Err(LeaseError::InvalidDuration)
        );
    }
}
