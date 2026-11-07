use std::collections::BTreeMap;
use std::fmt;
use std::fmt::Write;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use reqwest::cookie::Jar;
use reqwest::redirect::Policy;
use reqwest::{Client, Response, Url};
use serde_json::from_str;
use tokio::sync::Semaphore;

use crate::crypto::epg_auth::{AuthenticatorInput, identity_encode, make_authenticator};

use super::request::send_with_client_retry;
use super::session::SessionBundle;
use super::session::{AuthCoordinator, SessionClock};
use super::{
    ChannelGroupSource, merge_category_groups, parse_boot_channels, parse_channel_list,
    parse_programmes, parse_tvod, tvod_times_seconds,
};
use crate::lifecycle::CancellationToken;

const CHANNELS_PATH: &str = "/EPG/jsp/getchannellistHWCU.jsp";
const CHANNEL_AJAX_PATH: &str = "/EPG/jsp/jkjiaoyutest/en/function/ajax/epg7getChannelByAjax.jsp";
const HLS_PATH: &str = "/EPG/jsp/jkjiaoyutest/en/epg4ChannelPlayByAjax.jsp";
const TVOD_RESPONSE_LIMIT: usize = 1024 * 1024;
const LIVE_CATEGORY_ID: &str = "10000100000000090000000000001692";
const GROUP_CATEGORIES: [(&str, &str); 7] = [
    ("catauto5122", "4K超高清"),
    ("catauto2625", "央视"),
    ("10000100000000090000000000001691", "本地"),
    ("10000100000000090000000000001689", "卫视"),
    ("catauto2561", "新闻"),
    ("10000100000000090000000000001690", "高清"),
    ("10000100000000090000000000005665", "付费频道"),
];

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

    /// Creates a client whose coordinator observes service shutdown.
    ///
    /// # Errors
    /// Returns a redacted transport error when reqwest rejects client construction.
    pub fn with_shutdown(config: EpgConfig, shutdown: CancellationToken) -> Result<Self, EpgError> {
        Self::with_coordinator(
            config,
            AuthCoordinator::with_shutdown(Arc::new(SessionClock), shutdown),
        )
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

    /// Authenticates on demand while binding the shared flight to shutdown.
    ///
    /// # Errors
    /// Returns only safe stage, class, status, field, and attempt information.
    pub async fn authenticate_with_shutdown(
        &self,
        shutdown: crate::lifecycle::CancellationToken,
    ) -> Result<Arc<SessionBundle>, EpgError> {
        let client = self.clone();
        self.coordinator
            .ensure_authenticated_with_shutdown(
                move |generation| async move {
                    client
                        .authenticate_generation(generation)
                        .await
                        .map(Arc::new)
                },
                shutdown,
            )
            .await
    }

    /// Reauthenticates after a request observed the supplied session generation expire.
    ///
    /// # Errors
    /// Returns only safe stage, class, status, field, and attempt information.
    pub async fn force_reauthenticate_with_shutdown(
        &self,
        observed_generation: u64,
        shutdown: crate::lifecycle::CancellationToken,
    ) -> Result<Arc<SessionBundle>, EpgError> {
        let client = self.clone();
        self.coordinator
            .force_reauthenticate_with_shutdown(
                observed_generation,
                move |generation| async move {
                    client
                        .authenticate_generation(generation)
                        .await
                        .map(Arc::new)
                },
                shutdown,
            )
            .await
    }

    /// Returns the most recently committed generation.
    pub async fn current_session(&self) -> Option<Arc<SessionBundle>> {
        self.coordinator.current().await
    }

    /// Fetches boot channels, required codes, and optional category groups.
    ///
    /// # Errors
    /// Returns a redacted request, status, parse, or provider error. Code failures are hard;
    /// optional category failures are skipped to preserve provider compatibility.
    pub async fn fetch_channels(
        &self,
        session: &SessionBundle,
    ) -> Result<Vec<super::models::BootChannel>, EpgError> {
        let path = session
            .origin()
            .join(CHANNELS_PATH)
            .map_err(|_| EpgError::class(EpgErrorStage::Request, EpgErrorClass::InvalidOrigin))?;
        let form = [
            ("conntype", "dhcp".to_owned()),
            ("UserToken", session.user_token().to_owned()),
            ("tempKey", session.temp_key().to_owned()),
            ("stbid", "310019".to_owned()),
            ("SupportHD", "1".to_owned()),
            ("UserID", self.config.user_id.clone()),
            ("Lang", "1".to_owned()),
        ];
        let response = send_with_client_retry(
            session.control_client(),
            |client| {
                client
                    .post(path.clone())
                    .header("Content-Type", "application/x-www-form-urlencoded")
                    .body(encode_form(&form))
            },
            &super::session::TokioSleeper,
            EpgErrorStage::Request,
            Arc::clone(&self.control_permits),
            self.config.pool_timeout,
            self.config.write_timeout,
        )
        .await?;
        ensure_status(&response, EpgErrorStage::Request, 1)?;
        let body = response
            .text()
            .await
            .map_err(|_| EpgError::class(EpgErrorStage::Request, EpgErrorClass::Parse))?;
        let mut channels = parse_boot_channels(&body)
            .map_err(|_| EpgError::class(EpgErrorStage::Request, EpgErrorClass::Parse))?;
        let codes = self
            .fetch_channel_category(session, LIVE_CATEGORY_ID)
            .await?;
        let mut groups = Vec::new();
        for (category, title) in GROUP_CATEGORIES {
            if let Ok(response) = self
                .fetch_channel_category_response(session, category)
                .await
            {
                groups.push(ChannelGroupSource {
                    title: title.to_owned(),
                    response: Ok(response),
                });
            }
        }
        let group_map = merge_category_groups(&groups);
        for channel in &mut channels {
            channel.code = codes.get(&channel.channel_id).cloned().unwrap_or_default();
            channel.group_title = group_map
                .get(&channel.channel_id)
                .cloned()
                .unwrap_or_else(|| "全部".to_owned());
        }
        Ok(channels)
    }

    /// Fetches one programme window through the shared authenticated control client.
    ///
    /// # Errors
    /// Returns a redacted request, status, parse, or provider error. The provider's `051002`
    /// empty-window response is returned as an empty vector.
    pub async fn fetch_programmes(
        &self,
        session: &SessionBundle,
        channel_id: &str,
        channel_code: &str,
        start_time_ms: i64,
        end_time_ms: i64,
    ) -> Result<Vec<super::models::Programme>, EpgError> {
        let path = session
            .origin()
            .join(CHANNEL_AJAX_PATH)
            .map_err(|_| EpgError::class(EpgErrorStage::Request, EpgErrorClass::InvalidOrigin))?;
        let form = [
            ("action", "getChannelProg".to_owned()),
            ("code", channel_code.to_owned()),
            ("channelID", channel_id.to_owned()),
            ("endTime", end_time_ms.to_string()),
            ("startTime", start_time_ms.to_string()),
            ("offset", "0".to_owned()),
            ("limit", "2000".to_owned()),
        ];
        let response = send_with_client_retry(
            session.control_client(),
            |client| {
                client
                    .post(path.clone())
                    .header("VIS-AJAX", "AjaxHttpRequest")
                    .header("Accept", "*/*")
                    .header("Content-Type", "application/x-www-form-urlencoded")
                    .body(encode_form(&form))
            },
            &super::session::TokioSleeper,
            EpgErrorStage::Request,
            Arc::clone(&self.control_permits),
            self.config.pool_timeout,
            self.config.write_timeout,
        )
        .await?;
        ensure_status(&response, EpgErrorStage::Request, 1)?;
        let body = response
            .text()
            .await
            .map_err(|_| EpgError::class(EpgErrorStage::Request, EpgErrorClass::Parse))?;
        parse_programmes(&body).map_err(|error| match error.kind {
            super::programme_parser::ProgrammeParserErrorKind::ProviderStatus => {
                EpgError::status(EpgErrorStage::Request, 0, 1)
            }
            super::programme_parser::ProgrammeParserErrorKind::InvalidJson => {
                EpgError::class(EpgErrorStage::Request, EpgErrorClass::Parse)
            }
        })
    }

    /// Resolves one live channel through the Python-compatible HLS operation.
    ///
    /// # Errors
    /// Returns a redacted request, provider, cancellation, parse, or invalid playback URL error.
    pub async fn hls_url(
        &self,
        channel_id: &str,
        shutdown: CancellationToken,
    ) -> Result<Url, EpgError> {
        let mut session = self.authenticate_with_shutdown(shutdown.clone()).await?;
        for attempt in 0..2_u8 {
            match self.fetch_hls(&session, channel_id, shutdown.clone()).await {
                Ok(address) => return validate_play_url(&address.hls_url),
                Err(error) if attempt == 0 && matches!(error.status_code(), Some(401 | 403)) => {
                    session = self
                        .force_reauthenticate_with_shutdown(session.generation(), shutdown.clone())
                        .await?;
                }
                Err(error) => return Err(error),
            }
        }
        Err(EpgError::class(
            EpgErrorStage::Request,
            EpgErrorClass::Transport,
        ))
    }

    /// Resolves one programme through the authenticated TVOD operation.
    ///
    /// # Errors
    /// Returns a redacted request, provider, cancellation, or invalid playback URL error.
    pub async fn tvod_url(
        &self,
        programme: &super::models::Programme,
        shutdown: CancellationToken,
    ) -> Result<Url, EpgError> {
        let mut session = self.authenticate_with_shutdown(shutdown.clone()).await?;
        for attempt in 0..2_u8 {
            match self.fetch_tvod(&session, programme, shutdown.clone()).await {
                Ok(address) => return validate_play_url(&address.play_url),
                Err(error) if attempt == 0 && matches!(error.status_code(), Some(401 | 403)) => {
                    session = self
                        .force_reauthenticate_with_shutdown(session.generation(), shutdown.clone())
                        .await?;
                }
                Err(error) => return Err(error),
            }
        }
        Err(EpgError::class(
            EpgErrorStage::Request,
            EpgErrorClass::Transport,
        ))
    }

    async fn fetch_tvod(
        &self,
        session: &SessionBundle,
        programme: &super::models::Programme,
        shutdown: CancellationToken,
    ) -> Result<super::models::TvodAddress, EpgError> {
        let path = session
            .origin()
            .join(CHANNEL_AJAX_PATH)
            .map_err(|_| EpgError::class(EpgErrorStage::Request, EpgErrorClass::InvalidOrigin))?;
        let (start_time, end_time) =
            tvod_times_seconds(programme).map_err(|error| match error.kind {
                super::programme_parser::ProgrammeParserErrorKind::ProviderStatus => {
                    EpgError::status(EpgErrorStage::Request, 0, 1)
                }
                super::programme_parser::ProgrammeParserErrorKind::InvalidJson => {
                    EpgError::class(EpgErrorStage::Request, EpgErrorClass::Parse)
                }
            })?;
        let form = [
            ("action", "getTvodPlayUrl".to_owned()),
            ("channelID", programme.channel_id.clone()),
            ("playbillID", programme.programme_id.clone()),
            ("startTime", start_time.to_string()),
            ("endTime", end_time.to_string()),
        ];
        let response = tokio::select! {
            () = shutdown.cancelled() => return Err(EpgError::cancelled(EpgErrorStage::Request)),
            response = send_with_client_retry(
                session.control_client(),
                |client| {
                    client
                        .post(path.clone())
                        .header("VIS-AJAX", "AjaxHttpRequest")
                        .header("Accept", "*/*")
                        .header("Content-Type", "application/x-www-form-urlencoded")
                        .body(encode_form(&form))
                },
                &super::session::TokioSleeper,
                EpgErrorStage::Request,
                Arc::clone(&self.control_permits),
                self.config.pool_timeout,
                self.config.write_timeout,
            ) => response?,
        };
        ensure_status(&response, EpgErrorStage::Request, 1)?;
        let body = tokio::select! {
            () = shutdown.cancelled() => return Err(EpgError::cancelled(EpgErrorStage::Request)),
            body = bounded_response_text(response) => body?,
        };
        parse_tvod(&body).map_err(|error| match error.kind {
            super::programme_parser::ProgrammeParserErrorKind::ProviderStatus => {
                EpgError::status(EpgErrorStage::Request, 0, 1)
            }
            super::programme_parser::ProgrammeParserErrorKind::InvalidJson => {
                EpgError::class(EpgErrorStage::Request, EpgErrorClass::Parse)
            }
        })
    }

    async fn fetch_hls(
        &self,
        session: &SessionBundle,
        channel_id: &str,
        shutdown: CancellationToken,
    ) -> Result<super::models::HlsAddress, EpgError> {
        let path = session
            .origin()
            .join(HLS_PATH)
            .map_err(|_| EpgError::class(EpgErrorStage::Request, EpgErrorClass::InvalidOrigin))?;
        let mut path = path;
        path.query_pairs_mut()
            .append_pair("action", "getChannelHlsUrl")
            .append_pair("channelId", channel_id);
        let response = tokio::select! {
            () = shutdown.cancelled() => return Err(EpgError::cancelled(EpgErrorStage::Request)),
            response = send_with_client_retry(
                session.control_client(),
                |client| {
                    client
                        .get(path.clone())
                        .header("VIS-AJAX", "AjaxHttpRequest")
                        .header("Accept", "*/*")
                },
                &super::session::TokioSleeper,
                EpgErrorStage::Request,
                Arc::clone(&self.control_permits),
                self.config.pool_timeout,
                self.config.write_timeout,
            ) => response?,
        };
        ensure_status(&response, EpgErrorStage::Request, 1)?;
        let body = tokio::select! {
            () = shutdown.cancelled() => return Err(EpgError::cancelled(EpgErrorStage::Request)),
            body = bounded_response_text(response) => body?,
        };
        from_str(&body).map_err(|_| EpgError::class(EpgErrorStage::Request, EpgErrorClass::Parse))
    }

    async fn fetch_channel_category_response(
        &self,
        session: &SessionBundle,
        category: &str,
    ) -> Result<super::models::ChannelListResponse, EpgError> {
        let path = session
            .origin()
            .join(CHANNEL_AJAX_PATH)
            .map_err(|_| EpgError::class(EpgErrorStage::Request, EpgErrorClass::InvalidOrigin))?;
        let form = [
            ("action", "getChannelList"),
            ("cateID", category),
            ("type", ""),
        ];
        let response = send_with_client_retry(
            session.control_client(),
            |client| {
                client
                    .post(path.clone())
                    .header("VIS-AJAX", "AjaxHttpRequest")
                    .header("Accept", "*/*")
                    .header("Content-Type", "application/x-www-form-urlencoded")
                    .body(encode_form(&form))
            },
            &super::session::TokioSleeper,
            EpgErrorStage::Request,
            Arc::clone(&self.control_permits),
            self.config.pool_timeout,
            self.config.write_timeout,
        )
        .await?;
        ensure_status(&response, EpgErrorStage::Request, 1)?;
        let body = response
            .text()
            .await
            .map_err(|_| EpgError::class(EpgErrorStage::Request, EpgErrorClass::Parse))?;
        parse_channel_list(&body).map_err(|error| match error.kind {
            super::channel_parser::ChannelParserErrorKind::ProviderStatus => {
                EpgError::status(EpgErrorStage::Request, 0, 1)
            }
            super::channel_parser::ChannelParserErrorKind::InvalidJson => {
                EpgError::class(EpgErrorStage::Request, EpgErrorClass::Parse)
            }
        })
    }

    async fn fetch_channel_category(
        &self,
        session: &SessionBundle,
        category: &str,
    ) -> Result<BTreeMap<String, String>, EpgError> {
        let response = self
            .fetch_channel_category_response(session, category)
            .await?;
        Ok(response
            .data
            .into_iter()
            .filter(|channel| !channel.code.is_empty())
            .map(|channel| (channel.epg_id, channel.code))
            .collect())
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

async fn bounded_response_text(mut response: Response) -> Result<String, EpgError> {
    if response
        .content_length()
        .is_some_and(|length| length > TVOD_RESPONSE_LIMIT as u64)
    {
        return Err(EpgError::class(
            EpgErrorStage::Request,
            EpgErrorClass::Parse,
        ));
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| EpgError::class(EpgErrorStage::Request, EpgErrorClass::Parse))?
    {
        let next_length = bytes
            .len()
            .checked_add(chunk.len())
            .ok_or_else(|| EpgError::class(EpgErrorStage::Request, EpgErrorClass::Parse))?;
        if next_length > TVOD_RESPONSE_LIMIT {
            return Err(EpgError::class(
                EpgErrorStage::Request,
                EpgErrorClass::Parse,
            ));
        }
        bytes.extend_from_slice(&chunk);
    }
    String::from_utf8(bytes)
        .map_err(|_| EpgError::class(EpgErrorStage::Request, EpgErrorClass::Parse))
}

fn validate_play_url(value: &str) -> Result<Url, EpgError> {
    let url = Url::parse(value)
        .map_err(|_| EpgError::class(EpgErrorStage::Request, EpgErrorClass::InvalidOrigin))?;
    let host = url
        .host_str()
        .ok_or_else(|| EpgError::class(EpgErrorStage::Request, EpgErrorClass::InvalidOrigin))?;
    let port = url
        .port_or_known_default()
        .ok_or_else(|| EpgError::class(EpgErrorStage::Request, EpgErrorClass::InvalidOrigin))?;
    if !matches!(url.scheme(), "http" | "https")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
        || port == 0
        || host
            .chars()
            .any(|character| character.is_ascii_whitespace() || character == '\\')
    {
        return Err(EpgError::class(
            EpgErrorStage::Request,
            EpgErrorClass::InvalidOrigin,
        ));
    }
    Ok(url)
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
        let value = value.trim_start();
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
