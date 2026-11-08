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
    /// stay ahead of the main table's priority of 32766 and avoid the kernel's
    /// reserved tables 253-255 (default, main, local).
    #[arg(long, default_value_t = crate::dhcp::DEFAULT_ROUTE_TABLE, value_parser = route_table)]
    pub route_table: u32,
    /// Hold the DHCP lease for netifd and report it through `--lease-report`,
    /// renewing at half the lease. This process never serves HTTP; netifd owns
    /// the address and routes.
    #[arg(long, conflicts_with = "network_managed")]
    pub lease_daemon: bool,
    /// Script run with each lease report in its environment (`IPTV_ADDRESS`,
    /// `IPTV_PREFIX`, `IPTV_GATEWAY`, ... and `IPTV_EVENT`).
    #[arg(long, value_name = "SCRIPT", requires = "lease_daemon")]
    pub lease_report: Option<std::path::PathBuf>,
    /// Serve without touching the network: take the lease address from
    /// `--lease-address` and let a supervisor own the interface.
    #[arg(long, conflicts_with = "lease_daemon")]
    pub network_managed: bool,
    /// Address netifd put on the interface, required with `--network-managed`.
    #[arg(
        long,
        value_name = "ADDRESS",
        requires = "network_managed",
        required_if_eq("network_managed", "true")
    )]
    pub lease_address: Option<std::net::Ipv4Addr>,
}

impl Config {
    pub fn from_environment() -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let config = Self::parse();
        // DHCP credentials matter only where this process runs the exchange,
        // EPG credentials only where it serves.
        let dhcp = !config.network_managed;
        let epg = !config.lease_daemon;
        for (needed, value, name) in [
            (dhcp, &config.dhcp_user, "IPTV_DHCP_USER"),
            (dhcp, &config.dhcp_password, "IPTV_DHCP_PASSWORD"),
            (epg, &config.epg_user, "IPTV_USER_ID"),
            (epg, &config.epg_password, "IPTV_PASSWORD"),
        ] {
            if needed && value.as_str().is_empty() {
                return Err(Box::new(ConfigError::MissingCredential(name)));
            }
        }
        Ok(config)
    }
}

/// Parses `--route-table`: 1-32765, excluding the reserved 253-255.
fn route_table(value: &str) -> Result<u32, String> {
    let table: u32 = value
        .parse()
        .map_err(|_| format!("route table must be a number, got {value:?}"))?;
    match table {
        253..=255 => Err(format!("route table {table} is reserved by the kernel")),
        1..=32765 => Ok(table),
        _ => Err(format!("route table {table} is outside 1-32765")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn route_table_rejects_reserved_and_out_of_range_tables() {
        assert_eq!(route_table("4788"), Ok(4788));
        assert_eq!(route_table("252"), Ok(252));
        for value in ["0", "253", "254", "255", "32766", "x"] {
            assert!(route_table(value).is_err(), "{value} accepted");
        }
    }

    #[test]
    fn network_managed_requires_a_lease_address() {
        let args = [
            "iptv-media-server",
            "--network-managed",
            "--interface",
            "eth1",
        ];
        let missing = Config::try_parse_from(args).map(|_| ());
        assert_eq!(
            missing.map_err(|error| error.kind()),
            Err(clap::error::ErrorKind::MissingRequiredArgument)
        );
        let given = Config::try_parse_from(args.into_iter().chain(["--lease-address", "10.0.0.2"]));
        assert!(given.is_ok());
    }
}
