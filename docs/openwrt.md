# OpenWrt

`deploy/openwrt/luci-app-iptv` is a LuCI application (JavaScript views, no
`luci-compat`) with a procd service, a UCI config (`/etc/config/iptv`) and
`Services > IPTV` pages for status, settings and the log.

## Packages

Releases publish the application with the matching server binary as both

- `.ipk` — opkg, OpenWrt 24.10 and older
- `.apk` — apk-tools 3, OpenWrt 25.x and later

```bash
opkg install ./luci-app-iptv_1.0.0_x86_64.ipk                # 24.10 and older
apk add --allow-untrusted ./luci-app-iptv_1.0.0_x86_64.apk   # 25.x
```

The packages are unsigned, hence `--allow-untrusted` for apk; opkg installs a
local file without a flag. Both formats were verified on their respective releases (24.10.4 with opkg,
25.12.6 with apk-tools 3.0.5).

The packages carry OpenWrt's architecture names, not Rust's: `x86_64`,
`aarch64_cortex-a53` and `arm_cortex-a7_neon-vfpv4`. A router reporting any other
architecture refuses them, so build the package for it. The script packages a
binary you supply; it does not build one:

```bash
scripts/make-openwrt-package.sh \
    --binary target/aarch64-unknown-linux-musl/release/iptv-media-server \
    --arch aarch64_cortex-a53 --version 1.0.0 --output-dir dist
```

`--binary` must be built for the router's CPU — `aarch64` for
`aarch64_cortex-a53`, `armv7` for `arm_cortex-a7_neon-vfpv4`.

## Building the packages

apk v3 is a block container, not a tar, and only `apk mkpkg` writes it. The
apk-tools inside an OpenWrt image is built without that applet, so the script
needs one that has it:

```bash
export IPTV_APK_MKPKG=/path/to/openwrt-sdk/staging_dir/host/bin/apk
```

It refuses rather than emitting a package the router would reject. The release
workflow fetches the SDK for this.

An `.ipk` is a gzipped tar of `debian-binary`, `control.tar.gz` and
`data.tar.gz` — not the `ar` archive Debian uses. `dpkg` tools read the `ar`
form happily while opkg answers "Malformed package file", so verify on a real
OpenWrt release rather than on the host.

## Two modes

**Standalone** (`option managed '0'`): one process does everything, exactly as on
any other Linux host — it acquires and renews the lease, configures the
interface and routing table itself, and serves.

**Managed by netifd** (`option managed '1'`): the router's own network stack owns
the interface, so the lease shows under `Network > Interfaces` and the routes
under `Status > Routing`. Add an interface with the `iptv` protocol:

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
the lease and reports it through `/usr/libexec/iptv-lease-report`, the same way
`udhcpc` reports through `/lib/netifd/dhcp.script`. The media server runs with
`--network-managed --lease-address <address>`, never touches the network itself,
and is restarted by procd whenever the `iptv` interface changes.

`ip4table` keeps the IPTV routes out of the main table. netifd's rules also send
traffic *to* the IPTV subnet through that table, which standalone mode does not
do.

procd's `respawn` takes the role of systemd's `Restart=on-failure` in both modes.
