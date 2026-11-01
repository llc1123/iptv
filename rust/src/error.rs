//! Stable, redacted application errors.

use std::fmt;

/// Errors raised while parsing or validating operator configuration.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ConfigError {
    /// A required credential was not provided.
    #[error("missing required credential: {0}")]
    MissingCredential(&'static str),
    /// The HTTP port is outside the usable range.
    #[error("port must be between 1 and 65535")]
    InvalidPort,
    /// The refresh interval must be positive.
    #[error("refresh interval must be greater than zero")]
    InvalidRefreshInterval,
    /// The shutdown grace period must be positive.
    #[error("shutdown grace period must be greater than zero")]
    InvalidShutdownGrace,
}

/// Application-level failures suitable for a process diagnostic.
#[derive(Debug, thiserror::Error)]
pub enum AppError {
    /// Command-line parsing failed.
    #[error("{0}")]
    Arguments(#[from] clap::Error),
    /// Configuration validation failed.
    #[error("configuration error: {0}")]
    Configuration(#[from] ConfigError),
}

impl AppError {
    /// Returns the process exit code for this error.
    #[must_use]
    pub fn exit_code(&self) -> i32 {
        match self {
            Self::Arguments(error) => match error.kind() {
                clap::error::ErrorKind::DisplayHelp | clap::error::ErrorKind::DisplayVersion => 0,
                _ => 2,
            },
            Self::Configuration(_) => 2,
        }
    }
}

impl fmt::Display for super::config::Secret {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("[redacted]")
    }
}
