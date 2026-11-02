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

#[tokio::test]
async fn ninth_base_evicts_an_inactive_entry_when_other_bases_are_active() {
    let snapshot = Snapshot::new(
        1,
        vec![channel("c1")],
        Vec::new(),
        LogoView::from_channels(&[channel("c1")]),
        GuideZone::Fixed { offset_minutes: 0 },
        0,
    )
    .expect("valid fixture");
    let inactive = BaseUrl::parse("http://inactive.test").expect("valid base");
    snapshot
        .render_cached(RenderKind::M3u, &inactive, None)
        .await
        .expect("inactive render");

    let mut active = Vec::new();
    for index in 0..7 {
        let base = format!("http://active{index}.test");
        let acquired = snapshot
            .acquire_flight(RenderKind::M3u, &base)
            .expect("active flight");
        let CacheAcquire::Flight {
            flight,
            owner: true,
        } = acquired
        else {
            panic!("new base must create owner flight");
        };
        active.push((base, flight));
    }

    let ninth = BaseUrl::parse("http://ninth.test").expect("valid base");
    snapshot
        .render_cached(RenderKind::M3u, &ninth, None)
        .await
        .expect("ninth base evicts inactive entry");
    assert_eq!(snapshot.cached_base_count().expect("cache count"), 8);

    let CacheAcquire::Flight {
        owner: true,
        flight,
    } = snapshot
        .acquire_flight(RenderKind::M3u, inactive.as_str())
        .expect("evicted inactive base can be recreated")
    else {
        panic!("inactive base should have been evicted");
    };
    snapshot
        .finish_flight(
            RenderKind::M3u,
            inactive.as_str(),
            &flight,
            &Err(RenderError::Cancelled),
        )
        .expect("recreated inactive flight remains owned");
    for (base, flight) in active {
        snapshot
            .finish_flight(
                RenderKind::M3u,
                &base,
                &flight,
                &Err(RenderError::Cancelled),
            )
            .expect("active flight remains owned");
    }
}
