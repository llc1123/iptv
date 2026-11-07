use crate::{
    config::Config,
    epg::{BootChannel, EpgClient, EpgError, Programme, ProgrammeQuery},
    error::RequestError,
    guide::{GuideZone, SystemGuideClock, build_windows_from_clock},
    snapshot::Snapshot,
};
use futures_util::stream::{self, StreamExt};
use std::{sync::Arc, time::Duration};
use tokio::{
    sync::{RwLock, mpsc, oneshot, watch},
    time::{self, Instant},
};

#[derive(Clone, Copy, Debug)]
pub struct RefreshCounts {
    pub channels: usize,
    pub guide_programmes: usize,
}

pub type RefreshReply = oneshot::Sender<Result<RefreshCounts, RequestError>>;

pub async fn refresh_once(
    client: &EpgClient,
    config: &Config,
    zone: &GuideZone,
    previous: Option<&[Programme]>,
) -> Result<(Vec<BootChannel>, Vec<Programme>), EpgError> {
    let channels = client.channels().await?;
    let windows = build_windows_from_clock(
        &SystemGuideClock,
        zone,
        config.guide_past_days,
        config.guide_future_days,
    )
    .map_err(|_| EpgError::Provider("guide windows could not be built".to_owned()))?;
    let requests = channels
        .iter()
        .filter(|channel| !channel.code.is_empty())
        .flat_map(|channel| {
            windows.iter().map(move |window| ProgrammeQuery {
                channel_id: channel.channel_id.clone(),
                channel_code: channel.code.clone(),
                start_time_ms: window.start_ms,
                end_time_ms: window.end_ms.saturating_sub(1),
            })
        })
        .collect::<Vec<_>>();
    let results = stream::iter(requests)
        .map(|query| async move {
            let result = client.programmes(&query).await;
            (query, result)
        })
        .buffer_unordered(8)
        .collect::<Vec<_>>()
        .await;
    let successful = results.iter().filter(|(_, result)| result.is_ok()).count();
    if successful == 0 {
        return Err(EpgError::Provider(
            "all programme windows failed".to_owned(),
        ));
    }
    let mut programmes = Vec::new();
    for (query, result) in results {
        match result {
            Ok(items) => programmes.extend(items),
            Err(error) => {
                tracing::debug!(%error, channel_id = %query.channel_id, "programme window failed");
                if let Some(previous) = previous {
                    programmes.extend(
                        previous
                            .iter()
                            .filter(|programme| {
                                programme.channel_id == query.channel_id
                                    && programme.start_time_ms < query.end_time_ms
                                    && programme.end_time_ms > query.start_time_ms
                            })
                            .cloned(),
                    );
                }
            }
        }
    }
    Ok((channels, programmes))
}

pub async fn run_loop(
    client: EpgClient,
    config: Config,
    zone: GuideZone,
    state: Arc<RwLock<Arc<Snapshot>>>,
    mut requests: mpsc::Receiver<RefreshReply>,
    refresh_immediately: bool,
    mut shutdown: watch::Receiver<bool>,
) {
    let period = Duration::from_secs(config.refresh_interval_seconds);
    let first_tick = Instant::now()
        + if refresh_immediately {
            Duration::ZERO
        } else {
            period
        };
    let mut interval = time::interval_at(first_tick, period);
    loop {
        let mut replies = Vec::new();
        let trigger = tokio::select! {
            _ = interval.tick() => true,
            Some(reply) = requests.recv() => { replies.push(reply); while let Ok(reply) = requests.try_recv() { replies.push(reply); } true },
            result = shutdown.changed() => {
                if result.is_err() || *shutdown.borrow() {
                    while let Ok(reply) = requests.try_recv() {
                        let _ = reply.send(Err(RequestError::new("service", "service is shutting down")));
                    }
                    return;
                }
                false
            },
        };
        if !trigger {
            continue;
        }
        let previous = state.read().await;
        let previous_programmes = previous.programmes().to_vec();
        drop(previous);
        let result = refresh_once(&client, &config, &zone, Some(&previous_programmes)).await;
        match result {
            Ok((channels, programmes)) => {
                let snapshot = Snapshot::new(
                    channels,
                    programmes,
                    zone.clone(),
                    config.guide_past_days.saturating_add(1),
                );
                let counts = RefreshCounts {
                    channels: snapshot.channels().len(),
                    guide_programmes: snapshot.programmes().len(),
                };
                *state.write().await = snapshot;
                let cache = crate::guide::GuideCache::new(config.guide_cache.clone());
                let cached_programmes = state.read().await.programmes().to_vec();
                let cache_result =
                    match tokio::task::spawn_blocking(move || cache.save(&cached_programmes)).await
                    {
                        Ok(result) => result,
                        Err(error) => Err(std::io::Error::other(error.to_string())),
                    };
                let result = cache_result.map_err(|error| {
                    tracing::warn!(%error, "guide cache write failed");
                    RequestError::new("guide-cache", error.to_string())
                });
                for reply in replies {
                    if reply.send(result.clone().map(|()| counts)).is_err() {
                        tracing::debug!("refresh reply receiver closed");
                    }
                }
            }
            Err(error) => {
                tracing::warn!(%error, "metadata refresh cycle failed; retaining snapshot");
                let request_error = RequestError::new("epg-guide", error.to_string());
                for reply in replies {
                    if reply.send(Err(request_error.clone())).is_err() {
                        tracing::debug!("refresh reply receiver closed");
                    }
                }
            }
        }
    }
}
