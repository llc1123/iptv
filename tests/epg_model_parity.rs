#![doc = "Focused JSON parity tests for Python EPG model coercion."]

use iptv_media_server::epg::{Programme, TvodAddress};
use serde_json::{Value, json};

#[test]
fn tvod_address_coerces_numbers_for_every_python_string_field() {
    let parsed: TvodAddress = serde_json::from_value(json!({
        "ID": 123, "channelID": 456, "startTime": 1001, "endTime": 2002, "playURL": 789
    }))
    .expect("numeric TVOD fields accepted");
    assert_eq!(parsed.programme_id, "123");
    assert_eq!(parsed.channel_id, "456");
    assert_eq!(parsed.play_url, "789");
    assert_eq!((parsed.start_time_ms, parsed.end_time_ms), (1001, 2002));
    assert!(
        serde_json::from_value::<TvodAddress>(json!({
            "ID": "", "channelID": "", "startTime": 0, "endTime": 0, "playURL": ""
        }))
        .is_ok()
    );

    for field in ["ID", "channelID", "playURL"] {
        let mut object = json!({
            "ID": "p1", "channelID": "c1", "startTime": 1001, "endTime": 2002, "playURL": "url"
        })
        .as_object()
        .expect("object payload")
        .clone();
        object.insert(field.to_owned(), Value::Null);
        assert!(serde_json::from_value::<TvodAddress>(Value::Object(object)).is_err());
    }
}

#[test]
fn programme_numeric_id_is_rejected_but_string_id_is_valid() {
    let string_id: Programme = serde_json::from_value(json!({
        "ID": "123", "channelID": "c1", "name": "News", "startTime": 1001, "endTime": 2002
    }))
    .expect("string programme ID is valid");
    assert_eq!(string_id.programme_id, "123");
    assert!(
        serde_json::from_value::<Programme>(json!({
            "ID": 123, "channelID": "c1", "name": "News", "startTime": 1001, "endTime": 2002
        }))
        .is_err()
    );
}
