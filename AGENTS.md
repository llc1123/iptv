# AGENTS.md

Guidance for coding agents and contributors working in this repository.

## What this is

An IPTV media server for a provider network that authenticates every DHCP
exchange (CTC Option 60) and every EPG session. It acquires a lease on the
IPTV interface, logs in to the provider's EPG, and serves an M3U playlist, an
XMLTV guide, channel logos, and an HLS proxy for live, replay and catch-up
playback.

The server is one static Rust binary. It replaces an earlier Python
implementation, which was removed once parity was pinned by fixtures: the
expected values it produced are frozen under `tests/fixtures/` and must not
change.

## Layout

```
src/                    the server
  main.rs, lib.rs         entry point; `run` dispatches the server
  config.rs               CLI flags and environment (clap)
  startup.rs              composition: lease, interface, EPG, guide, router,
                          renewal, signal-driven shutdown with bounded drain
  dhcp/                   raw-L2 DHCP (pnet_datalink), packet codec, lease,
                          interface setup via hand-encoded rtnetlink
  renewal.rs              lease renewal loop
  crypto/                 Option 60 and EPG authenticator
  epg/                    EPG client, provider response parsing, models
  guide/                  guide windows (chrono-tz) and the guide cache
  refresh.rs              periodic guide refresh with retry rounds
  snapshot.rs             immutable channel/programme snapshot + render cache
  render.rs               M3U and XMLTV rendering
  http/                   axum router: feeds, logos, metadata sync, HLS proxy
  hls/                    playlist rewriting and resource tokens
tests/                  integration, router, golden-parity and crypto tests
  fixtures/             frozen golden fixtures (never hand-edited)
  support/              mock provider for router tests
deploy/systemd/         unit template for Linux hosts
deploy/openwrt/         LuCI application `luci-app-iptv` (package if needed)
scripts/verify.sh       local gate
scripts/test-real-e2e.sh  opt-in live gate against the real provider
scripts/make-openwrt-package.sh  builds the .ipk and .apk release artifacts
.github/workflows/      ci-rust, ci-scripts, ci-luci, release
var/                    local runtime state, credentials and captures
                        (ignored; never commit or overwrite)
```

## Toolchain

- Rust is pinned to 1.87.0 (`rust-toolchain.toml`) with rustfmt and clippy.
- Static builds: `x86_64-unknown-linux-musl`, plus aarch64/armv7 musl linked
  with `rust-lld` via `.cargo/config.toml`. The dependency tree has no C code.
- Shell is checked with shellcheck (error severity) and shfmt; the LuCI views
  with `node --check`.

## Verification

Run before every push:

```sh
bash scripts/verify.sh
```

It runs, offline: `cargo fmt --check`, `cargo clippy --all-targets
--all-features -- -D warnings`, `cargo test --tests --bins --all-features`,
a release build, the frozen-fixture integrity test, shellcheck and shfmt on the
packaging scripts, and `node --check` on the LuCI views. Prepare a fresh host
with `cargo fetch`. CI sets `VERIFY_ALLOW_NETWORK=1` so cargo may fetch.

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
- `tests/fixtures/` is the contract. The expected values there were produced
  by the Python implementation this crate replaced, and are frozen:
  `tests/fixture_integrity.rs` checks each file against `manifest.json`'s
  SHA-256 and fails on an unlisted file or a missing coverage case. Never edit
  a fixture to make a test pass; change the code, and if the behaviour really
  changes, update the manifest in the same commit and say so in the README.
- Behaviour that differs from what the fixtures record is deliberate,
  documented in the README, and covered by a test (lease renewal, 503 retries,
  routing isolation, logo directory, HTTP-only).
- Keep it lean: no defensive checks inside trusted code, no abstractions for
  hypothetical needs; validate at system boundaries (CLI, HTTP requests,
  provider responses, DHCP packets).
- Redact credentials (`dhcp_user`, `dhcp_password`, `epg_user`,
  `epg_password`) in every `Debug` output and log line.
- The IPTV network never leaks into the host's routing: the standalone server
  adds its lease without a prefix route and keeps the subnet and default
  route in a dedicated table selected by a `from <lease>` rule; provider
  clients bind to the interface and the lease address. Do not hardcode
  provider subnets. In netifd mode the binary must not touch the network at
  all: netifd owns the address, routes and rules.
- The LuCI views are plain scripts: the LuCI loader wraps each file in a
  function, so they use `'require'` headers and a top-level `return
  view.extend(...)`. They are not ES modules; do not add a bundler step.
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
