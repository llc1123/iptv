//! Operator-compatible command-line and environment configuration.

use std::{fmt, path::PathBuf};

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

    /// Exposes the credential only to the component that must encode it.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Secret([redacted])")
    }
}

/// Validated configuration consumed by the future server startup path.
#[derive(Clone, PartialEq, Eq)]
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
    /// Exact upstream HLS host allowlist entries.
    pub upstream_allow_hosts: Vec<String>,
    /// Upstream HLS CIDR allowlist entries.
    pub upstream_allow_cidrs: Vec<String>,
    /// Grace period for shutdown in seconds.
    pub shutdown_grace_seconds: u64,
}

impl fmt::Debug for Config {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Config")
            .field("interface", &self.interface)
            .field("epg_mac", &"[REDACTED]")
            .field("stb_id", &"[REDACTED]")
            .field("stb_id_prefix", &"[REDACTED]")
            .field("dhcp_user", &self.dhcp_user)
            .field("dhcp_password", &self.dhcp_password)
            .field("epg_user", &self.epg_user)
            .field("epg_password", &self.epg_password)
            .field("authentication_url", &"[REDACTED]")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("guide_past_days", &self.guide_past_days)
            .field("guide_future_days", &self.guide_future_days)
            .field("guide_timezone", &self.guide_timezone)
            .field("guide_cache", &self.guide_cache)
            .field("logo_catalog", &self.logo_catalog)
            .field("refresh_interval_seconds", &self.refresh_interval_seconds)
            .field("routes", &format_args!("[{} entries]", self.routes.len()))
            .field(
                "upstream_allow_hosts",
                &format_args!("[{} entries]", self.upstream_allow_hosts.len()),
            )
            .field(
                "upstream_allow_cidrs",
                &format_args!("[{} entries]", self.upstream_allow_cidrs.len()),
            )
            .field("shutdown_grace_seconds", &self.shutdown_grace_seconds)
            .finish()
    }
}

#[derive(Parser)]
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
    #[arg(long = "upstream-allow-host", value_name = "HOST[:PORT]")]
    upstream_allow_hosts: Vec<String>,
    #[arg(long = "upstream-allow-cidr", value_name = "CIDR")]
    upstream_allow_cidrs: Vec<String>,
    #[arg(long, default_value_t = 60)]
    shutdown_grace_seconds: u64,
}

impl fmt::Debug for Arguments {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Arguments")
            .field("interface", &self.interface)
            .field("epg_mac", &self.epg_mac)
            .field("stb_id", &self.stb_id)
            .field("stb_id_prefix", &self.stb_id_prefix)
            .field("dhcp_user", &"[REDACTED]")
            .field("dhcp_password", &"[REDACTED]")
            .field("epg_user", &"[REDACTED]")
            .field("epg_password", &"[REDACTED]")
            .field("authentication_url", &self.authentication_url)
            .field("host", &self.host)
            .field("port", &self.port)
            .field("guide_past_days", &self.guide_past_days)
            .field("guide_future_days", &self.guide_future_days)
            .field("guide_timezone", &self.guide_timezone)
            .field("guide_cache", &self.guide_cache)
            .field("logo_catalog", &self.logo_catalog)
            .field("refresh_interval_seconds", &self.refresh_interval_seconds)
            .field("routes", &self.routes)
            .field("upstream_allow_hosts", &self.upstream_allow_hosts)
            .field("upstream_allow_cidrs", &self.upstream_allow_cidrs)
            .field("shutdown_grace_seconds", &self.shutdown_grace_seconds)
            .finish()
    }
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
            upstream_allow_hosts: arguments.upstream_allow_hosts,
            upstream_allow_cidrs: if arguments.upstream_allow_cidrs.is_empty() {
                vec!["10.223.0.0/16".to_owned(), "20.248.0.0/13".to_owned()]
            } else {
                arguments.upstream_allow_cidrs
            },
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
    fn upstream_allowlists_are_repeatable_and_default_cidrs_are_preserved() {
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
            "--upstream-allow-host",
            "cdn.example.invalid:8443",
            "--upstream-allow-host",
            "media.example.invalid",
            "--upstream-allow-cidr",
            "192.0.2.0/24",
        ])
        .expect("valid upstream allowlists");
        let config = Config::try_from(arguments).expect("valid configuration");
        assert_eq!(
            config.upstream_allow_hosts,
            ["cdn.example.invalid:8443", "media.example.invalid"]
        );
        assert_eq!(config.upstream_allow_cidrs, ["192.0.2.0/24"]);

        let defaults = Config::try_from(valid_arguments()).expect("valid configuration");
        assert_eq!(
            defaults.upstream_allow_cidrs,
            ["10.223.0.0/16", "20.248.0.0/13"]
        );
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
    fn parsed_arguments_debug_redacts_all_credentials_and_keeps_safe_fields() {
        let arguments = valid_arguments();
        let rendered = format!("{arguments:?}");

        for secret in ["dhcp-user", "dhcp-password", "epg-user", "epg-password"] {
            assert!(!rendered.contains(secret));
        }
        assert!(rendered.contains("dhcp_user"));
        assert!(rendered.contains("dhcp_password"));
        assert!(rendered.contains("epg_user"));
        assert!(rendered.contains("epg_password"));
        assert!(rendered.contains("interface"));
        assert!(rendered.contains("eth0"));
        assert!(rendered.contains("port"));
        assert!(rendered.contains("8080"));
        assert_eq!(rendered.matches("[REDACTED]").count(), 4);
    }

    #[test]
    fn config_debug_redacts_credentials_urls_and_allowlist_values() {
        let mut arguments = valid_arguments();
        arguments.epg_mac = Some("aa:bb:cc:dd:ee:ff".to_owned());
        arguments.authentication_url = "https://secret.example.invalid/auth?token=abc".to_owned();
        arguments.upstream_allow_hosts = vec!["private.example.invalid".to_owned()];
        arguments.upstream_allow_cidrs = vec!["10.0.0.0/8".to_owned()];
        let config = Config::try_from(arguments).expect("valid configuration");
        let rendered = format!("{config:?}");

        for secret in [
            "aa:bb:cc:dd:ee:ff",
            "https://secret.example.invalid/auth?token=abc",
            "dhcp-user",
            "dhcp-password",
            "epg-user",
            "epg-password",
            "private.example.invalid",
            "10.0.0.0/8",
        ] {
            assert!(!rendered.contains(secret));
        }
        assert!(rendered.contains("Config"));
        assert!(rendered.contains("port"));
        assert!(rendered.contains("8080"));
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
