#!/usr/bin/env bash
set -Eeuo pipefail

ROOT=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
cd "$ROOT"
LOG=$(mktemp)
trap 'rm -f "$LOG"' EXIT

print_command() {
    printf '+ '
    printf '%q ' "$@"
    printf '\n'
}

redact_output() {
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
}

fail() {
    printf 'FAIL: %s\n' "$1" >&2
    exit 1
}

run() {
    print_command "$@"
    if "$@" >"$LOG" 2>&1; then
        printf 'PASS: command completed\n\n'
        return 0
    else
        local status=$?
        printf 'FAIL: command exited %d; diagnostics follow\n' "$status" >&2
        redact_output
        exit "$status"
    fi
}

[[ -f rust/Cargo.lock ]] || fail 'required lockfile is missing: rust/Cargo.lock'
git ls-files --error-unmatch rust/Cargo.lock >/dev/null 2>&1 || fail 'required lockfile is not tracked: rust/Cargo.lock'
[[ -f tools/oracle/generate.py ]] || fail 'required fixture generator is missing: tools/oracle/generate.py'
[[ -d rust/tests/fixtures ]] || fail 'required fixture directory is missing: rust/tests/fixtures'

git check-ignore --no-index -q rust/target/ || fail 'Rust build output is not ignored: rust/target/'

command -v rustup >/dev/null 2>&1 || fail 'required tool is missing: rustup (Rust 1.87.0 is not available)'
command -v uv >/dev/null 2>&1 || fail 'required tool is missing: uv (offline fixture verification cannot run)'

run rustup run 1.87.0 rustc --version
run rustup run 1.87.0 rustfmt --version
run rustup run 1.87.0 cargo --offline --locked clippy --version

run rustup run 1.87.0 cargo --offline --locked fmt --manifest-path rust/Cargo.toml --all -- --check
run rustup run 1.87.0 cargo --offline check --manifest-path rust/Cargo.toml --locked
run rustup run 1.87.0 cargo --offline test --manifest-path rust/Cargo.toml --locked --all-targets --all-features
run rustup run 1.87.0 cargo --offline --locked clippy --manifest-path rust/Cargo.toml --all-targets --all-features -- -D warnings

run uv run --offline --no-sync python tools/oracle/generate.py --check --output-dir rust/tests/fixtures
fixture_hash() {
    git ls-files -z rust/tests/fixtures \
        | sort -z \
        | while IFS= read -r -d '' path; do sha256sum "$path"; done \
        | sha256sum \
        | cut -d ' ' -f 1
}

FIXTURE_HASH_BEFORE=$(fixture_hash)
run uv run --offline --no-sync python tools/oracle/generate.py --check --output-dir rust/tests/fixtures
FIXTURE_HASH_AFTER=$(fixture_hash)
[[ "$FIXTURE_HASH_BEFORE" == "$FIXTURE_HASH_AFTER" ]] || fail 'fixture hashes changed between deterministic checks; inspect rust/tests/fixtures'
printf 'PASS: fixture hashes stable across consecutive checks\n\n'

run rustup run 1.87.0 cargo --offline build --manifest-path rust/Cargo.toml --locked --release --bin iptv-media-server
printf 'PASS: local Rust verification completed\n'
