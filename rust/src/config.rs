use clap::Parser;
use std::{fmt, path::PathBuf, str::FromStr};
use thiserror::Error;

const DEFAULT_AUTH_URL: &str = "https://example.invalid/EDS/jsp/AuthenticationURL";

#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);
impl Secret {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl FromStr for Secret {
    type Err = std::convert::Infallible;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Ok(Self(value.to_owned()))
    }
}
impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("***")
    }
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("missing required credential: {0}")]
    MissingCredential(&'static str),
}

#[derive(Clone, Debug, Parser, PartialEq, Eq)]
#[command(name = "iptv-media-server", version, about = "IPTV media server")]
pub struct Config {
    #[arg(long, default_value = "eth0", env = "IPTV_INTERFACE")]
    pub interface: String,
    #[arg(long, env = "IPTV_EPG_MAC")]
    pub epg_mac: Option<String>,
    #[arg(long, default_value = "")]
    pub stb_id: String,
    #[arg(long, default_value = "", env = "IPTV_STB_ID_PREFIX")]
    pub stb_id_prefix: String,
    #[arg(long, default_value = "", env = "IPTV_DHCP_USER")]
    pub dhcp_user: Secret,
    #[arg(long, default_value = "", env = "IPTV_DHCP_PASSWORD")]
    pub dhcp_password: Secret,
    #[arg(
        long = "user-id",
        visible_alias = "epg-user",
        default_value = "",
        env = "IPTV_USER_ID"
    )]
    pub epg_user: Secret,
    #[arg(
        long = "password",
        visible_alias = "epg-password",
        default_value = "",
        env = "IPTV_PASSWORD"
    )]
    pub epg_password: Secret,
    #[arg(long, default_value = DEFAULT_AUTH_URL, env = "IPTV_AUTH_URL")]
    pub authentication_url: String,
    #[arg(long, default_value = "127.0.0.1")]
    pub host: String,
    #[arg(long, default_value_t = 8080, value_parser = clap::value_parser!(u16).range(1..=65535))]
    pub port: u16,
    #[arg(long, default_value_t = 7)]
    pub guide_past_days: u16,
    #[arg(long, default_value_t = 2)]
    pub guide_future_days: u16,
    #[arg(long, default_value = "Asia/Shanghai")]
    pub guide_timezone: String,
    #[arg(long, default_value = "var/logo-catalog/guide.json")]
    pub guide_cache: PathBuf,
    /// Directory of `<channel-id>.png` logos.
    #[arg(long, default_value = "var/logo-catalog/logos")]
    pub logo_dir: PathBuf,
    #[arg(long, default_value_t = 3600, value_parser = clap::value_parser!(u64).range(1..))]
    pub refresh_interval_seconds: u64,
    /// Routing table and rule priority that carry the IPTV network; it must
    /// stay ahead of the main table's priority of 32766.
    #[arg(
        long,
        default_value_t = crate::dhcp::DEFAULT_ROUTE_TABLE,
        value_parser = clap::value_parser!(u32).range(1..=32765)
    )]
    pub route_table: u32,
    /// Acquire the DHCP lease, print it for netifd and exit without serving:
    /// the integration point for an `OpenWrt` protocol handler, where
    /// netifd owns the address and routes.
    #[arg(long)]
    pub lease_only: bool,
}

impl Config {
    pub fn from_environment() -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let config = Self::parse();
        // Lease-only runs serve nothing, so the EPG credentials are unused.
        let credentials: &[(&crate::config::Secret, &'static str)] = if config.lease_only {
            &[
                (&config.dhcp_user, "IPTV_DHCP_USER"),
                (&config.dhcp_password, "IPTV_DHCP_PASSWORD"),
            ]
        } else {
            &[
                (&config.dhcp_user, "IPTV_DHCP_USER"),
                (&config.dhcp_password, "IPTV_DHCP_PASSWORD"),
                (&config.epg_user, "IPTV_USER_ID"),
                (&config.epg_password, "IPTV_PASSWORD"),
            ]
        };
        for (value, name) in credentials {
            if value.as_str().is_empty() {
                return Err(Box::new(ConfigError::MissingCredential(name)));
            }
        }
        Ok(config)
    }
}
