//! Raw-L2 DHCP acquisition and Linux interface setup.

mod lease;
mod linux;
pub mod packet;

use thiserror::Error;

pub use lease::{DhcpLease, LeaseError};
pub use linux::{acquire_lease, configure_interface, ClientConfig, LinuxError};
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
