use std::env;
use std::fmt;
use std::fs;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::{Request, StatusCode, header::HOST};
use futures_util::stream;
use iptv_media_server::epg::{BootChannel, Programme};
use iptv_media_server::guide::GuideZone;
use iptv_media_server::health::HealthState;
use iptv_media_server::hls::{Clock, TokenFactory, TokenStore, rewrite_playlist};
use iptv_media_server::http::router::build_router;
use iptv_media_server::http::{
    AppState, AppStateParts, EpochClock, ForwardRequest, ForwardResponse, HlsForwarder, HlsFuture,
    HlsTarget, PlaybackError, PlaybackFuture, PlaybackResolver,
};
use iptv_media_server::lifecycle::{CancellationSource, CancellationToken, Lifecycle};
use iptv_media_server::render::{BaseUrl, RenderKind};
use iptv_media_server::snapshot::{LogoView, Snapshot, SnapshotStore};
use reqwest::Url;
use ring::digest::{SHA256, digest};
use serde::Serialize;
use tokio::task::JoinSet;
use tower::ServiceExt;

const SCHEMA_VERSION: u32 = 1;
const SEED: u64 = 0x4954_5054_564d_4f43;
const EPOCH: u64 = 1_700_000_000;
const STREAMS: usize = 1_000;
const CHANNELS: usize = STREAMS;
const PROGRAMMES: usize = STREAMS;
const URI_REFS_PER_STREAM: usize = 5;
const TOKEN_RESOURCES: usize = STREAMS * URI_REFS_PER_STREAM;
const LOGICAL_BITRATE_BPS: u64 = 4_000_000;
const WARMUP_SECONDS: u64 = 10;
const MEASURED_SECONDS: u64 = 60;
const XMLTV_TARGET_RPS: usize = 2_000;
const XMLTV_REQUESTS: usize = XMLTV_TARGET_RPS * 60;
const CLEANUP_LIMIT_NS: u64 = 1_000_000_000;
const RSS_LIMIT_BYTES: u64 = 256 * 1024 * 1024;
const BASE_URL: &str = "http://benchmark.invalid";

#[derive(Debug)]
struct BenchError(&'static str);

impl fmt::Display for BenchError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.0)
    }
}

impl std::error::Error for BenchError {}

#[derive(Serialize)]
struct Report {
    schema_version: u32,
    status: &'static str,
    failure_code: Option<&'static str>,
    workload: Workload,
    counts: Counts,
    timings_ns: Timings,
    digests: Digests,
    memory: Memory,
    observed_bounds: Bounds,
}

#[derive(Serialize)]
struct Workload {
    seed_hex: &'static str,
    epoch: u64,
    streams: usize,
    channels: usize,
    programmes: usize,
    logical_bitrate_bps: u64,
    warmup_seconds: u64,
    measured_seconds: u64,
    xmltv_target_rps: usize,
    xmltv_target_requests: usize,
    uri_refs_per_stream: usize,
    token_resources: usize,
}

#[derive(Serialize)]
struct Counts {
    stream_first_byte: usize,
    playlist_rewrites: usize,
    token_resolutions: usize,
    xmltv_requests: usize,
    unexpected_errors: usize,
    disconnects: usize,
    cleanup_active_requests: usize,
    render_invocations: usize,
    cached_bases: usize,
}

#[derive(Serialize)]
struct Distribution {
    count: usize,
    p50: u64,
    p95: u64,
    p99: u64,
    max: u64,
}

#[derive(Serialize)]
struct Timings {
    stream_first_byte: Distribution,
    playlist_rewrite: Distribution,
    xmltv: Distribution,
    cleanup: Distribution,
}

#[derive(Serialize)]
struct Digests {
    #[serde(rename = "playlist_input_sha256")]
    playlist_input: String,
    #[serde(rename = "playlist_output_sha256")]
    playlist_output: String,
    #[serde(rename = "m3u_sha256")]
    m3u: String,
    #[serde(rename = "xmltv_sha256")]
    xmltv: String,
}

#[derive(Serialize)]
struct Memory {
    proc_status_available: bool,
    rss_baseline_bytes: Option<u64>,
    rss_current_bytes: Option<u64>,
    rss_peak_bytes: Option<u64>,
    rss_delta_bytes: Option<u64>,
}

#[derive(Serialize)]
struct Bounds {
    stream_p99_limit_ns: u64,
    stream_p99_ns: u64,
    xmltv_p99_limit_ns: u64,
    xmltv_p99_ns: u64,
    rss_delta_limit_bytes: u64,
    cleanup_limit_ns: u64,
    cleanup_ns: u64,
}

struct FixedEpoch;
impl EpochClock for FixedEpoch {
    fn now(&self) -> u64 {
        EPOCH
    }
}

struct FixedClock;
impl Clock for FixedClock {
    fn now(&self) -> Duration {
        Duration::from_secs(42)
    }
}

struct SeededTokens(AtomicUsize);
impl TokenFactory for SeededTokens {
    fn token(&self) -> String {
        let index = self.0.fetch_add(1, Ordering::Relaxed);
        format!("{SEED:016x}{index:08x}")
    }
}

struct Playback;
impl PlaybackResolver for Playback {
    fn resolve_live<'a>(&'a self, _channel_id: &'a str) -> PlaybackFuture<'a> {
        Box::pin(async {
            Url::parse("http://benchmark.invalid/live.m3u8").map_err(|_| PlaybackError::Unavailable)
        })
    }

    fn resolve_replay<'a>(
        &'a self,
        _programme: &'a Programme,
        _cancellation: CancellationToken,
    ) -> PlaybackFuture<'a> {
        Box::pin(async {
            Url::parse("http://benchmark.invalid/replay.m3u8")
                .map_err(|_| PlaybackError::Unavailable)
        })
    }
}

struct Forwarder {
    tokens: Arc<TokenStore>,
    token_resolutions: AtomicUsize,
}

impl HlsForwarder for Forwarder {
    fn forward(&self, request: ForwardRequest) -> HlsFuture<'_> {
        let body = match request.target {
            HlsTarget::Url(_) => b"#EXTM3U\nsegment.ts\n".to_vec(),
            HlsTarget::Token(token) => match self.tokens.resolve(&token) {
                Ok(_) => {
                    self.token_resolutions.fetch_add(1, Ordering::Relaxed);
                    b"segment".to_vec()
                }
                Err(_) => Vec::new(),
            },
        };
        Box::pin(async move {
            let mut headers = axum::http::HeaderMap::new();
            headers.insert(
                "content-type",
                axum::http::HeaderValue::from_static("video/mp2t"),
            );
            Ok(ForwardResponse {
                status: StatusCode::OK,
                headers,
                body: Body::from_stream(stream::once(async move {
                    Ok::<_, std::convert::Infallible>(body)
                })),
            })
        })
    }
}

fn channel(index: usize) -> BootChannel {
    let id = format!("c{index:04}");
    BootChannel {
        channel_id: id.clone(),
        name: format!("Channel {index:04}"),
        mix_no: index.to_string(),
        channel_url: String::new(),
        time_shift_url: String::new(),
        channel_sdp: String::new(),
        time_shift: "1".to_owned(),
        time_shift_length: "1".to_owned(),
        logo_url: String::new(),
        code: format!("code-{index:04}"),
        group_title: "benchmark".to_owned(),
    }
}

fn programme(index: usize) -> Programme {
    Programme {
        title: format!("Programme {index:04}"),
        start_time_ms: 1_000_000,
        end_time_ms: 2_000_000,
        programme_id: format!("p{index:04}"),
        channel_id: format!("c{index:04}"),
    }
}

fn request(method: &str, uri: &str) -> Result<Request<Body>, BenchError> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header(HOST, "benchmark.invalid")
        .body(Body::empty())
        .map_err(|_| BenchError("request_build"))
}

fn sha256(bytes: &[u8]) -> String {
    hex::encode(digest(&SHA256, bytes).as_ref())
}

fn distribution(mut values: Vec<u64>) -> Result<Distribution, BenchError> {
    values.sort_unstable();
    let count = values.len();
    let last = count.checked_sub(1).ok_or(BenchError("empty_timing"))?;
    let percentile = |percent: usize| values[(count * percent / 100).min(last)];
    Ok(Distribution {
        count,
        p50: percentile(50),
        p95: percentile(95),
        p99: percentile(99),
        max: values[last],
    })
}

fn rss() -> Option<(u64, u64)> {
    let text = fs::read_to_string("/proc/self/status").ok()?;
    let value = |name: &str| {
        text.lines()
            .find_map(|line| {
                let (key, value) = line.split_once(':')?;
                (key == name)
                    .then(|| value.trim().strip_suffix(" kB")?.parse::<u64>().ok())
                    .flatten()
            })?
            .checked_mul(1024)
    };
    Some((value("VmRSS")?, value("VmHWM")?))
}

fn report(
    status: &'static str,
    failure_code: Option<&'static str>,
    counts: Counts,
    timings: Timings,
    digests: Digests,
    memory: Memory,
    bounds: Bounds,
) -> Report {
    Report {
        schema_version: SCHEMA_VERSION,
        status,
        failure_code,
        workload: Workload {
            seed_hex: "49545054564d4f43",
            epoch: EPOCH,
            streams: STREAMS,
            channels: CHANNELS,
            programmes: PROGRAMMES,
            logical_bitrate_bps: LOGICAL_BITRATE_BPS,
            warmup_seconds: WARMUP_SECONDS,
            measured_seconds: MEASURED_SECONDS,
            xmltv_target_rps: XMLTV_TARGET_RPS,
            xmltv_target_requests: XMLTV_REQUESTS,
            uri_refs_per_stream: URI_REFS_PER_STREAM,
            token_resources: TOKEN_RESOURCES,
        },
        counts,
        timings_ns: timings,
        digests,
        memory,
        observed_bounds: bounds,
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "the benchmark scenario keeps setup, cohort, cache, and cleanup order visible"
)]
async fn execute(assertions: bool) -> Result<Report, BenchError> {
    let rss_before = rss();
    if assertions && rss_before.is_none() {
        return Err(BenchError("rss_unavailable"));
    }
    let channels: Vec<_> = (0..CHANNELS).map(channel).collect();
    let programmes: Vec<_> = (0..PROGRAMMES).map(programme).collect();
    let snapshot = Snapshot::new(
        1,
        channels.clone(),
        programmes,
        LogoView::from_channels(&channels),
        GuideZone::Fixed { offset_minutes: 0 },
        1,
    )
    .map_err(|_| BenchError("snapshot_build"))?;
    let snapshots = SnapshotStore::new();
    snapshots
        .publish(Arc::clone(&snapshot))
        .map_err(|_| BenchError("snapshot_publish"))?;
    let tokens = Arc::new(TokenStore::new(
        Arc::new(FixedClock),
        Arc::new(SeededTokens(AtomicUsize::new(0))),
    ));
    let cancellation = CancellationSource::new();
    let final_url = Url::parse("http://benchmark.invalid/hls/index.m3u8")
        .map_err(|_| BenchError("url_build"))?;
    let playlist_input = b"#EXTM3U\nseg-0.ts\nseg-1.ts\nseg-2.ts\nseg-3.ts\nseg-4.ts\n";
    let mut playlist_times = Vec::with_capacity(STREAMS);
    let mut all_tokens = Vec::with_capacity(TOKEN_RESOURCES);
    let mut rewrite = None;
    for _ in 0..STREAMS {
        let start = Instant::now();
        let current = rewrite_playlist(playlist_input, &final_url, &cancellation.token(), &tokens)
            .map_err(|_| BenchError("playlist_rewrite"))?;
        playlist_times.push(
            u64::try_from(start.elapsed().as_nanos()).map_err(|_| BenchError("timing_overflow"))?,
        );
        all_tokens.extend(
            current
                .resources
                .iter()
                .map(|resource| resource.token.clone()),
        );
        rewrite = Some(current);
    }
    let rewrite = rewrite.ok_or(BenchError("playlist_empty"))?;
    if rewrite.resources.len() != URI_REFS_PER_STREAM {
        return Err(BenchError("playlist_reference_count"));
    }
    if tokens.len() != TOKEN_RESOURCES {
        return Err(BenchError("token_resource_count"));
    }
    let input_digest = sha256(playlist_input);
    let output_digest = sha256(&rewrite.body);
    let app = AppState::from_parts(AppStateParts {
        lifecycle: Lifecycle::new(),
        snapshots,
        health: HealthState::new(),
        playback: Arc::new(Playback),
        hls: Arc::new(Forwarder {
            tokens: Arc::clone(&tokens),
            token_resolutions: AtomicUsize::new(0),
        }),
        tokens: Arc::clone(&tokens),
        logos: None,
        clock: Arc::new(FixedEpoch),
    });
    let router = build_router(app.clone());
    let warmup_base = BaseUrl::parse(BASE_URL).map_err(|_| BenchError("base_url"))?;
    snapshot
        .render_cached(
            RenderKind::M3u,
            &warmup_base,
            Some(&app.lifecycle().cancellation_token()),
        )
        .await
        .map_err(|_| BenchError("warmup_m3u"))?;
    snapshot
        .render_cached(
            RenderKind::Xmltv,
            &warmup_base,
            Some(&app.lifecycle().cancellation_token()),
        )
        .await
        .map_err(|_| BenchError("warmup_xmltv"))?;
    let mut streams = JoinSet::new();
    for index in 0..STREAMS {
        let router = router.clone();
        streams.spawn(async move {
            let start = Instant::now();
            router
                .oneshot(
                    request("GET", &format!("/stream/c{index:04}"))
                        .map_err(|_| BenchError("stream_request"))?,
                )
                .await
                .map(|response| {
                    (
                        response.status() == StatusCode::OK,
                        u64::try_from(start.elapsed().as_nanos()),
                    )
                })
                .map_err(|_| BenchError("stream_response"))
        });
    }
    let mut stream_times = Vec::with_capacity(STREAMS);
    let mut unexpected_errors = 0;
    let mut failure_code = None;
    while let Some(result) = streams.join_next().await {
        match result {
            Ok(Ok((true, Ok(duration)))) => stream_times.push(duration),
            Ok(Ok((false, _)) | Err(_)) | Err(_) => {
                unexpected_errors += 1;
                failure_code = Some("stream_cohort");
            }
            Ok(Ok((true, Err(_)))) => {
                unexpected_errors += 1;
                failure_code = Some("stream_timing");
            }
        }
    }
    let token_list = all_tokens;
    let mut token_tasks = JoinSet::new();
    for token in token_list {
        let router = router.clone();
        token_tasks.spawn(async move {
            let start = Instant::now();
            router
                .oneshot(
                    request("GET", &format!("/_hls/{token}"))
                        .map_err(|_| BenchError("token_request"))?,
                )
                .await
                .map(|response| {
                    (
                        response.status() == StatusCode::OK,
                        u64::try_from(start.elapsed().as_nanos()),
                    )
                })
                .map_err(|_| BenchError("token_response"))
        });
    }
    let mut token_times = Vec::with_capacity(TOKEN_RESOURCES);
    while let Some(result) = token_tasks.join_next().await {
        match result {
            Ok(Ok((true, Ok(duration)))) => token_times.push(duration),
            Ok(Ok((false, _)) | Err(_)) | Err(_) => {
                unexpected_errors += 1;
                failure_code = Some("token_cohort");
            }
            Ok(Ok((true, Err(_)))) => {
                unexpected_errors += 1;
                failure_code = Some("token_timing");
            }
        }
    }
    let mut xmltv_times = Vec::with_capacity(XMLTV_REQUESTS);
    for _ in 0..XMLTV_REQUESTS {
        let start = Instant::now();
        let response = router
            .clone()
            .oneshot(request("GET", "/xmltv.xml").map_err(|_| BenchError("xmltv_request"))?)
            .await
            .map_err(|_| BenchError("xmltv_response"))?;
        if response.status() != StatusCode::OK {
            unexpected_errors += 1;
            failure_code = Some("xmltv_status");
        }
        xmltv_times.push(
            u64::try_from(start.elapsed().as_nanos()).map_err(|_| BenchError("timing_overflow"))?,
        );
    }
    let xmltv = snapshot
        .render_cached(
            RenderKind::Xmltv,
            &warmup_base,
            Some(&app.lifecycle().cancellation_token()),
        )
        .await
        .map_err(|_| BenchError("xmltv_cache"))?;
    let m3u = snapshot
        .render_cached(
            RenderKind::M3u,
            &warmup_base,
            Some(&app.lifecycle().cancellation_token()),
        )
        .await
        .map_err(|_| BenchError("m3u_cache"))?;
    let cancellation = CancellationSource::new();
    cancellation.cancel();
    let cancellation_base =
        BaseUrl::parse("http://cancel.invalid").map_err(|_| BenchError("base_url"))?;
    if snapshot
        .render_cached(
            RenderKind::M3u,
            &cancellation_base,
            Some(&cancellation.token()),
        )
        .await
        .is_ok()
    {
        unexpected_errors += 1;
    }
    let cleanup_start = Instant::now();
    let lifecycle = app.lifecycle();
    lifecycle.begin_shutdown();
    let _ = lifecycle.drain(Duration::from_secs(1)).await;
    let cleanup_ns = u64::try_from(cleanup_start.elapsed().as_nanos())
        .map_err(|_| BenchError("timing_overflow"))?;
    let rss_after = rss();
    let rss_delta = rss_before
        .zip(rss_after)
        .map(|(before, after)| after.0.saturating_sub(before.0));
    let memory = Memory {
        proc_status_available: rss_before.is_some() && rss_after.is_some(),
        rss_baseline_bytes: rss_before.map(|value| value.0),
        rss_current_bytes: rss_after.map(|value| value.0),
        rss_peak_bytes: rss_after.map(|value| value.1),
        rss_delta_bytes: rss_delta,
    };
    let timings = Timings {
        stream_first_byte: distribution(stream_times)?,
        playlist_rewrite: distribution(playlist_times)?,
        xmltv: distribution(xmltv_times)?,
        cleanup: distribution(vec![cleanup_ns])?,
    };
    let counts = Counts {
        stream_first_byte: timings.stream_first_byte.count,
        playlist_rewrites: STREAMS,
        token_resolutions: token_times.len(),
        xmltv_requests: XMLTV_REQUESTS,
        unexpected_errors,
        disconnects: 0,
        cleanup_active_requests: lifecycle.active_requests(),
        render_invocations: snapshot.render_invocations(),
        cached_bases: snapshot
            .cached_base_count()
            .map_err(|_| BenchError("cache_count"))?,
    };
    let bounds = Bounds {
        stream_p99_limit_ns: 50_000_000,
        stream_p99_ns: timings.stream_first_byte.p99,
        xmltv_p99_limit_ns: 20_000_000,
        xmltv_p99_ns: timings.xmltv.p99,
        rss_delta_limit_bytes: RSS_LIMIT_BYTES,
        cleanup_limit_ns: CLEANUP_LIMIT_NS,
        cleanup_ns,
    };
    if assertions
        && (unexpected_errors != 0
            || counts.token_resolutions != TOKEN_RESOURCES
            || counts.cleanup_active_requests != 0
            || bounds.stream_p99_ns > bounds.stream_p99_limit_ns
            || bounds.xmltv_p99_ns > bounds.xmltv_p99_limit_ns
            || rss_delta.is_some_and(|delta| delta > RSS_LIMIT_BYTES)
            || cleanup_ns > CLEANUP_LIMIT_NS)
    {
        return Err(BenchError("threshold"));
    }
    Ok(report(
        "PASS",
        failure_code,
        counts,
        timings,
        Digests {
            playlist_input: input_digest,
            playlist_output: output_digest,
            m3u: sha256(&m3u),
            xmltv: sha256(&xmltv),
        },
        memory,
        bounds,
    ))
}

fn selftest() -> Result<(), BenchError> {
    if TOKEN_RESOURCES != STREAMS * URI_REFS_PER_STREAM
        || XMLTV_REQUESTS != 120_000
        || SEED != 0x4954_5054_564d_4f43
    {
        return Err(BenchError("manifest"));
    }
    Ok(())
}

pub fn run() {
    let args: Vec<_> = env::args().skip(1).collect();
    let assertions = args.iter().any(|arg| arg == "--assert");
    if args.iter().any(|arg| arg == "--selftest") {
        if let Err(error) = selftest() {
            eprintln!("selftest failure code: {error}");
            std::process::exit(1);
        }
        return;
    }
    if args
        .iter()
        .any(|arg| !matches!(arg.as_str(), "--assert" | "--offline" | "--bench"))
    {
        eprintln!("benchmark failure code: invalid_args");
        std::process::exit(2);
    }
    let Ok(runtime) = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    else {
        eprintln!("benchmark failure code: runtime");
        std::process::exit(1);
    };
    match runtime.block_on(execute(assertions)) {
        Ok(report) => match serde_json::to_string(&report) {
            Ok(json) => println!("{json}"),
            Err(_) => std::process::exit(1),
        },
        Err(error) => {
            println!(
                "{{\"schema_version\":{SCHEMA_VERSION},\"status\":\"FAIL\",\"failure_code\":\"{}\"}}",
                error.0
            );
            std::process::exit(1);
        }
    }
}
