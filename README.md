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

The provider's channel data carries no logos (`ChannelLogURL` is always
empty); the builder matches channels through a third-party EPG matching
service and downloads the logos it points to.

## Rust media server

this crate contains the Rust server of the media server with the same endpoints as
the Python server, and the same flags for the shared options (`--interface`,
`--epg-mac`, `--stb-id`, `--user-id`, `--password`, `--authentication-url`,
`--host`, `--port`, `--guide-past-days`, `--guide-future-days`,
`--guide-timezone`, `--guide-cache`). It has no `--logo-catalog` or
`--metadata-converter-url`: logos come from `--logo-dir` (see below) and there
is no catalog builder. `--stb-id-prefix` and `--refresh-interval-seconds`
exist only in the Rust server.

Both servers read `IPTV_EPG_MAC`, `IPTV_DHCP_USER`, `IPTV_DHCP_PASSWORD`,
`IPTV_USER_ID`, `IPTV_PASSWORD` and `IPTV_AUTH_URL`. The Rust server also reads
`IPTV_INTERFACE` and `IPTV_STB_ID_PREFIX`; Python ignores both, so it uses
`eth0` unless `--interface` is given. Pass `--interface` explicitly to either
server when the interface is not `eth0` and not in the environment. The
systemd unit below relies on `IPTV_INTERFACE` and passes no `--interface`.

The Python implementation remains the reference: deterministic fixtures generated from it by
`tools/oracle/generate.py` live in `tests/fixtures/` and pin the Rust
output byte for byte. The only intended difference is that HLS resource URLs
lose their `#fragment` before they are proxied.

Unlike the Python server, the Rust server also renews its DHCP lease. At half
the lease time it sends a renewal REQUEST carrying the address it holds
(`ciaddr`), a fresh authenticator, and the server that granted the lease as its
destination, so the provider extends that binding instead of handing out
whatever a fresh DISCOVER would offer. Measured on the live network: a DISCOVER
at T1 was answered with a different address, while the renewal REQUEST kept the
original one. A captured set-top box renews the same way — `ciaddr` set, no
option 50 or 54, request addressed to the server — except that it also unicasts
at layer two, which needs the server's MAC; the frame stays broadcast there.
A refused renewal falls back to a full DISCOVER/REQUEST exchange, retried until
expiry. If the lease expires or the
provider assigns a different address, the process exits with status 1 so a
supervisor such as the systemd unit below restarts it on the new lease.

The Rust server keeps the IPTV network out of the host's routing. The leased
address is added without a prefix route, and its subnet and a default route
via the lease's router go into one routing table, which only traffic from the
leased address reaches (`ip rule` priority equal to that table id, ahead of
`main`). Both numbers come from `--route-table` (default 4788, "IPTV" on a
keypad; range 1-32765 so the rule stays ahead of `main`). Raise it if a host
already uses that table. The EPG and HLS clients bind to that address and the
interface, so they reach the provider whatever the host's own routes or
overlapping subnets are, and the host's other traffic never enters the IPTV
network. No provider subnets are assumed; the Python server instead adds fixed
routes to the main table.

The provider answers many concurrent programme-window requests with HTTP 503;
the Python server drops those windows (on the live service more than half of
the guide). The Rust server retries them with backoff and lower concurrency
before keeping the previous snapshot's programmes for any window that still
fails.

Prepare the host once, then run the offline gate:

```bash
rustup toolchain install 1.87.0 --profile minimal --component rustfmt --component clippy
rustup run 1.87.0 cargo fetch --locked
uv sync
bash scripts/verify.sh
```

`verify.sh` runs rustfmt, clippy with warnings denied, the test suite
(in-process loopback mock provider only), the fixture generator in `--check`
mode, and a release build.

The live gate starts the release binary against the real provider. It needs
the Python server stopped and a mode-600 `var/iptv-e2e.env` holding the
`IPTV_*` variables listed above:

```bash
IPTV_ENABLE_REAL_E2E=1 bash scripts/test-real-e2e.sh
```

The Rust server does not use `catalog.json`. A channel's logo is
`<logo-dir>/<channel-id>.png` (`--logo-dir`, default `var/logo-catalog/logos`),
maintained by the operator; channels with such a file get a `tvg-logo`/`icon`
pointing at `/logos/<channel-id>`, others get none. Files are checked on each
feed render, so logos added later appear after the next metadata refresh.

`deploy/systemd/iptv-media-server.service` is a unit template that reads the same
variables from `/opt/iptv/var/iptv.env` and grants only `CAP_NET_RAW` and
`CAP_NET_ADMIN`, which DHCP needs.

### Static builds (containers, OpenWrt)

The server is a single self-contained binary: it configures the interface over
rtnetlink instead of calling `ip`, contains no C code and no `unsafe`, and
speaks plain HTTP only (the provider's endpoints are all `http://`). A static
musl build therefore runs in a `FROM scratch`/distroless image or on OpenWrt
with nothing else installed:

```bash
rustup target add --toolchain 1.87.0 x86_64-unknown-linux-musl aarch64-unknown-linux-musl
rustup run 1.87.0 cargo build  --release \
  --target x86_64-unknown-linux-musl
# Cross targets link with the bundled LLD; no C cross toolchain is needed.
CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER=rust-lld \
  rustup run 1.87.0 cargo build  --release \
  --target aarch64-unknown-linux-musl
```

`armv7-unknown-linux-musleabihf` builds the same way. MIPS targets are tier 3
in Rust 1.87 and need a nightly toolchain with `-Zbuild-std`.

At run time the process needs `CAP_NET_RAW` and `CAP_NET_ADMIN`, `/proc`
(to check them), and the IPTV interface in its network namespace, e.g.
`--network host --cap-add NET_RAW --cap-add NET_ADMIN` for a container.
### OpenWrt

`deploy/openwrt/luci-app-iptv` is a LuCI application (JavaScript views, no
`luci-compat`) with a procd service, a UCI config (`/etc/config/iptv`) and
`Services > IPTV` pages for status, settings and the log. It depends on
`rpcd-mod-file`. The service runs in one of two modes:

- **Standalone** (`option managed '0'`): one process does everything, exactly
  as on any other Linux host: it acquires and renews the lease, configures the
  interface and routing table itself, and serves.
- **Managed by netifd** (`option managed '1'`): the router's own network stack
  owns the interface. Add an interface with the `iptv` protocol, for example

  ```
  config interface 'iptv'
  	option proto 'iptv'
  	option device 'eth1'
  	option ip4table '4788'
  	option dhcp_user '...'
  	option dhcp_password '...'
  ```

  The interface must be named `iptv`: the procd service reads its address and
  restarts on its events by that name, and `proto iptv` is what gives it the
  authenticated DHCP.

  netifd then runs `iptv-media-server --lease-daemon`, which acquires and
  renews the lease and reports it through `/usr/libexec/iptv-lease-report`, the
  same way `udhcpc` reports through `/lib/netifd/dhcp.script`. The lease shows
  under `Network > Interfaces`, the routes and rules under `Status > Routing`,
  and `ip4table` keeps the IPTV routes out of the main table. The media server
  runs with `--network-managed --lease-address <address>`, never touches the
  network itself, and is restarted by procd whenever the `iptv` interface
  changes. netifd's rules also send traffic *to* the IPTV subnet through that
  table, which the standalone mode does not do.

procd's `respawn` takes the role of systemd's `Restart=on-failure` in both
modes.

## Public-release hygiene

The repository ignores local captures, device dumps, APKs, native libraries,
logs, keystores, generated catalogs, reverse-engineering workspaces, and
runtime caches. Review the ignore rules before adding files with `git add -A`.

Only publish source and configuration templates. Never publish account
credentials, authentication tokens, signed playback URLs, DHCP captures,
device MAC addresses, private network addresses, or files copied from a
subscriber device.
