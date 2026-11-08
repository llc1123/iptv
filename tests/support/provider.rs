#![expect(
    clippy::unwrap_used,
    reason = "loopback provider uses deterministic fixture assertions"
)]

use std::{
    net::SocketAddr,
    sync::atomic::{AtomicBool, Ordering},
};
use tokio::net::TcpStream;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestRecord {
    pub method: String,
    pub path: String,
    pub query: String,
    pub form_fields: Vec<String>,
    pub accept: Option<String>,
    pub range: Option<String>,
    pub user_agent: Option<String>,
    pub body: String,
}

#[derive(Debug, Default)]
pub struct ProviderState {
    pub fail_bad_programmes: AtomicBool,
    pub fail_all_programmes: AtomicBool,
}

pub async fn read_request(stream: &mut TcpStream) -> std::io::Result<RequestRecord> {
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
    let header_end = header_end_placeholder(&bytes);
    let header_text = String::from_utf8_lossy(&bytes[..header_end]).to_string();
    let request_line = header_text.lines().next().unwrap();
    let mut request_parts = request_line.split_whitespace();
    let method = request_parts.next().unwrap().to_owned();
    let target = request_parts.next().unwrap();
    let (path, query) = target.split_once('?').map_or((target, ""), |parts| parts);
    let path = path.to_owned();
    let query = query.to_owned();
    let headers = header_text
        .lines()
        .skip(1)
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.to_ascii_lowercase(), value.trim().to_owned()))
        .collect::<std::collections::HashMap<_, _>>();
    let content_length = header_text
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
    let body = String::from_utf8_lossy(&bytes[header_end..header_end + content_length]).to_string();
    Ok(RequestRecord {
        method,
        path,
        query,
        form_fields: body
            .split('&')
            .filter_map(|field| field.split_once('=').map(|(name, _)| name.to_owned()))
            .collect(),
        accept: headers.get("accept").cloned(),
        range: headers.get("range").cloned(),
        user_agent: headers.get("user-agent").cloned(),
        body,
    })
}

fn header_end_placeholder(bytes: &[u8]) -> usize {
    bytes
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .unwrap()
        + 4
}

pub fn response_for(request: &RequestRecord, address: SocketAddr, state: &ProviderState) -> String {
    let route = request.path.as_str();
    let request_body = request.body.as_str();
    let body = match route {
        "/EPG/jsp/authLoginHWCU.jsp" => "EncryptToken=token".into(),
        "/EPG/jsp/ValidAuthenticationHWCU.jsp" => "CUSetConfig('UserToken','user-token');CUSetConfig('SessionID','session');CUSetConfig('shareKey','key')".into(),
        "/EPG/jsp/getchannellistHWCU.jsp" => "ChannelID=\"channel\" ChannelName=\"News\" UserChannelID=\"1\" TimeShiftURL=\"shift\" TimeShift=\"1\" ChannelID=\"live\" ChannelName=\"Live\" UserChannelID=\"2\" TimeShiftURL=\"shift\" TimeShift=\"0\" ChannelID=\"bad\" ChannelName=\"Bad\" UserChannelID=\"3\" TimeShiftURL=\"shift\" TimeShift=\"1\" ChannelID=\"content-type\" ChannelName=\"Content\" UserChannelID=\"4\" TimeShiftURL=\"shift\" TimeShift=\"1\" ChannelID=\"large\" ChannelName=\"Large\" UserChannelID=\"5\" TimeShiftURL=\"shift\" TimeShift=\"1\"".into(),
        "/EPG/jsp/jkjiaoyutest/en/function/ajax/epg7getChannelByAjax.jsp" if request_body.contains("getChannelProg") => r#"{"status":"1","data":[{"ID":"programme","channelID":"channel","startTime":1000,"endTime":2000,"name":"Programme"}]}"#.into(),
        "/EPG/jsp/jkjiaoyutest/en/function/ajax/epg7getChannelByAjax.jsp" if request_body.contains("getTvodPlayUrl") => format!(r#"{{"status":"1","data":{{"ID":"programme","channelID":"channel","startTime":1000,"endTime":2000,"playURL":"http://{address}/tvod.m3u8"}}}}"#),
        "/EPG/jsp/jkjiaoyutest/en/function/ajax/epg7getChannelByAjax.jsp" => r#"{"status":"1","data":[{"ID":"channel","code":"code","name":"News","mixNo":"1"},{"ID":"live","code":"code","name":"Live","mixNo":"2"},{"ID":"bad","code":"code","name":"Bad","mixNo":"3"},{"ID":"content-type","code":"code","name":"Content","mixNo":"4"},{"ID":"large","code":"code","name":"Large","mixNo":"5"}]}"#.into(),
        "/EPG/jsp/jkjiaoyutest/en/epg4ChannelPlayByAjax.jsp" if request.query.contains("channelId=content-type") => format!(r#"{{"hlsUrl":"http://{address}/playlist"}}"#),
        "/EPG/jsp/jkjiaoyutest/en/epg4ChannelPlayByAjax.jsp" if request.query.contains("channelId=large") => format!(r#"{{"hlsUrl":"http://{address}/large.m3u8"}}"#),
        "/EPG/jsp/jkjiaoyutest/en/epg4ChannelPlayByAjax.jsp" if request.query.contains("channelId=bad") => format!(r#"{{"hlsUrl":"http://{address}/fail.m3u8"}}"#),
        "/EPG/jsp/jkjiaoyutest/en/epg4ChannelPlayByAjax.jsp" => format!(r#"{{"hlsUrl":"http://{address}/live.m3u8"}}"#),
        "/live.m3u8" | "/tvod.m3u8" | "/playlist" => "#EXTM3U\n#EXTINF:1,\nsegment.ts#fragment\n".into(),
        "/large.m3u8" => "#EXTM3U\n".into(),
        "/segment.ts" => "segment-bytes".into(),
        _ => String::new(),
    };
    let programme_failure = request_body.contains("getChannelProg")
        && (state.fail_all_programmes.load(Ordering::Relaxed)
            || (state.fail_bad_programmes.load(Ordering::Relaxed)
                && request_body.contains("channelID=bad")));
    let status_line = if programme_failure || route == "/fail.m3u8" {
        "500 Internal Server Error"
    } else if !route.contains("/EPG/")
        && !route.contains("/EDS/")
        && !matches!(
            route,
            "/EPG/jsp/authLoginHWCU.jsp"
                | "/EPG/jsp/ValidAuthenticationHWCU.jsp"
                | "/EPG/jsp/getchannellistHWCU.jsp"
                | "/EPG/jsp/jkjiaoyutest/en/function/ajax/epg7getChannelByAjax.jsp"
                | "/EPG/jsp/jkjiaoyutest/en/epg4ChannelPlayByAjax.jsp"
                | "/live.m3u8"
                | "/tvod.m3u8"
                | "/playlist"
                | "/large.m3u8"
                | "/segment.ts"
                | "/fail.m3u8"
        )
    {
        "404 Not Found"
    } else {
        "200 OK"
    };
    let lower_route = route.to_ascii_lowercase();
    let content_type = if route == "/playlist"
        || std::path::Path::new(&lower_route)
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
    let (status_line, extra_headers) = if route == "/segment.ts" && request.range.is_some() {
        (
            "206 Partial Content",
            "Content-Range: bytes 0-12/13\r\nAccept-Ranges: bytes\r\n",
        )
    } else {
        (status_line, "")
    };
    let content_length = if route == "/large.m3u8" {
        4 * 1024 * 1024 + 1
    } else {
        body.len()
    };
    format!(
        "HTTP/1.1 {status_line}\r\nContent-Type: {content_type}\r\nContent-Length: {content_length}\r\n{extra_headers}Connection: close\r\n\r\n{body}"
    )
}
