//! HLS upstream URL admission and transport policy.

use std::collections::HashSet;
use std::future::Future;
use std::net::{IpAddr, SocketAddr};
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use reqwest::header::{
    ACCEPT, ACCEPT_ENCODING, CONTENT_ENCODING, HeaderMap, HeaderName, HeaderValue, RANGE,
    USER_AGENT,
};
use reqwest::{Client, StatusCode, Url};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::lifecycle::CancellationToken;

const MAX_URL_BYTES: usize = 4096;
const MAX_REDIRECTS: usize = 10;
const HOST_SHARDS: usize = 64;
const HOST_PERMITS: usize = 64;
const PERMIT_TIMEOUT: Duration = Duration::from_secs(10);
/// Maximum body size accepted from an upstream HLS response.
pub const MAX_RESPONSE_BODY_BYTES: u64 = 64 * 1024 * 1024;

/// A boxed asynchronous resolver result.
pub type ResolveFuture =
    Pin<Box<dyn Future<Output = Result<Vec<SocketAddr>, ResolverError>> + Send>>;

/// Resolver used once for each redirect hop.
pub trait Resolver: Send + Sync {
    /// Resolves a hostname and port without applying policy itself.
    fn resolve(&self, host: &str, port: u16) -> ResolveFuture;
}

/// Fixed resolver failure with no address or hostname disclosure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolverError;
impl std::fmt::Display for ResolverError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("request unavailable")
    }
}
impl std::error::Error for ResolverError {}

/// Fixed HLS policy failures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HlsPolicyError {
    /// The URL or host syntax is invalid.
    InvalidTarget,
    /// The target is outside configured policy.
    DisallowedTarget,
    /// Redirect policy rejected the response.
    RedirectRejected,
    /// The per-host permit deadline elapsed.
    PermitTimeout,
    /// The request was cancelled.
    Cancelled,
    /// Transport failed without retaining source details.
    Transport,
    /// The upstream response body exceeds the fixed policy bound.
    ResponseTooLarge,
}
impl std::fmt::Display for HlsPolicyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("request unavailable")
    }
}
impl std::error::Error for HlsPolicyError {}

/// Request headers admitted to an upstream HLS request.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RequestHeaders {
    /// Optional media negotiation value.
    pub accept: Option<String>,
    /// Optional byte range.
    pub range: Option<String>,
    /// Optional bounded user agent.
    pub user_agent: Option<String>,
}

impl RequestHeaders {
    /// Converts selected headers to a reqwest map.
    ///
    /// # Errors
    /// Returns a fixed error when a selected value exceeds its bound or is not a valid header.
    pub fn to_header_map(&self) -> Result<HeaderMap, HlsPolicyError> {
        let mut map = HeaderMap::new();
        for (name, value) in [
            (ACCEPT, self.accept.as_deref()),
            (RANGE, self.range.as_deref()),
            (USER_AGENT, self.user_agent.as_deref()),
        ] {
            if let Some(value) = value {
                if value.is_empty()
                    || value.len() > 1024
                    || value.bytes().any(|byte| byte.is_ascii_control())
                {
                    return Err(HlsPolicyError::InvalidTarget);
                }
                map.insert(
                    name,
                    HeaderValue::from_str(value).map_err(|_| HlsPolicyError::InvalidTarget)?,
                );
            }
        }
        map.insert(ACCEPT_ENCODING, HeaderValue::from_static("identity"));
        Ok(map)
    }
}

/// A response retaining its host permit until the response is dropped.
pub struct HlsResponse {
    /// Upstream response body and status.
    pub response: reqwest::Response,
    /// Owned permit held until this response is dropped.
    pub permit: OwnedSemaphorePermit,
}
impl std::fmt::Debug for HlsResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("HlsResponse { response: [redacted], permit: held }")
    }
}

/// Bounded upstream response body retained with its host permit.
pub struct HlsBody {
    response: reqwest::Response,
    _permit: OwnedSemaphorePermit,
    bytes_read: u64,
}

impl std::fmt::Debug for HlsBody {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("HlsBody { response: [redacted], permit: held }")
    }
}

impl HlsResponse {
    /// Moves the response into a bounded reader for route-level streaming.
    #[must_use]
    pub fn into_body(self) -> HlsBody {
        HlsBody {
            response: self.response,
            _permit: self.permit,
            bytes_read: 0,
        }
    }
}

impl HlsBody {
    /// Reads one bounded body chunk while retaining the host permit.
    ///
    /// # Errors
    /// Returns a fixed error for transport failures or when the cumulative body exceeds the
    /// policy bound.
    pub async fn next_chunk(&mut self) -> Result<Option<Vec<u8>>, HlsPolicyError> {
        let chunk = self
            .response
            .chunk()
            .await
            .map_err(|_| HlsPolicyError::Transport)?;
        let Some(chunk) = chunk else {
            return Ok(None);
        };
        let chunk_length =
            u64::try_from(chunk.len()).map_err(|_| HlsPolicyError::ResponseTooLarge)?;
        self.bytes_read = self
            .bytes_read
            .checked_add(chunk_length)
            .ok_or(HlsPolicyError::ResponseTooLarge)?;
        if self.bytes_read > MAX_RESPONSE_BODY_BYTES {
            return Err(HlsPolicyError::ResponseTooLarge);
        }
        Ok(Some(chunk.to_vec()))
    }
}

/// Validated HLS upstream policy and bounded host admission.
pub struct HlsPolicy<R> {
    hosts: HashSet<HostRule>,
    cidrs: Vec<Cidr>,
    resolver: Arc<R>,
    shards: Vec<Arc<Semaphore>>,
}

impl<R: Resolver + 'static> HlsPolicy<R> {
    /// Creates a policy from exact host rules and CIDR rules.
    ///
    /// # Errors
    /// Returns a fixed error for malformed host or CIDR rules.
    pub fn new(
        hosts: impl IntoIterator<Item = String>,
        cidrs: impl IntoIterator<Item = String>,
        resolver: Arc<R>,
    ) -> Result<Self, HlsPolicyError> {
        let hosts = hosts
            .into_iter()
            .map(|host| HostRule::parse(&host))
            .collect::<Result<HashSet<_>, _>>()?;
        let cidrs = cidrs
            .into_iter()
            .map(|cidr| Cidr::parse(&cidr))
            .collect::<Result<Vec<_>, _>>()?;
        let shards = (0..HOST_SHARDS)
            .map(|_| Arc::new(Semaphore::new(HOST_PERMITS)))
            .collect();
        Ok(Self {
            hosts,
            cidrs,
            resolver,
            shards,
        })
    }

    /// Validates URL syntax and configured host before DNS resolution.
    ///
    /// # Errors
    /// Returns a fixed error when URL syntax or policy admission fails.
    pub fn validate_url(&self, url: &Url) -> Result<(), HlsPolicyError> {
        let host = url.host_str().ok_or(HlsPolicyError::InvalidTarget)?;
        if url.as_str().len() > MAX_URL_BYTES
            || !matches!(url.scheme(), "http" | "https")
            || !url.username().is_empty()
            || url.password().is_some()
            || url.fragment().is_some()
            || host
                .chars()
                .any(|character| character.is_ascii_whitespace() || character == '\\')
        {
            return Err(HlsPolicyError::InvalidTarget);
        }
        let port = url
            .port_or_known_default()
            .ok_or(HlsPolicyError::InvalidTarget)?;
        if port == 0 {
            return Err(HlsPolicyError::InvalidTarget);
        }
        if self.host_allowed(url)? {
            return Ok(());
        }
        if let Ok(ip) = host.parse::<IpAddr>() {
            if self.cidrs.iter().any(|cidr| cidr.contains(ip)) && !unconditionally_unsafe_ip(ip) {
                return Ok(());
            }
        }
        Err(HlsPolicyError::DisallowedTarget)
    }

    fn host_allowed(&self, url: &Url) -> Result<bool, HlsPolicyError> {
        let rule = HostRule::from_url(url)?;
        let effective_port = url.port_or_known_default();
        Ok(self.hosts.iter().any(|allowed| {
            allowed.host == rule.host && (allowed.port.is_none() || allowed.port == effective_port)
        }))
    }

    /// Resolves and validates every address for one redirect hop.
    ///
    /// # Errors
    /// Returns a fixed error when resolution fails or any address is unsafe.
    pub async fn validate_resolution(&self, url: &Url) -> Result<Vec<SocketAddr>, HlsPolicyError> {
        self.validate_url(url)?;
        let host = url.host_str().ok_or(HlsPolicyError::InvalidTarget)?;
        let port = url
            .port_or_known_default()
            .ok_or(HlsPolicyError::InvalidTarget)?;
        let host_allowed = self.host_allowed(url)?;
        if let Ok(ip) = host.parse::<IpAddr>() {
            return if unconditionally_unsafe_ip(ip) {
                Err(HlsPolicyError::DisallowedTarget)
            } else {
                Ok(vec![SocketAddr::new(ip, port)])
            };
        }
        let addresses = self
            .resolver
            .resolve(host, port)
            .await
            .map_err(|_| HlsPolicyError::Transport)?;
        if addresses.is_empty()
            || addresses.iter().any(|address| {
                unconditionally_unsafe_ip(address.ip())
                    || (!self.cidrs.is_empty()
                        && !self.cidrs.iter().any(|cidr| cidr.contains(address.ip()))
                        && !host_allowed)
            })
        {
            return Err(HlsPolicyError::DisallowedTarget);
        }
        Ok(addresses)
    }

    /// Acquires the bounded media permit with timeout and lifecycle cancellation.
    ///
    /// # Errors
    /// Returns a fixed cancellation or permit-timeout error.
    pub async fn acquire(
        &self,
        host: &str,
        cancellation: &CancellationToken,
    ) -> Result<OwnedSemaphorePermit, HlsPolicyError> {
        if cancellation.is_cancelled() {
            return Err(HlsPolicyError::Cancelled);
        }
        let index = stable_hash(host) % HOST_SHARDS;
        tokio::select! { () = cancellation.cancelled() => Err(HlsPolicyError::Cancelled), permit = tokio::time::timeout(PERMIT_TIMEOUT, self.shards[index].clone().acquire_owned()) => permit.map_err(|_| HlsPolicyError::PermitTimeout).and_then(|result| result.map_err(|_| HlsPolicyError::Cancelled)) }
    }

    /// Builds a no-redirect, no-proxy, retry-free HTTP/1 client pinned to addresses.
    ///
    /// # Errors
    /// Returns a fixed error when the pinned client cannot be constructed.
    pub fn client_for(&self, addresses: Vec<SocketAddr>) -> Result<Client, HlsPolicyError> {
        let resolver = PinnedResolver {
            addresses: Arc::new(addresses),
        };
        Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .retry(reqwest::retry::never())
            .referer(false)
            .http1_only()
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(30))
            .read_timeout(Duration::from_secs(30))
            .dns_resolver(resolver)
            .build()
            .map_err(|_| HlsPolicyError::Transport)
    }

    /// Performs a GET with manual redirect validation and selected headers.
    ///
    /// # Errors
    /// Returns a fixed error for invalid targets, rejected redirects, cancellation, or transport failure.
    pub async fn get(
        &self,
        mut url: Url,
        headers: &RequestHeaders,
        cancellation: &CancellationToken,
    ) -> Result<HlsResponse, HlsPolicyError> {
        let mut previous: Option<Url> = None;
        for _ in 0..=MAX_REDIRECTS {
            self.validate_url(&url)?;
            if previous
                .as_ref()
                .is_some_and(|old| old.scheme() == "https" && url.scheme() == "http")
            {
                return Err(HlsPolicyError::RedirectRejected);
            }
            let addresses = self.validate_resolution(&url).await?;
            let client = self.client_for(addresses)?;
            let permit = self
                .acquire(url.host_str().unwrap_or_default(), cancellation)
                .await?;
            let response = tokio::select! { () = cancellation.cancelled() => return Err(HlsPolicyError::Cancelled), result = client.get(url.clone()).headers(headers.to_header_map()?).send() => result.map_err(|_| HlsPolicyError::Transport)? };
            if !response.status().is_redirection() {
                validate_content_length(response.content_length())?;
                return Ok(HlsResponse { response, permit });
            }
            let location = response
                .headers()
                .get(reqwest::header::LOCATION)
                .ok_or(HlsPolicyError::RedirectRejected)?
                .to_str()
                .map_err(|_| HlsPolicyError::RedirectRejected)?;
            let next = url
                .join(location)
                .map_err(|_| HlsPolicyError::RedirectRejected)?;
            drop(permit);
            previous = Some(url);
            url = next;
        }
        Err(HlsPolicyError::RedirectRejected)
    }
}

/// Validates a known response body size before the response reaches a route.
///
/// # Errors
/// Returns [`HlsPolicyError::ResponseTooLarge`] when the known size exceeds the policy bound.
pub fn validate_content_length(content_length: Option<u64>) -> Result<(), HlsPolicyError> {
    if content_length.is_some_and(|length| length > MAX_RESPONSE_BODY_BYTES) {
        Err(HlsPolicyError::ResponseTooLarge)
    } else {
        Ok(())
    }
}

struct PinnedResolver {
    addresses: Arc<Vec<SocketAddr>>,
}
impl reqwest::dns::Resolve for PinnedResolver {
    fn resolve(&self, _name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let addresses = self.addresses.clone();
        Box::pin(std::future::ready(Ok(
            Box::new(addresses.as_ref().clone().into_iter()) as reqwest::dns::Addrs,
        )))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct HostRule {
    host: String,
    port: Option<u16>,
}
impl HostRule {
    fn parse(value: &str) -> Result<Self, HlsPolicyError> {
        let parsed =
            Url::parse(&format!("http://{value}/")).map_err(|_| HlsPolicyError::InvalidTarget)?;
        let host = parsed
            .host_str()
            .ok_or(HlsPolicyError::InvalidTarget)?
            .to_ascii_lowercase();
        if parsed.username() != ""
            || parsed.password().is_some()
            || parsed.path() != "/"
            || host.contains('*')
            || host.starts_with('.')
            || host.ends_with('.')
        {
            return Err(HlsPolicyError::InvalidTarget);
        }
        Ok(Self {
            host,
            port: parsed.port(),
        })
    }
    fn from_url(url: &Url) -> Result<Self, HlsPolicyError> {
        Ok(Self {
            host: url
                .host_str()
                .ok_or(HlsPolicyError::InvalidTarget)?
                .to_ascii_lowercase(),
            port: url.port(),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Cidr {
    network: IpAddr,
    prefix: u8,
}
impl Cidr {
    fn parse(value: &str) -> Result<Self, HlsPolicyError> {
        let (network, prefix) = value.split_once('/').ok_or(HlsPolicyError::InvalidTarget)?;
        let network: IpAddr = network.parse().map_err(|_| HlsPolicyError::InvalidTarget)?;
        let prefix = prefix.parse().map_err(|_| HlsPolicyError::InvalidTarget)?;
        let max = if network.is_ipv4() { 32 } else { 128 };
        if prefix > max {
            return Err(HlsPolicyError::InvalidTarget);
        }
        Ok(Self { network, prefix })
    }
    fn contains(self, address: IpAddr) -> bool {
        if self.network.is_ipv4() != address.is_ipv4() {
            return false;
        }
        let (network, address, bits) = match (self.network, address) {
            (IpAddr::V4(network), IpAddr::V4(address)) => (
                u128::from(u32::from(network)),
                u128::from(u32::from(address)),
                32,
            ),
            (IpAddr::V6(network), IpAddr::V6(address)) => {
                (u128::from(network), u128::from(address), 128)
            }
            _ => return false,
        };
        let mask = if self.prefix == 0 {
            0
        } else {
            u128::MAX << (bits - u32::from(self.prefix))
        };
        network & mask == address & mask
    }
}

fn unconditionally_unsafe_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(value) => {
            value.is_loopback()
                || value.is_unspecified()
                || value.is_multicast()
                || value.is_link_local()
                || value.octets() == [255, 255, 255, 255]
        }
        IpAddr::V6(value) => {
            value.is_loopback()
                || value.is_unspecified()
                || value.is_multicast()
                || value.is_unique_local()
                || value.segments()[0] == 0xfe80
                || value.to_ipv4_mapped().is_some()
        }
    }
}
fn stable_hash(value: &str) -> usize {
    value.bytes().fold(0usize, |hash, byte| {
        hash.wrapping_mul(31).wrapping_add(usize::from(byte))
    })
}

/// Returns only safe media response metadata.
pub fn safe_response_headers(headers: &HeaderMap) -> HeaderMap {
    let mut safe = HeaderMap::new();
    for name in [
        HeaderName::from_static("content-type"),
        HeaderName::from_static("content-length"),
        HeaderName::from_static("content-range"),
        HeaderName::from_static("accept-ranges"),
        CONTENT_ENCODING,
    ] {
        if let Some(value) = headers.get(&name) {
            safe.insert(name, value.clone());
        }
    }
    safe
}

/// Maps non-success upstream statuses to a fixed error.
///
/// # Errors
/// Returns a fixed transport error for non-success statuses.
pub fn map_status(status: StatusCode) -> Result<(), HlsPolicyError> {
    if status.is_success() {
        Ok(())
    } else {
        Err(HlsPolicyError::Transport)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestResolver;
    impl Resolver for TestResolver {
        fn resolve(&self, _host: &str, port: u16) -> ResolveFuture {
            Box::pin(std::future::ready(Ok(vec![SocketAddr::from((
                [10, 223, 1, 1],
                port,
            ))])))
        }
    }

    fn policy() -> Result<HlsPolicy<TestResolver>, HlsPolicyError> {
        HlsPolicy::new(
            vec!["example.invalid".to_owned()],
            vec!["10.223.0.0/16".to_owned()],
            Arc::new(TestResolver),
        )
    }

    #[test]
    fn url_policy_rejects_controls_userinfo_fragments_and_unsafe_ips() {
        let policy = policy();
        assert!(policy.is_ok());
        let Some(policy) = policy.ok() else {
            return;
        };
        for value in [
            "http://example.invalid/a#fragment",
            "http://user:password@example.invalid/a",
            "http://127.0.0.1/a",
            "http://192.168.1.1/a",
        ] {
            let url = Url::parse(value);
            assert!(url.is_ok());
            if let Ok(url) = url {
                assert!(policy.validate_url(&url).is_err());
            }
        }
    }

    #[test]
    fn selected_headers_and_response_metadata_are_narrow() {
        let headers = RequestHeaders {
            accept: Some("video/mp2t".to_owned()),
            range: Some("bytes=0-1".to_owned()),
            user_agent: Some("iptv".to_owned()),
        };
        let map = headers.to_header_map();
        assert!(map.is_ok());
        if let Ok(map) = map {
            assert_eq!(map[ACCEPT_ENCODING], "identity");
        }
        let mut upstream = HeaderMap::new();
        upstream.insert("content-type", HeaderValue::from_static("video/mp2t"));
        upstream.insert("content-encoding", HeaderValue::from_static("gzip"));
        upstream.insert("server", HeaderValue::from_static("secret"));
        let safe = safe_response_headers(&upstream);
        assert!(safe.contains_key("content-type"));
        assert_eq!(safe[CONTENT_ENCODING], "gzip");
        assert!(!safe.contains_key("server"));
    }

    #[test]
    fn configured_private_cidr_is_allowed_but_loopback_is_not() {
        let policy = HlsPolicy::new(
            Vec::<String>::new(),
            vec!["10.223.0.0/16".to_owned()],
            Arc::new(TestResolver),
        );
        assert!(policy.is_ok());
        let Some(policy) = policy.ok() else {
            return;
        };
        let private = Url::parse("http://10.223.1.2/live.m3u8");
        assert!(private.is_ok());
        if let Ok(private) = private {
            assert!(policy.validate_url(&private).is_ok());
        }
        let loopback = Url::parse("http://127.0.0.1/live.m3u8");
        assert!(loopback.is_ok());
        if let Ok(loopback) = loopback {
            assert!(policy.validate_url(&loopback).is_err());
        }
    }

    #[test]
    fn known_response_content_length_over_limit_is_rejected() {
        assert_eq!(
            validate_content_length(Some(MAX_RESPONSE_BODY_BYTES + 1)),
            Err(HlsPolicyError::ResponseTooLarge)
        );
        assert!(validate_content_length(Some(MAX_RESPONSE_BODY_BYTES)).is_ok());
        assert!(validate_content_length(None).is_ok());
    }
}
