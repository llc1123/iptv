use crate::{
    config::Config,
    epg::{BootChannel, EpgClient, EpgError, Programme, ProgrammeQuery},
    guide::{GuideZone, SystemGuideClock, build_windows_from_clock},
    snapshot::Snapshot,
};
use futures_util::stream::{self, StreamExt};
use std::{sync::Arc, time::Duration};
use tokio::{
    sync::{RwLock, mpsc, oneshot, watch},
    time::{self, Instant},
};

pub type RefreshReply = oneshot::Sender<Result<(usize, usize), String>>;

pub async fn refresh_once(
    client: &EpgClient,
    config: &Config,
    zone: &GuideZone,
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
    let programmes = stream::iter(requests)
        .map(|query| async move { client.programmes(&query).await })
        .buffer_unordered(8)
        .filter_map(|result| async move {
            match result {
                Ok(items) => Some(items),
                Err(error) => {
                    tracing::debug!(%error, "programme window failed");
                    None
                }
            }
        })
        .fold(Vec::new(), |mut all, items| async move {
            all.extend(items);
            all
        })
        .await;
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
            result = shutdown.changed() => { if result.is_ok() && *shutdown.borrow() { return; } false },
        };
        if !trigger {
            continue;
        }
        let result = refresh_once(&client, &config, &zone).await;
        match result {
            Ok((channels, programmes)) => {
                let snapshot = Snapshot::new(
                    channels,
                    programmes,
                    zone.clone(),
                    config.guide_past_days.saturating_add(1),
                );
                let counts = (snapshot.channels().len(), snapshot.programmes().len());
                *state.write().await = snapshot;
                let cache = crate::guide::GuideCache::new(config.guide_cache.clone());
                let cached_programmes = state.read().await.programmes().to_vec();
                if let Ok(Err(error)) =
                    tokio::task::spawn_blocking(move || cache.save(&cached_programmes)).await
                {
                    tracing::warn!(%error, "guide cache write failed");
                }
                for reply in replies {
                    if reply.send(Ok(counts)).is_err() {
                        tracing::debug!("refresh reply receiver closed");
                    }
                }
            }
            Err(error) => {
                tracing::warn!(%error, "metadata refresh cycle failed; retaining snapshot");
                for reply in replies {
                    if reply.send(Err(error.to_string())).is_err() {
                        tracing::debug!("refresh reply receiver closed");
                    }
                }
            }
        }
    }
}
