#![expect(
    clippy::expect_used,
    clippy::panic,
    reason = "unit fixtures use fixed valid values"
)]

use super::*;

fn channel(id: &str) -> BootChannel {
    BootChannel {
        channel_id: id.to_owned(),
        name: id.to_owned(),
        mix_no: String::new(),
        channel_url: String::new(),
        time_shift_url: String::new(),
        channel_sdp: String::new(),
        time_shift: String::new(),
        time_shift_length: String::new(),
        logo_url: String::new(),
        code: String::new(),
        group_title: String::new(),
    }
}

fn programme(id: &str, start: i64, end: i64, title: &str) -> Programme {
    Programme {
        title: title.to_owned(),
        start_time_ms: start,
        end_time_ms: end,
        programme_id: id.to_owned(),
        channel_id: "c1".to_owned(),
    }
}

#[test]
fn snapshot_rejects_duplicate_channel_ids_and_logo_misalignment() {
    let duplicate = Snapshot::new(
        1,
        vec![channel("c1"), channel("c1")],
        Vec::new(),
        LogoView::new(vec![None, None]),
        GuideZone::Fixed { offset_minutes: 0 },
        0,
    );
    assert!(matches!(
        duplicate,
        Err(SnapshotError::InvalidChannelIdentifiers)
    ));

    let misaligned = Snapshot::new(
        1,
        vec![channel("c1")],
        Vec::new(),
        LogoView::new(Vec::new()),
        GuideZone::Fixed { offset_minutes: 0 },
        0,
    );
    assert!(matches!(misaligned, Err(SnapshotError::LogoAlignment)));
}

#[test]
fn store_rejects_non_newer_generations_and_keeps_old_snapshot() {
    let store = SnapshotStore::new();
    let first = Snapshot::new(
        2,
        vec![channel("c1")],
        Vec::new(),
        LogoView::from_channels(&[channel("c1")]),
        GuideZone::Fixed { offset_minutes: 0 },
        0,
    )
    .expect("valid fixture");
    store.publish(Arc::clone(&first)).expect("first publish");
    let older = Snapshot::new(
        1,
        vec![channel("c1")],
        Vec::new(),
        LogoView::from_channels(&[channel("c1")]),
        GuideZone::Fixed { offset_minutes: 0 },
        0,
    )
    .expect("valid fixture");
    assert_eq!(store.publish(older), Err(SnapshotError::GenerationNotNewer));
    assert_eq!(
        store
            .load()
            .expect("store read")
            .map(|item| item.generation()),
        Some(2)
    );
}

#[test]
fn snapshot_preserves_xmltv_sort_only_by_channel_and_start() {
    let snapshot = Snapshot::new(
        1,
        vec![channel("c1")],
        vec![programme("b", 0, 20, "b"), programme("a", 0, 10, "a")],
        LogoView::from_channels(&[channel("c1")]),
        GuideZone::Fixed { offset_minutes: 0 },
        0,
    )
    .expect("valid fixture");
    assert_eq!(snapshot.xmltv_programmes[0].programme_id, "b");
}

#[tokio::test]
async fn cache_hit_reuses_one_render_and_keeps_m3u_xmltv_slots_separate() {
    let snapshot = Snapshot::new(
        1,
        vec![channel("c1")],
        Vec::new(),
        LogoView::from_channels(&[channel("c1")]),
        GuideZone::Fixed { offset_minutes: 0 },
        0,
    )
    .expect("valid fixture");
    let base = BaseUrl::parse("http://example.test").expect("valid base");
    let first = snapshot
        .render_cached(RenderKind::M3u, &base, None)
        .await
        .expect("m3u render");
    let second = snapshot
        .render_cached(RenderKind::M3u, &base, None)
        .await
        .expect("m3u cache hit");
    let _xmltv = snapshot
        .render_cached(RenderKind::Xmltv, &base, None)
        .await
        .expect("xmltv render");
    assert_eq!(first.as_ref(), second.as_ref());
    assert_eq!(snapshot.render_invocations(), 2);
    assert_eq!(snapshot.cached_base_count().expect("cache count"), 1);
}

#[test]
fn ninth_active_base_is_rejected_without_evicting_flights() {
    let snapshot = Snapshot::new(
        1,
        vec![channel("c1")],
        Vec::new(),
        LogoView::from_channels(&[channel("c1")]),
        GuideZone::Fixed { offset_minutes: 0 },
        0,
    )
    .expect("valid fixture");
    let mut flights = Vec::new();
    for index in 0..8 {
        let base = format!("http://example{index}.test");
        let acquired = snapshot
            .acquire_flight(RenderKind::M3u, &base)
            .expect("flight");
        let CacheAcquire::Flight {
            flight,
            owner: true,
        } = acquired
        else {
            panic!("new base must create owner flight");
        };
        flights.push((base, flight));
    }
    assert!(matches!(
        snapshot.acquire_flight(RenderKind::M3u, "http://example8.test"),
        Err(RenderError::CacheCapacity)
    ));
    for (base, flight) in flights {
        snapshot
            .finish_flight(RenderKind::M3u, &base, &flight, Err(RenderError::Cancelled))
            .expect("owner still owns flight");
    }
}

#[tokio::test]
async fn cancelled_waiter_does_not_cancel_owner_and_lost_owner_cannot_publish() {
    let snapshot = Snapshot::new(
        1,
        vec![channel("c1")],
        Vec::new(),
        LogoView::from_channels(&[channel("c1")]),
        GuideZone::Fixed { offset_minutes: 0 },
        0,
    )
    .expect("valid fixture");
    let base = BaseUrl::parse("http://example.test").expect("valid base");
    let CacheAcquire::Flight {
        flight,
        owner: true,
    } = snapshot
        .acquire_flight(RenderKind::M3u, base.as_str())
        .expect("owner flight")
    else {
        panic!("new base must create owner flight");
    };
    let source = crate::lifecycle::CancellationSource::new();
    source.cancel();
    let token = source.token();
    assert_eq!(
        snapshot
            .render_cached(RenderKind::M3u, &base, Some(&token))
            .await,
        Err(RenderError::Cancelled)
    );
    snapshot
        .finish_flight(
            RenderKind::M3u,
            base.as_str(),
            &flight,
            Err(RenderError::Cancelled),
        )
        .expect("owner still owns flight");
    let CacheAcquire::Flight {
        flight: lost,
        owner: true,
    } = snapshot
        .acquire_flight(RenderKind::M3u, base.as_str())
        .expect("new owner flight")
    else {
        panic!("failed result must not be cached");
    };
    snapshot
        .render_cache
        .lock()
        .expect("cache lock")
        .bases
        .clear();
    assert_eq!(
        snapshot.finish_flight(RenderKind::M3u, base.as_str(), &lost, Ok(Arc::from([]))),
        Err(RenderError::OwnershipLost)
    );
}

#[tokio::test]
async fn ninth_completed_base_evicts_the_oldest_inactive_entry() {
    let snapshot = Snapshot::new(
        1,
        vec![channel("c1")],
        Vec::new(),
        LogoView::from_channels(&[channel("c1")]),
        GuideZone::Fixed { offset_minutes: 0 },
        0,
    )
    .expect("valid fixture");
    for index in 0..9 {
        let base = BaseUrl::parse(&format!("http://example{index}.test")).expect("valid base");
        snapshot
            .render_cached(RenderKind::M3u, &base, None)
            .await
            .expect("render");
    }
    assert_eq!(snapshot.cached_base_count().expect("cache count"), 8);
}
