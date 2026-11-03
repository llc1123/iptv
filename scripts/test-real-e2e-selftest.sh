#!/usr/bin/env bash
set -Eeuo pipefail

ROOT=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
GATE=$ROOT/scripts/test-real-e2e.sh
TMP=$(mktemp -d)
trap 'rm -rf -- "$TMP"' EXIT

assert_status() {
    local expected=$1
    shift
    local output status
    set +e
    output=$("$@" 2>&1)
    status=$?
    set -e
    (( status == expected )) || { printf 'FAIL: unexpected status\n' >&2; exit 1; }
    printf '%s\n' "$output"
}

[[ "$(assert_status 0 env -u IPTV_ENABLE_REAL_E2E "$GATE")" == SKIP:* ]] || exit 1
[[ "$(assert_status 2 env IPTV_ENABLE_REAL_E2E=0 "$GATE")" == REFUSE:* ]] || exit 1

mkdir -p "$TMP/scripts" "$TMP/var" "$TMP/rust/src"
cp "$GATE" "$TMP/scripts/test-real-e2e.sh"
chmod 700 "$TMP/scripts/test-real-e2e.sh"
printf '%s\n' 'fn main() {}' >"$TMP/rust/src/main.rs"

run_env_case() {
    local contents=$1
    local expected_text=$2
    local output status
    printf '%s\n' "$contents" >"$TMP/var/iptv-e2e.env"
    chmod 600 "$TMP/var/iptv-e2e.env"
    set +e
    output=$(cd "$TMP" && env -i IPTV_ENABLE_REAL_E2E=1 "$TMP/scripts/test-real-e2e.sh" 2>&1)
    status=$?
    set -e
    (( status == 2 )) || { printf 'FAIL: env case status\n' >&2; exit 1; }
    [[ "$output" == *"$expected_text"* ]] || { printf 'FAIL: env case classification\n' >&2; exit 1; }
    [[ "$output" != *'secret-value'* && "$output" != *"$TMP"* ]] || { printf 'FAIL: output leaked secret or path\n' >&2; exit 1; }
}

run_env_case "IPTV_INTERFACE='lo'" 'missing a required assignment'
run_env_case $'IPTV_INTERFACE=lo' 'malformed assignment'
run_env_case "IPTV_INTERFACE='lo'
IPTV_UNKNOWN='value'" 'unknown assignment'
run_env_case "IPTV_INTERFACE='lo'
IPTV_INTERFACE='lo'" 'duplicate assignment'
run_env_case "IPTV_INTERFACE='\$(touch $TMP/marker)'" 'unsafe expansion syntax'
run_env_case "IPTV_INTERFACE='lo'
IPTV_DHCP_USER='secret-value'" 'missing a required assignment'

[[ ! -e "$TMP/marker" ]] || { printf 'FAIL: command substitution executed\n' >&2; exit 1; }

cat >"$TMP/var/iptv-e2e.env" <<'EOF'
IPTV_INTERFACE='lo'
IPTV_DHCP_USER='dhcp-user'
IPTV_DHCP_PASSWORD='dhcp-password'
IPTV_USER_ID='user-id'
IPTV_PASSWORD='epg-password'
IPTV_STB_ID_PREFIX='stb-prefix'
IPTV_AUTH_URL='https://provider.example/auth'
EOF
chmod 600 "$TMP/var/iptv-e2e.env"
set +e
output=$(cd "$TMP" && env -i IPTV_ENABLE_REAL_E2E=1 IPTV_PASSWORD=secret-value "$TMP/scripts/test-real-e2e.sh" 2>&1)
status=$?
set -e
(( status == 2 )) || { printf 'FAIL: caller credential status\n' >&2; exit 1; }
[[ "$output" == *'credentials must come only from var/iptv-e2e.env'* ]] || { printf 'FAIL: caller credential classification\n' >&2; exit 1; }
[[ "$output" != *'secret-value'* && "$output" != *"$TMP"* ]] || { printf 'FAIL: caller credential leaked\n' >&2; exit 1; }
printf 'PASS: real E2E gate self-tests completed\n'
