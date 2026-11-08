//! Live check that a renewal keeps the address the client already holds.
//!
//! Ignored by default: it needs the IPTV interface, root (raw sockets), and the
//! provider's credentials. Run it only when no other server is using the
//! interface:
//!
//! ```sh
//! set -a; . var/iptv-e2e.env; set +a
//! cargo test --manifest-path rust/Cargo.toml --test renewal_live -- --ignored --nocapture
//! ```

use iptv_media_server::dhcp::{ClientConfig, acquire_lease, renew_lease};
use std::env;

#[test]
#[ignore = "live: needs the IPTV interface, raw sockets and provider credentials"]
fn renewal_keeps_the_address_the_provider_leased() {
    let interface = env::var("IPTV_INTERFACE").expect("IPTV_INTERFACE");
    let user = env::var("IPTV_DHCP_USER").expect("IPTV_DHCP_USER");
    let password = env::var("IPTV_DHCP_PASSWORD").expect("IPTV_DHCP_PASSWORD");
    let prefix = env::var("IPTV_STB_ID_PREFIX").unwrap_or_default();
    let client = ClientConfig {
        user: &user,
        password: &password,
        stb_id_prefix: &prefix,
    };
    let (held, _) = acquire_lease(&interface, &client).expect("acquire");
    let renewed = renew_lease(&interface, &client, &held).expect("renew");
    assert_eq!(
        renewed.address, held.address,
        "renewal moved the client off its own lease"
    );
    assert_eq!(renewed.lease_seconds, held.lease_seconds);
}
