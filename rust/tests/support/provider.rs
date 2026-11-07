#![expect(
    clippy::unwrap_used,
    reason = "loopback provider uses deterministic fixture assertions"
)]

use std::net::SocketAddr;
use tokio::net::TcpStream;

pub async fn read_request(stream: &mut TcpStream) -> std::io::Result<(String, String)> {
    let mut bytes = Vec::new();
    loop {
        stream.readable().await?;
        let mut chunk = [0_u8; 4096];
        match stream.try_read(&mut chunk) {
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
    let path = text
        .lines()
        .next()
        .unwrap()
        .split_whitespace()
        .nth(1)
        .unwrap()
        .to_owned();
    let header_end = bytes
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .unwrap()
        + 4;
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
    Ok((
        path,
        text[header_end..header_end + content_length].to_owned(),
    ))
}

pub fn response_for(path: &str, request_body: &str, address: SocketAddr) -> String {
    let route = path.split('?').next().unwrap_or_default();
    let body = match route {
        "/EPG/jsp/authLoginHWCU.jsp" => "EncryptToken=token".into(),
        "/EPG/jsp/ValidAuthenticationHWCU.jsp" => "CUSetConfig('UserToken','user-token');CUSetConfig('SessionID','session');CUSetConfig('shareKey','key')".into(),
        "/EPG/jsp/getchannellistHWCU.jsp" => "ChannelID=\"channel\" ChannelName=\"News\" UserChannelID=\"1\" TimeShiftURL=\"shift\" TimeShift=\"1\"".into(),
        "/EPG/jsp/jkjiaoyutest/en/function/ajax/epg7getChannelByAjax.jsp" if request_body.contains("getChannelProg") => r#"{"status":"1","data":[{"ID":"programme","channelID":"channel","startTime":1000,"endTime":2000,"name":"Programme"}]}"#.into(),
        "/EPG/jsp/jkjiaoyutest/en/function/ajax/epg7getChannelByAjax.jsp" if request_body.contains("getTvodPlayUrl") => format!(r#"{{"status":"1","data":{{"ID":"programme","channelID":"channel","startTime":1000,"endTime":2000,"playURL":"http://{address}/tvod.m3u8"}}}}"#),
        "/EPG/jsp/jkjiaoyutest/en/function/ajax/epg7getChannelByAjax.jsp" => r#"{"status":"1","data":[{"ID":"channel","code":"code","name":"News","mixNo":"1"}]}"#.into(),
        "/EPG/jsp/jkjiaoyutest/en/epg4ChannelPlayByAjax.jsp" if path.contains("channelId=bad") => format!(r#"{{"hlsUrl":"http://{address}/fail.m3u8"}}"#),
        "/EPG/jsp/jkjiaoyutest/en/epg4ChannelPlayByAjax.jsp" => format!(r#"{{"hlsUrl":"http://{address}/live.m3u8"}}"#),
        "/live.m3u8" | "/tvod.m3u8" => "#EXTM3U\n#EXTINF:1,\nsegment.ts#fragment\n".into(),
        "/segment.ts" => "segment-bytes".into(),
        _ => String::new(),
    };
    let status = if route == "/fail.m3u8" {
        "500 Internal Server Error"
    } else {
        "200 OK"
    };
    let lower_route = route.to_ascii_lowercase();
    let content_type = if std::path::Path::new(&lower_route)
        .extension()
        .is_some_and(|extension| extension == "m3u8")
    {
        "application/vnd.apple.mpegurl"
    } else if std::path::Path::new(&lower_route)
        .extension()
        .is_some_and(|extension| extension == "ts")
    {
        "video/mpeg"
    } else {
        "text/plain"
    };
    format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}
