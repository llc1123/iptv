use super::packet::{DhcpReply, PacketError};
use std::net::Ipv4Addr;
use std::time::{Duration, Instant};
use thiserror::Error;
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DhcpLease {
    pub address: Ipv4Addr,
    pub router: Ipv4Addr,
    pub server: Ipv4Addr,
    pub subnet_mask: Ipv4Addr,
    pub lease_seconds: u32,
    pub vendor_specific: Vec<u8>,
    expires_at: Instant,
}
impl DhcpLease {
    pub fn prefix_length(&self) -> u8 {
        prefix_length(self.subnet_mask).unwrap_or(0)
    }
    pub fn is_expired(&self, now: Instant) -> bool {
        now >= self.expires_at
    }
    pub const fn expires_at(&self) -> Instant {
        self.expires_at
    }
}
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum LeaseError {
    #[error("DHCP ACK lacks required lease data")]
    MissingField,
    #[error("DHCP subnet mask is not contiguous")]
    InvalidPrefix,
    #[error("DHCP router is outside the leased subnet")]
    RouterOutsideSubnet,
    #[error("DHCP lease duration is invalid")]
    InvalidDuration,
    #[error("DHCP lease expiry is invalid")]
    InvalidExpiry,
    #[error(transparent)]
    Packet(#[from] PacketError),
}
pub fn from_ack(reply: DhcpReply, acquired_at: Instant) -> Result<DhcpLease, LeaseError> {
    let lease_seconds = reply.lease_seconds.ok_or(LeaseError::MissingField)?;
    let vendor_specific = reply.vendor_specific.ok_or(LeaseError::MissingField)?;
    let subnet_mask = reply.subnet_mask.ok_or(LeaseError::MissingField)?;
    let router = reply.router.ok_or(LeaseError::MissingField)?;
    let server = reply.server.ok_or(LeaseError::MissingField)?;
    let prefix = prefix_length(subnet_mask).ok_or(LeaseError::InvalidPrefix)?;
    let mask = u32::from_be_bytes(subnet_mask.octets());
    if (u32::from_be_bytes(reply.address.octets()) & mask)
        != (u32::from_be_bytes(router.octets()) & mask)
    {
        return Err(LeaseError::RouterOutsideSubnet);
    }
    if lease_seconds == 0 {
        return Err(LeaseError::InvalidDuration);
    }
    let expires_at = acquired_at
        .checked_add(Duration::from_secs(u64::from(lease_seconds)))
        .ok_or(LeaseError::InvalidExpiry)?;
    let _ = prefix;
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
    let expected = if prefix == 0 {
        0
    } else {
        u32::MAX << (32 - u32::from(prefix))
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
            vendor_specific: Some(vec![1]),
            subnet_mask: Some(mask),
            router: Some(router),
        }
    }
    #[test]
    fn lease_interpretation_validates_prefix_router_duration_and_expiry() {
        let now = Instant::now();
        let lease = from_ack(
            reply(
                Ipv4Addr::new(255, 255, 255, 0),
                Ipv4Addr::new(192, 0, 2, 1),
                60,
            ),
            now,
        )
        .expect("valid ACK yields a lease");
        assert_eq!(lease.prefix_length(), 24);
        assert!(!lease.is_expired(now));
        assert!(lease.is_expired(now + Duration::from_secs(60)));
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
