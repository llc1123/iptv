#!/usr/bin/env bash
set -Eeuo pipefail

ROOT=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
cd "$ROOT"

case "${IPTV_ENABLE_MOCK_BENCH+x}:${IPTV_ENABLE_MOCK_BENCH-}" in
    :) printf 'SKIP: IPTV_ENABLE_MOCK_BENCH is unset\n'; exit 0 ;;
    x:1) ;;
    *) printf 'REFUSE: IPTV_ENABLE_MOCK_BENCH must equal 1\n'; exit 2 ;;
esac

command -v rustup >/dev/null 2>&1 || { printf 'REFUSE: rustup unavailable\n'; exit 2; }
command -v python3 >/dev/null 2>&1 || { printf 'REFUSE: python3 unavailable\n'; exit 2; }
command -v timeout >/dev/null 2>&1 || { printf 'REFUSE: timeout unavailable\n'; exit 2; }
mkdir -p var/bench
tmp=$(mktemp -d)
chmod 700 "$tmp"
report_tmp="$tmp/report.json"
output='var/bench/mock-report.json'
cleanup_failed=0
cleanup() {
    if ! rm -rf -- "$tmp"; then cleanup_failed=1; fi
}
trap cleanup EXIT

set +e
timeout --signal=TERM --kill-after=5s 90s rustup run 1.87.0 cargo bench --offline --locked --manifest-path rust/Cargo.toml --bench media_server -- --assert --offline >"$report_tmp" 2>/dev/null
status=$?
set -e

classification=FAIL
if [[ "$status" -eq 124 || "$status" -eq 137 ]]; then classification=TIMEOUT; fi
if [[ "$status" -eq 0 ]]; then classification=PASS; fi
if [[ "$classification" == PASS ]]; then
    if ! python3 -c 'import json,sys; value=json.load(open(sys.argv[1])); raise SystemExit(0 if value.get("status") == "PASS" and value.get("schema_version") == 1 else 1)' "$report_tmp"; then classification=FAIL; fi
fi
if ! printf '{"schema_version":1,"classification":"%s"}\n' "$classification" >"$tmp/wrapper.json"; then classification=CLEANUP_FAILURE; fi
if [[ "$classification" == PASS ]]; then cp "$report_tmp" "$tmp/final.json"; else cp "$tmp/wrapper.json" "$tmp/final.json"; fi
if ! mv -f -- "$tmp/final.json" "$output"; then classification=CLEANUP_FAILURE; fi
if ! rm -rf -- "$tmp"; then classification=CLEANUP_FAILURE; fi
trap - EXIT
printf '%s\n' "$classification"
case "$classification" in PASS) exit 0 ;; TIMEOUT) exit 124 ;; CLEANUP_FAILURE) exit 1 ;; *) exit 1 ;; esac
