//! Fixed-code application errors with redacted boundary contracts.

use std::fmt;

use serde::Serialize;

/// Stable operation identifiers exposed to clients and logs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum OperationCode {
    /// Invalid command-line input.
    Arguments,
    /// Invalid operator configuration.
    Configuration,
    /// Upstream HLS proxy failure.
    HlsProxy,
    /// EPG HTTP failure.
    EpgHttp,
    /// EPG replay failure.
    EpgReplay,
    /// Guide cache failure.
    GuideCache,
    /// Logo failure.
    Logo,
    /// Metadata refresh failure.
    Refresh,
    /// Persistence failure.
    Persistence,
    /// Generic startup failure.
    Startup,
    /// Health subsystem failure.
    Health,
}

impl OperationCode {
    /// Returns the stable wire representation of this operation.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Arguments => "arguments",
            Self::Configuration => "configuration",
            Self::HlsProxy => "hls-proxy",
            Self::EpgHttp => "epg-http",
            Self::EpgReplay => "epg-replay",
            Self::GuideCache => "guide-cache",
            Self::Logo => "logo",
            Self::Refresh => "refresh",
            Self::Persistence => "persistence",
            Self::Startup => "startup",
            Self::Health => "health",
        }
    }
}

impl fmt::Display for OperationCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Command-line outcomes whose process exit behavior is stable and redacted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArgumentError {
    /// Help was requested.
    Help,
    /// Version was requested.
    Version,
    /// Arguments were invalid.
    Invalid,
}

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

/// Startup failures represented without source error details.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartupError {
    /// Configuration could not be loaded or validated.
    Configuration,
    /// Startup failed after configuration.
    Startup,
}

/// Request failures represented without upstream details.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestError {
    /// HLS proxy failed.
    HlsProxy,
    /// EPG HTTP request failed.
    EpgHttp,
    /// EPG replay request failed.
    EpgReplay,
    /// Guide cache request failed.
    GuideCache,
    /// Logo request failed.
    Logo,
}

/// Metadata refresh failures represented without source details.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefreshError {
    /// Metadata refresh failed.
    Refresh,
}

/// Persistence failures represented without source details.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PersistenceError {
    /// Snapshot persistence failed.
    Persistence,
}

impl fmt::Display for StartupError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("service unavailable")
    }
}

impl fmt::Display for RequestError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("request unavailable")
    }
}

impl fmt::Display for RefreshError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("refresh unavailable")
    }
}

impl fmt::Display for PersistenceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("persistence unavailable")
    }
}

impl std::error::Error for StartupError {}
impl std::error::Error for RequestError {}
impl std::error::Error for RefreshError {}
impl std::error::Error for PersistenceError {}

/// The application error boundary used by startup and request code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppError {
    /// Command-line handling failed or requested output.
    Arguments(ArgumentError),
    /// Startup failed.
    Startup(StartupError),
    /// A request failed.
    Request(RequestError),
    /// Refresh failed.
    Refresh(RefreshError),
    /// Persistence failed.
    Persistence(PersistenceError),
}

impl std::error::Error for AppError {}

impl From<ConfigError> for AppError {
    fn from(_: ConfigError) -> Self {
        Self::Startup(StartupError::Configuration)
    }
}

impl From<clap::Error> for AppError {
    fn from(error: clap::Error) -> Self {
        match error.kind() {
            clap::error::ErrorKind::DisplayHelp => Self::Arguments(ArgumentError::Help),
            clap::error::ErrorKind::DisplayVersion => Self::Arguments(ArgumentError::Version),
            _ => Self::Arguments(ArgumentError::Invalid),
        }
    }
}

impl AppError {
    /// Returns the stable HTTP status for an error response.
    #[must_use]
    pub const fn http_status(self) -> u16 {
        match self {
            Self::Request(RequestError::HlsProxy) => 502,
            Self::Arguments(_) => 400,
            Self::Startup(_) | Self::Request(_) | Self::Refresh(_) | Self::Persistence(_) => 503,
        }
    }

    /// Returns the fixed public message for this error class.
    #[must_use]
    pub const fn public_message(self) -> &'static str {
        match self {
            Self::Arguments(ArgumentError::Help) => "help",
            Self::Arguments(ArgumentError::Version) => "version",
            Self::Arguments(ArgumentError::Invalid) => "invalid arguments",
            Self::Startup(_) => "service unavailable",
            Self::Request(_) => "request unavailable",
            Self::Refresh(_) => "refresh unavailable",
            Self::Persistence(_) => "persistence unavailable",
        }
    }

    /// Returns the stable operation code for this error.
    #[must_use]
    pub const fn operation_code(self) -> OperationCode {
        match self {
            Self::Arguments(_) => OperationCode::Arguments,
            Self::Startup(StartupError::Configuration) => OperationCode::Configuration,
            Self::Startup(StartupError::Startup) => OperationCode::Startup,
            Self::Request(RequestError::HlsProxy) => OperationCode::HlsProxy,
            Self::Request(RequestError::EpgHttp) => OperationCode::EpgHttp,
            Self::Request(RequestError::EpgReplay) => OperationCode::EpgReplay,
            Self::Request(RequestError::GuideCache) => OperationCode::GuideCache,
            Self::Request(RequestError::Logo) => OperationCode::Logo,
            Self::Refresh(RefreshError::Refresh) => OperationCode::Refresh,
            Self::Persistence(PersistenceError::Persistence) => OperationCode::Persistence,
        }
    }

    /// Returns the process exit code for this error.
    #[must_use]
    pub const fn exit_code(self) -> i32 {
        match self {
            Self::Arguments(ArgumentError::Help | ArgumentError::Version) => 0,
            Self::Arguments(ArgumentError::Invalid) => 2,
            Self::Startup(_) | Self::Request(_) | Self::Refresh(_) | Self::Persistence(_) => 1,
        }
    }

    /// Builds the redacted JSON-compatible response body.
    #[must_use]
    pub const fn response_body(self) -> ResponseBody {
        ResponseBody {
            error: self.operation_code(),
            message: self.public_message(),
        }
    }

    /// Builds the redacted structured log fields.
    #[must_use]
    pub const fn log_fields(self) -> LogFields {
        LogFields {
            operation: self.operation_code(),
        }
    }
}

impl fmt::Display for AppError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.public_message())
    }
}

/// Fixed response fields safe to serialize at an HTTP boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ResponseBody {
    /// Stable operation code.
    pub error: OperationCode,
    /// Fixed, non-source-derived public message.
    pub message: &'static str,
}

/// Fixed structured fields safe to send to a logger.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct LogFields {
    /// Stable operation code.
    pub operation: OperationCode,
}

impl fmt::Display for super::config::Secret {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("[redacted]")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hls_proxy_is_the_only_bad_gateway_mapping() {
        assert_eq!(AppError::Request(RequestError::HlsProxy).http_status(), 502);
        assert_eq!(AppError::Request(RequestError::EpgHttp).http_status(), 503);
        assert_eq!(AppError::Refresh(RefreshError::Refresh).http_status(), 503);
        assert_eq!(
            AppError::Persistence(PersistenceError::Persistence).http_status(),
            503
        );
        assert_eq!(AppError::Startup(StartupError::Startup).http_status(), 503);
    }

    #[test]
    fn argument_exit_codes_distinguish_help_version_and_invalid_input() {
        assert_eq!(AppError::Arguments(ArgumentError::Help).exit_code(), 0);
        assert_eq!(AppError::Arguments(ArgumentError::Version).exit_code(), 0);
        assert_eq!(AppError::Arguments(ArgumentError::Invalid).exit_code(), 2);
    }

    #[test]
    fn every_operational_error_has_a_nonzero_exit_code() {
        let errors = [
            AppError::Startup(StartupError::Configuration),
            AppError::Request(RequestError::HlsProxy),
            AppError::Refresh(RefreshError::Refresh),
            AppError::Persistence(PersistenceError::Persistence),
        ];
        assert!(errors.iter().all(|error| error.exit_code() > 0));
    }

    #[test]
    fn response_and_log_contracts_discard_sensitive_source_values() {
        let error = AppError::Request(RequestError::HlsProxy);
        let rendered = serde_json::to_string(&error.response_body());
        assert!(rendered.is_ok());
        let rendered = rendered.unwrap_or_default();
        for secret in [
            "https://user:password@example.invalid/signed-token?token=secret",
            "Authorization: Bearer secret-token",
            "aa:bb:cc:dd:ee:ff",
        ] {
            assert!(!rendered.contains(secret));
        }
        assert_eq!(error.log_fields().operation, OperationCode::HlsProxy);
        assert_eq!(error.to_string(), "request unavailable");
    }

    #[test]
    fn operation_codes_use_python_wire_strings() {
        assert_eq!(OperationCode::HlsProxy.as_str(), "hls-proxy");
        assert_eq!(OperationCode::EpgHttp.as_str(), "epg-http");
        assert_eq!(OperationCode::GuideCache.as_str(), "guide-cache");
        assert_eq!(OperationCode::Refresh.as_str(), "refresh");
    }
}
