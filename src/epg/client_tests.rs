use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};

use tokio::net::{TcpListener, TcpStream};

use super::{EpgClient, EpgConfig, Programme, ProgrammeQuery};

struct Provider {
    address: SocketAddr,
    requests: Arc<Mutex<Vec<(String, String)>>>,
    task: tokio::task::JoinHandle<()>,
}

async fn provider() -> Provider {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .expect("provider listener");
    let address = listener.local_addr().expect("provider address");
    let requests = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&requests);
    let task = tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            let Ok((path, body)) = request(&mut stream).await else {
                return;
            };
            recorded
                .lock()
                .expect("request lock")
                .push((path.clone(), body.clone()));
            match path.as_str() {
                "/EDS/jsp/AuthenticationURL" => send(&mut stream, b"HTTP/1.1 302 Found\r\nLocation: /discovered\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await,
                "/discovered" => return_response(&mut stream, "").await,
                "/EPG/jsp/authLoginHWCU.jsp" => return_response(&mut stream, "EncryptToken=token%201").await,
                "/EPG/jsp/ValidAuthenticationHWCU.jsp" => return_response(&mut stream, "CUSetConfig('UserToken','user-token');CUSetConfig('SessionID','session');CUSetConfig('shareKey','key')").await,
                "/EPG/jsp/getchannellistHWCU.jsp" => return_response(&mut stream, "ChannelID=\"channel\" ChannelName=\"News\" UserChannelID=\"1\" TimeShiftURL=\"shift\"").await,
                "/EPG/jsp/jkjiaoyutest/en/function/ajax/epg7getChannelByAjax.jsp" if body.contains("getChannelProg") => return_response(&mut stream, "{\"status\":\"1\",\"errCode\":\"051002\",\"data\":[]}").await,
                "/EPG/jsp/jkjiaoyutest/en/function/ajax/epg7getChannelByAjax.jsp" if body.contains("getTvodPlayUrl") => return_response(&mut stream, "{\"status\":\"1\",\"data\":{\"ID\":\"programme\",\"channelID\":\"channel\",\"startTime\":1000,\"endTime\":2000,\"playURL\":\"http://media/tvod.m3u8\"}}").await,
                "/EPG/jsp/jkjiaoyutest/en/function/ajax/epg7getChannelByAjax.jsp" => return_response(&mut stream, "{\"status\":\"1\",\"data\":[{\"ID\":\"channel\",\"code\":\"code\",\"name\":\"News\",\"mixNo\":\"1\"}]}").await,
                "/EPG/jsp/jkjiaoyutest/en/epg4ChannelPlayByAjax.jsp" => return_response(&mut stream, "{\"hlsUrl\":\"http://media/live.m3u8\"}").await,
                _ => send(&mut stream, b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await,
            }
        }
    });
    Provider {
        address,
        requests,
        task,
    }
}

async fn request(stream: &mut TcpStream) -> std::io::Result<(String, String)> {
    let mut bytes = Vec::new();
    loop {
        stream.readable().await?;
        let mut chunk = [0_u8; 4096];
        match stream.try_read(&mut chunk) {
            Ok(0) => break,
            Ok(size) => {
                bytes.extend_from_slice(&chunk[..size]);
                if bytes.windows(4).any(|window| window == b"\r\n\r\n") {
                    break;
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(error) => return Err(error),
        }
    }
    let text = String::from_utf8_lossy(&bytes);
    let first = text.lines().next().unwrap_or_default();
    let path = first
        .split_whitespace()
        .nth(1)
        .unwrap_or_default()
        .to_owned();
    let header_end = bytes
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map_or(bytes.len(), |position| position + 4);
    let content_length = text[..header_end]
        .lines()
        .find_map(|line| {
            line.split_once(':')
                .filter(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                .and_then(|(_, value)| value.trim().parse::<usize>().ok())
        })
        .unwrap_or(0);
    while bytes.len() < header_end + content_length {
        stream.readable().await?;
        let mut chunk = [0_u8; 4096];
        match stream.try_read(&mut chunk) {
            Ok(size) => bytes.extend_from_slice(&chunk[..size]),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(error) => return Err(error),
        }
    }
    let text = String::from_utf8_lossy(&bytes);
    let body = text[header_end..header_end + content_length].to_owned();
    Ok((path.split('?').next().unwrap_or_default().to_owned(), body))
}

async fn send(stream: &mut TcpStream, bytes: &[u8]) {
    let mut written = 0;
    while written < bytes.len() {
        stream.writable().await.expect("provider writable");
        match stream.try_write(&bytes[written..]) {
            Ok(size) => written += size,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(error) => panic!("provider write: {error}"),
        }
    }
}

async fn return_response(stream: &mut TcpStream, body: &str) {
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    send(stream, response.as_bytes()).await;
}

fn client(provider: &Provider) -> EpgClient {
    EpgClient::new(EpgConfig {
        authentication_url: format!("http://{}/EDS/jsp/AuthenticationURL", provider.address),
        user_id: "user".to_owned(),
        password: "password".to_owned(),
        stb_id: "stb".to_owned(),
        stb_mac: "00:11:22:33:44:55".to_owned(),
        source_ip: IpAddr::V4(Ipv4Addr::LOCALHOST),
        interface: None,
    })
    .expect("client")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn authentication_redirect_and_request_forms_are_python_shaped() {
    let provider = provider().await;
    let client = client(&provider);

    let channels = client.channels().await.expect("channels");
    assert_eq!(channels[0].code, "code");
    assert_eq!(
        client.hls_url("channel").await.expect("hls"),
        "http://media/live.m3u8"
    );
    let query = ProgrammeQuery {
        channel_id: "channel".to_owned(),
        channel_code: "code".to_owned(),
        start_time_ms: 1_000,
        end_time_ms: 2_000,
    };
    assert_eq!(client.programmes(&query).await.expect("programmes"), []);

    let requests = provider.requests.lock().expect("request lock").clone();
    let login = requests
        .iter()
        .find(|(path, _)| path.ends_with("authLoginHWCU.jsp"))
        .expect("login");
    assert_eq!(login.1, "UserID=user&VIP=");
    let validation = requests
        .iter()
        .find(|(path, _)| path.ends_with("ValidAuthenticationHWCU.jsp"))
        .expect("validation");
    let keys: Vec<_> = validation
        .1
        .split('&')
        .map(|field| field.split('=').next().unwrap_or_default())
        .collect();
    assert_eq!(&keys[..4], &["UserID", "Lang", "SupportHD", "NetUserID"]);
    assert_eq!(keys.last(), Some(&"VIP"));
    provider.task.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_channels_share_one_authentication_and_tvod_parses() {
    let provider = provider().await;
    let client = client(&provider);
    let (left, right) = tokio::join!(client.channels(), client.channels());
    assert_eq!(left.expect("left channels").len(), 1);
    assert_eq!(right.expect("right channels").len(), 1);
    let programme = Programme {
        title: "News".to_owned(),
        start_time_ms: 1_000,
        end_time_ms: 2_000,
        programme_id: "programme".to_owned(),
        channel_id: "channel".to_owned(),
    };
    assert_eq!(
        client.tvod_url(&programme).await.expect("tvod"),
        "http://media/tvod.m3u8"
    );
    let requests = provider.requests.lock().expect("request lock");
    assert_eq!(
        requests
            .iter()
            .filter(|(path, _)| path.ends_with("AuthenticationURL"))
            .count(),
        1
    );
    assert_eq!(
        requests
            .iter()
            .filter(|(path, _)| path.ends_with("authLoginHWCU.jsp"))
            .count(),
        1
    );
    provider.task.abort();
}
