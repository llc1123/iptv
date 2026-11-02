use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use super::{EpgClient, EpgConfig};

#[tokio::test]
async fn loopback_auth_sequence_is_single_flight_and_cookie_bound() {
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .expect("bind loopback");
    let address = listener.local_addr().expect("read loopback address");
    let requests = Arc::new(AtomicUsize::new(0));
    let observed = Arc::clone(&requests);
    let server = tokio::spawn(async move {
        for index in 0..4 {
            let (mut stream, _) = listener.accept().await.expect("accept request");
            let request = read_request(&mut stream).await;
            if index == 0 {
                assert!(request.starts_with("GET /EDS/jsp/AuthenticationURL?UserID=fixture-user&Action=Login&Mode=MENU HTTP/1.1"));
                let location = format!("Location: http://{address}/EPG/jsp/authLoginHWCU.jsp\r\n");
                write_response(&mut stream, "302 Found", &location, "").await;
            } else if index == 1 {
                assert!(request.contains("GET /EPG/jsp/authLoginHWCU.jsp HTTP/1.1"));
                write_response(
                    &mut stream,
                    "200 OK",
                    "Set-Cookie: session=fixture; Path=/\r\n",
                    "EncryptToken=fixture-token",
                )
                .await;
            } else if index == 2 {
                assert!(request.contains("POST /EPG/jsp/authLoginHWCU.jsp HTTP/1.1"));
                assert!(
                    request
                        .to_ascii_lowercase()
                        .contains("cookie: session=fixture")
                );
                assert!(request.ends_with("UserID=fixture-user&VIP="));
                write_response(&mut stream, "200 OK", "", "EncryptToken=fixture-token").await;
            } else {
                assert!(request.contains("POST /EPG/jsp/ValidAuthenticationHWCU.jsp HTTP/1.1"));
                assert!(
                    request
                        .to_ascii_lowercase()
                        .contains("cookie: session=fixture")
                );
                assert!(request.contains("UserID=fixture-user&Lang=0&SupportHD=1"));
                write_response(&mut stream, "200 OK", "", "CUSetConfig('UserToken','fixture-user-token');CTCSetConfig('SessionID','fixture-session');").await;
            }
            observed.fetch_add(1, Ordering::Relaxed);
        }
    });
    let config = EpgConfig {
        authentication_url: format!("http://{address}/EDS/jsp/AuthenticationURL"),
        user_id: "fixture-user".into(),
        password: "fixture-password".into(),
        stb_id: "fixture-stb".into(),
        stb_mac: "02:00:00:00:00:01".into(),
        source_ip: std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
        stb_type: "fixture-type".into(),
        area_id: "fixture-area".into(),
        user_group_id: "fixture-group".into(),
        nonce: 7,
    };
    let client = EpgClient::new(config).expect("build client");
    let mut callers = Vec::new();
    for _ in 0..50 {
        let client = client.clone();
        callers.push(tokio::spawn(async move {
            client.authenticate().await.expect("authenticate")
        }));
    }
    for caller in callers {
        caller.await.expect("caller");
    }
    server.await.expect("server");
    assert_eq!(requests.load(Ordering::Relaxed), 4);
}

#[test]
fn errors_and_debug_views_never_render_secret_values() {
    let error = super::EpgError::field(super::EpgErrorStage::Login, "EncryptToken");
    let rendered = format!("{error:?} {error}");
    assert!(!rendered.contains("fixture-password"));
    assert!(!rendered.contains("fixture-token"));
    assert!(rendered.contains("EncryptToken"));
}

async fn read_request(stream: &mut tokio::net::TcpStream) -> String {
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 1024];
    loop {
        let count = stream.read(&mut buffer).await.expect("read request");
        if count == 0 {
            break;
        }
        bytes.extend_from_slice(&buffer[..count]);
        if let Some(header_end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            let headers = String::from_utf8_lossy(&bytes[..header_end + 4]);
            let length = headers
                .lines()
                .find_map(|line| line.strip_prefix("Content-Length: "))
                .and_then(|value| value.parse::<usize>().ok())
                .unwrap_or(0);
            if bytes.len() >= header_end + 4 + length {
                break;
            }
        }
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

async fn write_response(
    stream: &mut tokio::net::TcpStream,
    status: &str,
    headers: &str,
    body: &str,
) {
    let response = format!(
        "HTTP/1.1 {status}\r\n{headers}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream
        .write_all(response.as_bytes())
        .await
        .expect("write response");
}
