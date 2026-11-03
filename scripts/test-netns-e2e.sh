#!/usr/bin/env bash
set -Eeuo pipefail

case "${IPTV_ENABLE_NETNS_E2E:-}" in
    "")
        printf 'SKIP: set IPTV_ENABLE_NETNS_E2E=1 to run the opt-in namespace policy test\n'
        exit 0
        ;;
    1) ;;
    *)
        printf 'REFUSE: IPTV_ENABLE_NETNS_E2E must equal exactly 1\n' >&2
        exit 2
        ;;
esac

if [[ "$(uname -s)" != "Linux" ]]; then
    printf 'REFUSE: network namespace e2e requires Linux\n' >&2
    exit 2
fi
if ! command -v unshare >/dev/null 2>&1; then
    printf 'SKIP: required tool unavailable: unshare\n'
    exit 0
fi
if ! command -v ip >/dev/null 2>&1; then
    printf 'SKIP: required tool unavailable: ip\n'
    exit 0
fi

ROOT=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
cd "$ROOT"
printf 'RUN: anonymous user and network namespace focused policy test\n'
exec unshare --user --map-root-user --net --mount-proc -- bash -c '
    set -Eeuo pipefail
    ip link set lo up
    rustup run 1.87.0 cargo test --offline --manifest-path rust/Cargo.toml --locked --test media_server_integration public_routes_cover_feeds_playback_tokens_head_405_404_and_shutdown -- --test-threads=1
'
