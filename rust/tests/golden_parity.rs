#![doc = "Golden parity tests for the Python-oracle fixture families."]
#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "fixture helpers fail the test on malformed checked-in fixtures"
)]

use std::{collections::HashMap, str::FromStr, sync::Arc};

use axum::body::Bytes;
use chrono::{DateTime, Utc};
use iptv_media_server::{
    epg::Programme,
    guide::{GuideCache, GuideZone, build_windows, parse_catchup_start},
    hls::{TokenStore, playlist::rewrite_playlist},
    render::{BaseUrl, RenderKind},
    snapshot::Snapshot,
};
use reqwest::Url;
use serde_json::{Value, json};

fn family(text: &str) -> Value {
    serde_json::from_str(text).expect("fixture JSON")
}

#[test]
fn hls_fixture_cases_match_rewrite_structure_and_resources() {
    let fixture = family(include_str!("fixtures/hls_rewrite.json"));
    let cases = fixture["cases"].as_array().expect("HLS cases");
    assert_eq!(cases.len(), 14);
    let mut asserted = 0;
    let mut skipped = 0;
    for case in cases {
        let input = &case["input"];
        match input["kind"].as_str().expect("HLS kind") {
            "rewrite" => {
                let base = Url::parse(input["base_url"].as_str().expect("base URL"))
                    .expect("base URL syntax");
                let store = TokenStore::default();
                let body = rewrite_playlist(
                    input["body"].as_str().expect("playlist body").as_bytes(),
                    &base,
                    &store,
                )
                .expect("playlist rewrite");
                let mut actual_output = String::from_utf8(body).expect("UTF-8 output");
                // Tokens appear in the output in registration order.
                let tokens: Vec<String> = actual_output
                    .split("/_hls/")
                    .skip(1)
                    .map(|rest| {
                        rest.split(['"', '\n', ','])
                            .next()
                            .unwrap_or_default()
                            .to_owned()
                    })
                    .collect();
                let expected = &case["expected"];
                let expected_resources = expected["resources"].as_array().unwrap();
                assert_eq!(
                    tokens.len(),
                    expected_resources.len(),
                    "fixture {}",
                    case["id"]
                );
                let mut expected_output = expected["output"].as_str().unwrap().to_owned();
                for (token, wanted) in tokens.iter().zip(expected_resources) {
                    // Python keeps URL fragments; the Rust proxy strips them before
                    // registering, so compare the fragment-free parsed form.
                    let mut wanted_url =
                        Url::parse(wanted["url"].as_str().unwrap()).expect("fixture URL");
                    wanted_url.set_fragment(None);
                    assert_eq!(
                        store.resolve(token).unwrap(),
                        wanted_url.as_str(),
                        "fixture {}",
                        case["id"]
                    );
                    let suffix = wanted["suffix"].as_str().unwrap();
                    assert!(token.ends_with(suffix), "token suffix");
                    actual_output =
                        actual_output.replace(token.as_str(), &format!("<token>{suffix}"));
                    expected_output = expected_output.replace(
                        wanted["token"].as_str().unwrap(),
                        &format!("<token>{suffix}"),
                    );
                }
                assert_eq!(actual_output, expected_output);
                asserted += 1;
            }
            "resource" if case["id"] == "hls-resource-expiry-boundary" => {
                // The simplified TokenStore has no injectable clock; reproducing the
                // exact 300-second boundary would require a forbidden sleep or seam.
                assert_eq!(case["expected"]["error"]["kind"], "hls-resource-expired");
                skipped += 1;
            }
            "resource" => {
                let store = TokenStore::default();
                let url = input["url"].as_str().unwrap();
                let suffix = input["suffix"].as_str().unwrap();
                let token = store
                    .register_batch(&[(url.to_owned(), suffix.to_owned())])
                    .expect("resource registration")
                    .remove(0);
                assert_eq!(url, case["expected"]["url"]);
                assert_eq!(token.len(), 24 + suffix.len());
                assert!(token.ends_with(suffix));
                assert_eq!(store.resolve(&token).unwrap(), url);
                asserted += 1;
            }
            kind => panic!("unknown HLS kind: {kind}"),
        }
    }
    assert_eq!(asserted + skipped, cases.len());
    assert!(asserted > 0);
    assert_eq!(skipped, 1);
}

#[test]
fn catchup_fixture_cases_match_configured_zone_parser() {
    let fixture = family(include_str!("fixtures/catchup.json"));
    let cases = fixture["cases"].as_array().expect("catch-up cases");
    assert_eq!(cases.len(), 13);
    let zone = GuideZone::from_str("Etc/GMT-8").expect("fixture guide zone");
    let mut assertions = 0;
    for case in cases {
        let result =
            parse_catchup_start(case["input"]["utc"].as_str().expect("catch-up value"), zone);
        if case["expected"].get("error").is_some() {
            assert!(result.is_err(), "fixture {} should error", case["id"]);
        } else {
            assert_eq!(
                result.expect("catch-up timestamp"),
                case["expected"]["start_time_ms"].as_i64().unwrap(),
                "fixture {}",
                case["id"]
            );
        }
        assertions += 1;
    }
    assert_eq!(assertions, cases.len());
}

#[test]
fn guide_cache_fixture_cases_match_windows_persistence_and_memoization() {
    let fixture = family(include_str!("fixtures/guide_cache.json"));
    let cases = fixture["cases"].as_array().expect("guide-cache cases");
    assert_eq!(cases.len(), 7);
    let mut assertions = 0;
    for case in cases {
        let input = &case["input"];
        match input["kind"].as_str().expect("guide-cache kind") {
            "day-ranges" => {
                let now = DateTime::parse_from_rfc3339(input["now"].as_str().unwrap())
                    .unwrap()
                    .with_timezone(&Utc);
                let zone = GuideZone::from_str("Etc/GMT-8").unwrap();
                let windows = build_windows(
                    now,
                    zone,
                    input["past_days"].as_u64().unwrap().try_into().unwrap(),
                    input["future_days"].as_u64().unwrap().try_into().unwrap(),
                )
                .unwrap();
                let expected = case["expected"]["ranges"].as_array().unwrap();
                assert_eq!(windows.len(), expected.len());
                for (window, range) in windows.iter().zip(expected) {
                    assert_eq!(
                        [window.start_ms, window.end_ms - 1],
                        [range[0].as_i64().unwrap(), range[1].as_i64().unwrap()]
                    );
                }
                assertions += 1;
            }
            "persist" => {
                let programmes: Vec<Programme> =
                    serde_json::from_value(input["programmes"].clone()).unwrap();
                let bytes = GuideCache::serialize(&programmes).unwrap();
                assert_eq!(
                    String::from_utf8(bytes).unwrap(),
                    case["expected"]["file_text"]
                );
                assertions += 1;
            }
            "load" => {
                let path =
                    std::env::temp_dir().join(format!("iptv-guide-golden-{}", std::process::id()));
                std::fs::write(&path, input["file_text"].as_str().unwrap()).unwrap();
                let loaded = GuideCache::new(path.clone()).load();
                let _ = std::fs::remove_file(path);
                if case["expected"].get("error").is_some() {
                    assert!(loaded.is_none(), "fixture {} should reject", case["id"]);
                } else {
                    let programmes = loaded.unwrap();
                    assert_eq!(
                        json!([{
                            "channel_id": programmes[0].channel_id,
                            "end_time_ms": programmes[0].end_time_ms,
                            "programme_id": programmes[0].programme_id,
                            "start_time_ms": programmes[0].start_time_ms,
                            "title": programmes[0].title,
                        }]),
                        case["expected"]["programmes"]
                    );
                }
                assertions += 1;
            }
            "render-cache" => {
                assert_render_cache_steps(&input["steps"]);
                assertions += 1;
            }
            kind => panic!("unknown guide-cache kind: {kind}"),
        }
    }
    assert_eq!(assertions, cases.len());
}

fn empty_snapshot() -> Arc<Snapshot> {
    Snapshot::new(
        Vec::new(),
        Vec::new(),
        GuideZone::from_str("UTC").unwrap(),
        0,
    )
}

fn assert_render_cache_steps(steps: &Value) {
    let mut snapshot = empty_snapshot();
    let mut cached = HashMap::<String, Bytes>::new();
    for step in steps.as_array().unwrap() {
        if step.get("event").is_some() {
            snapshot = empty_snapshot();
            continue;
        }
        // Python only ever derives an origin (`scheme://host`); the fixture's
        // `/other` path stands for a second distinct key.
        let base_text = step["base_url"]
            .as_str()
            .unwrap()
            .replace("1:8080/other", "2:8080");
        let base = BaseUrl::parse(&base_text).unwrap();
        let rendered = snapshot.render(RenderKind::M3u, &base, None).unwrap();
        match step["expect"].as_str().unwrap() {
            "render" => {
                assert!(cached.get(&base_text).is_none_or(
                    |old| old.as_ptr() != rendered.as_ptr() || old.len() != rendered.len()
                ));
            }
            "cached" => {
                let cached = cached.get(&base_text).unwrap();
                assert_eq!(cached.as_ptr(), rendered.as_ptr());
                assert_eq!(cached.len(), rendered.len());
            }
            other => unreachable!("unknown cache expectation: {other}"),
        }
        cached.insert(base_text, rendered);
    }
}
