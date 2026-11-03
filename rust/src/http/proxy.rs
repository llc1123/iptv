//! Policy-controlled HLS forwarding and backpressure-aware body streaming.

use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;

use axum::body::Body;
use axum::http::header::{CONTENT_ENCODING, CONTENT_TYPE};
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode};
use futures_util::stream;
use reqwest::Url;

use crate::hls::policy::{map_status, safe_response_headers};
use crate::hls::{
    HlsBody, HlsPolicy, HlsPolicyError, RequestHeaders, TokenStore, rewrite_playlist,
};
use crate::lifecycle::{CancellationToken, RequestGuard};

use super::{ForwardRequest, ForwardResponse, HlsForwarder, HlsFuture, HlsTarget, ProxyError};

/// System DNS resolver used by the production HLS policy.
#[derive(Debug, Default)]
pub struct SystemResolver;

impl crate::hls::Resolver for SystemResolver {
    fn resolve(&self, host: &str, port: u16) -> crate::hls::policy::ResolveFuture {
        let host = host.to_owned();
        Box::pin(async move {
            tokio::net::lookup_host((host, port))
                .await
                .map(Iterator::collect::<Vec<SocketAddr>>)
                .map_err(|_| crate::hls::policy::ResolverError)
        })
    }
}

/// Production HLS forwarder backed by the configured policy and token store.
pub struct PolicyForwarder<R> {
    policy: Arc<HlsPolicy<R>>,
    tokens: Arc<TokenStore>,
}

impl<R> std::fmt::Debug for PolicyForwarder<R> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("PolicyForwarder([redacted])")
    }
}

impl<R: crate::hls::Resolver + 'static> PolicyForwarder<R> {
    /// Creates a forwarder from an already validated policy and token store.
    #[must_use]
    pub fn new(policy: Arc<HlsPolicy<R>>, tokens: Arc<TokenStore>) -> Self {
        Self { policy, tokens }
    }
}

impl<R: crate::hls::Resolver + 'static> HlsForwarder for PolicyForwarder<R> {
    fn forward(&self, request: ForwardRequest) -> HlsFuture<'_> {
        Box::pin(async move {
            let url = match request.target {
                HlsTarget::Url(url) => url,
                HlsTarget::Token(token) => self
                    .tokens
                    .resolve(&token)
                    .map_err(|_| ProxyError::Unavailable)
                    .and_then(|resource| {
                        Url::parse(&resource.url).map_err(|_| ProxyError::Unavailable)
                    })?,
            };
            let response = self
                .policy
                .get(url, &request.headers, &request.cancellation)
                .await
                .map_err(|_| ProxyError::Unavailable)?;
            build_response(
                response,
                request.headers,
                request.cancellation,
                request.admission,
                &self.tokens,
            )
            .await
        })
    }
}

async fn build_response(
    response: crate::hls::HlsResponse,
    _request_headers: RequestHeaders,
    cancellation: CancellationToken,
    admission: Arc<RequestGuard>,
    tokens: &TokenStore,
) -> Result<ForwardResponse, ProxyError> {
    let status = response.response.status();
    map_status(status).map_err(|_| ProxyError::Unavailable)?;
    if !matches!(status, StatusCode::OK | StatusCode::PARTIAL_CONTENT) {
        return Err(ProxyError::Unavailable);
    }
    let final_url = response.response.url().clone();
    let upstream_headers = response.response.headers().clone();
    let playlist = is_playlist(&final_url, &upstream_headers);
    if playlist {
        if upstream_headers.contains_key(CONTENT_ENCODING)
            || upstream_headers
                .get(reqwest::header::CONTENT_LENGTH)
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.parse::<u64>().ok())
                .is_some_and(|length| length > crate::hls::PLAYLIST_MAX_BYTES as u64)
        {
            return Err(ProxyError::Unavailable);
        }
        let mut body = response.into_body();
        let mut bytes = Vec::new();
        loop {
            let chunk = tokio::select! {
                () = cancellation.cancelled() => return Err(ProxyError::Unavailable),
                chunk = body.next_chunk() => chunk.map_err(|_| ProxyError::Unavailable)?,
            };
            let Some(chunk) = chunk else {
                break;
            };
            let next_length = bytes
                .len()
                .checked_add(chunk.len())
                .ok_or(ProxyError::Unavailable)?;
            if next_length > crate::hls::PLAYLIST_MAX_BYTES {
                return Err(ProxyError::Unavailable);
            }
            bytes.extend_from_slice(&chunk);
        }
        let rewritten = rewrite_playlist(&bytes, &final_url, &cancellation, tokens)
            .map_err(|_| ProxyError::Unavailable)?;
        let mut headers = HeaderMap::new();
        headers.insert(
            CONTENT_TYPE,
            HeaderValue::from_static("application/vnd.apple.mpegurl"),
        );
        insert_length(&mut headers, rewritten.body.len());
        return Ok(ForwardResponse {
            status,
            headers,
            body: Body::from(rewritten.body),
        });
    }

    let mut headers = narrow_headers(&upstream_headers);
    if headers
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.eq_ignore_ascii_case("video/mpeg"))
    {
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("video/mp2t"));
    }
    let body = response.into_body();
    let stream = media_stream(body, cancellation, admission);
    Ok(ForwardResponse {
        status,
        headers,
        body: Body::from_stream(stream),
    })
}

struct MediaState {
    body: HlsBody,
    cancellation: CancellationToken,
    _admission: Arc<RequestGuard>,
}

fn media_stream(
    body: HlsBody,
    cancellation: CancellationToken,
    admission: Arc<RequestGuard>,
) -> impl futures_util::Stream<Item = Result<Vec<u8>, Infallible>> + Send {
    stream::unfold(
        MediaState {
            body,
            cancellation,
            _admission: admission,
        },
        |mut state| async move {
            let next = tokio::select! {
                () = state.cancellation.cancelled() => None,
                chunk = state.body.next_chunk() => chunk.ok().flatten(),
            }?;
            Some((Ok(next), state))
        },
    )
}

fn is_playlist(url: &Url, headers: &reqwest::header::HeaderMap) -> bool {
    let path_match = url.path().to_ascii_lowercase().ends_with(".m3u8");
    let mime_match = headers
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| {
            let value = value.to_ascii_lowercase();
            value.contains("mpegurl") || value.contains("x-mpegurl")
        });
    path_match || mime_match
}

fn narrow_headers(source: &reqwest::header::HeaderMap) -> HeaderMap {
    let safe = safe_response_headers(source);
    let mut output = HeaderMap::new();
    for name in [
        "content-type",
        "content-length",
        "content-range",
        "accept-ranges",
        "content-encoding",
    ] {
        if let Some(value) = safe.get(name) {
            if let Ok(header_name) = HeaderName::from_bytes(name.as_bytes()) {
                output.insert(header_name, value.clone());
            }
        }
    }
    output
}

fn insert_length(headers: &mut HeaderMap, length: usize) {
    if let Ok(value) = HeaderValue::from_str(&length.to_string()) {
        headers.insert(axum::http::header::CONTENT_LENGTH, value);
    }
}

impl From<HlsPolicyError> for ProxyError {
    fn from(_: HlsPolicyError) -> Self {
        Self::Unavailable
    }
}
