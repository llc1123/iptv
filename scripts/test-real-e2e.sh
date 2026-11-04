#!/usr/bin/env bash
set -Eeuo pipefail

if [[ -z "${IPTV_ENABLE_REAL_E2E+x}" ]]; then
    printf 'SKIP: set IPTV_ENABLE_REAL_E2E=1 to run the real E2E gate\n'
    exit 0
fi
case "$IPTV_ENABLE_REAL_E2E" in
    "")
        printf 'REFUSE: IPTV_ENABLE_REAL_E2E must equal exactly 1\n' >&2
        exit 2
        ;;
    1) ;;
    *)
        printf 'REFUSE: IPTV_ENABLE_REAL_E2E must equal exactly 1\n' >&2
        exit 2
        ;;
esac

ROOT=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
ENV_FILE=$ROOT/var/iptv-e2e.env
SERVER=$ROOT/rust/target/release/iptv-media-server

refuse() {
    printf 'REFUSE: %s\n' "$1" >&2
    exit 2
}

[[ "$(uname -s)" == Linux ]] || refuse 'real E2E requires Linux'
for tool in grep mktemp setsid stat timeout; do
    command -v "$tool" >/dev/null 2>&1 || refuse "required tool unavailable: $tool"
done

[[ -f "$ENV_FILE" && ! -L "$ENV_FILE" ]] || refuse 'var/iptv-e2e.env must be a regular non-symlink file'
ENV_MODE=$(stat -c '%a' "$ENV_FILE")
ENV_OWNER_READ=$(stat -c '%A' "$ENV_FILE")
[[ "$ENV_OWNER_READ" == -r* ]] || refuse 'var/iptv-e2e.env must be owner-readable'
(( 10#$ENV_MODE % 100 == 0 )) || refuse 'var/iptv-e2e.env must not be group- or world-readable'

for credential in IPTV_DHCP_USER IPTV_DHCP_PASSWORD IPTV_USER_ID IPTV_PASSWORD IPTV_EPG_MAC IPTV_STB_ID_PREFIX IPTV_AUTH_URL; do
    [[ -z "${!credential+x}" ]] || refuse 'credentials must come only from var/iptv-e2e.env'
done

declare -A CONFIG=()
while IFS= read -r line || [[ -n "$line" ]]; do
    [[ -z "$line" ]] && continue
    [[ "$line" =~ ^([A-Z][A-Z0-9_]*)=\'([^\']*)\'$ ]] || refuse 'env file contains a malformed assignment'
    key=${BASH_REMATCH[1]}
    value=${BASH_REMATCH[2]}
    case "$key" in
        IPTV_INTERFACE|IPTV_DHCP_USER|IPTV_DHCP_PASSWORD|IPTV_USER_ID|IPTV_PASSWORD|IPTV_EPG_MAC|IPTV_STB_ID_PREFIX|IPTV_AUTH_URL) ;;
        *) refuse 'env file contains an unknown assignment' ;;
    esac
    [[ -z "${CONFIG[$key]+x}" ]] || refuse 'env file contains a duplicate assignment'
    [[ -n "$value" ]] || refuse 'env file contains an empty value'
    [[ "$value" != *'$'* && "$value" != *'`'* && "$value" != *'\\'* ]] || refuse 'env file contains unsafe expansion syntax'
    case "$value" in
        http://your-eds-host:port/*|https://your-eds-host:port/*)
            refuse 'env file contains a placeholder value'
            ;;
        your-*|YOUR-*|changeme|CHANGEME|example|EXAMPLE|placeholder|PLACEHOLDER|test|TEST)
            refuse 'env file contains a placeholder value'
            ;;
    esac
    CONFIG["$key"]=$value
done < "$ENV_FILE"

for required in IPTV_INTERFACE IPTV_DHCP_USER IPTV_DHCP_PASSWORD IPTV_USER_ID IPTV_PASSWORD IPTV_STB_ID_PREFIX IPTV_AUTH_URL; do
    [[ -n "${CONFIG[$required]+x}" ]] || refuse 'env file is missing a required assignment'
done

RUN_DIR=''
SERVER_PID=''
cleanup_status=0
cleanup() {
    local wait_status
    if [[ -n "$SERVER_PID" ]] && kill -0 "$SERVER_PID" 2>/dev/null; then
        kill -TERM -- "-$SERVER_PID" 2>/dev/null || cleanup_status=1
        if ! timeout --signal=KILL 10s tail --pid="$SERVER_PID" -f /dev/null >/dev/null 2>&1; then
            kill -KILL -- "-$SERVER_PID" 2>/dev/null || cleanup_status=1
            wait "$SERVER_PID" 2>/dev/null || true
        fi
    fi
    if [[ -n "$RUN_DIR" ]]; then
        rm -rf -- "$RUN_DIR" || cleanup_status=1
    fi
    if (( cleanup_status != 0 )); then
        printf 'CLEANUP_FAILURE: unable to clean up real E2E process state\n' >&2
        return 1
    fi
}
report_failure_stage() {
    local line
    [[ -f "$RUN_DIR/server.err" ]] || return 0
    while IFS= read -r line; do
        case "$line" in
            startup_stage=preflight|startup_stage=guide-window|startup_stage=dhcp|startup_stage=epg-client|startup_stage=epg-bootstrap|startup_stage=epg-or-refresh|startup_stage=warm-cache|startup_stage=hls-policy|startup_stage=first-snapshot|startup_stage=serve)
                printf 'STARTUP_STAGE: %s\n' "${line#startup_stage=}" >&2
                ;;
        esac
    done < "$RUN_DIR/server.err"
}
trap cleanup EXIT

[[ -x "$SERVER" ]] || refuse 'Rust server executable is unavailable; startup is not implemented'
[[ -f "$ROOT/rust/src/main.rs" ]] || refuse 'Rust startup source is unavailable'
grep -Fq 'startup is not implemented' "$ROOT/rust/src/main.rs" &&
    refuse 'Rust server startup is not implemented'

RUN_DIR=$(mktemp -d)
chmod 700 "$RUN_DIR"

set +e
ENV_ARGS=(
    "IPTV_INTERFACE=${CONFIG[IPTV_INTERFACE]}"
    "IPTV_DHCP_USER=${CONFIG[IPTV_DHCP_USER]}"
    "IPTV_DHCP_PASSWORD=${CONFIG[IPTV_DHCP_PASSWORD]}"
    "IPTV_USER_ID=${CONFIG[IPTV_USER_ID]}"
    "IPTV_PASSWORD=${CONFIG[IPTV_PASSWORD]}"
    "IPTV_STB_ID_PREFIX=${CONFIG[IPTV_STB_ID_PREFIX]}"
    "IPTV_AUTH_URL=${CONFIG[IPTV_AUTH_URL]}"
)
[[ -n "${CONFIG[IPTV_EPG_MAC]+x}" ]] && ENV_ARGS+=("IPTV_EPG_MAC=${CONFIG[IPTV_EPG_MAC]}")
setsid env -i "${ENV_ARGS[@]}" timeout --signal=TERM --kill-after=10s 300s "$SERVER" \
    --interface "${CONFIG[IPTV_INTERFACE]}" --host 127.0.0.1 --port 8080 \
    >"$RUN_DIR/server.out" 2>"$RUN_DIR/server.err" &
SERVER_PID=$!
wait "$SERVER_PID"
status=$?
set -e

if (( status == 124 )); then
    report_failure_stage
    printf 'TIMEOUT: real E2E server deadline exceeded\n' >&2
    exit 124
elif (( status != 0 )); then
    report_failure_stage
    printf 'FAIL: real E2E server exited unsuccessfully\n' >&2
    exit 1
fi
printf 'PASS: real E2E gate completed\n'
