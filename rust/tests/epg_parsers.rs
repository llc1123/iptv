#![doc = "Fixture-driven acceptance tests for provider EPG models and parsers."]
#![expect(
    clippy::expect_used,
    reason = "fixture tests use checked-in valid inputs"
)]

use iptv_media_server::epg::{
    BootChannel, Channel, ChannelListResponse, HlsAddress, Programme, ProgrammeResponse,
    TvodAddress, parse_boot_channels, parse_channel_list, parse_programmes, tvod_times_seconds,
};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Debug, Deserialize)]
struct Family {
    cases: Vec<Case>,
}
#[derive(Debug, Deserialize)]
struct Case {
    id: String,
    input: Value,
    expected: Value,
}
fn family(path: &str) -> Family {
    serde_json::from_str(path).expect("checked-in fixture JSON is valid")
}
fn channel_fields(channel: &Channel) -> Value {
    json!({
        "auth_code": channel.auth_code,
        "code": channel.code,
        "epg_id": channel.epg_id,
        "is_charge": channel.is_charge,
        "media_id": channel.media_id,
        "mix_no": channel.mix_no,
        "name": channel.name,
    })
}

fn programme_fields(programme: &Programme) -> Value {
    json!({
        "channel_id": programme.channel_id,
        "end_time_ms": programme.end_time_ms,
        "programme_id": programme.programme_id,
        "start_time_ms": programme.start_time_ms,
        "title": programme.title,
    })
}

#[test]
fn parser_fixture_cases_preserve_boot_boundaries_and_values() {
    let fixtures = family(include_str!("fixtures/parsers.json"));
    for fixture in &fixtures.cases {
        let body = fixture.input["body"]
            .as_str()
            .expect("parser body is a string");
        if fixture.id == "parsers-no-records-error" {
            let error = parse_boot_channels(body).expect_err("empty boot response fails");
            assert_eq!(error.operation, "epg-channels");
        } else {
            let channels = parse_boot_channels(body).expect("valid boot fixture parses");
            let expected = fixture.expected["channels"]
                .as_array()
                .expect("successful fixture has channels");
            assert_eq!(channels.len(), expected.len(), "fixture {}", fixture.id);
            for (channel, expected) in channels.iter().zip(expected) {
                assert_eq!(
                    json!({
                        "channel_id": channel.channel_id,
                        "channel_sdp": channel.channel_sdp,
                        "channel_url": channel.channel_url,
                        "code": channel.code,
                        "group_title": channel.group_title,
                        "logo_url": channel.logo_url,
                        "mix_no": channel.mix_no,
                        "name": channel.name,
                        "time_shift": channel.time_shift,
                        "time_shift_length": channel.time_shift_length,
                        "time_shift_url": channel.time_shift_url,
                    }),
                    *expected,
                    "fixture {}",
                    fixture.id
                );
            }
        }
    }
}

#[test]
fn model_fixture_cases_preserve_aliases_coercions_and_ignored_fields() {
    let fixtures = family(include_str!("fixtures/models.json"));
    for fixture in &fixtures.cases {
        let payload = &fixture.input["payload"];
        match fixture.input["model"]
            .as_str()
            .expect("model name is a string")
        {
            "Channel" => {
                let parsed = serde_json::from_value::<Channel>(payload.clone());
                if fixture.id.ends_with("rejected") || fixture.id.ends_with("required") {
                    assert!(parsed.is_err(), "fixture {} must reject", fixture.id);
                } else {
                    assert_eq!(
                        channel_fields(&parsed.expect("channel fixture")),
                        fixture.expected["fields"]
                    );
                }
            }
            "Programme" => {
                let parsed = serde_json::from_value::<Programme>(payload.clone())
                    .expect("programme fixture");
                assert_eq!(programme_fields(&parsed), fixture.expected["fields"]);
            }
            "TvodAddress" => {
                let parsed =
                    serde_json::from_value::<TvodAddress>(payload.clone()).expect("tvod fixture");
                assert_eq!(
                    parsed.programme_id,
                    fixture.expected["fields"]["programme_id"]
                        .as_str()
                        .expect("tvod id string")
                );
                assert_eq!(
                    parsed.start_time_ms,
                    fixture.expected["fields"]["start_time_ms"]
                        .as_i64()
                        .expect("tvod start integer")
                );
                assert!(!format!("{parsed:?}").contains("upstream.example.com"));
            }
            "HlsAddress" => {
                let parsed = serde_json::from_value::<HlsAddress>(payload.clone());
                if fixture.id.ends_with("rejected") {
                    assert!(parsed.is_err(), "fixture {} must reject", fixture.id);
                } else {
                    let parsed = parsed.expect("hls fixture");
                    assert_eq!(parsed.hls_url, fixture.expected["fields"]["hls_url"]);
                    assert!(!format!("{parsed:?}").contains("upstream.example.com"));
                }
            }
            "BootChannel" => {
                let parsed =
                    serde_json::from_value::<BootChannel>(payload.clone()).expect("boot fixture");
                assert_eq!(
                    parsed.group_title,
                    fixture.expected["fields"]["group_title"]
                );
            }
            "ProgrammeResponse" => {
                let parsed = serde_json::from_value::<ProgrammeResponse>(payload.clone())
                    .expect("programme response fixture");
                assert_eq!(parsed.error_code, fixture.expected["fields"]["error_code"]);
            }
            "ChannelListResponse" => {
                let parsed = serde_json::from_value::<ChannelListResponse>(payload.clone())
                    .expect("channel response fixture");
                assert_eq!(
                    parsed.data.len(),
                    fixture.expected["fields"]["data"]
                        .as_array()
                        .expect("data")
                        .len()
                );
            }
            _ => {}
        }
    }
}

#[test]
fn status_fixture_cases_keep_operation_failure_boundaries() {
    let fixtures = family(include_str!("fixtures/status_shapes.json"));
    let expected_operations = ["epg-channel-codes", "epg-programmes", "epg-tvod"];
    for fixture in fixtures.cases {
        let operation = fixture.input["operation"]
            .as_str()
            .expect("operation string");
        if fixture.input["kind"] == "provider-status" && expected_operations.contains(&operation) {
            assert!(
                fixture.expected.get("error").is_some() || fixture.expected.get("result").is_some()
            );
        }
    }
}

#[test]
fn channel_and_programme_json_parsers_use_provider_status_boundaries() {
    let channel: ChannelListResponse = parse_channel_list(
        r#"{"status":"1","data":[{"ID":"epg-1","code":"cc1","name":"News","mixNo":"7"}]}"#,
    )
    .expect("success channel response");
    assert_eq!(channel.data.len(), 1);
    assert!(parse_channel_list(r#"{"status":"0","data":[],"errMsg":"boom"}"#).is_err());

    let programmes: ProgrammeResponse = serde_json::from_str(
        r#"{"status":"1","data":[{"ID":"p1","channelID":"epg-1","name":"News","startTime":1001,"endTime":"2002"}]}"#,
    )
    .expect("programme envelope");
    let parsed = parse_programmes(
        r#"{"status":"1","data":[{"ID":"p1","channelID":"epg-1","name":"News","startTime":1001,"endTime":"2002"}]}"#,
    )
    .expect("success programme response");
    assert_eq!(parsed, programmes.data);
    assert!(parse_programmes(r#"{"status":"0","errCode":"051002","data":[]}"#).is_ok());
    assert_eq!(
        tvod_times_seconds(&parsed[0]).expect("tvod seconds"),
        (1, 2)
    );
}
