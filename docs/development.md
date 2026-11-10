# Development and verification

## The local gate

```bash
cargo fetch --locked
bash scripts/verify.sh
```

It needs, besides the pinned Rust toolchain: `shellcheck`, `shfmt` **3.8.0**
(formatting shifts between minor versions, and CI pins that release) and
`node` for the LuCI views.

Offline against a populated cache; CI sets `VERIFY_ALLOW_NETWORK=1` so cargo may
fetch. It runs rustfmt, clippy with warnings denied, the test suite (in-process
loopback mock provider only), a release build, the frozen-fixture integrity
check, shellcheck and shfmt over the packaging scripts, and a syntax, menu and
ACL check on the LuCI views.

## Frozen fixtures

`tests/fixtures/` holds the deterministic expected values produced by the
project's original Python implementation, recorded before it was removed. They
are immutable: `tests/fixture_integrity.rs` checks every file against
`manifest.json`'s SHA-256, refuses unlisted files, and fails if a required
coverage case disappears.

To change behaviour, change the code and update the manifest deliberately in the
same commit — never edit a fixture to make a test pass.

## Static builds

```bash
rustup target add --toolchain 1.87.0 \
  x86_64-unknown-linux-musl aarch64-unknown-linux-musl armv7-unknown-linux-musleabihf
cargo build --release --locked --target x86_64-unknown-linux-musl --bin iptv-media-server
```

Cross targets link with the bundled LLD (configured in `.cargo/config.toml`), so
no C cross toolchain is needed. MIPS targets are tier 3 in Rust 1.87 and need a
nightly toolchain with `-Zbuild-std`.

A static musl build runs in a `FROM scratch`/distroless image with nothing else
installed, e.g. `--network host --cap-add NET_RAW --cap-add NET_ADMIN`.

## Live gates

Both need a mode-600 `var/iptv-e2e.env` holding the `IPTV_*` variables, and the
IPTV interface free of other lease owners. An unused port proves nothing: a
running server or a netifd `proto iptv` daemon competes for the same DHCP binding
and can cut off playback that is already running.

```bash
IPTV_ENABLE_REAL_E2E=1 bash scripts/test-real-e2e.sh
```

A renewal keeps an existing binding, so it can be exercised without waiting for
T1:

```bash
set -a; . var/iptv-e2e.env; set +a
cargo test --test renewal_live -- --ignored
```

It performs a real DHCP exchange, so it needs `CAP_NET_RAW` and `CAP_NET_ADMIN`
like the server; run it as root, or `setcap` the test binary after each rebuild.

## Continuous integration

| Workflow | Pull requests | Pushes | Checks |
| --- | --- | --- | --- |
| `ci-rust.yml` | every one | `src/`, `tests/`, `Cargo.*` | fmt, clippy, tests, release build |
| `ci-scripts.yml` | every one | `scripts/`, `deploy/`, fixtures | shellcheck, shfmt, fixture integrity |
| `ci-luci.yml` | every one | LuCI views and their check | JSON validity, view syntax, menu/ACL coverage |
| `release.yml` | — | a `v*` tag | three static binaries plus `.ipk`/`.apk` per architecture |

A push only runs the workflows its paths touch, but every pull request runs all
three, so each can be a required check without leaving the others pending. The
job names below are what branch protection matches on — not the workflow names:

- `fmt, clippy, test, build`
- `shellcheck and shfmt`
- `frozen fixture integrity`
- `view syntax and menu wiring`

## Automerge

`renovate.json` automerges every update, major bumps and the toolchain included.
Renovate's own PR needs the [Renovate app](https://github.com/apps/renovate)
installed.

`platformAutomerge` does not make CI a precondition by itself: GitHub merges as
soon as branch protection allows. Require the four job names above on `main`, or
a broken bump can merge before its checks finish.
