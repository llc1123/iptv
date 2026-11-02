#![deny(unsafe_code)]
//! Typed configuration and error contracts for the Rust IPTV media server.

pub mod config;
pub mod crypto;
pub mod dhcp;
pub mod error;
pub mod health;
pub mod lifecycle;
pub mod preflight;
mod python_scan;
/// Startup admission composition for the Python preflight and lifecycle guards.
pub mod startup_gate;

/// Parses the process arguments and validates the operator configuration.
///
/// # Errors
/// Returns [`error::AppError`] when argument parsing or configuration validation fails.
pub fn load_config() -> Result<config::Config, error::AppError> {
    config::Config::from_environment()
}
