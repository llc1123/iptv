use iptv_media_server::crypto::{ctc_option60, epg_auth};
use serde_json::Value;

#[test]
fn python_crypto_vectors_match_exactly() {
    let option60: Value =
        serde_json::from_str(include_str!("fixtures/option60.json")).expect("option60 fixture");
    for case in option60["cases"].as_array().expect("option60 cases") {
        let input = &case["input"];
        let expected = &case["expected"];
        match input["direction"].as_str().expect("option60 direction") {
            "encode" => {
                let request = ctc_option60::Option60Input {
                    password: input["password"].as_str().expect("password"),
                    login: input["login"].as_str().expect("login"),
                    random: hex::decode(input["r_hex"].as_str().expect("random"))
                        .expect("random hex")
                        .try_into()
                        .expect("random length"),
                    monotonic_ms: hex::decode(input["ts_hex"].as_str().expect("timestamp"))
                        .expect("timestamp hex")
                        .try_into()
                        .expect("timestamp length"),
                };
                let result = ctc_option60::encode(&request);
                if expected.get("error").is_some() {
                    assert!(result.is_err(), "{} should error", case["id"]);
                } else {
                    assert_eq!(
                        hex::encode(result.expect("option60 output")),
                        expected["wire_hex"].as_str().expect("wire hex"),
                        "{}",
                        case["id"]
                    );
                }
            }
            "decode" => {
                let wire =
                    hex::decode(input["wire_hex"].as_str().expect("wire hex")).expect("wire bytes");
                let result = ctc_option60::decode(&wire);
                assert!(result.is_err(), "{} should error", case["id"]);
            }
            direction => panic!("unsupported option60 direction: {direction}"),
        }
    }

    let epg_auth: Value =
        serde_json::from_str(include_str!("fixtures/epg_auth.json")).expect("epg auth fixture");
    for case in epg_auth["cases"].as_array().expect("epg auth cases") {
        let input = &case["input"];
        let expected = &case["expected"];
        let request = epg_auth::AuthenticatorInput {
            encrypt_token: input["encrypt_token"].as_str().expect("token"),
            user_id: input["user_id"].as_str().expect("user id"),
            stb_id: input["stb_id"].as_str().expect("stb id"),
            ip_address: input["ip_address"].as_str().expect("IP address"),
            mac: input["mac"].as_str().expect("MAC"),
            password: input["password"].as_str().expect("password"),
            custom_str: input["custom_str"].as_str().expect("custom string"),
            random_value: input["random_value"]
                .as_u64()
                .expect("random value")
                .try_into()
                .expect("random value range"),
        };
        let result = epg_auth::make_authenticator(&request);
        if expected.get("error").is_some() {
            assert!(result.is_err(), "{} should error", case["id"]);
        } else {
            assert_eq!(
                result.expect("authenticator output"),
                expected["authenticator_hex"]
                    .as_str()
                    .expect("authenticator hex"),
                "{}",
                case["id"]
            );
        }
    }
}
