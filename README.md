# IPTV media server

This project provides a small Python media server for an IPoE IPTV
deployment. It authenticates against the EPG service, refreshes channel and
programme metadata, serves M3U/XMLTV data, and proxies live HLS and
time-shift/replay streams.

The repository does not ship credentials, account identifiers, device
addresses, capture files, APKs, or deployment-specific endpoints. Supply
those values at runtime for an authorized service.

## Install

```bash
uv sync
```

The public console commands are:

```text
iptv-server
iptv-logo-catalog
```

## Configure credentials and deployment values

Set these environment variables before starting the server. The values below
are placeholders and must be replaced with credentials for your own account.

```bash
export IPTV_DHCP_USER='your-dhcp-user'
export IPTV_DHCP_PASSWORD='your-dhcp-password'
export IPTV_USER_ID='your-epg-user-id'
export IPTV_PASSWORD='your-epg-password'
export IPTV_AUTH_URL='http://your-eds-host:port/EDS/jsp/AuthenticationURL'
export IPTV_STB_ID_PREFIX='your-stb-id-prefix'
export IPTV_INTERFACE='eth0'
```

Optional settings:

| Setting | Purpose |
| --- | --- |
| `IPTV_EPG_MAC` | Authorized set-top-box MAC used for EPG authentication; defaults to the selected interface MAC. |

The DHCP implementation uses the selected interface MAC as the Ethernet
source. It only places the configured set-top-box identity in BOOTP fields;
it does not spoof the Ethernet source address.

## Run the media server

```bash
sudo -E uv run iptv-server \
  --interface "$IPTV_INTERFACE" \
  --logo-catalog var/logo-catalog/catalog.json \
  --host 127.0.0.1 \
  --port 8080
```

The server exposes:

| URL | Purpose |
| --- | --- |
| `/channels.m3u` | Live-channel M3U configuration |
| `/xmltv.xml` | XMLTV channel and programme metadata |
| `/stream/<channel-id>.m3u8` | Live HLS entry point |
| `/replay/<channel-id>/<programme-id>.m3u8` | Replay HLS entry point |
| `/catchup/<channel-id>.m3u8` | Catch-up HLS entry point |
| `/logos/<channel-id>` | Locally cached channel logo |
| `POST /metadata/sync` | Refresh channel metadata and logos |
| `/healthz` | Service status |

The service keeps upstream HLS URLs in memory and rewrites playlists through
its own short-lived proxy routes. It does not persist upstream authentication
parameters.

## Build the optional logo catalog

The catalog is generated from a live channel response and is intentionally a
runtime artifact. Do not commit it or any downloaded logos:

```bash
sudo -E uv run iptv-logo-catalog \
  --interface "$IPTV_INTERFACE" \
  --output var/logo-catalog/catalog.json
```

## Public-release hygiene

The repository ignores local captures, device dumps, APKs, native libraries,
logs, keystores, generated catalogs, reverse-engineering workspaces, and
runtime caches. Review the ignore rules before adding files with `git add -A`.

Only publish source and configuration templates. Never publish account
credentials, authentication tokens, signed playback URLs, DHCP captures,
device MAC addresses, private network addresses, or files copied from a
subscriber device.

## Rust local verification and release preflight

The Rust package is pinned to Rust 1.87.0 in `rust-toolchain.toml`. Prepare a
host explicitly; verification never installs tools, resolves dependencies, or
changes network state:

The raw DHCP implementation pins `dhcproto` to 0.14.0 because the 0.15.0
dependency graph requires Rust 1.88. DHCP option decisions remain in the
compatibility parser: unlike `dhcproto`'s map decoder, it preserves the
Python client's first-occurrence behavior for duplicate options.

```bash
rustup toolchain install 1.87.0 --profile minimal --component rustfmt --component clippy
uv sync --offline
cc --version
ld --version
```

The install command is preparation only. `bash scripts/verify.sh` refuses a
missing lockfile, generator, fixture directory, pinned toolchain, or ignored
Rust target directory. From any working directory it returns to the repository
root and runs these gates in order: pinned rustfmt check; locked Cargo check;
locked all-target/all-feature tests; strict all-target/all-feature clippy with
warnings denied; two offline `uv` generator checks with stable fixture hashes;
and a locked release build. The generator is run with `--check`, so tracked
fixtures are never rewritten. `rust/Cargo.lock` is committed and must remain
tracked.

For the Rust test suite alone, use the offline mock entrypoint:

```bash
bash scripts/test-mock.sh
```

It runs `rustup run 1.87.0 cargo test --offline --manifest-path
rust/Cargo.toml --locked --all-targets --all-features`. It does not start a
provider, contact the network, install packages, or require DHCP capabilities.

## Opt-in Rust mock benchmark

The repeatable mock benchmark is separate from verification and tests. It uses
`AppState`, `build_router`, the snapshot render cache, playlist rewriting, and
HLS token registration through `tower::ServiceExt::oneshot`; it opens no
sockets and contacts no provider. The workload is exactly 1,000 streams,
1,000 channels, 1,000 programmes, 5,000 bounded in-memory token resources,
and 4,000,000 logical bits per second. The 10-second warmup and 60-second
measurement window are workload metadata; the benchmark issues the exact
120,000 XMLTV requests without sleeping, so host scheduling does not add a
fixed wait to local verification.

Run it only when the opt-in is explicit:

```bash
IPTV_ENABLE_MOCK_BENCH=1 bash scripts/bench-mock.sh
```

The wrapper writes the redacted machine-readable report to
`var/bench/mock-report.json` atomically. Its stdout classification is `PASS`,
`SKIP`, `REFUSE`, `FAIL`, `TIMEOUT`, or `CLEANUP_FAILURE`; unset opt-in is
`SKIP`, and any value other than `1` is `REFUSE` with exit 2. The report schema
is version 1 and contains fixed workload counts, SHA-256 digests of bounded
input/output bodies, sorted integer-nanosecond p50/p95/p99/max timing values,
request/error/disconnect counts, `/proc/self/status` RSS observations, and
stable failure codes. It excludes timestamps, PIDs, paths, URLs, tokens,
credentials, and raw diagnostics. Timing and RSS thresholds are host-sensitive
and the bitrate is logical only; no physical media is generated.

## Opt-in real E2E gate

The real E2E gate is deliberately excluded from `scripts/verify.sh` and
`scripts/test-mock.sh`. It is an explicit, non-default gate for a future
working Rust server only:

```bash
IPTV_ENABLE_REAL_E2E=1 bash scripts/test-real-e2e.sh
```

When enabled, the gate requires Linux, the required local tooling, and a
regular owner-readable `var/iptv-e2e.env` file with mode `600` (or stricter).
That file is parsed as quoted data, never sourced. Only the allowlisted IPTV
assignments are accepted; duplicate, unknown, malformed, empty, placeholder,
unsafe-expansion, and caller-provided credential values are refused. Values
are never printed. The gate binds the future server to localhost, uses an
isolated mode-`700` temporary directory, passes only an explicit environment
allowlist through `env -i`, applies a fixed 300-second deadline, and cleans up
the process group with one grace/escalation path. It does not use `sudo`,
`ip`, `unshare`, a fixed external port, raw DHCP, or a live provider.

The current Rust `main` is still a scaffold without startup, so an enabled
invocation returns `REFUSE` before creating mutable network state. Outcomes are
`SKIP` (unset opt-in), `REFUSE` (unsafe, unsupported, or unavailable
prerequisite), `FAIL` (server failure), `TIMEOUT` (the 300-second deadline),
and `CLEANUP_FAILURE` (cleanup could not complete). Run the offline parser and
classification checks with:

```bash
bash scripts/test-real-e2e-selftest.sh
```

Release hygiene is local and explicit: review `git status --short` and
`git diff --check` before staging; publish source, templates, the tracked
`rust/Cargo.lock`, and deterministic fixtures only. Keep `rust/target/`, local
evidence, captures, credentials, tokens, signed playback URLs, device MACs,
private addresses, and generated runtime output untracked. The verification
scripts print commands and pass/fail results, but redact command diagnostics
that contain URLs, addresses, MACs, tokens, credentials, or repository paths.

The current Rust package is a scaffold. Local checks cover compilation,
formatting, lints, deterministic fixture parity, and offline tests; they do not
prove live DHCP, provider compatibility, HLS serving, capability admission, or
production deployment. The eventual live DHCP server requires
`CAP_NET_RAW` and `CAP_NET_ADMIN`, and its startup preflight must verify those
before transmitting packets or changing interface state. Do not grant these
capabilities merely to run local tests.
