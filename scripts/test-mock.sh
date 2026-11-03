#!/usr/bin/env bash
set -Eeuo pipefail

ROOT=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
cd "$ROOT"
LOG=$(mktemp)
trap 'rm -f "$LOG"' EXIT

[[ -f rust/Cargo.lock ]] || { printf 'FAIL: required lockfile is missing: rust/Cargo.lock\n' >&2; exit 1; }
[[ -d rust/tests ]] || { printf 'FAIL: required Rust test directory is missing: rust/tests\n' >&2; exit 1; }
command -v rustup >/dev/null 2>&1 || { printf 'FAIL: required tool is missing: rustup (Rust 1.87.0 is not available)\n' >&2; exit 1; }

printf '+ rustup run 1.87.0 cargo test --offline --manifest-path rust/Cargo.toml --locked --tests --bins --examples --all-features\n'
if rustup run 1.87.0 cargo test --offline --manifest-path rust/Cargo.toml --locked --tests --bins --examples --all-features >"$LOG" 2>&1; then
    printf 'PASS: offline Rust mock suite completed\n'
else
    status=$?
    printf 'FAIL: offline Rust mock suite exited %d; diagnostics follow\n' "$status" >&2
    while IFS= read -r line; do
        line=$(printf '%s\n' "$line" | sed -E \
            -e "s|$ROOT|<repo>|g" \
            -e 's#https?://[^[:space:]<>]+#<redacted-url>#g' \
            -e 's#([0-9A-Fa-f]{2}:){5}[0-9A-Fa-f]{2}#<redacted-mac>#g' \
            -e 's#(^|[^[:alnum:]_])(10|172\.(1[6-9]|2[0-9]|3[01])|192\.168|127)\.[0-9.]+#\1<redacted-address>#g' \
            -e 's#(^|[[:space:]:=])(token|password|secret|credential)([[:space:]:=/-]+)[^[:space:]<>]+#\1\2\3<redacted>#Ig')
        case "$line" in
            */*) printf '<redacted-path-diagnostic>\n' >&2 ;;
            *) printf '%s\n' "$line" >&2 ;;
        esac
    done < "$LOG"
    exit "$status"
fi
