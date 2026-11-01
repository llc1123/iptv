use std::fmt;
use std::net::SocketAddr;
use std::path::Path;

use crate::config::Config;
use crate::lifecycle::{InstanceLock, InstanceLockError, PortGuard, PortGuardError};
use crate::preflight::{PythonPreflight, StartupRefusal};

/// Owns the lock and listener acquired during startup admission.
pub struct StartupGate {
    lock: InstanceLock,
    port: PortGuard,
}

impl fmt::Debug for StartupGate {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("StartupGate([redacted])")
    }
}

impl StartupGate {
    /// Borrows the retained instance lock.
    #[must_use]
    pub const fn lock(&self) -> &InstanceLock {
        &self.lock
    }

    /// Borrows the retained listener guard.
    #[must_use]
    pub const fn port(&self) -> &PortGuard {
        &self.port
    }
}

/// Acquires startup resources in preflight, lock, listener order.
///
/// # Errors
/// Returns a fixed refusal and releases any newly acquired lock if listener
/// binding fails.
pub fn acquire_startup_gate(
    config: &Config,
    state_path: &Path,
    preflight: &PythonPreflight,
) -> Result<StartupGate, StartupRefusal> {
    preflight.inspect()?;
    let address = format!("{}:{}", config.host, config.port)
        .parse::<SocketAddr>()
        .map_err(|_| StartupRefusal::PortConflict)?;
    acquire_startup_gate_after_preflight(state_path, address)
}

#[cfg(test)]
fn acquire_startup_gate_at(
    state_path: &Path,
    preflight: &PythonPreflight,
    address: SocketAddr,
) -> Result<StartupGate, StartupRefusal> {
    preflight.inspect()?;
    acquire_startup_gate_after_preflight(state_path, address)
}

fn acquire_startup_gate_after_preflight(
    state_path: &Path,
    address: SocketAddr,
) -> Result<StartupGate, StartupRefusal> {
    let lock = InstanceLock::create_new(state_path).map_err(|error| map_lock_error(&error))?;
    let port = PortGuard::bind(address).map_err(|error| map_port_error(&error))?;
    Ok(StartupGate { lock, port })
}

fn map_lock_error(error: &InstanceLockError) -> StartupRefusal {
    match error {
        InstanceLockError::AlreadyHeld => StartupRefusal::InstanceLockBusy,
        InstanceLockError::Malformed => StartupRefusal::InstanceLockMalformed,
        InstanceLockError::Mismatched => StartupRefusal::InstanceLockMismatch,
        InstanceLockError::Io(_) | InstanceLockError::Encode(_) => {
            StartupRefusal::InstanceLockInaccessible
        }
    }
}

fn map_port_error(error: &PortGuardError) -> StartupRefusal {
    match error {
        PortGuardError::Bind(_) => StartupRefusal::PortConflict,
    }
}

impl From<StartupRefusal> for crate::error::AppError {
    fn from(_: StartupRefusal) -> Self {
        Self::Startup(crate::error::StartupError::Startup)
    }
}

#[cfg(test)]
#[path = "startup_gate_tests.rs"]
mod tests;
