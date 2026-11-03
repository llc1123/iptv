use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Mutex, watch};
use tokio::task::JoinSet;

use super::wire::{
    Request, digest, form_keys, read_request, write_response, write_response_with_location,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RequestEvent {
    pub method: String,
    pub route: String,
    pub body_len: usize,
    pub body_digest: String,
    pub form_keys: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RedactedEvent {
    pub method: String,
    pub route: String,
    pub body_len: usize,
    pub body_digest: String,
    pub form_keys_digest: String,
}

#[derive(Clone)]
struct State {
    events: Arc<Mutex<Vec<RequestEvent>>>,
    tvod_calls: Arc<AtomicUsize>,
}

pub struct MockProvider {
    origin: String,
    state: State,
    shutdown: watch::Sender<bool>,
    task: Option<tokio::task::JoinHandle<io::Result<()>>>,
}

impl MockProvider {
    pub async fn start() -> io::Result<Self> {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).await?;
        let address = listener.local_addr()?;
        let origin = format!("http://{address}");
        let (shutdown, mut shutdown_rx) = watch::channel(false);
        let state = State {
            events: Arc::new(Mutex::new(Vec::new())),
            tvod_calls: Arc::new(AtomicUsize::new(0)),
        };
        let task_state = state.clone();
        let task = tokio::spawn(async move {
            let mut connections = JoinSet::new();
            loop {
                tokio::select! {
                    biased;
                    changed = shutdown_rx.changed() => {
                        changed.map_err(|error| io::Error::other(error.to_string()))?;
                        if *shutdown_rx.borrow() { break; }
                    },
                    accepted = listener.accept() => {
                        let (stream, _) = accepted?;
                        let state = task_state.clone();
                        connections.spawn(async move { handle_connection(stream, state).await });
                    }
                }
            }
            connections.abort_all();
            while let Some(result) = connections.join_next().await {
                result.map_err(|error| io::Error::other(error.to_string()))??;
            }
            Ok(())
        });
        Ok(Self {
            origin,
            state,
            shutdown,
            task: Some(task),
        })
    }

    pub fn origin(&self) -> &str {
        &self.origin
    }

    pub async fn events(&self) -> Vec<RequestEvent> {
        self.state.events.lock().await.clone()
    }

    pub async fn redacted_events(&self) -> Vec<RedactedEvent> {
        self.events()
            .await
            .iter()
            .map(|event| RedactedEvent {
                method: event.method.clone(),
                route: event.route.clone(),
                body_len: event.body_len,
                body_digest: event.body_digest.clone(),
                form_keys_digest: digest(event.form_keys.join("&").as_bytes()),
            })
            .collect()
    }

    pub async fn shutdown(mut self) -> io::Result<()> {
        self.shutdown.send_replace(true);
        if let Some(task) = self.task.take() {
            task.await
                .map_err(|error| io::Error::other(error.to_string()))??;
        }
        Ok(())
    }
}

async fn handle_connection(mut stream: TcpStream, state: State) -> io::Result<()> {
    let request = read_request(&mut stream).await?;
    let event = RequestEvent {
        method: request.method.clone(),
        route: request.route.clone(),
        body_len: request.body.len(),
        body_digest: digest(&request.body),
        form_keys: form_keys(&request.body),
    };
    state.events.lock().await.push(event);
    if request.route.contains("heartbeat")
        || request.route.contains("heartbit")
        || request.route.contains("keepalive")
        || request.route.contains("keep-alive")
        || request.route.contains("/log/epgplay")
    {
        return write_response(&mut stream, "404 Not Found", b"").await;
    }
    let (status, body, location) = response_for(&request, &state);
    write_response_with_location(&mut stream, status, &body, location.as_deref()).await
}

fn response_for(request: &Request, state: &State) -> (&'static str, Vec<u8>, Option<String>) {
    match (request.method.as_str(), request.route.as_str()) {
        ("GET", "/EDS/jsp/AuthenticationURL") => (
            "302 Found",
            Vec::new(),
            Some("/EPG/jsp/authLoginHWCU.jsp".to_owned()),
        ),
        ("GET" | "POST", "/EPG/jsp/authLoginHWCU.jsp") => (
            "200 OK",
            b"EncryptToken=fixture-encrypt-token".to_vec(),
            None,
        ),
        ("POST", "/EPG/jsp/ValidAuthenticationHWCU.jsp") => (
            "200 OK",
            b"CUSetConfig('UserToken','fixture-user-token');CTCSetConfig('SessionID','fixture-session');".to_vec(),
            None,
        ),
        ("POST", "/EPG/jsp/getchannellistHWCU.jsp") => (
            "200 OK",
            b"x ChannelID=\"c1\" ChannelName=\"One\" UserChannelID=\"1\" TimeShiftURL=\"replay\"".to_vec(),
            None,
        ),
        ("POST", "/EPG/jsp/jkjiaoyutest/en/function/ajax/epg7getChannelByAjax.jsp")
            if String::from_utf8_lossy(&request.body).contains("getChannelProg") => (
                "200 OK",
                b"{\"status\":\"1\",\"data\":[{\"ID\":\"p1\",\"channelID\":\"c1\",\"name\":\"One programme\",\"startTime\":1000000,\"endTime\":2000000}]}".to_vec(),
                None,
            ),
        ("POST", "/EPG/jsp/jkjiaoyutest/en/function/ajax/epg7getChannelByAjax.jsp")
            if String::from_utf8_lossy(&request.body).contains("getChannelList") => (
            "200 OK",
            b"{\"status\":\"1\",\"data\":[{\"ID\":\"c1\",\"code\":\"code-1\",\"name\":\"One\",\"mixNo\":\"1\"}]}".to_vec(),
            None,
        ),
        ("POST", "/EPG/jsp/jkjiaoyutest/en/function/ajax/epg7getChannelByAjax.jsp")
            if String::from_utf8_lossy(&request.body).contains("getTvodPlayUrl") => {
            let call = state.tvod_calls.fetch_add(1, Ordering::SeqCst);
            if call == 0 {
                ("401 Unauthorized", b"{\"status\":\"0\"}".to_vec(), None)
            } else {
                ("200 OK", b"{\"status\":\"1\",\"data\":{\"ID\":\"p1\",\"channelID\":\"c1\",\"startTime\":1000000,\"endTime\":2000000,\"playURL\":\"http://media.example.test/live.m3u8\"}}".to_vec(), None)
            }
        }
        _ => ("404 Not Found", Vec::new(), None),
    }
}
