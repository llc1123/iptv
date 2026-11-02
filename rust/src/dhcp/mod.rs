//! Raw-L2 DHCP acquisition and interface setup.

pub mod lease;
pub mod linux;
pub mod packet;

use std::time::Instant;

use thiserror::Error;

pub use lease::{DhcpLease, LeaseError};
pub use linux::{DhcpClient, LinuxError, Platform, SetupError, SetupStage, configure_interface};
pub use packet::{DhcpReply, MacAddress, PacketError, ReplyKind};

/// Failures from the DHCP acquisition boundary.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum DhcpError {
    /// A packet failed strict framing or DHCP validation.
    #[error(transparent)]
    Packet(#[from] PacketError),
    /// A validated ACK contained an unusable lease.
    #[error(transparent)]
    Lease(#[from] LeaseError),
    /// The platform could not perform the requested operation.
    #[error(transparent)]
    Platform(#[from] LinuxError),
    /// CTC Option 60 could not be encoded.
    #[error(transparent)]
    Crypto(#[from] crate::crypto::CryptoError),
    /// A DHCP stage exceeded its bounded timeout.
    #[error("DHCP stage timed out")]
    StageTimeout,
}

/// The result of setup or a later lifecycle event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LifecycleResult {
    /// The address and all requested routes were applied.
    Applied(DhcpLease),
    /// Kernel state was intentionally left in place after mutation.
    Preserved {
        /// The lease whose address remains configured.
        lease: DhcpLease,
        /// The reason the lifecycle stopped.
        cause: LifecycleCause,
        /// The setup stage that failed, when setup failed.
        failed_stage: Option<SetupStage>,
    },
}

/// A lifecycle event that never rolls back an already-applied lease.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LifecycleCause {
    /// Startup failed after interface mutation began.
    StartupFailure,
    /// Shutdown began after interface mutation.
    Shutdown,
    /// The lease reached its expiry time.
    LeaseExpired,
}

/// Returns a typed preserved-state result for a lease that must remain configured.
#[must_use]
pub fn preserve_state(lease: DhcpLease, cause: LifecycleCause) -> LifecycleResult {
    LifecycleResult::Preserved {
        lease,
        cause,
        failed_stage: None,
    }
}

/// Returns whether a lease is expired at the supplied monotonic instant.
#[must_use]
pub fn lease_expired(lease: &DhcpLease, now: Instant) -> bool {
    lease.is_expired(now)
}
