#!/usr/bin/env bash
# Opt-in live gate: starts the release binary against the real provider using
# credentials from var/iptv-e2e.env, waits for /healthz, then fetches both
# feeds. Stop any other server on the interface first. Extra arguments are
# passed to the server, e.g. `--guide-cache /tmp/guide.json`.
set -Eeuo pipefail

if [[ "${IPTV_ENABLE_REAL_E2E:-}" != 1 ]]; then
    printf 'SKIP: set IPTV_ENABLE_REAL_E2E=1 to run the real E2E gate\n'
    exit 0
fi

ROOT=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
ENV_FILE=$ROOT/var/iptv-e2e.env
SERVER=$ROOT/rust/target/release/iptv-media-server
PORT=${IPTV_E2E_PORT:-8080}

[[ -x "$SERVER" ]] || { printf 'REFUSE: build %s first\n' "${SERVER#"$ROOT"/}" >&2; exit 2; }
[[ -f "$ENV_FILE" ]] || { printf 'REFUSE: var/iptv-e2e.env is missing\n' >&2; exit 2; }
[[ $(stat -c '%a' "$ENV_FILE") == 600 ]] || { printf 'REFUSE: var/iptv-e2e.env must be mode 600\n' >&2; exit 2; }

LOG=$(mktemp)
SERVER_PID=''
cleanup() {
    if [[ -n "$SERVER_PID" ]]; then
        kill -TERM "$SERVER_PID" 2>/dev/null || true
        wait "$SERVER_PID" 2>/dev/null || true
    fi
    rm -f -- "$LOG"
}
trap cleanup EXIT

(
    set -a
    # shellcheck disable=SC1090
    . "$ENV_FILE"
    set +a
    exec "$SERVER" --interface "$IPTV_INTERFACE" --host 127.0.0.1 --port "$PORT" "$@"
) >"$LOG" 2>&1 &
SERVER_PID=$!

base=http://127.0.0.1:$PORT
for _ in $(seq 1 300); do
    if ! kill -0 "$SERVER_PID" 2>/dev/null; then
        printf 'FAIL: server exited during startup; last log lines:\n' >&2
        tail -n 20 "$LOG" >&2
        exit 1
    fi
    if curl -fsS --max-time 2 "$base/healthz" >/dev/null 2>&1; then
        curl -fsS --max-time 30 "$base/channels.m3u" >/dev/null
        curl -fsS --max-time 30 "$base/xmltv.xml" >/dev/null
        printf 'PASS: real E2E gate completed\n'
        exit 0
    fi
    sleep 1
done
printf 'TIMEOUT: server was not ready within 300 seconds\n' >&2
exit 124
