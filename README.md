# IPTV media server

A single-binary media server for an IPoE IPTV deployment. It acquires the
provider's authenticated DHCP lease (CTC Option 60 on every exchange), logs in
to the EPG service, keeps channel and programme metadata refreshed, and serves
an M3U playlist, an XMLTV guide, channel logos and an HLS proxy for live, replay
and catch-up playback.

The repository does not ship credentials, account identifiers, device addresses,
capture files, APKs, or deployment-specific endpoints. Supply those values at
runtime for an authorized service.

## Install

Download the binary for your architecture from the
[releases](https://github.com/llc1123/iptv/releases) page — `x86_64`, `aarch64`
and `armv7` static musl builds, each with `.ipk`/`.apk` OpenWrt packages — or
build it:

```bash
rustup toolchain install 1.99.1 --profile minimal
rustup target add --toolchain 1.99.1 x86_64-unknown-linux-musl
cargo build --release --locked --target x86_64-unknown-linux-musl --bin iptv-media-server
```

The result, `target/x86_64-unknown-linux-musl/release/iptv-media-server`, is one
statically linked file with no C dependencies, no `unsafe` and no runtime
packages beyond the kernel. It configures the interface over rtnetlink instead
of calling `ip`. Dropping `--target` links against the host libc and does not
produce that.

OpenWrt packaging, its architecture names and the netifd mode are in
[docs/openwrt.md](docs/openwrt.md).

## Configure

Set these environment variables (or the matching flags from `--help`). The
values below are placeholders.

```bash
export IPTV_DHCP_USER='your-dhcp-user'
export IPTV_DHCP_PASSWORD='your-dhcp-password'
export IPTV_USER_ID='your-epg-user-id'
export IPTV_PASSWORD='your-epg-password'
export IPTV_AUTH_URL='http://your-eds-host:port/EDS/jsp/AuthenticationURL'
export IPTV_STB_ID_PREFIX='your-stb-id-prefix'
export IPTV_INTERFACE='eth0'
```

| Setting | Purpose |
| --- | --- |
| `IPTV_EPG_MAC` | Authorized set-top-box MAC for EPG authentication; defaults to the interface MAC. |
| `IPTV_INTERFACE` | Interface carrying the IPTV network, default `eth0`. |

The server puts the configured set-top-box identity in the BOOTP fields only; it
does not spoof the Ethernet source address.

`/opt/iptv/var/iptv.env`, read by the systemd unit as its `EnvironmentFile`,
takes the same names without the `export` prefix — it is not a shell script, and
`export` makes systemd ignore the line.

## Run

```bash
sudo -E target/x86_64-unknown-linux-musl/release/iptv-media-server \
    --interface "$IPTV_INTERFACE" --host 127.0.0.1 --port 8080
```

It needs `CAP_NET_RAW` and `CAP_NET_ADMIN`, `/proc` to check them, and the IPTV
interface in its namespace.

There is no authentication: anyone who can reach the port can use the
subscription's streams and trigger refreshes. Loopback is the default for that
reason. To serve other machines, bind a trusted address and restrict it with a
firewall, or put an authenticating reverse proxy in front. `deploy/systemd/iptv-media-server.service` grants
exactly those capabilities and expects this layout:

```bash
sudo install -d /opt/iptv/target/release /opt/iptv/var
sudo install -m 0755 target/x86_64-unknown-linux-musl/release/iptv-media-server \
    /opt/iptv/target/release/iptv-media-server
# An existing file keeps its credentials: touch never truncates.
sudo touch /opt/iptv/var/iptv.env && sudo chmod 0600 /opt/iptv/var/iptv.env
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

Upstream HLS URLs are held in memory and rewritten through short-lived proxy
routes; the upstream authentication parameters are never persisted.

## Channel logos

Logos are the operator's to supply: a channel's logo is
`<logo-dir>/<channel-id>.png` (`--logo-dir`, default `var/logo-catalog/logos`).
Channels with such a file get a `tvg-logo`/`icon` pointing at
`/logos/<channel-id>`; the rest get none. The provider's own data carries no
logos.

## More

- [docs/behaviour.md](docs/behaviour.md) — DHCP renewal, routing isolation, one
  session per subscription, guide refresh, logos.
- [docs/openwrt.md](docs/openwrt.md) — packages, netifd mode, LuCI application.
- [docs/development.md](docs/development.md) — the gate, live tests, CI, automerge.
