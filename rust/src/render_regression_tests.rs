
use super::*;

fn zone() -> GuideZone {
    GuideZone::Fixed {
        offset_minutes: 480,
    }
}

fn channel(id: &str, name: &str) -> BootChannel {
    BootChannel {
        channel_id: id.to_owned(),
        name: name.to_owned(),
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

#[test]
fn xmltv_empty_text_uses_self_closing_display_name_and_title() {
    let base = BaseUrl::parse("http://example.test").expect("valid base");
    let programme = Programme {
        title: String::new(),
        start_time_ms: 0,
        end_time_ms: 1,
        programme_id: String::new(),
        channel_id: "c1".to_owned(),
    };

    let xml = String::from_utf8(
        render_xmltv(&[channel("c1", "")], &[programme], &zone(), &base).expect("empty XML text"),
    )
    .expect("UTF-8");

    assert!(xml.contains("<display-name />"));
    assert!(xml.contains("<title />"));
    assert!(!xml.contains("<display-name></display-name>"));
    assert!(!xml.contains("<title></title>"));
}

#[test]
fn direct_xmltv_render_sorts_dedupes_and_skips_unknown_programmes() {
    let base = BaseUrl::parse("http://example.test").expect("valid base");
    let channels = [channel("c2", "Two"), channel("c1", "One")];
    let programmes = [
        Programme {
            title: "late".to_owned(),
            start_time_ms: 20,
            end_time_ms: 30,
            programme_id: "late-id".to_owned(),
            channel_id: "c1".to_owned(),
        },
        Programme {
            title: "two".to_owned(),
            start_time_ms: 10,
            end_time_ms: 20,
            programme_id: "two-id".to_owned(),
            channel_id: "c2".to_owned(),
        },
        Programme {
            title: "early".to_owned(),
            start_time_ms: 10,
            end_time_ms: 20,
            programme_id: "early-id".to_owned(),
            channel_id: "c1".to_owned(),
        },
        Programme {
            title: "early".to_owned(),
            start_time_ms: 10,
            end_time_ms: 20,
            programme_id: "duplicate-id".to_owned(),
            channel_id: "c1".to_owned(),
        },
        Programme {
            title: "ghost".to_owned(),
            start_time_ms: 0,
            end_time_ms: 1,
            programme_id: "ghost-id".to_owned(),
            channel_id: "missing".to_owned(),
        },
    ];

    let xml = String::from_utf8(
        render_xmltv(&channels, &programmes, &zone(), &base).expect("direct XMLTV render"),
    )
    .expect("UTF-8");
    let early = xml.find("<title>early</title>").expect("early programme");
    let late = xml.find("<title>late</title>").expect("late programme");
    let two = xml
        .find("<title>two</title>")
        .expect("second channel programme");

    assert!(early < late);
    assert!(late < two);
    assert_eq!(xml.matches("<programme ").count(), 3);
    assert_eq!(xml.matches("<title>early</title>").count(), 1);
    assert!(!xml.contains("<title>ghost</title>"));
}
