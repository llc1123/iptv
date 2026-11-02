use std::fmt;
use std::fmt::Write;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use reqwest::cookie::Jar;
use reqwest::redirect::Policy;
use reqwest::{Client, Response, Url};
use tokio::sync::Semaphore;

use crate::crypto::epg_auth::{AuthenticatorInput, identity_encode, make_authenticator};

use super::request::send_with_client_retry;
use super::session::AuthCoordinator;
use super::session::SessionBundle;

const USER_AGENT: &str = "Mozilla/5.0 (X11; Linux x86_64; SkyworthBrowser) AppleWebKit/534.24 (KHTML, like Gecko) Safari/534.24 SkWebKit-SH-CU";
const ACCEPT_LANGUAGE: &str = "zh-CN,en-US;q=0.9";
const REQUESTED_WITH: &str = "com.skyworth.iptv";

/// Configuration required to authenticate one set-top box.
#[derive(Clone)]
pub struct EpgConfig {
    /// Discovery endpoint.
    pub authentication_url: String,
    /// Provider user identifier.
    pub user_id: String,
    /// Provider password.
    pub password: String,
    /// Set-top-box identifier.
    pub stb_id: String,
    /// Set-top-box MAC address.
    pub stb_mac: String,
    /// Local source address for upstream sockets.
    pub source_ip: IpAddr,
    /// Provider STB type field.
    pub stb_type: String,
    /// Provider area identifier.
    pub area_id: String,
    /// Provider user-group identifier.
    pub user_group_id: String,
    /// Deterministic authenticator nonce.
    pub nonce: u32,
    /// Deadline for the request send and write phase.
    pub write_timeout: Duration,
    /// Deadline for control and media permit acquisition.
    pub pool_timeout: Duration,
    /// Maximum number of concurrent media requests.
    pub media_pool_size: usize,
}

impl fmt::Debug for EpgConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EpgConfig")
            .field(
                "authentication_url_scheme",
                &Url::parse(&self.authentication_url)
                    .ok()
                    .map(|url| url.scheme().to_owned()),
            )
            .field(
                "authentication_url_has_explicit_port",
                &Url::parse(&self.authentication_url)
                    .ok()
                    .is_some_and(|url| url.port().is_some()),
            )
            .field("user_id_present", &!self.user_id.is_empty())
            .field("user_id", &"[REDACTED]")
            .field("password_present", &!self.password.is_empty())
            .field("password", &"[REDACTED]")
            .field("stb_id_present", &!self.stb_id.is_empty())
            .field("stb_id", &"[REDACTED]")
            .field("stb_mac_present", &!self.stb_mac.is_empty())
            .field("stb_mac", &"[REDACTED]")
            .field("source_ip", &"[REDACTED]")
            .field("stb_type", &"[REDACTED]")
            .field("area_id", &"[REDACTED]")
            .field("user_group_id", &"[REDACTED]")
            .field("nonce", &self.nonce)
            .field("write_timeout", &self.write_timeout)
            .field("pool_timeout", &self.pool_timeout)
            .field("media_pool_size", &self.media_pool_size)
            .finish()
    }
}

/// Authentication phase used by a redacted error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EpgErrorStage {
    /// Discovery request.
    Discovery,
    /// Login request.
    Login,
    /// Authentication validation request.
    Authentication,
    /// Authenticated business request.
    Request,
}

/// Safe error classification for EPG operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EpgErrorClass {
    /// Connection failure.
    Transport,
    /// Non-success HTTP status.
    HttpStatus,
    /// Request send or write deadline elapsed.
    Timeout,
    /// Permit or HTTP pool admission deadline elapsed.
    PoolTimeout,
    /// Invalid redirect origin.
    InvalidOrigin,
    /// Required response field was absent.
    MissingField,
    /// Response text was malformed.
    Parse,
    /// Existing crypto primitive rejected the input.
    Crypto,
    /// Lifecycle shutdown cancelled the flight.
    Cancelled,
}

/// Cloneable error that never retains upstream-sensitive data.
#[derive(Clone, PartialEq, Eq)]
pub struct EpgError {
    stage: EpgErrorStage,
    class: EpgErrorClass,
    status: Option<u16>,
    field: Option<&'static str>,
    attempt: u8,
}

impl fmt::Debug for EpgError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EpgError")
            .field("stage", &self.stage)
            .field("class", &self.class)
            .field("status", &self.status)
            .field("field", &self.field)
            .field("attempt", &self.attempt)
            .finish()
    }
}

impl fmt::Display for EpgError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "EPG {:?} {:?}", self.stage, self.class)
    }
}

impl std::error::Error for EpgError {}

impl From<EpgError> for crate::error::AppError {
    fn from(_: EpgError) -> Self {
        Self::Request(crate::error::RequestError::EpgHttp)
    }
}

impl EpgError {
    pub(crate) const fn transport(stage: EpgErrorStage, attempt: u8, timeout: bool) -> Self {
        Self {
            stage,
            class: if timeout {
                EpgErrorClass::Timeout
            } else {
                EpgErrorClass::Transport
            },
            status: None,
            field: None,
            attempt,
        }
    }
    pub(crate) const fn timeout(stage: EpgErrorStage, attempt: u8) -> Self {
        Self {
            stage,
            class: EpgErrorClass::Timeout,
            status: None,
            field: None,
            attempt,
        }
    }
    pub(crate) const fn pool_timeout(stage: EpgErrorStage, attempt: u8) -> Self {
        Self {
            stage,
            class: EpgErrorClass::PoolTimeout,
            status: None,
            field: None,
            attempt,
        }
    }
    pub(crate) const fn cancelled(stage: EpgErrorStage) -> Self {
        Self {
            stage,
            class: EpgErrorClass::Cancelled,
            status: None,
            field: None,
            attempt: 0,
        }
    }
    fn status(stage: EpgErrorStage, status: u16, attempt: u8) -> Self {
        Self {
            stage,
            class: EpgErrorClass::HttpStatus,
            status: Some(status),
            field: None,
            attempt,
        }
    }
    fn field(stage: EpgErrorStage, field: &'static str) -> Self {
        Self {
            stage,
            class: EpgErrorClass::MissingField,
            status: None,
            field: Some(field),
            attempt: 1,
        }
    }
    fn class(stage: EpgErrorStage, class: EpgErrorClass) -> Self {
        Self {
            stage,
            class,
            status: None,
            field: None,
            attempt: 1,
        }
    }
    #[must_use]
    /// Returns the safe operation stage.
    pub const fn stage(&self) -> EpgErrorStage {
        self.stage
    }
    #[must_use]
    /// Returns the safe error class.
    pub const fn class_code(&self) -> EpgErrorClass {
        self.class
    }
    #[must_use]
    /// Returns the safe HTTP status, when observed.
    pub const fn status_code(&self) -> Option<u16> {
        self.status
    }
    #[must_use]
    /// Returns the safe missing field name, when applicable.
    pub const fn field_name(&self) -> Option<&'static str> {
        self.field
    }
    #[must_use]
    /// Returns the explicit transport attempt count.
    pub const fn attempt(&self) -> u8 {
        self.attempt
    }
}

/// Authenticated EPG client with shared control/media cookie state.
#[derive(Clone)]
pub struct EpgClient {
    config: Arc<EpgConfig>,
    coordinator: AuthCoordinator,
    _jar: Arc<Jar>,
    control_client: Client,
    media_client: Client,
    control_permits: Arc<Semaphore>,
    media_permits: Arc<Semaphore>,
}

impl fmt::Debug for EpgClient {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("EpgClient { config: [REDACTED], session: [REDACTED] }")
    }
}

impl EpgClient {
    /// Creates the two long-lived HTTP client handles.
    ///
    /// # Errors
    /// Returns a redacted transport error when reqwest rejects client construction.
    pub fn new(config: EpgConfig) -> Result<Self, EpgError> {
        Self::with_coordinator(config, AuthCoordinator::default())
    }

    /// Creates a client with an injected authentication coordinator.
    ///
    /// # Errors
    /// Returns a redacted transport error when reqwest rejects client construction.
    pub fn with_coordinator(
        config: EpgConfig,
        coordinator: AuthCoordinator,
    ) -> Result<Self, EpgError> {
        let jar = Arc::new(Jar::default());
        let control_client = build_client(config.source_ip, Arc::clone(&jar))?;
        let media_client = build_client(config.source_ip, Arc::clone(&jar))?;
        let control_permits = Arc::new(Semaphore::new(1));
        let media_permits = Arc::new(Semaphore::new(config.media_pool_size));
        Ok(Self {
            config: Arc::new(config),
            coordinator,
            _jar: jar,
            control_client,
            media_client,
            control_permits,
            media_permits,
        })
    }

    /// Authenticates on demand and reuses a fresh generation.
    ///
    /// # Errors
    /// Returns only safe stage, class, status, field, and attempt information.
    pub async fn authenticate(&self) -> Result<Arc<SessionBundle>, EpgError> {
        let client = self.clone();
        self.coordinator
            .ensure_authenticated(move |generation| async move {
                client
                    .authenticate_generation(generation)
                    .await
                    .map(Arc::new)
            })
            .await
    }

    /// Returns the most recently committed generation.
    pub async fn current_session(&self) -> Option<Arc<SessionBundle>> {
        self.coordinator.current().await
    }

    #[expect(
        clippy::too_many_lines,
        reason = "the provider requires one exact ordered authentication sequence"
    )]
    async fn authenticate_generation(&self, generation: u64) -> Result<SessionBundle, EpgError> {
        let discovery_url =
            parse_epg_url(&self.config.authentication_url, EpgErrorStage::Discovery)?;
        let mut discovery_url = discovery_url;
        discovery_url
            .query_pairs_mut()
            .append_pair("UserID", &self.config.user_id)
            .append_pair("Action", "Login")
            .append_pair("Mode", "MENU");
        let discovery = send_with_client_retry(
            &self.control_client,
            |client| client.get(discovery_url.clone()),
            &super::session::TokioSleeper,
            EpgErrorStage::Discovery,
            Arc::clone(&self.control_permits),
            self.config.pool_timeout,
            self.config.write_timeout,
        )
        .await?;
        ensure_status(&discovery, EpgErrorStage::Discovery, 1)?;
        let origin = final_origin(discovery.url())?;
        let login_path = origin
            .join("/EPG/jsp/authLoginHWCU.jsp")
            .map_err(|_| EpgError::class(EpgErrorStage::Login, EpgErrorClass::InvalidOrigin))?;
        let login_form = encode_form(&[("UserID", self.config.user_id.as_str()), ("VIP", "")]);
        let login = send_with_client_retry(
            &self.control_client,
            |client| {
                client
                    .post(login_path.clone())
                    .header("Content-Type", "application/x-www-form-urlencoded")
                    .body(login_form.clone())
            },
            &super::session::TokioSleeper,
            EpgErrorStage::Login,
            Arc::clone(&self.control_permits),
            self.config.pool_timeout,
            self.config.write_timeout,
        )
        .await?;
        ensure_status(&login, EpgErrorStage::Login, 1)?;
        let mut cookie_snapshot = capture_cookie_snapshot(&login);
        let encrypt_token = extract_query_token(
            &login
                .text()
                .await
                .map_err(|_| EpgError::class(EpgErrorStage::Login, EpgErrorClass::Parse))?,
            "EncryptToken",
        )?;
        let source_ip = self.config.source_ip.to_string();
        let authenticator = make_authenticator(&AuthenticatorInput {
            encrypt_token: &encrypt_token,
            user_id: &self.config.user_id,
            stb_id: &self.config.stb_id,
            ip_address: &source_ip,
            mac: &self.config.stb_mac,
            password: &self.config.password,
            custom_str: "$CTC",
            random_value: self.config.nonce,
        })
        .map_err(|_| EpgError::class(EpgErrorStage::Authentication, EpgErrorClass::Crypto))?;
        let form = [
            ("UserID", self.config.user_id.clone()),
            ("Lang", "0".into()),
            ("SupportHD", "1".into()),
            ("NetUserID", String::new()),
            ("Authenticator", authenticator),
            ("STBType", self.config.stb_type.clone()),
            ("STBVersion", String::new()),
            ("conntype", "dhcp".into()),
            ("STBID", self.config.stb_id.clone()),
            ("templateName", "EPG8".into()),
            ("areaId", self.config.area_id.clone()),
            ("userToken", encrypt_token),
            ("userGroupId", self.config.user_group_id.clone()),
            ("productPackageId", String::new()),
            ("mac", self.config.stb_mac.clone()),
            ("SoftwareVersion", String::new()),
            ("VIP", String::new()),
        ];
        let validation_path = origin
            .join("/EPG/jsp/ValidAuthenticationHWCU.jsp")
            .map_err(|_| {
                EpgError::class(EpgErrorStage::Authentication, EpgErrorClass::InvalidOrigin)
            })?;
        let validation_form = encode_form(&form);
        let validation = send_with_client_retry(
            &self.control_client,
            |client| {
                client
                    .post(validation_path.clone())
                    .header("Content-Type", "application/x-www-form-urlencoded")
                    .body(validation_form.clone())
            },
            &super::session::TokioSleeper,
            EpgErrorStage::Authentication,
            Arc::clone(&self.control_permits),
            self.config.pool_timeout,
            self.config.write_timeout,
        )
        .await?;
        ensure_status(&validation, EpgErrorStage::Authentication, 1)?;
        cookie_snapshot.extend(capture_cookie_snapshot(&validation));
        let body = validation
            .text()
            .await
            .map_err(|_| EpgError::class(EpgErrorStage::Authentication, EpgErrorClass::Parse))?;
        let user_token = extract_config(&body, "UserToken")?;
        let session_id = extract_config(&body, "SessionID")?;
        let temp_key = identity_encode(&session_id, &extract_config_optional(&body, "shareKey"));
        Ok(SessionBundle::new(
            generation,
            Instant::now(),
            origin,
            user_token,
            session_id,
            temp_key,
            self.control_client.clone(),
            self.media_client.clone(),
            Arc::clone(&self.media_permits),
            self.config.pool_timeout,
            cookie_snapshot,
        ))
    }
}

fn build_client(source_ip: IpAddr, jar: Arc<Jar>) -> Result<Client, EpgError> {
    Client::builder()
        .cookie_provider(jar)
        .use_rustls_tls()
        .http1_only()
        .no_proxy()
        .retry(reqwest::retry::never())
        .redirect(Policy::limited(20))
        .local_address(source_ip)
        .tcp_nodelay(true)
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(30))
        .read_timeout(Duration::from_secs(30))
        .pool_idle_timeout(Duration::from_secs(30))
        .pool_max_idle_per_host(10)
        .default_headers({
            let mut headers = reqwest::header::HeaderMap::new();
            headers.insert(
                reqwest::header::USER_AGENT,
                reqwest::header::HeaderValue::from_static(USER_AGENT),
            );
            headers.insert(
                reqwest::header::ACCEPT_LANGUAGE,
                reqwest::header::HeaderValue::from_static(ACCEPT_LANGUAGE),
            );
            headers.insert(
                "X-Requested-With",
                reqwest::header::HeaderValue::from_static(REQUESTED_WITH),
            );
            headers
        })
        .build()
        .map_err(|_| EpgError::class(EpgErrorStage::Request, EpgErrorClass::Transport))
}

fn ensure_status(response: &Response, stage: EpgErrorStage, attempt: u8) -> Result<(), EpgError> {
    response
        .error_for_status_ref()
        .map(|_| ())
        .map_err(|error| {
            EpgError::status(
                stage,
                error.status().map_or(0, |status| status.as_u16()),
                attempt,
            )
        })
}

fn final_origin(url: &Url) -> Result<Url, EpgError> {
    validate_origin(url, EpgErrorStage::Discovery)?;
    let mut origin = url.clone();
    origin.set_path("");
    origin.set_query(None);
    origin.set_fragment(None);
    Ok(origin)
}

fn parse_epg_url(value: &str, stage: EpgErrorStage) -> Result<Url, EpgError> {
    let authority = value
        .split_once("://")
        .and_then(|(_, rest)| rest.split(['/', '?', '#']).next())
        .unwrap_or_default();
    if authority.contains('@') {
        return Err(EpgError::class(stage, EpgErrorClass::InvalidOrigin));
    }
    let url = Url::parse(value).map_err(|_| EpgError::class(stage, EpgErrorClass::Parse))?;
    validate_origin(&url, stage)?;
    Ok(url)
}

fn validate_origin(url: &Url, stage: EpgErrorStage) -> Result<(), EpgError> {
    let authority = url
        .as_str()
        .split_once("://")
        .and_then(|(_, rest)| rest.split(['/', '?', '#']).next())
        .unwrap_or_default();
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || url.port().is_none()
        || authority.contains('@')
    {
        return Err(EpgError::class(stage, EpgErrorClass::InvalidOrigin));
    }
    Ok(())
}

fn extract_query_token(body: &str, name: &'static str) -> Result<String, EpgError> {
    for (index, _) in body.match_indices(name) {
        if index > 0
            && body[..index]
                .chars()
                .next_back()
                .is_some_and(|character| character.is_ascii_alphanumeric() || character == '_')
        {
            continue;
        }
        let mut suffix = &body[index + name.len()..];
        suffix = suffix.trim_start();
        let Some(value) = suffix.strip_prefix('=') else {
            continue;
        };
        let value = value
            .strip_prefix('"')
            .or_else(|| value.strip_prefix('\''))
            .unwrap_or(value)
            .split(['&', '"', '\'', '<', '>', ' ', '\t', '\r', '\n'])
            .next()
            .unwrap_or("");
        let decoded = percent_decode(value);
        if !decoded.is_empty() {
            return Ok(decoded);
        }
    }
    Err(EpgError::field(EpgErrorStage::Login, name))
}

fn extract_config(body: &str, name: &'static str) -> Result<String, EpgError> {
    let value = extract_config_optional(body, name);
    if value.is_empty() {
        Err(EpgError::field(EpgErrorStage::Authentication, name))
    } else {
        Ok(value)
    }
}

fn extract_config_optional(body: &str, name: &str) -> String {
    ["CU", "CTC"]
        .iter()
        .find_map(|prefix| {
            let marker = format!("{prefix}SetConfig('{name}','");
            body.split_once(&marker)
                .and_then(|(_, rest)| rest.split_once("')"))
                .map(|(value, _)| value.to_owned())
        })
        .unwrap_or_default()
}

fn capture_cookie_snapshot(response: &Response) -> Vec<String> {
    response
        .headers()
        .get_all(reqwest::header::SET_COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .filter_map(|value| value.split(';').next())
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .collect()
}

fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut output = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            if let (Some(high), Some(low)) =
                (hex_digit(bytes[index + 1]), hex_digit(bytes[index + 2]))
            {
                output.push(high * 16 + low);
                index += 3;
                continue;
            }
        }
        output.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&output).into_owned()
}

fn hex_digit(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

fn encode_form<N: AsRef<str>, V: AsRef<str>>(fields: &[(N, V)]) -> String {
    fields
        .iter()
        .map(|(name, value)| {
            format!(
                "{}={}",
                form_encode(name.as_ref()),
                form_encode(value.as_ref())
            )
        })
        .collect::<Vec<_>>()
        .join("&")
}

fn form_encode(value: &str) -> String {
    let mut encoded = String::new();
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                encoded.push(char::from(byte));
            }
            b' ' => encoded.push('+'),
            other => {
                let _ = write!(encoded, "%{other:02X}");
            }
        }
    }
    encoded
}
#[cfg(test)]
#[expect(
    clippy::expect_used,
    reason = "loopback fixture failures identify protocol regressions"
)]
#[path = "client_tests.rs"]
mod tests;
