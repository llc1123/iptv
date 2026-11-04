use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::{Notify, Semaphore};

use super::super::models::Programme;
use super::super::request::send_with_client_retry;
use super::super::session::{SessionBundle, Sleeper};
use super::{EpgClient, EpgConfig, EpgError, EpgErrorClass, EpgErrorStage, extract_query_token};
use crate::lifecycle::Lifecycle;

#[test]
fn query_token_accepts_whitespace_after_equals() {
    assert_eq!(
        extract_query_token("var EncryptToken = \"fixture-token\";", "EncryptToken")
            .expect("token"),
        "fixture-token"
    );
}

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
        write_timeout: std::time::Duration::from_secs(30),
        pool_timeout: std::time::Duration::from_secs(30),
        media_pool_size: 4,
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

#[test]
fn config_debug_redacts_url_userinfo_query_source_and_credentials() {
    let config = EpgConfig {
        authentication_url: "https://u:p@example.invalid:8443/path?token=secret".into(),
        user_id: "fixture-user".into(),
        password: "fixture-password".into(),
        stb_id: "fixture-stb".into(),
        stb_mac: "02:00:00:00:00:01".into(),
        source_ip: "127.0.0.1".parse().expect("source address"),
        stb_type: "fixture-type".into(),
        area_id: "fixture-area".into(),
        user_group_id: "fixture-group".into(),
        nonce: 7,
        write_timeout: Duration::from_secs(30),
        pool_timeout: Duration::from_secs(30),
        media_pool_size: 4,
    };
    let rendered = format!("{config:?}");
    for secret in [
        "https://u:p@example.invalid:8443/path?token=secret",
        "u:p",
        "example.invalid",
        "token=secret",
        "fixture-user",
        "fixture-password",
        "fixture-stb",
        "02:00:00:00:00:01",
        "127.0.0.1",
    ] {
        assert!(!rendered.contains(secret), "debug leaked {secret}");
    }
    assert!(rendered.contains("authentication_url_scheme: Some(\"https\")"));
    assert!(rendered.contains("authentication_url_has_explicit_port: true"));
    assert!(rendered.contains("user_id_present: true"));
    assert!(rendered.contains("media_pool_size: 4"));
}

#[test]
fn token_extraction_matches_python_boundaries_quotes_and_percent_decoding() {
    assert_eq!(
        super::extract_query_token("?EncryptToken=fixture-token", "EncryptToken")
            .expect("unquoted token"),
        "fixture-token"
    );
    assert_eq!(
        super::extract_query_token("EncryptToken=\"fixture-token\"", "EncryptToken")
            .expect("quoted token"),
        "fixture-token"
    );
    assert_eq!(
        super::extract_query_token("&EncryptToken=fixture%2Dtoken", "EncryptToken")
            .expect("encoded token"),
        "fixture-token"
    );
    assert_eq!(
        super::extract_query_token("NotEncryptToken=bad&EncryptToken=good", "EncryptToken")
            .expect("boundary token"),
        "good"
    );
    for body in [
        "EncryptToken=",
        "EncryptToken=\"\"",
        "EncryptToken",
        "NotEncryptToken=bad",
    ] {
        let error = super::extract_query_token(body, "EncryptToken").expect_err("missing token");
        assert_eq!(error.class_code(), EpgErrorClass::MissingField);
        assert_eq!(error.field_name(), Some("EncryptToken"));
    }
    assert_eq!(
        super::extract_config_optional("SetConfig('UserToken','wrong')", "UserToken"),
        ""
    );
    assert_eq!(
        super::extract_config_optional(
            "CTCSetConfig('UserToken','fixture-user-token')",
            "UserToken",
        ),
        "fixture-user-token"
    );
    for name in ["UserToken", "SessionID"] {
        for body in ["", "CUSetConfig('UserToken','')"] {
            let error = super::extract_config(
                if name == "UserToken" {
                    body
                } else {
                    "CUSetConfig('SessionID','')"
                },
                name,
            )
            .expect_err("missing config token");
            assert_eq!(error.class_code(), EpgErrorClass::MissingField);
            assert_eq!(error.field_name(), Some(name));
        }
    }
}

#[test]
fn origin_validation_rejects_userinfo_and_non_epg_origins() {
    for value in [
        "http://@host.example:1234/path",
        "http://user:pass@host.example:1234/path",
        "ftp://host.example:1234/path",
        "http:///path",
        "http://host.example/path",
    ] {
        let error = super::parse_epg_url(value, EpgErrorStage::Discovery)
            .expect_err("origin must be rejected");
        assert!(matches!(
            error.class_code(),
            EpgErrorClass::InvalidOrigin | EpgErrorClass::Parse
        ));
        assert_eq!(error.stage(), EpgErrorStage::Discovery);
    }
}

#[tokio::test]
async fn authentication_status_failures_do_not_retry_non_auth_requests() {
    for status in ["401 Unauthorized", "403 Forbidden", "500 Server Error"] {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("bind loopback");
        let address = listener.local_addr().expect("read loopback address");
        let requests = Arc::new(AtomicUsize::new(0));
        let observed = Arc::clone(&requests);
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept request");
            let _request = read_request(&mut stream).await;
            observed.fetch_add(1, Ordering::Relaxed);
            write_response(&mut stream, status, "", "").await;
        });
        let client = EpgClient::new(EpgConfig {
            authentication_url: format!("http://{address}/EDS/jsp/AuthenticationURL"),
            user_id: "fixture-user".into(),
            password: "fixture-password".into(),
            stb_id: "fixture-stb".into(),
            stb_mac: "02:00:00:00:00:01".into(),
            source_ip: "127.0.0.1".parse().expect("source address"),
            stb_type: "fixture-type".into(),
            area_id: "fixture-area".into(),
            user_group_id: "fixture-group".into(),
            nonce: 7,
            write_timeout: Duration::from_secs(1),
            pool_timeout: Duration::from_secs(1),
            media_pool_size: 1,
        })
        .expect("build client");
        let error = client.authenticate().await.expect_err("status failure");
        assert_eq!(error.class_code(), EpgErrorClass::HttpStatus);
        assert_eq!(
            error.status_code(),
            Some(status[0..3].parse().expect("status"))
        );
        server.await.expect("server");
        assert_eq!(requests.load(Ordering::Relaxed), 1);
    }
}

#[tokio::test]
async fn tvod_posts_ordered_form_and_returns_validated_url() {
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .expect("bind loopback");
    let address = listener.local_addr().expect("read loopback address");
    let server = tokio::spawn(async move {
        for index in 0..5 {
            let (mut stream, _) = listener.accept().await.expect("accept request");
            let request = read_request(&mut stream).await;
            if index < 4 {
                write_auth_response(index, &address.to_string(), &mut stream, &request).await;
            } else {
                assert!(request.contains(
                    "POST /EPG/jsp/jkjiaoyutest/en/function/ajax/epg7getChannelByAjax.jsp HTTP/1.1"
                ));
                assert!(request.ends_with(
                    "action=getTvodPlayUrl&channelID=c1&playbillID=p1&startTime=1000&endTime=2000"
                ));
                write_response(
                    &mut stream,
                    "200 OK",
                    "",
                    r#"{"status":"1","data":{"ID":"p1","channelID":"c1","startTime":"1000000","endTime":"2000000","playURL":"http://127.0.0.1:1234/replay.m3u8"}}"#,
                )
                .await;
            }
        }
    });
    let client = fixture_client(&address.to_string());
    let programme = Programme {
        title: "Fixture".into(),
        start_time_ms: 1_000_000,
        end_time_ms: 2_000_000,
        programme_id: "p1".into(),
        channel_id: "c1".into(),
    };
    let url = client
        .tvod_url(&programme, Lifecycle::new().cancellation_token())
        .await
        .expect("valid TVOD URL");
    assert_eq!(url.as_str(), "http://127.0.0.1:1234/replay.m3u8");
    server.await.expect("server");
}

#[tokio::test]
async fn hls_get_uses_python_shape_and_validates_response_url() {
    let result = hls_result(r#"{"status":"1","hlsUrl":"http://127.0.0.1:1234/live.m3u8"}"#)
        .await
        .expect("valid HLS URL");
    assert_eq!(result.as_str(), "http://127.0.0.1:1234/live.m3u8");
}

#[tokio::test]
async fn hls_rejects_invalid_response_url() {
    let error = hls_result(r#"{"hlsUrl":"ftp://127.0.0.1/live.m3u8"}"#)
        .await
        .expect_err("invalid HLS URL");
    assert_eq!(error.class_code(), EpgErrorClass::InvalidOrigin);
}

#[tokio::test]
async fn tvod_reauthenticates_once_for_auth_status_and_retries() {
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .expect("bind loopback");
    let address = listener.local_addr().expect("read loopback address");
    let server = tokio::spawn(async move {
        for index in 0..10 {
            let (mut stream, _) = listener.accept().await.expect("accept request");
            let request = read_request(&mut stream).await;
            if index < 4 || (5..9).contains(&index) {
                let auth_index = if index < 4 { index } else { index - 5 };
                write_auth_response(auth_index, &address.to_string(), &mut stream, &request).await;
            } else if index == 4 {
                assert!(request.contains(
                    "POST /EPG/jsp/jkjiaoyutest/en/function/ajax/epg7getChannelByAjax.jsp"
                ));
                write_response(&mut stream, "401 Unauthorized", "", "").await;
            } else {
                assert!(request.contains(
                    "POST /EPG/jsp/jkjiaoyutest/en/function/ajax/epg7getChannelByAjax.jsp"
                ));
                write_response(
                    &mut stream,
                    "200 OK",
                    "",
                    r#"{"status":"1","data":{"ID":"p1","channelID":"c1","startTime":"1000000","endTime":"2000000","playURL":"https://127.0.0.1:1234/replay.m3u8"}}"#,
                )
                .await;
            }
        }
    });
    let client = fixture_client(&address.to_string());
    let programme = Programme {
        title: "Fixture".into(),
        start_time_ms: 1_000_000,
        end_time_ms: 2_000_000,
        programme_id: "p1".into(),
        channel_id: "c1".into(),
    };
    let url = client
        .tvod_url(&programme, Lifecycle::new().cancellation_token())
        .await
        .expect("reauthenticated TVOD URL");
    assert_eq!(url.scheme(), "https");
    server.await.expect("server");
}

#[tokio::test]
async fn tvod_cancellation_does_not_wait_for_provider_body() {
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .expect("bind loopback");
    let address = listener.local_addr().expect("read loopback address");
    let server = tokio::spawn(async move {
        for index in 0..5 {
            let (mut stream, _) = listener.accept().await.expect("accept request");
            let request = read_request(&mut stream).await;
            if index < 4 {
                write_auth_response(index, &address.to_string(), &mut stream, &request).await;
            } else {
                std::future::pending::<()>().await;
            }
        }
    });
    let client = fixture_client(&address.to_string());
    let lifecycle = Lifecycle::new();
    let cancellation = lifecycle.cancellation_token();
    let task = tokio::spawn({
        let client = client.clone();
        let cancellation = cancellation.clone();
        async move {
            client
                .tvod_url(
                    &Programme {
                        title: "Fixture".into(),
                        start_time_ms: 1_000_000,
                        end_time_ms: 2_000_000,
                        programme_id: "p1".into(),
                        channel_id: "c1".into(),
                    },
                    cancellation,
                )
                .await
        }
    });
    tokio::task::yield_now().await;
    lifecycle.begin_shutdown();
    let error = tokio::time::timeout(Duration::from_secs(1), task)
        .await
        .expect("cancellation deadline")
        .expect("request task")
        .expect_err("cancelled TVOD");
    assert_eq!(error.class_code(), EpgErrorClass::Cancelled);
    server.abort();
}

#[tokio::test]
async fn tvod_rejects_invalid_urls_and_oversized_responses() {
    let invalid = tvod_result(
        r#"{"status":"1","data":{"ID":"p1","channelID":"c1","startTime":"1000000","endTime":"2000000","playURL":"ftp://127.0.0.1/replay.m3u8"}}"#.to_owned(),
    )
    .await
    .expect_err("invalid scheme");
    assert_eq!(invalid.class_code(), EpgErrorClass::InvalidOrigin);

    let oversized = tvod_result("x".repeat(1_048_577))
        .await
        .expect_err("body bound");
    assert_eq!(oversized.class_code(), EpgErrorClass::Parse);
}

async fn tvod_result(body: String) -> Result<reqwest::Url, EpgError> {
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .expect("bind loopback");
    let address = listener.local_addr().expect("read loopback address");
    let server = tokio::spawn(async move {
        for index in 0..5 {
            let (mut stream, _) = listener.accept().await.expect("accept request");
            let request = read_request(&mut stream).await;
            if index < 4 {
                write_auth_response(index, &address.to_string(), &mut stream, &request).await;
            } else {
                write_response(&mut stream, "200 OK", "", &body).await;
            }
        }
    });
    let client = fixture_client(&address.to_string());
    let result = client
        .tvod_url(
            &Programme {
                title: "Fixture".into(),
                start_time_ms: 1_000_000,
                end_time_ms: 2_000_000,
                programme_id: "p1".into(),
                channel_id: "c1".into(),
            },
            Lifecycle::new().cancellation_token(),
        )
        .await;
    server.await.expect("server");
    result
}

async fn hls_result(body: &str) -> Result<reqwest::Url, EpgError> {
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .expect("bind loopback");
    let address = listener.local_addr().expect("read loopback address");
    let body = body.to_owned();
    let server = tokio::spawn(async move {
        for index in 0..5 {
            let (mut stream, _) = listener.accept().await.expect("accept request");
            let request = read_request(&mut stream).await;
            if index < 4 {
                write_auth_response(index, &address.to_string(), &mut stream, &request).await;
            } else {
                assert!(request.contains(
                    "GET /EPG/jsp/jkjiaoyutest/en/epg4ChannelPlayByAjax.jsp?action=getChannelHlsUrl&channelId=c1 HTTP/1.1"
                ));
                let headers = request.to_ascii_lowercase();
                assert!(headers.contains("vis-ajax: ajaxhttprequest"));
                assert!(headers.contains("accept: */*"));
                write_response(&mut stream, "200 OK", "", &body).await;
            }
        }
    });
    let result = fixture_client(&address.to_string())
        .hls_url("c1", Lifecycle::new().cancellation_token())
        .await;
    server.await.expect("server");
    result
}

#[tokio::test]
async fn shutdown_bound_client_cancels_before_starting_authentication() {
    let source = crate::lifecycle::CancellationSource::new();
    source.cancel();
    let client = EpgClient::with_shutdown(
        EpgConfig {
            authentication_url: "http://127.0.0.1:1/EDS/jsp/AuthenticationURL".to_owned(),
            user_id: "fixture-user".to_owned(),
            password: "fixture-password".to_owned(),
            stb_id: "fixture-stb".to_owned(),
            stb_mac: "02:00:00:00:00:01".to_owned(),
            source_ip: std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
            stb_type: "fixture-type".to_owned(),
            area_id: "fixture-area".to_owned(),
            user_group_id: "fixture-group".to_owned(),
            nonce: 7,
            write_timeout: Duration::from_secs(1),
            pool_timeout: Duration::from_secs(1),
            media_pool_size: 1,
        },
        source.token(),
    )
    .expect("build client");
    let error = client
        .authenticate()
        .await
        .expect_err("shutdown must cancel");
    assert!(format!("{error:?}").contains("Cancelled"));
}

fn fixture_client(address: &str) -> EpgClient {
    EpgClient::new(EpgConfig {
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
        write_timeout: Duration::from_secs(1),
        pool_timeout: Duration::from_secs(1),
        media_pool_size: 1,
    })
    .expect("build client")
}

async fn write_auth_response(
    index: usize,
    address: &str,
    stream: &mut tokio::net::TcpStream,
    request: &str,
) {
    match index {
        0 => {
            assert!(request.starts_with("GET /EDS/jsp/AuthenticationURL?UserID=fixture-user"));
            write_response(
                stream,
                "302 Found",
                &format!("Location: http://{address}/EPG/jsp/authLoginHWCU.jsp\r\n"),
                "",
            )
            .await;
        }
        1 => {
            assert!(request.contains("GET /EPG/jsp/authLoginHWCU.jsp"));
            write_response(
                stream,
                "200 OK",
                "Set-Cookie: session=fixture; Path=/\r\n",
                "EncryptToken=fixture-token",
            )
            .await;
        }
        2 => {
            assert!(request.contains("POST /EPG/jsp/authLoginHWCU.jsp"));
            write_response(stream, "200 OK", "", "EncryptToken=fixture-token").await;
        }
        _ => {
            assert!(request.contains("POST /EPG/jsp/ValidAuthenticationHWCU.jsp"));
            write_response(
                stream,
                "200 OK",
                "",
                "CUSetConfig('UserToken','fixture-user-token');CTCSetConfig('SessionID','fixture-session');",
            )
            .await;
        }
    }
}

#[tokio::test]
async fn redirect_limit_and_parse_failures_are_redacted_and_bounded() {
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .expect("bind loopback");
    let address = listener.local_addr().expect("read loopback address");
    let server = tokio::spawn(async move {
        for index in 0..=20 {
            let (mut stream, _) = listener.accept().await.expect("accept redirect");
            let _request = read_request(&mut stream).await;
            let location = format!("Location: http://{address}/redirect/{}\r\n", index + 1);
            write_response(&mut stream, "302 Found", &location, "").await;
        }
    });
    let client = EpgClient::new(EpgConfig {
        authentication_url: format!("http://{address}/redirect/0"),
        user_id: "fixture-user".into(),
        password: "fixture-password".into(),
        stb_id: "fixture-stb".into(),
        stb_mac: "02:00:00:00:00:01".into(),
        source_ip: "127.0.0.1".parse().expect("source address"),
        stb_type: "fixture-type".into(),
        area_id: "fixture-area".into(),
        user_group_id: "fixture-group".into(),
        nonce: 7,
        write_timeout: Duration::from_secs(1),
        pool_timeout: Duration::from_secs(1),
        media_pool_size: 1,
    })
    .expect("build client");
    let error = client.authenticate().await.expect_err("redirect limit");
    assert_eq!(error.class_code(), EpgErrorClass::Transport);
    server.await.expect("redirect server");

    let invalid = EpgClient::new(EpgConfig {
        authentication_url: "not a URL".into(),
        user_id: "fixture-user".into(),
        password: "fixture-password".into(),
        stb_id: "fixture-stb".into(),
        stb_mac: "02:00:00:00:00:01".into(),
        source_ip: "127.0.0.1".parse().expect("source address"),
        stb_type: "fixture-type".into(),
        area_id: "fixture-area".into(),
        user_group_id: "fixture-group".into(),
        nonce: 7,
        write_timeout: Duration::from_secs(1),
        pool_timeout: Duration::from_secs(1),
        media_pool_size: 1,
    })
    .expect("build client");
    let error = invalid.authenticate().await.expect_err("parse failure");
    assert_eq!(error.class_code(), EpgErrorClass::Parse);
}

#[tokio::test]
async fn request_send_timeout_cancels_the_bounded_request_future() {
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .expect("bind loopback");
    let address = listener.local_addr().expect("read loopback address");
    let accepted = Arc::new(Notify::new());
    let observed = Arc::clone(&accepted);
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("accept request");
        let _request = read_request(&mut stream).await;
        observed.notify_one();
        std::future::pending::<()>().await;
    });
    let client = EpgClient::new(EpgConfig {
        authentication_url: format!("http://{address}/EDS/jsp/AuthenticationURL"),
        user_id: "fixture-user".into(),
        password: "fixture-password".into(),
        stb_id: "fixture-stb".into(),
        stb_mac: "02:00:00:00:00:01".into(),
        source_ip: "127.0.0.1".parse().expect("source address"),
        stb_type: "fixture-type".into(),
        area_id: "fixture-area".into(),
        user_group_id: "fixture-group".into(),
        nonce: 7,
        write_timeout: Duration::from_millis(10),
        pool_timeout: Duration::from_secs(1),
        media_pool_size: 1,
    })
    .expect("build client");
    let task = tokio::spawn(async move { client.authenticate().await });
    accepted.notified().await;
    let error = tokio::time::timeout(Duration::from_secs(1), task)
        .await
        .expect("request deadline")
        .expect("request task")
        .expect_err("write timeout");
    assert_eq!(error.class_code(), EpgErrorClass::Timeout);
    server.abort();
}

#[tokio::test]
async fn media_permit_timeout_is_bounded_and_separate_from_control_client() {
    let client = reqwest::Client::builder()
        .no_proxy()
        .build()
        .expect("client");
    let permits = Arc::new(Semaphore::new(1));
    let bundle = SessionBundle::new(
        0,
        std::time::Instant::now(),
        reqwest::Url::parse("http://127.0.0.1:1234").expect("origin"),
        "fixture-user-token".into(),
        "fixture-session".into(),
        "fixture-key".into(),
        client.clone(),
        client,
        Arc::clone(&permits),
        Duration::ZERO,
        vec!["session=fixture".into()],
    );
    let held = bundle.acquire_media_permit().await.expect("first permit");
    let error = bundle
        .acquire_media_permit()
        .await
        .expect_err("saturated media pool");
    assert_eq!(error.class_code(), EpgErrorClass::PoolTimeout);
    drop(held);
}

struct ImmediateSleeper {
    calls: Arc<AtomicUsize>,
}

impl Sleeper for ImmediateSleeper {
    fn sleep(&self, _duration: Duration) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        Box::pin(async {})
    }
}

#[tokio::test]
async fn connection_retry_uses_one_bounded_backoff_and_two_attempts() {
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .expect("bind loopback");
    let address = listener.local_addr().expect("read loopback address");
    drop(listener);
    let client = reqwest::Client::builder()
        .no_proxy()
        .build()
        .expect("client");
    let calls = Arc::new(AtomicUsize::new(0));
    let sleeper = ImmediateSleeper {
        calls: Arc::clone(&calls),
    };
    let error = send_with_client_retry(
        &client,
        |client| client.get(format!("http://{address}/")),
        &sleeper,
        EpgErrorStage::Discovery,
        Arc::new(Semaphore::new(1)),
        Duration::from_secs(1),
        Duration::from_secs(1),
    )
    .await
    .expect_err("closed loopback port");
    assert_eq!(error.class_code(), EpgErrorClass::Transport);
    assert_eq!(error.attempt(), 2);
    assert_eq!(calls.load(Ordering::Relaxed), 1);
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
