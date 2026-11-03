use std::io;

use md5::{Digest, Md5};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

const MAX_REQUEST_BYTES: usize = 64 * 1024;
const MAX_RESPONSE_BYTES: usize = 64 * 1024;

pub(super) struct Request {
    pub(super) method: String,
    pub(super) route: String,
    pub(super) body: Vec<u8>,
}

pub(super) async fn read_request(stream: &mut TcpStream) -> io::Result<Request> {
    let mut bytes = Vec::with_capacity(1024);
    let mut chunk = [0_u8; 1024];
    let header_end = loop {
        let count = stream.read(&mut chunk).await?;
        if count == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "request ended",
            ));
        }
        bytes.extend_from_slice(&chunk[..count]);
        if bytes.len() > MAX_REQUEST_BYTES {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "request cap"));
        }
        if let Some(end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            break end + 4;
        }
    };
    let headers = String::from_utf8_lossy(&bytes[..header_end]);
    let mut lines = headers.lines();
    let first = lines
        .next()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "request line"))?;
    let mut first_parts = first.split_ascii_whitespace();
    let method = first_parts
        .next()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "request method"))?
        .to_owned();
    let route = first_parts
        .next()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "request route"))?
        .split('?')
        .next()
        .unwrap_or_default()
        .to_owned();
    let content_length = lines
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then_some(value.trim())
        })
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(0);
    if content_length > MAX_REQUEST_BYTES || header_end + content_length > MAX_REQUEST_BYTES {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "body cap"));
    }
    while bytes.len() < header_end + content_length {
        let count = stream.read(&mut chunk).await?;
        if count == 0 {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "body ended"));
        }
        bytes.extend_from_slice(&chunk[..count]);
    }
    Ok(Request {
        method,
        route,
        body: bytes[header_end..header_end + content_length].to_vec(),
    })
}

pub(super) fn form_keys(body: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(body)
        .split('&')
        .filter_map(|field| field.split_once('=').map(|(key, _)| key.to_owned()))
        .collect()
}

pub(super) fn digest(bytes: &[u8]) -> String {
    let mut hasher = Md5::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

pub(super) async fn write_response(
    stream: &mut TcpStream,
    status: &str,
    body: &[u8],
) -> io::Result<()> {
    write_response_with_location(stream, status, body, None).await
}

pub(super) async fn write_response_with_location(
    stream: &mut TcpStream,
    status: &str,
    body: &[u8],
    location: Option<&str>,
) -> io::Result<()> {
    if body.len() > MAX_RESPONSE_BYTES {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "response cap"));
    }
    let location = location.map_or_else(String::new, |value| format!("Location: {value}\r\n"));
    let response = format!(
        "HTTP/1.1 {status}\r\n{location}Content-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(response.as_bytes()).await?;
    stream.write_all(body).await
}
