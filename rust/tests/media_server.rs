use clap::Parser;
use iptv_media_server::config::Config;

#[test]
fn cli_keeps_operator_flag_names() {
    let config = Config::try_parse_from([
        "iptv-media-server",
        "--dhcp-user",
        "dhcp",
        "--dhcp-password",
        "secret",
        "--user-id",
        "epg",
        "--password",
        "password",
        "--port",
        "8080",
    ])
    .expect("valid operator configuration");
    assert_eq!(config.port, 8080);
    assert_eq!(config.epg_user.as_str(), "epg");
}
