#!/usr/bin/env bash
set -Eeuo pipefail

ROOT=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
MODE=check
case "${1:-}" in
    "") ;;
    --check) MODE=check ;;
    --occupied-state) MODE=occupied ;;
    *)
        printf 'REFUSE: unknown migration dry-run option\n' >&2
        exit 2
        ;;
esac

if [[ "$MODE" == occupied ]]; then
    printf 'REFUSE: migration lock is occupied; no cutover attempted\n' >&2
    exit 2
fi

required=(
    "$ROOT/rust/Cargo.lock"
    "$ROOT/scripts/verify.sh"
    "$ROOT/scripts/test-mock.sh"
    "$ROOT/scripts/e2e-netns.sh"
    "$ROOT/scripts/e2e-real.sh"
    "$ROOT/systemd/iptv-media-server.service"
)
for path in "${required[@]}"; do
    [[ -e "$path" ]] || {
        printf 'REFUSE: migration prerequisite is missing\n' >&2
        exit 2
    }
done

grep -Fq 'CAP_NET_RAW CAP_NET_ADMIN' "$ROOT/systemd/iptv-media-server.service" || {
    printf 'REFUSE: systemd capability preflight is incomplete\n' >&2
    exit 2
}
grep -Fq 'IPTV_RUN_REAL_E2E' "$ROOT/scripts/e2e-real.sh" || {
    printf 'REFUSE: real E2E gate adapter is incomplete\n' >&2
    exit 2
}

printf 'PASS: migration dry-run complete; no process, interface, route, or state was changed\n'
