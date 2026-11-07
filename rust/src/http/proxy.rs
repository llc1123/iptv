use std::sync::Arc;

use axum::body::Body;
use axum::http::header::{ACCEPT_RANGES, CONTENT_LENGTH, CONTENT_RANGE, CONTENT_TYPE};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use reqwest::Url;

use crate::hls::{RequestHeaders, TokenStore, rewrite_playlist};

use super::{ForwardRequest, ForwardResponse, HlsForwarder, HlsFuture, HlsTarget, ProxyError};

pub struct PolicyForwarder {
    client: reqwest::Client,
    tokens: Arc<TokenStore>,
}

impl std::fmt::Debug for PolicyForwarder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("HlsForwarder([redacted])")
    }
}

impl PolicyForwarder {
    pub fn new(tokens: Arc<TokenStore>) -> Self {
        Self {
            client: reqwest::Client::new(),
            tokens,
        }
    }
}

impl HlsForwarder for PolicyForwarder {
    fn forward(&self, request: ForwardRequest) -> HlsFuture<'_> {
        Box::pin(async move {
            let url = match request.target {
                HlsTarget::Url(url) => url,
                HlsTarget::Token(token) => Url::parse(
                    &self
                        .tokens
                        .resolve(&token)
                        .map_err(|_| ProxyError::Unavailable)?
                        .url,
                )
                .map_err(|_| ProxyError::Unavailable)?,
            };
            let mut builder = self.client.get(url);
            builder = copy_request_headers(builder, &request.headers);
            let response = builder.send().await.map_err(|_| ProxyError::Unavailable)?;
            let status = response.status();
            if !status.is_success() && status != StatusCode::PARTIAL_CONTENT {
                return Err(ProxyError::Unavailable);
            }
            let final_url = response.url().clone();
            let headers = response.headers().clone();
            if is_playlist(&final_url, &headers) {
                let bytes = response
                    .bytes()
                    .await
                    .map_err(|_| ProxyError::Unavailable)?;
                if bytes.len() > crate::hls::PLAYLIST_MAX_BYTES {
                    return Err(ProxyError::Unavailable);
                }
                let rewritten =
                    rewrite_playlist(&bytes, &final_url, &request.cancellation, &self.tokens)
                        .map_err(|_| ProxyError::Unavailable)?;
                let mut output = HeaderMap::new();
                output.insert(
                    CONTENT_TYPE,
                    HeaderValue::from_static("application/vnd.apple.mpegurl"),
                );
                insert_length(&mut output, rewritten.body.len());
                return Ok(ForwardResponse {
                    status,
                    headers: output,
                    body: Body::from(rewritten.body),
                });
            }
            let mut output = narrow_headers(&headers);
            if output
                .get(CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .is_some_and(|value| value.eq_ignore_ascii_case("video/mpeg"))
            {
                output.insert(CONTENT_TYPE, HeaderValue::from_static("video/mp2t"));
            }
            let bytes = response
                .bytes()
                .await
                .map_err(|_| ProxyError::Unavailable)?;
            Ok(ForwardResponse {
                status,
                headers: output,
                body: Body::from(bytes),
            })
        })
    }
}

fn copy_request_headers(
    mut builder: reqwest::RequestBuilder,
    headers: &RequestHeaders,
) -> reqwest::RequestBuilder {
    if let Some(value) = &headers.accept {
        builder = builder.header("accept", value);
    }
    if let Some(value) = &headers.range {
        builder = builder.header("range", value);
    }
    if let Some(value) = &headers.user_agent {
        builder = builder.header("user-agent", value);
    }
    builder
}

fn is_playlist(url: &Url, headers: &reqwest::header::HeaderMap) -> bool {
    url.path().to_ascii_lowercase().ends_with(".m3u8")
        || headers
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.to_ascii_lowercase().contains("mpegurl"))
}

fn narrow_headers(source: &reqwest::header::HeaderMap) -> HeaderMap {
    let mut output = HeaderMap::new();
    for name in [CONTENT_TYPE, CONTENT_LENGTH, CONTENT_RANGE, ACCEPT_RANGES] {
        if let Some(value) = source.get(&name) {
            output.insert(name, value.clone());
        }
    }
    output
}

fn insert_length(headers: &mut HeaderMap, length: usize) {
    if let Ok(value) = HeaderValue::from_str(&length.to_string()) {
        headers.insert(CONTENT_LENGTH, value);
    }
}
