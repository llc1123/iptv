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

## Rust media server

`rust/` contains a Rust rewrite of the media server with the same endpoints,
flags (`--interface`, `--user-id`, `--password`, ...) and environment
variables as the Python server. The Python implementation remains the
reference: deterministic fixtures generated from it by
`tools/oracle/generate.py` live in `rust/tests/fixtures/` and pin the Rust
output byte for byte. The only intended difference is that HLS resource URLs
lose their `#fragment` before they are proxied.

Unlike the Python server, the Rust server also renews its DHCP lease: it
repeats the authenticated DHCP exchange at half the lease time and retries
until expiry. If the lease expires or the server hands out a different
address, the process exits with status 1 so a supervisor such as the systemd
unit below restarts it on the new lease.

Prepare the host once, then run the offline gate:

```bash
rustup toolchain install 1.87.0 --profile minimal --component rustfmt --component clippy
rustup run 1.87.0 cargo fetch --manifest-path rust/Cargo.toml --locked
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

`systemd/iptv-media-server.service` is a unit template that reads the same
variables from `/opt/iptv/var/iptv.env` and grants only `CAP_NET_RAW` and
`CAP_NET_ADMIN`, which DHCP needs.

## Public-release hygiene

The repository ignores local captures, device dumps, APKs, native libraries,
logs, keystores, generated catalogs, reverse-engineering workspaces, and
runtime caches. Review the ignore rules before adding files with `git add -A`.

Only publish source and configuration templates. Never publish account
credentials, authentication tokens, signed playback URLs, DHCP captures,
device MAC addresses, private network addresses, or files copied from a
subscriber device.
