#!/usr/bin/env bash
# Local gate: Rust fmt/clippy/tests/release build plus the Python oracle
# fixture check. Runs offline; prepare the host with `cargo fetch` and
# `uv sync` first.
set -Eeuo pipefail

cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.."

run() {
    printf '+ %s\n' "$*"
    "$@"
}

cargo=(rustup run 1.87.0 cargo)
manifest=(--offline --locked)

run "${cargo[@]}" fmt --all -- --check
run "${cargo[@]}" clippy "${manifest[@]}" --all-targets --all-features -- -D warnings
run "${cargo[@]}" test "${manifest[@]}" --tests --bins --all-features
run uv run --offline --no-sync python tools/oracle/generate.py --check --output-dir tests/fixtures
run "${cargo[@]}" build "${manifest[@]}" --release --bin iptv-media-server
printf 'PASS: local verification completed\n'
