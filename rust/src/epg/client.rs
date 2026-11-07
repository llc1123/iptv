use std::collections::BTreeMap;
use std::fmt;
use std::fmt::Write as _;
use std::net::IpAddr;
use std::time::{Duration, Instant};

use reqwest::redirect::Policy;
use reqwest::{Client, Response, Url};
use thiserror::Error;
use tokio::sync::Mutex;

use crate::crypto::epg_auth::{AuthenticatorInput, identity_encode, make_authenticator};

use super::channel_parser::{ChannelGroupSource, merge_category_groups, non_empty_codes};
use super::models::{BootChannel, Programme};
use super::{parse_boot_channels, parse_channel_list, parse_programmes, parse_tvod};

const CHANNELS_PATH: &str = "/EPG/jsp/getchannellistHWCU.jsp";
const HLS_PATH: &str = "/EPG/jsp/jkjiaoyutest/en/epg4ChannelPlayByAjax.jsp";
const CHANNEL_AJAX_PATH: &str = "/EPG/jsp/jkjiaoyutest/en/function/ajax/epg7getChannelByAjax.jsp";
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
const AUTH_REFRESH: Duration = Duration::from_secs(600);

/// Configuration required by the EPG provider.
#[derive(Clone)]
pub struct EpgConfig {
    pub authentication_url: String,
    pub user_id: String,
    pub password: String,
    pub stb_id: String,
    pub stb_mac: String,
    pub source_ip: IpAddr,
    pub stb_type: String,
    pub area_id: String,
    pub user_group_id: String,
}

impl fmt::Debug for EpgConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EpgConfig")
            .field("authentication_url", &self.authentication_url)
            .field("user_id", &"***")
            .field("password", &"***")
            .field("stb_id", &self.stb_id)
            .field("stb_mac", &self.stb_mac)
            .field("source_ip", &self.source_ip)
            .field("stb_type", &self.stb_type)
            .field("area_id", &self.area_id)
            .field("user_group_id", &self.user_group_id)
            .finish()
    }
}

/// Errors returned by EPG transport, authentication, and response parsing.
#[derive(Debug, Error)]
pub enum EpgError {
    #[error("EPG HTTP request failed")]
    Http(#[source] reqwest::Error),
    #[error("EPG returned HTTP status {0}")]
    Status(u16),
    #[error("EPG discovery failed")]
    Discovery,
    #[error("EPG response omitted {0}")]
    MissingField(&'static str),
    #[error("EPG provider rejected the request: {0}")]
    Provider(String),
    #[error("EPG response could not be parsed: {0}")]
    Parse(String),
}

/// Parameters for one programme-window request.
pub struct ProgrammeQuery {
    pub channel_id: String,
    pub channel_code: String,
    pub start_time_ms: i64,
    pub end_time_ms: i64,
}

#[derive(Clone)]
struct Session {
    origin: Url,
    user_token: String,
    temp_key: String,
    authenticated_at: Instant,
}

/// Shared authenticated EPG client.
#[derive(Clone)]
pub struct EpgClient {
    config: std::sync::Arc<EpgConfig>,
    client: Client,
    session: std::sync::Arc<Mutex<Option<Session>>>,
}

impl fmt::Debug for EpgClient {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("EpgClient { config: [REDACTED], session: [REDACTED] }")
    }
}

impl EpgClient {
    /// Builds a cookie-preserving HTTP client bound to the configured source address.
    ///
    /// # Errors
    /// Returns [`EpgError::Http`] when reqwest rejects the client configuration.
    pub fn new(config: EpgConfig) -> Result<Self, EpgError> {
        let client = Client::builder()
            .cookie_store(true)
            .use_rustls_tls()
            .http1_only()
            .no_proxy()
            .redirect(Policy::limited(20))
            .local_address(config.source_ip)
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
                    reqwest::header::HeaderValue::from_static("zh-CN,en-US;q=0.9"),
                );
                headers.insert(
                    "X-Requested-With",
                    reqwest::header::HeaderValue::from_static("com.skyworth.iptv"),
                );
                headers
            })
            .build()
            .map_err(EpgError::Http)?;
        Ok(Self {
            config: std::sync::Arc::new(config),
            client,
            session: std::sync::Arc::new(Mutex::new(None)),
        })
    }

    /// Performs discovery, login, and validation, replacing the current session atomically.
    ///
    /// # Errors
    /// Returns an error when discovery, authentication, transport, or response parsing fails.
    pub async fn authenticate(&self) -> Result<(), EpgError> {
        let mut session = self.session.lock().await;
        self.authenticate_locked(&mut session).await
    }

    async fn authenticate_locked(&self, session: &mut Option<Session>) -> Result<(), EpgError> {
        let discovered = self.discovery().await?;
        let login = self
            .send(
                self.client
                    .post(
                        discovered
                            .join("/EPG/jsp/authLoginHWCU.jsp")
                            .map_err(|_| EpgError::Discovery)?,
                    )
                    .header("Content-Type", "application/x-www-form-urlencoded")
                    .body(form(&[("UserID", &self.config.user_id), ("VIP", "")])),
            )
            .await?;
        let login_body = login.text().await.map_err(EpgError::Http)?;
        let encrypt_token =
            extract(&login_body, "EncryptToken").ok_or(EpgError::MissingField("EncryptToken"))?;
        let authenticator = make_authenticator(&AuthenticatorInput {
            encrypt_token: &encrypt_token,
            user_id: &self.config.user_id,
            stb_id: &self.config.stb_id,
            ip_address: &self.config.source_ip.to_string(),
            mac: &self.config.stb_mac,
            password: &self.config.password,
            custom_str: "$CTC",
            random_value: random_nonce(),
        })
        .map_err(|_| EpgError::Provider("authentication failed".to_owned()))?;
        let validation_body = form(&[
            ("UserID", &self.config.user_id),
            ("Lang", "0"),
            ("SupportHD", "1"),
            ("NetUserID", ""),
            ("Authenticator", &authenticator),
            ("STBType", &self.config.stb_type),
            ("STBVersion", ""),
            ("conntype", "dhcp"),
            ("STBID", &self.config.stb_id),
            ("templateName", "EPG8"),
            ("areaId", &self.config.area_id),
            ("userToken", &encrypt_token),
            ("userGroupId", &self.config.user_group_id),
            ("productPackageId", ""),
            ("mac", &self.config.stb_mac),
            ("SoftwareVersion", ""),
            ("VIP", ""),
        ]);
        let validation_url = discovered
            .join("/EPG/jsp/ValidAuthenticationHWCU.jsp")
            .map_err(|_| EpgError::Discovery)?;
        let validation = self
            .send(
                self.client
                    .post(validation_url)
                    .header("Content-Type", "application/x-www-form-urlencoded")
                    .body(validation_body),
            )
            .await?;
        let body = validation.text().await.map_err(EpgError::Http)?;
        let user_token =
            extract_js_config(&body, "UserToken").ok_or(EpgError::MissingField("UserToken"))?;
        let session_id =
            extract_js_config(&body, "SessionID").ok_or(EpgError::MissingField("SessionID"))?;
        let share_key = extract_js_config(&body, "shareKey").unwrap_or_default();
        *session = Some(Session {
            origin: discovered,
            user_token,
            temp_key: identity_encode(&session_id, &share_key),
            authenticated_at: Instant::now(),
        });
        Ok(())
    }

    /// Returns boot channels with provider codes and category labels merged in.
    pub async fn channels(&self) -> Result<Vec<BootChannel>, EpgError> {
        let session = self.ensure_session().await?;
        let body = self
            .post_form(
                &session,
                CHANNELS_PATH,
                &[
                    ("conntype", "dhcp"),
                    ("UserToken", &session.user_token),
                    ("tempKey", &session.temp_key),
                    ("stbid", "310019"),
                    ("SupportHD", "1"),
                    ("UserID", &self.config.user_id),
                    ("Lang", "1"),
                ],
                false,
            )
            .await?;
        let mut channels = parse_boot_channels(&body)
            .map_err(|_| EpgError::Parse("boot channel response".to_owned()))?;
        let codes = self.channel_category(&session, LIVE_CATEGORY_ID).await?;
        let mut groups = Vec::new();
        for (category, title) in GROUP_CATEGORIES {
            if let Ok(response) = self.channel_list(&session, category).await {
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

    /// Resolves a live channel's HLS address.
    pub async fn hls_url(&self, channel_id: &str) -> Result<String, EpgError> {
        let session = self.ensure_session().await?;
        let mut url = session
            .origin
            .join(HLS_PATH)
            .map_err(|_| EpgError::Discovery)?;
        url.query_pairs_mut()
            .append_pair("action", "getChannelHlsUrl")
            .append_pair("channelId", channel_id);
        let request = self
            .client
            .get(url)
            .header("VIS-AJAX", "AjaxHttpRequest")
            .header("Accept", "*/*");
        let response = self.send(request).await?;
        let value: serde_json::Value =
            serde_json::from_str(&response.text().await.map_err(EpgError::Http)?)
                .map_err(|_| EpgError::Parse("hls response".to_owned()))?;
        value
            .get("hlsUrl")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
            .ok_or(EpgError::MissingField("hlsUrl"))
    }

    /// Resolves a programme's TVOD playback address.
    pub async fn tvod_url(&self, programme: &Programme) -> Result<String, EpgError> {
        let session = self.ensure_session().await?;
        let start_time = div_seconds(programme.start_time_ms);
        let end_time = div_seconds(programme.end_time_ms);
        let body = self
            .post_form(
                &session,
                CHANNEL_AJAX_PATH,
                &[
                    ("action", "getTvodPlayUrl"),
                    ("channelID", programme.channel_id.as_str()),
                    ("playbillID", programme.programme_id.as_str()),
                    ("startTime", &start_time),
                    ("endTime", &end_time),
                ],
                true,
            )
            .await?;
        let address = parse_tvod(&body).map_err(|error| match error.kind {
            super::programme_parser::ProgrammeParserErrorKind::ProviderStatus => {
                EpgError::Provider("tvod request rejected".to_owned())
            }
            super::programme_parser::ProgrammeParserErrorKind::InvalidJson => {
                EpgError::Parse("tvod response".to_owned())
            }
        })?;
        Ok(address.play_url)
    }

    /// Fetches programmes for one channel and time window.
    pub async fn programmes(&self, query: &ProgrammeQuery) -> Result<Vec<Programme>, EpgError> {
        let session = self.ensure_session().await?;
        let end_time = query.end_time_ms.to_string();
        let start_time = query.start_time_ms.to_string();
        let body = self
            .post_form(
                &session,
                CHANNEL_AJAX_PATH,
                &[
                    ("action", "getChannelProg"),
                    ("code", query.channel_code.as_str()),
                    ("channelID", query.channel_id.as_str()),
                    ("endTime", &end_time),
                    ("startTime", &start_time),
                    ("offset", "0"),
                    ("limit", "2000"),
                ],
                true,
            )
            .await?;
        parse_programmes(&body).map_err(|error| match error.kind {
            super::programme_parser::ProgrammeParserErrorKind::ProviderStatus => {
                EpgError::Provider("programme request rejected".to_owned())
            }
            super::programme_parser::ProgrammeParserErrorKind::InvalidJson => {
                EpgError::Parse("programme response".to_owned())
            }
        })
    }

    /// Returns the shared HTTP client used for authenticated and media requests.
    #[must_use]
    pub fn http_client(&self) -> &Client {
        &self.client
    }

    async fn ensure_session(&self) -> Result<Session, EpgError> {
        let mut session = self.session.lock().await;
        if session
            .as_ref()
            .is_some_and(|value| value.authenticated_at.elapsed() < AUTH_REFRESH)
        {
            return session.clone().ok_or(EpgError::Discovery);
        }
        self.authenticate_locked(&mut session).await?;
        session.clone().ok_or(EpgError::Discovery)
    }

    async fn discovery(&self) -> Result<Url, EpgError> {
        let mut url =
            Url::parse(&self.config.authentication_url).map_err(|_| EpgError::Discovery)?;
        url.query_pairs_mut()
            .append_pair("UserID", &self.config.user_id)
            .append_pair("Action", "Login")
            .append_pair("Mode", "MENU");
        let response = self.send(self.client.get(url)).await?;
        let mut origin = response.url().clone();
        if origin.host_str().is_none() {
            return Err(EpgError::Discovery);
        }
        origin.set_path("");
        origin.set_query(None);
        origin.set_fragment(None);
        Ok(origin)
    }

    async fn channel_category(
        &self,
        session: &Session,
        category: &str,
    ) -> Result<BTreeMap<String, String>, EpgError> {
        let response = self.channel_list(session, category).await?;
        Ok(non_empty_codes(&response.data))
    }

    async fn channel_list(
        &self,
        session: &Session,
        category: &str,
    ) -> Result<super::models::ChannelListResponse, EpgError> {
        let body = self
            .post_form(
                session,
                CHANNEL_AJAX_PATH,
                &[
                    ("action", "getChannelList"),
                    ("cateID", category),
                    ("type", ""),
                ],
                true,
            )
            .await?;
        parse_channel_list(&body).map_err(|error| match error.kind {
            super::channel_parser::ChannelParserErrorKind::ProviderStatus => {
                EpgError::Provider("channel category rejected".to_owned())
            }
            super::channel_parser::ChannelParserErrorKind::InvalidJson => {
                EpgError::Parse("channel category response".to_owned())
            }
        })
    }

    async fn post_form(
        &self,
        session: &Session,
        path: &str,
        fields: &[(&str, &str)],
        ajax: bool,
    ) -> Result<String, EpgError> {
        let url = session.origin.join(path).map_err(|_| EpgError::Discovery)?;
        let mut request = self
            .client
            .post(url)
            .header("Content-Type", "application/x-www-form-urlencoded");
        if ajax {
            request = request
                .header("VIS-AJAX", "AjaxHttpRequest")
                .header("Accept", "*/*");
        }
        self.send(request.body(form(fields)))
            .await?
            .text()
            .await
            .map_err(EpgError::Http)
    }

    async fn send(&self, request: reqwest::RequestBuilder) -> Result<Response, EpgError> {
        let response = request.send().await.map_err(EpgError::Http)?;
        if response.status().is_success() {
            Ok(response)
        } else {
            Err(EpgError::Status(response.status().as_u16()))
        }
    }
}

fn div_seconds(value: i64) -> String {
    value.div_euclid(1_000).to_string()
}

fn random_nonce() -> u32 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.subsec_nanos() % 100_000_000)
}

fn form(fields: &[(&str, &str)]) -> String {
    fields
        .iter()
        .map(|(name, value)| format!("{}={}", form_encode(name), form_encode(value)))
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

fn extract(body: &str, name: &str) -> Option<String> {
    let bytes = body.as_bytes();
    let mut cursor = 0;
    while let Some(relative) = body[cursor..].find(name) {
        let start = cursor + relative;
        let boundary = start == 0
            || !body[..start]
                .chars()
                .next_back()
                .is_some_and(|character| character.is_ascii_alphanumeric() || character == '_');
        if boundary {
            let mut index = start + name.len();
            while bytes.get(index).is_some_and(u8::is_ascii_whitespace) {
                index += 1;
            }
            if bytes.get(index) == Some(&b'=') {
                index += 1;
                while bytes.get(index).is_some_and(u8::is_ascii_whitespace) {
                    index += 1;
                }
                if matches!(bytes.get(index), Some(b'"' | b'\'')) {
                    index += 1;
                }
                let end = body[index..]
                    .find(['&', '"', '\'', '<', '>', ' ', '\t', '\r', '\n'])
                    .map_or(body.len(), |offset| index + offset);
                return Some(percent_decode(&body[index..end]));
            }
        }
        cursor = start + name.len();
    }
    None
}

fn extract_js_config(body: &str, name: &str) -> Option<String> {
    ["CU", "CTC"].iter().find_map(|prefix| {
        let marker = format!("{prefix}SetConfig('{name}','");
        body.split_once(&marker)
            .and_then(|(_, rest)| rest.split_once("')"))
            .map(|(value, _)| value.to_owned())
    })
}

fn percent_decode(value: &str) -> String {
    let mut bytes = Vec::with_capacity(value.len());
    let raw = value.as_bytes();
    let mut index = 0;
    while index < raw.len() {
        if raw[index] == b'%' && index + 2 < raw.len() {
            if let (Some(high), Some(low)) = (hex(raw[index + 1]), hex(raw[index + 2])) {
                bytes.push(high * 16 + low);
                index += 3;
                continue;
            }
        }
        bytes.push(raw[index]);
        index += 1;
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

fn hex(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}
