#![deny(unsafe_code)]
//! Typed configuration and error contracts for the Rust IPTV media server.

pub mod config;
pub mod crypto;
pub mod dhcp;
pub mod epg;
pub mod error;
pub mod guide;
pub mod hls;
pub mod http;
pub mod logos;
pub mod refresh;
pub mod render;
pub mod renewal;
pub mod snapshot;
/// Production startup composition and process-owned shutdown orchestration.
pub mod startup;

/// Parses the process arguments and validates the operator configuration.
///
/// # Errors
pub fn load_config() -> Result<config::Config, Box<dyn std::error::Error + Send + Sync>> {
    config::Config::from_environment()
}

/// Runs the production server after configuration has been parsed.
///
/// # Errors
/// Returns a fixed operational error when startup or serving is refused.
pub async fn run(config: config::Config) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    startup::run(config).await
}
