# AGENTS.md

Guidance for coding agents and contributors working in this repository.

## What this is

An IPTV media server for a provider network that authenticates every DHCP
exchange (CTC Option 60) and every EPG session. It acquires a lease on the
IPTV interface, logs in to the provider's EPG, and serves an M3U playlist, an
XMLTV guide, channel logos, and an HLS proxy for live, replay and catch-up
playback.

Two implementations live side by side:

- `iptv_client/` — the original Python implementation. It is the behavioral
  reference: the Rust server is checked against fixtures generated from it.
- `rust/` — the Rust server (`iptv-media-server`), a single static binary that
  targets byte-for-byte output parity with Python. It is where new work goes.

## Layout

```
iptv_client/            Python reference implementation (uv project)
  service.py, web.py      refresh loop and HTTP routes
  dhcp.py, ctc_dhcp.py    DHCP client and CTC Option 60
  epg.py, auth.py         EPG client and authenticator
  xmltv.py, parsers.py    feed rendering and provider response parsing
  logo_catalog.py         third-party logo catalog builder (Python only)
rust/
  src/main.rs, lib.rs     entry point; `run` dispatches the server
  src/config.rs           CLI flags and environment (clap)
  src/startup.rs          composition: lease, interface, EPG, guide, router,
                          renewal, signal-driven shutdown with bounded drain
  src/dhcp/               raw-L2 DHCP (pnet_datalink), packet codec, lease,
                          interface setup via hand-encoded rtnetlink
  src/renewal.rs          lease renewal loop (full authenticated exchange)
  src/crypto/             Option 60 and EPG authenticator
  src/epg/                EPG client, provider response parsing, models
  src/guide/              guide windows (chrono-tz) and the guide cache
  src/refresh.rs          periodic guide refresh with retry rounds
  src/snapshot.rs         immutable channel/programme snapshot + render cache
  src/render.rs           M3U and XMLTV rendering
  src/http/               axum router: feeds, logos, metadata sync, HLS proxy
  src/hls/                playlist rewriting and resource tokens
  tests/                  integration, router, golden-parity and crypto tests
  tests/fixtures/         Python-oracle fixtures (generated, never hand-edited)
  tests/support/          mock provider for router tests
tools/oracle/generate.py  regenerates tests/fixtures from the Python code
scripts/verify.sh         local gate (see below)
scripts/test-real-e2e.sh  opt-in live gate against the real provider
systemd/                  unit template for Linux hosts
openwrt/                  OpenWrt packaging
var/                      local runtime state, credentials and captures
                          (ignored; never commit or overwrite)
```

## Toolchain

- Rust is pinned to 1.87.0 (`rust-toolchain.toml`) with rustfmt and clippy.
  The manifest lives under `rust/`, so pass
  `--manifest-path rust/Cargo.toml` or run cargo from `rust/`.
- Python uses `uv` (`uv sync`); the oracle generator and the Python server
  run through `uv run`.
- Static builds: `x86_64-unknown-linux-musl`, plus aarch64/armv7 musl linked
  with `rust-lld`. The dependency tree has no C code.

## Verification

Run before every push:

```sh
bash scripts/verify.sh
```

It runs, offline: `cargo fmt --check`, `cargo clippy --all-targets
--all-features -- -D warnings`, `cargo test --tests --bins --all-features`,
`tools/oracle/generate.py --check` against `rust/tests/fixtures`, and a
release build. Prepare a fresh host with `cargo fetch` and `uv sync`.

`cargo test` accepts one filter per invocation; run separate invocations for
separate filters.

The live gate needs credentials in `var/iptv-e2e.env` (mode 600) and the IPTV
interface up; it refuses to run if something already listens on its port:

```sh
IPTV_ENABLE_REAL_E2E=1 bash scripts/test-real-e2e.sh --guide-cache /tmp/guide.json
```

Keep the guide cache out of `var/` in test runs. Restore the interface to its
previous state afterwards.

## Code rules

- `#![forbid(unsafe_code)]`. Clippy runs with `all` denied and `pedantic`
  warned; `unwrap`, `expect`, `panic`, `todo` and `unimplemented` are denied
  outside tests. Do not silence lints with `allow`/`expect` attributes to get
  green; fix the code.
- Parity with Python is the contract. Behavior changes that diverge from the
  Python reference are deliberate, documented in the README, and covered by a
  test. Fixtures under `rust/tests/fixtures/` come only from
  `tools/oracle/generate.py`; never edit them by hand or add other files
  there.
- Keep it lean: no defensive checks inside trusted code, no abstractions for
  hypothetical needs; validate at system boundaries (CLI, HTTP requests,
  provider responses, DHCP packets).
- Redact credentials (`dhcp_user`, `dhcp_password`, `epg_user`,
  `epg_password`) in every `Debug` output and log line.
- The IPTV network never leaks into the host's routing: the standalone server
  adds its lease without a prefix route and keeps the subnet and default
  route in a dedicated table selected by a `from <lease>` rule; provider
  clients bind to the interface and the lease address. Do not hardcode
  provider subnets.
- Tests must be deterministic: no fixed sleeps or timing luck; await the
  exact event with a bounded timeout. Do not pin prose with tests.

## Git workflow

1. **Branch.** Every change is made on its own branch created from an
   up-to-date `main`. Never commit or push to `main` directly.
2. **Commit.** Use Conventional Commits with a scope and an English subject,
   e.g. `feat(rust): add lease daemon mode`, `fix(openwrt): export INTERFACE
   to the report script`, `docs(agents): describe the git workflow`. Every
   commit must be signed (SSH signing is configured; check with
   `git log --format='%h %G? %s'`, expecting `G`).
3. **Pull request.** Push the branch and open a PR against `main` describing
   what changed, how it was verified, and what is not yet verified.
4. **Review.** The PR must pass CodeRabbit review. Address each comment with a
   follow-up commit and reply on the thread with the fixing commit; when a
   comment is wrong, reply with the reasoning instead of changing the code.
   Repeat until no actionable comments remain. Only then is the work done.
5. **Merge.** Merge only after the user confirms, using **squash merge**.
