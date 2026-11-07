use crate::{
    config::Config,
    epg::{BootChannel, EpgClient, Programme, ProgrammeQuery},
    guide::{GuideZone, SystemGuideClock, build_windows_from_clock},
    snapshot::Snapshot,
};
use futures_util::stream::{self, StreamExt};
use std::{sync::Arc, time::Duration};
use tokio::sync::{Notify, RwLock, watch};

pub async fn refresh_once(
    client: &EpgClient,
    channels: &[BootChannel],
    config: &Config,
    zone: &GuideZone,
) -> (Vec<BootChannel>, Vec<Programme>) {
    let channels = match client.channels().await {
        Ok(value) => value,
        Err(error) => {
            tracing::warn!(%error, "metadata refresh channel request failed");
            return (channels.to_vec(), Vec::new());
        }
    };
    let windows = match build_windows_from_clock(
        &SystemGuideClock,
        zone,
        config.guide_past_days,
        config.guide_future_days,
    ) {
        Ok(value) => value,
        Err(error) => {
            tracing::warn!(%error, "metadata refresh windows failed");
            return (channels, Vec::new());
        }
    };
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
    (channels, programmes)
}

pub async fn run_loop(
    client: EpgClient,
    config: Config,
    zone: GuideZone,
    state: Arc<RwLock<Option<Arc<Snapshot>>>>,
    notify: Arc<Notify>,
    done: Arc<Notify>,
    mut shutdown: watch::Receiver<bool>,
) {
    let mut interval = tokio::time::interval(Duration::from_secs(config.refresh_interval_seconds));
    loop {
        tokio::select! { _ = interval.tick() => {}, () = notify.notified() => {}, result = shutdown.changed() => { if result.is_ok() && *shutdown.borrow() { return; } } }
        let old = state
            .read()
            .await
            .as_ref()
            .map(|snapshot| snapshot.channels().to_vec())
            .unwrap_or_default();
        let (channels, programmes) = refresh_once(&client, &old, &config, &zone).await;
        if channels.is_empty() && !old.is_empty() {
            tracing::warn!("metadata refresh retained previous snapshot");
            continue;
        }
        let snapshot = Snapshot::new(
            channels,
            programmes,
            zone.clone(),
            config.guide_past_days.saturating_add(1),
        );
        *state.write().await = Some(Arc::clone(&snapshot));
        let cache = crate::guide::GuideCache::new(config.guide_cache.clone());
        let programmes = snapshot.programmes().to_vec();
        if let Ok(Err(error)) = tokio::task::spawn_blocking(move || cache.save(&programmes)).await {
            tracing::warn!(%error, "guide cache write failed");
        }
        done.notify_waiters();
    }
}
