
use super::*;

use crate::epg::models::{BootChannel, Programme};

fn fixture() -> serde_json::Value {
    serde_json::from_str(include_str!("../tests/fixtures/m3u_xmltv.json"))
        .expect("tracked render fixture is valid JSON")
}

fn zone() -> GuideZone {
    GuideZone::Fixed {
        offset_minutes: 480,
    }
}

#[test]
fn every_render_fixture_matches_python_bytes() {
    let corpus = fixture();
    let cases = corpus["cases"].as_array().expect("fixture cases");
    for case in cases {
        let input = &case["input"];
        let channels: Vec<BootChannel> =
            serde_json::from_value(input["channels"].clone()).expect("fixture channels");
        let programmes: Vec<Programme> =
            serde_json::from_value(input["programmes"].clone()).expect("fixture programmes");
        let base = BaseUrl::parse(input["base_url"].as_str().expect("fixture base"))
            .expect("fixture base is valid");
        let renderer = input["renderer"].as_str().expect("fixture renderer");
        let expected = case["expected"]["text"].as_str().expect("fixture expected");
        let actual = match renderer {
            "m3u" => String::from_utf8(
                render_m3u(
                    &channels,
                    &base,
                    u16::try_from(input["catchup_days"].as_u64().expect("fixture days"))
                        .expect("fixture days fit"),
                )
                .expect("fixture m3u")
                .clone(),
            )
            .expect("fixture m3u UTF-8"),
            "xmltv" => String::from_utf8(
                render_xmltv(&channels, &programmes, &zone(), &base)
                    .expect("fixture xmltv")
                    .clone(),
            )
            .expect("fixture xmltv UTF-8"),
            other => panic!("unknown renderer {other}"),
        };
        assert_eq!(actual, expected, "fixture {}", case["id"]);
    }
}

#[test]
fn empty_feeds_keep_their_protocol_terminators() {
    let base = BaseUrl::parse("http://192.0.2.1:8080").expect("valid base");
    assert_eq!(
        render_m3u(&[], &base, 0).expect("empty m3u"),
        b"#EXTM3U x-tvg-url=\"http://192.0.2.1:8080/xmltv.xml\"\n"
    );
    assert_eq!(
        render_xmltv(&[], &[], &zone(), &base).expect("empty xmltv"),
        b"<?xml version='1.0' encoding='utf-8'?>\n<tv generator-info-name=\"iptv-media-server\" />"
    );
}

#[test]
fn base_validation_preserves_exact_key_and_rejects_header_input() {
    let base = BaseUrl::parse("http://example.test/").expect("valid trailing slash");
    assert_eq!(base.as_str(), "http://example.test/");
    assert_eq!(format!("{base:?}"), "BaseUrl(\"<redacted>\")");
    assert!(!format!("{base:?}").contains("example.test"));
    assert_eq!(
        BaseUrl::parse("X-Forwarded-Host: example.test"),
        Err(RenderError::InvalidBaseUrl)
    );
    assert_eq!(
        BaseUrl::parse("http://example.test/path"),
        Err(RenderError::InvalidBaseUrl)
    );
    assert_eq!(
        BaseUrl::parse("http://example.test:bad"),
        Err(RenderError::InvalidBaseUrl)
    );
    assert_eq!(
        BaseUrl::parse("http://example.test:0"),
        Err(RenderError::InvalidBaseUrl)
    );
    for value in [
        "http://example.test?token=secret",
        "http://example.test#fragment",
        "http://example.test/<xml>",
        "http://exa&mple.test",
        "http://-example.test",
        "http://example..test",
        "http://[not-ipv6]",
        "http://example.test:65536",
    ] {
        assert_eq!(
            BaseUrl::parse(value),
            Err(RenderError::InvalidBaseUrl),
            "{value} must not be accepted"
        );
    }
}

#[test]
fn path_components_use_uppercase_utf8_percent_encoding() {
    let base = BaseUrl::parse("http://example.test").expect("valid base");
    let channel = BootChannel {
        channel_id: "é /".to_owned(),
        name: "n".to_owned(),
        mix_no: String::new(),
        channel_url: String::new(),
        time_shift_url: String::new(),
        channel_sdp: String::new(),
        time_shift: String::new(),
        time_shift_length: String::new(),
        logo_url: String::new(),
        code: String::new(),
        group_title: String::new(),
    };
    let output = render_m3u(&[channel], &base, 0).expect("render");
    assert!(
        String::from_utf8(output)
            .expect("UTF-8")
            .contains("%C3%A9%20%2F")
    );
}

#[test]
fn exact_limit_is_accepted_and_one_more_byte_is_rejected() {
    let base = BaseUrl::parse("http://example.test").expect("valid base");
    let mut channel = BootChannel {
        channel_id: "c".to_owned(),
        name: String::new(),
        mix_no: String::new(),
        channel_url: String::new(),
        time_shift_url: String::new(),
        channel_sdp: String::new(),
        time_shift: String::new(),
        time_shift_length: String::new(),
        logo_url: "l".to_owned(),
        code: String::new(),
        group_title: String::new(),
    };
    let baseline = render_m3u(&[channel.clone()], &base, 0)
        .expect("baseline")
        .len();
    let name_len = (MAX_RENDER_BYTES - baseline) / 2;
    channel.name = "x".repeat(name_len);
    let mut exact = render_m3u(&[channel.clone()], &base, 0).expect("exact limit");
    if exact.len() == MAX_RENDER_BYTES - 1 {
        channel.logo_url.push('x');
        exact = render_m3u(&[channel.clone()], &base, 0).expect("exact limit");
    }
    assert_eq!(exact.len(), MAX_RENDER_BYTES);
    channel.logo_url.push('x');
    assert_eq!(render_m3u(&[channel], &base, 0), Err(RenderError::Oversize));
}

#[test]
fn line_controls_are_rejected_and_xml_attributes_use_elementtree_escapes() {
    let base = BaseUrl::parse("http://example.test").expect("valid base");
    let mut line_channel = BootChannel {
        channel_id: "c1".to_owned(),
        name: "bad\nname".to_owned(),
        mix_no: String::new(),
        channel_url: String::new(),
        time_shift_url: String::new(),
        channel_sdp: String::new(),
        time_shift: String::new(),
        time_shift_length: String::new(),
        logo_url: String::new(),
        code: String::new(),
        group_title: String::new(),
    };
    assert_eq!(
        render_m3u(&[line_channel.clone()], &base, 0),
        Err(RenderError::InvalidField)
    );
    line_channel.name = "ok".to_owned();
    line_channel.logo_url = "a\"\r\n\t&<>".to_owned();
    let xml = String::from_utf8(
        render_xmltv(&[line_channel], &[], &zone(), &base).expect("xml attributes"),
    )
    .expect("UTF-8");
    assert!(xml.contains("a&quot;&#13;&#10;&#09;&amp;&lt;&gt;"));
}
