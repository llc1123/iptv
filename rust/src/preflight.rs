//! Strict, read-only Python process preflight for startup admission.

use std::fmt;
use std::path::PathBuf;

pub(crate) const DEFAULT_INTERFACE: &str = "eth0";
pub(crate) const DEFAULT_HOST: &str = "127.0.0.1";
pub(crate) const DEFAULT_GUIDE_CACHE: &str = "var/logo-catalog/guide.json";
pub(crate) const DEFAULT_LOGO_CATALOG: &str = "var/logo-catalog/catalog.json";

/// The fixed public refusal classes for startup admission.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum StartupRefusal {
    /// An exact Python server identity is already running.
    PythonConflict,
    /// A direct Python server has a different identity.
    PythonIdentityMismatch,
    /// A wrapper, module, shell, or supervisor launched the server.
    WrapperMismatch,
    /// A required `/proc` field could not be read or decoded.
    ProcFieldInaccessible,
    /// More than one Python server candidate was observed.
    MultiplePythonCandidates,
    /// The Rust instance lock is already held.
    InstanceLockBusy,
    /// The Rust instance lock record is malformed.
    InstanceLockMalformed,
    /// The Rust instance lock record has the wrong version or owner.
    InstanceLockMismatch,
    /// The state directory or lock file could not be accessed.
    InstanceLockInaccessible,
    /// The requested listener could not be held.
    PortConflict,
}

impl fmt::Debug for StartupRefusal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.name())
    }
}

impl fmt::Display for StartupRefusal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.name())
    }
}

impl std::error::Error for StartupRefusal {}

impl StartupRefusal {
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::PythonConflict => "python conflict",
            Self::PythonIdentityMismatch => "python identity mismatch",
            Self::WrapperMismatch => "python wrapper mismatch",
            Self::ProcFieldInaccessible => "proc field inaccessible",
            Self::MultiplePythonCandidates => "multiple python candidates",
            Self::InstanceLockBusy => "instance lock busy",
            Self::InstanceLockMalformed => "instance lock malformed",
            Self::InstanceLockMismatch => "instance lock mismatch",
            Self::InstanceLockInaccessible => "instance lock inaccessible",
            Self::PortConflict => "port conflict",
        }
    }
}

/// The complete non-secret identity expected from a Python server process.
#[derive(Clone, PartialEq, Eq)]
pub struct PythonProcessSpec {
    /// Canonical Python interpreter path.
    pub interpreter: PathBuf,
    /// Exact console entrypoint name.
    pub entrypoint: String,
    /// Expected process UID.
    pub uid: u32,
    /// Expected process working directory.
    pub cwd: PathBuf,
    /// Expected IPTV interface.
    pub interface: String,
    /// Expected HTTP host.
    pub host: String,
    /// Expected HTTP port.
    pub port: u16,
    /// Expected guide-cache path.
    pub guide_cache: PathBuf,
    /// Expected logo-catalog path.
    pub logo_catalog: PathBuf,
    /// Expected state path.
    pub state_path: PathBuf,
    /// Exact command-line arguments observed by `/proc`.
    pub argv: Vec<String>,
}

impl fmt::Debug for PythonProcessSpec {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("PythonProcessSpec([redacted])")
    }
}

impl PythonProcessSpec {
    /// Creates a Python identity specification without inspecting the host.
    #[must_use]
    #[expect(
        clippy::too_many_arguments,
        reason = "the identity is intentionally complete"
    )]
    pub fn new(
        interpreter: PathBuf,
        entrypoint: String,
        uid: u32,
        cwd: PathBuf,
        interface: String,
        host: String,
        port: u16,
        guide_cache: PathBuf,
        logo_catalog: PathBuf,
        state_path: PathBuf,
        argv: Vec<String>,
    ) -> Self {
        Self {
            interpreter,
            entrypoint,
            uid,
            cwd,
            interface,
            host,
            port,
            guide_cache,
            logo_catalog,
            state_path,
            argv,
        }
    }
}

/// Read-only `/proc` preflight rooted at an injectable directory.
#[derive(Clone, PartialEq, Eq)]
pub struct PythonPreflight {
    pub(crate) proc_root: PathBuf,
    pub(crate) expected: Option<PythonProcessSpec>,
}

impl fmt::Debug for PythonPreflight {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("PythonPreflight([redacted])")
    }
}

impl PythonPreflight {
    /// Creates a preflight using a caller-supplied `/proc` root and identity.
    #[must_use]
    pub fn new(proc_root: PathBuf, expected: PythonProcessSpec) -> Self {
        Self {
            proc_root,
            expected: Some(expected),
        }
    }

    /// Creates a production preflight without inventing a Python identity.
    #[must_use]
    pub fn without_expected_identity(proc_root: PathBuf) -> Self {
        Self {
            proc_root,
            expected: None,
        }
    }
}

pub use crate::startup_gate::{StartupGate, acquire_startup_gate};

#[cfg(test)]
#[path = "preflight_tests.rs"]
mod tests;
