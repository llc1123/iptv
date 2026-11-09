# IPTV media server

A single-binary media server for an IPoE IPTV deployment. It acquires the
provider's authenticated DHCP lease (CTC Option 60 on every exchange), logs in
to the EPG service, keeps channel and programme metadata refreshed, and serves
an M3U playlist, an XMLTV guide, channel logos and an HLS proxy for live,
replay and catch-up playback.

The repository does not ship credentials, account identifiers, device
addresses, capture files, APKs, or deployment-specific endpoints. Supply those
values at runtime for an authorized service.

## Install

Grab the binary for your architecture from the
[releases](https://github.com/llc1123/iptv/releases) page — `x86_64`, `aarch64`
and `armv7` static musl builds, each with a matching `.ipk` and `.apk` OpenWrt
package — or build it from source:

```bash
rustup toolchain install 1.87.0 --profile minimal
rustup target add --toolchain 1.87.0 x86_64-unknown-linux-musl
cargo build --release --locked --target x86_64-unknown-linux-musl --bin iptv-media-server
```

That produces `target/x86_64-unknown-linux-musl/release/iptv-media-server`: one
self-contained file, statically linked, with no C dependencies, no `unsafe` and
no runtime packages beyond the kernel. It configures the interface over
rtnetlink instead of calling `ip`.

Dropping `--target` builds against the host's libc instead, which is fine for
local use but is not the self-contained binary the release ships and cannot run
in a `scratch` image or on OpenWrt.

The packages carry OpenWrt's architecture names, not Rust's: `x86_64`,
`aarch64_cortex-a53` and `arm_cortex-a7_neon-vfpv4`. A router reporting any
other architecture refuses them, so build the package for it. The script
packages a binary you pass in; it does not build one, so point `--binary` at a
build for the router's CPU (`aarch64` for `aarch64_cortex-a53`, `armv7` for
`arm_cortex-a7_neon-vfpv4`):

```bash
scripts/make-openwrt-package.sh --binary target/aarch64-unknown-linux-musl/release/iptv-media-server \
    --arch aarch64_cortex-a53 --version 1.0.0 --output-dir dist
```

## Configure credentials and deployment values

Set these environment variables (or the matching flags, listed by
`--help`). The values below are placeholders.

For a shell, export them:

```bash
export IPTV_DHCP_USER='your-dhcp-user'
export IPTV_DHCP_PASSWORD='your-dhcp-password'
export IPTV_USER_ID='your-epg-user-id'
export IPTV_PASSWORD='your-epg-password'
export IPTV_AUTH_URL='http://your-eds-host:port/EDS/jsp/AuthenticationURL'
export IPTV_STB_ID_PREFIX='your-stb-id-prefix'
export IPTV_INTERFACE='eth0'
```

`/opt/iptv/var/iptv.env`, which the systemd unit reads as its
`EnvironmentFile`, takes plain assignments instead: it is not a shell script, so
an `export` prefix makes systemd ignore the line and start the service without
that credential. Drop it:

```bash
IPTV_DHCP_USER='your-dhcp-user'
IPTV_DHCP_PASSWORD='your-dhcp-password'
IPTV_USER_ID='your-epg-user-id'
IPTV_PASSWORD='your-epg-password'
IPTV_AUTH_URL='http://your-eds-host:port/EDS/jsp/AuthenticationURL'
IPTV_STB_ID_PREFIX='your-stb-id-prefix'
IPTV_INTERFACE='eth0'
```

| Setting | Purpose |
| --- | --- |
| `IPTV_EPG_MAC` | Authorized set-top-box MAC for EPG authentication; defaults to the interface MAC. |
| `IPTV_INTERFACE` | Interface carrying the IPTV network, default `eth0`. |

The server puts the configured set-top-box identity in the BOOTP fields only;
it does not spoof the Ethernet source address.

## Run the media server

```bash
# From a source build (see Install for the target directory):
sudo -E target/x86_64-unknown-linux-musl/release/iptv-media-server --interface "$IPTV_INTERFACE" --host 127.0.0.1 --port 8080
# From a downloaded release, wherever you put it; the file name carries the
# version and architecture, e.g. iptv-media-server_1.0.0_x86_64:
sudo -E ./iptv-media-server_1.0.0_x86_64 --interface "$IPTV_INTERFACE" --host 127.0.0.1 --port 8080
```

It needs `CAP_NET_RAW` and `CAP_NET_ADMIN`, `/proc` to check them, and the
IPTV interface in its network namespace. The bundled
`deploy/systemd/iptv-media-server.service` grants exactly those capabilities
and reads the variables above from `/opt/iptv/var/iptv.env`.

The unit expects the tree at `/opt/iptv`, with the binary at
`/opt/iptv/target/release/iptv-media-server`, so a build has to be installed
there before the unit can start it:

```bash
sudo install -d /opt/iptv/target/release /opt/iptv/var
sudo install -m 0755 target/x86_64-unknown-linux-musl/release/iptv-media-server \
    /opt/iptv/target/release/iptv-media-server
# Create the environment file once; keep the credentials already in it.
[ -e /opt/iptv/var/iptv.env ] || sudo install -m 0600 /dev/null /opt/iptv/var/iptv.env
sudo install -m 0644 deploy/systemd/iptv-media-server.service /etc/systemd/system/
```

| URL | Purpose |
| --- | --- |
| `/channels.m3u` | Live-channel M3U playlist |
| `/xmltv.xml` | XMLTV channel and programme guide |
| `/stream/<channel-id>.m3u8` | Live HLS entry point |
| `/replay/<channel-id>/<programme-id>.m3u8` | Replay HLS entry point |
| `/catchup/<channel-id>.m3u8` | Catch-up HLS entry point |
| `/logos/<channel-id>` | Channel logo from `--logo-dir` |
| `POST /metadata/sync` | Refresh channel metadata |
| `/healthz` | Liveness |

Upstream HLS URLs are kept in memory and rewritten through short-lived proxy
routes; the upstream authentication parameters are never persisted.

## Channel logos

The provider's channel data carries no logos (`ChannelLogURL` is always empty),
so logos are the operator's to supply: a channel's logo is
`<logo-dir>/<channel-id>.png` (`--logo-dir`, default
`var/logo-catalog/logos`). Channels with such a file get a `tvg-logo`/`icon`
pointing at `/logos/<channel-id>`; the rest get none. Files are checked on each
feed render, so a logo added later appears after the next refresh.

## Behaviour worth knowing

**DHCP renewal.** At half the lease time the server sends a renewal REQUEST
carrying the address it holds (`ciaddr`), a fresh authenticator, and the server
that granted the lease as its destination, so the provider extends that binding
rather than handing out whatever a fresh DISCOVER would offer. Measured on the
live network: a DISCOVER at T1 was answered with a different address, while the
renewal REQUEST kept the original one. A captured set-top box renews the same
way — `ciaddr` set, no option 50 or 54, request addressed to the server — except
that it also unicasts at layer two, which needs the server's MAC; the frame
stays broadcast there. A refused renewal falls back to a full DISCOVER/REQUEST
exchange, retried until expiry. If the lease expires or the provider assigns a
different address, the process exits with status 1 so the supervisor restarts
it on the new lease.

**Routing isolation.** The leased address is added without a prefix route, and
its subnet and a default route via the lease's router go into one routing
table, which only traffic from the leased address reaches (`ip rule` priority
equal to that table id, ahead of `main`). Both numbers come from
`--route-table` (default 4788, "IPTV" on a keypad; range 1-32765 so the rule
stays ahead of `main`). Raise it if a host already uses that table. The EPG and
HLS clients bind to that address and the interface, so they reach the provider
whatever the host's own routes or overlapping subnets are, and the host's other
traffic never enters the IPTV network. No provider subnets are assumed.

**Guide refresh.** The provider answers many concurrent programme-window
requests with HTTP 503. The server retries those windows with backoff and lower
concurrency before keeping the previous snapshot's programmes for any window
that still fails, so a refresh never wipes the guide.

## Build and verify

```bash
cargo fetch --locked
bash scripts/verify.sh
```

`verify.sh` runs rustfmt, clippy with warnings denied, the test suite
(in-process loopback mock provider only), a release build, the frozen-fixture
integrity check, shellcheck and shfmt over the packaging scripts, and a syntax
check on the LuCI views. It runs offline against a populated cache; CI sets
`VERIFY_ALLOW_NETWORK=1` so cargo may fetch.

The live gate starts the release binary against the real provider. It needs
a mode-600 `var/iptv-e2e.env` holding the `IPTV_*` variables above, and nothing
else listening on its port.

Stop any other owner of the IPTV interface first — a running media server, or a
netifd `proto iptv` lease daemon on OpenWrt. The gate only checks that its port
is free, which says nothing about the interface: a second DHCP client competes
for the same binding and can cut off playback that is already running.

```bash
IPTV_ENABLE_REAL_E2E=1 bash scripts/test-real-e2e.sh
```

A DHCP renewal keeps an existing binding, so it can be exercised without
waiting for T1:

```bash
set -a; . var/iptv-e2e.env; set +a
cargo test --test renewal_live -- --ignored
```

It performs a real DHCP exchange, so it needs `CAP_NET_RAW` and `CAP_NET_ADMIN`
like the server above; run it as root, or `setcap` the test binary after each
rebuild.

### Golden fixtures are frozen

`tests/fixtures/` holds the deterministic expected values produced by the
project's original Python implementation, recorded before it was removed. They
are immutable: `tests/fixture_integrity.rs` checks every file against
`manifest.json`'s SHA-256, refuses unlisted files, and fails if a required
coverage case disappears. To change behaviour, change the code and update the
manifest deliberately in the same commit — never edit a fixture to make a test
pass.

### Static builds (containers, OpenWrt)

```bash
rustup target add --toolchain 1.87.0 \
  x86_64-unknown-linux-musl aarch64-unknown-linux-musl armv7-unknown-linux-musleabihf
cargo build --release --locked --target x86_64-unknown-linux-musl --bin iptv-media-server
```

Cross targets link with the bundled LLD (configured in `.cargo/config.toml`),
so no C cross toolchain is needed. MIPS targets are tier 3 in Rust 1.87 and
need a nightly toolchain with `-Zbuild-std`.

A static musl build runs in a `FROM scratch`/distroless image with nothing else
installed, e.g. `--network host --cap-add NET_RAW --cap-add NET_ADMIN`.

## OpenWrt

`deploy/openwrt/luci-app-iptv` is a LuCI application (JavaScript views, no
`luci-compat`) with a procd service, a UCI config (`/etc/config/iptv`) and
`Services > IPTV` pages for status, settings and the log. Releases publish it
as both an `.ipk` (opkg, OpenWrt 24.10 and older) and an `.apk`
(apk-tools 3, OpenWrt 25.x and later), each bundling the matching server
binary, built by `scripts/make-openwrt-package.sh`.

apk v3 is a block container, not a tar, and only `apk mkpkg` writes it — the
apk-tools inside an OpenWrt image is built without that applet, so the script
needs one that has it: point `IPTV_APK_MKPKG` at the OpenWrt SDK's host `apk`
(`staging_dir/host/bin/apk`) and it refuses rather than emitting a package the
router would reject. Install an unsigned `.ipk` with
`opkg install --force-signature`, or an unsigned `.apk` with
`apk add --allow-untrusted`; both were verified on their respective releases
(24.10.4 with opkg, 25.12.6 with apk-tools 3.0.5).

The service runs in one of two modes:

- **Standalone** (`option managed '0'`): one process does everything, exactly
  as on any other Linux host — it acquires and renews the lease, configures the
  interface and routing table itself, and serves.
- **Managed by netifd** (`option managed '1'`): the router's own network stack
  owns the interface. Add one with the `iptv` protocol:

  ```
  config interface 'iptv'
  	option proto 'iptv'
  	option device 'eth1'
  	option ip4table '4788'
  	option dhcp_user '...'
  	option dhcp_password '...'
  ```

  The interface must be named `iptv`: the procd service reads its address and
  restarts on its events by that name, and `proto iptv` is what provides the
  authenticated DHCP.

  netifd then runs `iptv-media-server --lease-daemon`, which acquires and renews
  the lease and reports it through `/usr/libexec/iptv-lease-report`, the same
  way `udhcpc` reports through `/lib/netifd/dhcp.script`. The lease shows under
  `Network > Interfaces`, the routes and rules under `Status > Routing`, and
  `ip4table` keeps the IPTV routes out of the main table. The media server runs
  with `--network-managed --lease-address <address>`, never touches the network
  itself, and is restarted by procd whenever the `iptv` interface changes.
  netifd's rules also send traffic *to* the IPTV subnet through that table,
  which standalone mode does not do.

procd's `respawn` takes the role of systemd's `Restart=on-failure` in both
modes.

## Continuous integration

Four workflows, each scoped to what it checks:

| Workflow | Pull requests | Pushes | Checks |
| --- | --- | --- | --- |
| `ci-rust.yml` | every one | `src/`, `tests/`, `Cargo.*` | fmt, clippy, tests, release build |
| `ci-scripts.yml` | every one | `scripts/`, `deploy/`, fixtures | shellcheck, shfmt, fixture integrity |
| `ci-luci.yml` | every one | the LuCI views and their check | JSON validity, view syntax, menu/ACL coverage |
| `release.yml` | — | a `v*` tag | three static binaries plus `.ipk`/`.apk` per architecture |

A push only runs the workflows its paths touch, but every pull request runs all
three, so each can be a required check without leaving the others pending.

`renovate.json` automerges every update, major bumps and the toolchain included.
Renovate's own PR needs the [Renovate app](https://github.com/apps/renovate)
installed for any of this to run.

`platformAutomerge` alone does not make CI a precondition: GitHub merges as soon
as branch protection allows, so require the `ci-rust`, `ci-scripts` and
`ci-luci` checks on `main` or a broken bump can merge before its checks finish.
Those workflows run on every pull request rather than only on the paths they
cover, because a required check that a pull request never triggers stays pending
and blocks it forever.

## Public-release hygiene

The repository ignores local captures, device dumps, APKs, native libraries,
logs, keystores, generated catalogs, reverse-engineering workspaces, and
runtime caches. Review the ignore rules before adding files with `git add -A`.

Only publish source and configuration templates. Never publish account
credentials, authentication tokens, signed playback URLs, DHCP captures,
device MAC addresses, private network addresses, or files copied from a
subscriber device.
