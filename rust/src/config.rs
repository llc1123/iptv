//! Operator-compatible command-line and environment configuration.

use std::path::PathBuf;

use clap::Parser;

use crate::error::{AppError, ConfigError};

const DEFAULT_AUTH_URL: &str = "https://example.invalid/EDS/jsp/AuthenticationURL";
const DEFAULT_GUIDE_CACHE: &str = "var/logo-catalog/guide.json";
const DEFAULT_LOGO_CATALOG: &str = "var/logo-catalog/catalog.json";

/// A credential that never reveals its value through formatting.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    fn required(value: String, name: &'static str) -> Result<Self, ConfigError> {
        if value.is_empty() {
            return Err(ConfigError::MissingCredential(name));
        }
        Ok(Self(value))
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("Secret([redacted])")
    }
}

/// Validated configuration consumed by the future server startup path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// Network interface used for IPTV acquisition.
    pub interface: String,
    /// Optional EPG authentication MAC.
    pub epg_mac: Option<String>,
    /// Explicit STB identifier.
    pub stb_id: String,
    /// STB identifier prefix used to derive an identifier from a MAC.
    pub stb_id_prefix: String,
    /// DHCP username.
    pub dhcp_user: Secret,
    /// DHCP password.
    pub dhcp_password: Secret,
    /// EPG username.
    pub epg_user: Secret,
    /// EPG password.
    pub epg_password: Secret,
    /// EPG authentication endpoint.
    pub authentication_url: String,
    /// HTTP bind address.
    pub host: String,
    /// HTTP bind port.
    pub port: u16,
    /// Past guide window in days.
    pub guide_past_days: u16,
    /// Future guide window in days.
    pub guide_future_days: u16,
    /// XMLTV timezone name.
    pub guide_timezone: String,
    /// Persisted guide cache.
    pub guide_cache: PathBuf,
    /// Persisted local logo catalog.
    pub logo_catalog: PathBuf,
    /// Refresh period in seconds.
    pub refresh_interval_seconds: u64,
    /// Optional provider route overrides.
    pub routes: Vec<String>,
    /// Grace period for shutdown in seconds.
    pub shutdown_grace_seconds: u64,
}

#[derive(Debug, Parser)]
#[command(name = "iptv-media-server", version, about = "IPTV media server")]
struct Arguments {
    #[arg(long, default_value = "eth0", env = "IPTV_INTERFACE")]
    interface: String,
    #[arg(long, env = "IPTV_EPG_MAC")]
    epg_mac: Option<String>,
    #[arg(long, default_value = "")]
    stb_id: String,
    #[arg(long, default_value = "", env = "IPTV_STB_ID_PREFIX")]
    stb_id_prefix: String,
    #[arg(long, default_value = "", env = "IPTV_DHCP_USER")]
    dhcp_user: String,
    #[arg(long, default_value = "", env = "IPTV_DHCP_PASSWORD")]
    dhcp_password: String,
    #[arg(
        long = "user-id",
        visible_alias = "epg-user",
        default_value = "",
        env = "IPTV_USER_ID"
    )]
    epg_user: String,
    #[arg(
        long = "password",
        visible_alias = "epg-password",
        default_value = "",
        env = "IPTV_PASSWORD"
    )]
    epg_password: String,
    #[arg(long, default_value = DEFAULT_AUTH_URL, env = "IPTV_AUTH_URL")]
    authentication_url: String,
    #[arg(long, default_value = "127.0.0.1")]
    host: String,
    #[arg(long, default_value_t = 8080)]
    port: u32,
    #[arg(long, default_value_t = 7)]
    guide_past_days: u16,
    #[arg(long, default_value_t = 2)]
    guide_future_days: u16,
    #[arg(long, default_value = "Asia/Shanghai")]
    guide_timezone: String,
    #[arg(long, default_value = DEFAULT_GUIDE_CACHE)]
    guide_cache: PathBuf,
    #[arg(long, default_value = DEFAULT_LOGO_CATALOG)]
    logo_catalog: PathBuf,
    #[arg(long, default_value_t = 3600)]
    refresh_interval_seconds: u64,
    #[arg(long = "route", value_name = "CIDR[,via=IP][,src=IP]")]
    routes: Vec<String>,
    #[arg(long, default_value_t = 60)]
    shutdown_grace_seconds: u64,
}

impl Config {
    /// Parses CLI/environment input and validates values without contacting the network.
    ///
    /// # Errors
    /// Returns [`AppError`] when parsing fails or a required configuration value is invalid.
    pub fn from_environment() -> Result<Self, AppError> {
        let arguments = Arguments::try_parse()?;
        Self::try_from(arguments).map_err(AppError::from)
    }

    fn try_from(arguments: Arguments) -> Result<Self, ConfigError> {
        let port = u16::try_from(arguments.port).map_err(|_| ConfigError::InvalidPort)?;
        if port == 0 {
            return Err(ConfigError::InvalidPort);
        }
        if arguments.refresh_interval_seconds == 0 {
            return Err(ConfigError::InvalidRefreshInterval);
        }
        if arguments.shutdown_grace_seconds == 0 {
            return Err(ConfigError::InvalidShutdownGrace);
        }

        Ok(Self {
            interface: arguments.interface,
            epg_mac: arguments.epg_mac,
            stb_id: arguments.stb_id,
            stb_id_prefix: arguments.stb_id_prefix,
            dhcp_user: Secret::required(arguments.dhcp_user, "IPTV_DHCP_USER")?,
            dhcp_password: Secret::required(arguments.dhcp_password, "IPTV_DHCP_PASSWORD")?,
            epg_user: Secret::required(arguments.epg_user, "IPTV_USER_ID")?,
            epg_password: Secret::required(arguments.epg_password, "IPTV_PASSWORD")?,
            authentication_url: arguments.authentication_url,
            host: arguments.host,
            port,
            guide_past_days: arguments.guide_past_days,
            guide_future_days: arguments.guide_future_days,
            guide_timezone: arguments.guide_timezone,
            guide_cache: arguments.guide_cache,
            logo_catalog: arguments.logo_catalog,
            refresh_interval_seconds: arguments.refresh_interval_seconds,
            routes: arguments.routes,
            shutdown_grace_seconds: arguments.shutdown_grace_seconds,
        })
    }
}

#[cfg(test)]
#[expect(clippy::expect_used, reason = "test fixtures are statically valid")]
mod tests {
    use super::{Arguments, Config, ConfigError, DEFAULT_AUTH_URL};
    use clap::Parser;

    fn valid_arguments() -> Arguments {
        Arguments::try_parse_from([
            "iptv-media-server",
            "--dhcp-user",
            "dhcp-user",
            "--dhcp-password",
            "dhcp-password",
            "--user-id",
            "epg-user",
            "--password",
            "epg-password",
        ])
        .expect("valid test arguments")
    }

    #[test]
    fn defaults_preserve_operator_contract() {
        let config = Config::try_from(valid_arguments()).expect("valid configuration");

        assert_eq!(config.interface, "eth0");
        assert_eq!(config.authentication_url, DEFAULT_AUTH_URL);
        assert_eq!(config.port, 8080);
        assert_eq!(config.guide_past_days, 7);
        assert_eq!(config.guide_future_days, 2);
        assert_eq!(config.guide_timezone, "Asia/Shanghai");
        assert_eq!(config.refresh_interval_seconds, 3600);
        assert_eq!(config.shutdown_grace_seconds, 60);
    }

    #[test]
    fn zero_refresh_interval_is_rejected() {
        let mut arguments = valid_arguments();
        arguments.refresh_interval_seconds = 0;

        assert_eq!(
            Config::try_from(arguments),
            Err(ConfigError::InvalidRefreshInterval)
        );
    }

    #[test]
    fn zero_port_is_rejected() {
        let mut arguments = valid_arguments();
        arguments.port = 0;

        assert_eq!(Config::try_from(arguments), Err(ConfigError::InvalidPort));
    }

    #[test]
    fn operator_overrides_are_parsed_into_typed_configuration() {
        let arguments = Arguments::try_parse_from([
            "iptv-media-server",
            "--dhcp-user",
            "dhcp-user",
            "--dhcp-password",
            "dhcp-password",
            "--user-id",
            "epg-user",
            "--password",
            "epg-password",
            "--route",
            "10.223.0.0/16,via=192.0.2.1,src=192.0.2.2",
            "--stb-id-prefix",
            "STB-",
            "--shutdown-grace-seconds",
            "15",
        ])
        .expect("valid operator overrides");
        let config = Config::try_from(arguments).expect("valid configuration");

        assert_eq!(config.routes, ["10.223.0.0/16,via=192.0.2.1,src=192.0.2.2"]);
        assert_eq!(config.stb_id_prefix, "STB-");
        assert_eq!(config.shutdown_grace_seconds, 15);
    }

    #[test]
    fn removed_converter_flag_is_rejected_without_echoing_value() {
        let error = Arguments::try_parse_from([
            "iptv-media-server",
            "--metadata-converter-url",
            "secret-converter-value",
        ])
        .expect_err("removed converter flag must be rejected");

        let rendered = error.to_string();
        assert!(rendered.contains("metadata-converter-url"));
        assert!(!rendered.contains("secret-converter-value"));
    }

    #[test]
    fn missing_credentials_are_named_without_values() {
        let arguments = Arguments::try_parse_from(["iptv-media-server"])
            .expect("argument parsing does not require credentials");
        let error = Config::try_from(arguments).expect_err("credentials are required");

        assert_eq!(error, ConfigError::MissingCredential("IPTV_DHCP_USER"));
        assert!(!error.to_string().contains("password"));
    }
}
