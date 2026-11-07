use super::AppState;
use crate::{
    error::RequestError,
    hls::playlist::{PLAYLIST_MAX_BYTES, rewrite_playlist},
    render::BaseUrl,
};
use axum::{
    body::Body,
    extract::Request,
    http::{HeaderMap, HeaderValue, StatusCode, Uri, header},
    response::{IntoResponse, Response},
};
use futures_util::stream;
use reqwest::Url;

pub(crate) async fn forward(state: &AppState, request: Request, url: String) -> Response {
    let Ok(url) = Url::parse(&url) else {
        tracing::warn!("upstream HLS URL was invalid");
        return RequestError::new("hls-proxy", "invalid upstream URL").into_response();
    };
    let mut builder = state.epg.http_client().get(url);
    for name in [header::ACCEPT, header::RANGE, header::USER_AGENT] {
        if let Some(value) = request.headers().get(&name) {
            builder = builder.header(name, value);
        }
    }
    let response = match builder.send().await {
        Ok(response) => response,
        Err(error) => {
            tracing::warn!(error = %error.without_url(), "upstream HLS request failed");
            return RequestError::new("hls-proxy", "upstream request failed").into_response();
        }
    };
    if !response.status().is_success() {
        tracing::warn!(status = %response.status(), "upstream HLS returned failure status");
        return RequestError::new("hls-proxy", "upstream request returned failure status")
            .into_response();
    }
    let final_url = response.url().clone();
    let content_type = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    if final_url.path().to_ascii_lowercase().ends_with(".m3u8")
        || content_type.to_ascii_lowercase().contains("mpegurl")
    {
        let bytes = match read_playlist(response).await {
            Ok(bytes) => bytes,
            Err(detail) => {
                tracing::warn!(detail, "upstream HLS playlist unavailable");
                return RequestError::new("hls-proxy", detail).into_response();
            }
        };
        let rewritten = match rewrite_playlist(&bytes, &final_url, &state.tokens) {
            Ok(body) => body,
            Err(error) => {
                tracing::warn!(%error, "HLS playlist rewrite failed");
                return RequestError::new("hls-proxy", "playlist rewrite failed").into_response();
            }
        };
        return (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "application/vnd.apple.mpegurl")],
            rewritten,
        )
            .into_response();
    }
    let status = response.status();
    let mut headers = HeaderMap::new();
    for name in [
        header::CONTENT_TYPE,
        header::CONTENT_LENGTH,
        header::CONTENT_RANGE,
        header::ACCEPT_RANGES,
    ] {
        if let Some(value) = response.headers().get(&name) {
            headers.insert(name, value.clone());
        }
    }
    if content_type
        .split(';')
        .next()
        .is_some_and(|value| value.trim().eq_ignore_ascii_case("video/mpeg"))
    {
        headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("video/mp2t"));
    }
    let body = Body::from_stream(stream::unfold(response, |mut response| async {
        match response.chunk().await {
            Ok(Some(bytes)) => Some((Ok::<_, reqwest::Error>(bytes), response)),
            Ok(None) => None,
            Err(error) => Some((Err(error), response)),
        }
    }));
    (status, headers, body).into_response()
}

/// Reads a playlist body, refusing anything over [`PLAYLIST_MAX_BYTES`].
async fn read_playlist(mut response: reqwest::Response) -> Result<Vec<u8>, &'static str> {
    const TOO_LARGE: &str = "playlist exceeds size limit";
    if response
        .content_length()
        .is_some_and(|length| length > PLAYLIST_MAX_BYTES as u64)
    {
        return Err(TOO_LARGE);
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| "playlist read failed")? {
        if bytes.len() + chunk.len() > PLAYLIST_MAX_BYTES {
            return Err(TOO_LARGE);
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

pub(crate) fn request_base(uri: &Uri, headers: &HeaderMap) -> Result<BaseUrl, ()> {
    let scheme = uri.scheme_str().unwrap_or("http");
    let host = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .ok_or(())?;
    BaseUrl::parse(&format!("{scheme}://{host}")).map_err(|_| ())
}

pub(crate) fn query_value(query: Option<&str>, wanted: &str) -> Option<String> {
    query?
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .find_map(|(key, value)| {
            (decode_component(key).ok().as_deref() == Some(wanted))
                .then(|| {
                    decode_component(value)
                        .ok()
                        .filter(|value| !value.is_empty())
                })
                .flatten()
        })
}

fn decode_component(value: &str) -> Result<String, ()> {
    let mut bytes = Vec::with_capacity(value.len());
    let raw = value.as_bytes();
    let mut index = 0;
    while index < raw.len() {
        match raw[index] {
            b'+' => {
                bytes.push(b' ');
                index += 1;
            }
            b'%' if index + 2 < raw.len() => {
                let high = hex_digit(raw[index + 1]).ok_or(())?;
                let low = hex_digit(raw[index + 2]).ok_or(())?;
                bytes.push(high * 16 + low);
                index += 3;
            }
            b'%' => return Err(()),
            byte => {
                bytes.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8(bytes).map_err(|_| ())
}

fn hex_digit(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}
