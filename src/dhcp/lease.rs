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
    /// Option 125, which the provider always sends; required like Python.
    pub vendor_specific: Vec<u8>,
    expires_at: Instant,
}

impl DhcpLease {
    pub fn prefix_length(&self) -> u32 {
        u32::from(self.subnet_mask).count_ones()
    }

    pub const fn expires_at(&self) -> Instant {
        self.expires_at
    }
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum LeaseError {
    #[error("DHCP ACK lacks required lease data")]
    MissingField,
    /// A zero-length lease would make renewal spin.
    #[error("DHCP lease duration is zero")]
    InvalidDuration,
    #[error(transparent)]
    Packet(#[from] PacketError),
}

/// The lease an ACK grants, counted from `acquired_at`.
pub fn from_ack(reply: DhcpReply, acquired_at: Instant) -> Result<DhcpLease, LeaseError> {
    let lease_seconds = reply.lease_seconds.ok_or(LeaseError::MissingField)?;
    if lease_seconds == 0 {
        return Err(LeaseError::InvalidDuration);
    }
    Ok(DhcpLease {
        address: reply.address,
        router: reply.router.ok_or(LeaseError::MissingField)?,
        server: reply.server.ok_or(LeaseError::MissingField)?,
        subnet_mask: reply.subnet_mask.ok_or(LeaseError::MissingField)?,
        lease_seconds,
        vendor_specific: reply.vendor_specific.ok_or(LeaseError::MissingField)?,
        expires_at: acquired_at + Duration::from_secs(u64::from(lease_seconds)),
    })
}

#[cfg(test)]
pub(crate) fn test_lease() -> DhcpLease {
    DhcpLease {
        address: Ipv4Addr::new(192, 0, 2, 44),
        router: Ipv4Addr::new(192, 0, 2, 1),
        server: Ipv4Addr::new(192, 0, 2, 1),
        subnet_mask: Ipv4Addr::new(255, 255, 255, 0),
        lease_seconds: 60,
        vendor_specific: vec![0x10],
        expires_at: Instant::now() + Duration::from_secs(60),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dhcp::packet::ReplyKind;

    fn reply(seconds: u32, router: Option<Ipv4Addr>) -> DhcpReply {
        DhcpReply {
            xid: 1,
            kind: ReplyKind::Ack,
            address: Ipv4Addr::new(192, 0, 2, 44),
            source: Ipv4Addr::new(192, 0, 2, 1),
            server: Some(Ipv4Addr::new(192, 0, 2, 1)),
            lease_seconds: Some(seconds),
            vendor_specific: Some(vec![1]),
            subnet_mask: Some(Ipv4Addr::new(255, 255, 255, 0)),
            router,
        }
    }

    #[test]
    fn ack_yields_a_lease_only_with_every_required_field() {
        let now = Instant::now();
        let router = Some(Ipv4Addr::new(192, 0, 2, 1));
        let lease = from_ack(reply(60, router), now).expect("valid ACK yields a lease");
        assert_eq!(lease.prefix_length(), 24);
        assert_eq!(lease.expires_at(), now + Duration::from_secs(60));
        assert_eq!(
            from_ack(reply(60, None), now),
            Err(LeaseError::MissingField)
        );
        assert_eq!(
            from_ack(reply(0, router), now),
            Err(LeaseError::InvalidDuration)
        );
    }
}
