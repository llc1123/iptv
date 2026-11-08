//! Raw-L2 DHCP acquisition and Linux interface setup.

mod lease;
mod linux;
pub mod packet;

use thiserror::Error;

#[cfg(test)]
pub(crate) use lease::test_lease;
pub use lease::{DhcpLease, LeaseError};
pub use linux::{
    ClientConfig, DEFAULT_ROUTE_TABLE, LinuxError, acquire_lease, configure_interface,
    interface_mac, remove_address,
};
pub use packet::{DhcpReply, MacAddress, PacketError, ReplyKind};

#[derive(Debug, Error, PartialEq, Eq)]
pub enum DhcpError {
    #[error(transparent)]
    Packet(#[from] PacketError),
    #[error(transparent)]
    Lease(#[from] LeaseError),
    #[error(transparent)]
    Platform(#[from] LinuxError),
    #[error(transparent)]
    Crypto(#[from] crate::crypto::CryptoError),
}
