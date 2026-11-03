#![expect(
    missing_docs,
    reason = "custom benchmark entrypoint is executable-only"
)]

mod media_server_support;

fn main() {
    media_server_support::run();
}
