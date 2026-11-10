#!/usr/bin/env bash
# Builds the LuCI package plus the server binary for one architecture, as both
# an opkg `.ipk` (OpenWrt 24.10 and older) and an apk `.apk` (25.x and later).
#
# Usage: make-package.sh --binary <iptv-media-server> --arch <openwrt-arch>
#                        --version <version> --output-dir <dir>
#
# The two formats are unpacked differently on the router, but their payload is
# the same tree, so the file list lives here once.
set -Eeuo pipefail

binary='' arch='' version='' out=''
while [ $# -gt 0 ]; do
    case "$1" in
        --binary)
            binary=$2
            shift 2
            ;;
        --arch)
            arch=$2
            shift 2
            ;;
        --version)
            version=$2
            shift 2
            ;;
        --output-dir)
            out=$2
            shift 2
            ;;
        *)
            printf 'unknown argument: %s\n' "$1" >&2
            exit 2
            ;;
    esac
done
[ -n "$binary" ] && [ -n "$arch" ] && [ -n "$version" ] && [ -n "$out" ] || {
    printf 'usage: %s --binary B --arch A --version V --output-dir D\n' "$0" >&2
    exit 2
}
[ -x "$binary" ] || {
    printf 'not executable: %s\n' "$binary" >&2
    exit 2
}
# Absolute, because the ipk step changes into a temporary directory.
mkdir -p "$out"
out=$(cd -- "$out" && pwd)

root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
app=$root/deploy/openwrt/luci-app-iptv
name=iptv-media-server
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

# apk v3 is not a gzip tar: it is a block container (`ADBd`) that only
# `apk mkpkg` writes, and the apk-tools shipped in an OpenWrt image is built
# without that applet. The SDK's host `apk` has it, so it is located through
# $IPTV_APK_MKPKG, then `apk` on PATH. A v2 tar would be refused as
# "v2 package format error" by the router, so refuse to guess.
apk_mkpkg=${IPTV_APK_MKPKG:-}
if [ -z "$apk_mkpkg" ]; then
    for candidate in $(command -v apk apk-mkpkg 2>/dev/null); do
        if "$candidate" mkpkg --help 2>&1 | grep -q 'Mkpkg options'; then
            apk_mkpkg=$candidate
            break
        fi
    done
fi
[ -n "$apk_mkpkg" ] || {
    printf 'REFUSE: no apk with mkpkg found; set IPTV_APK_MKPKG to the SDK host apk\n' >&2
    exit 2
}

# --- the payload both formats carry -----------------------------------------
pkg=$work/data
mkdir -p "$pkg"
# The LuCI application: views, protocol handler, init script, helpers, ACLs.
cp -a "$app/root/." "$pkg/"
# The web assets live under htdocs/luci-static/resources on the router, so the
# directory name is not repeated: htdocs maps straight onto /www.
mkdir -p "$pkg/www"
cp -a "$app/htdocs/luci-static" "$pkg/www/luci-static"
# The server binary itself.
install -D -m 0755 "$binary" "$pkg/usr/bin/$name"
mkdir -p "$pkg/etc/iptv/logos"

size=$(du -sk "$pkg" | cut -f1)
installed=$((size * 1024))

# --- .ipk (opkg) -------------------------------------------------------------
# An OpenWrt .ipk is a gzipped tar of debian-binary, control.tar.gz and
# data.tar.gz -- not the `ar` archive Debian uses. opkg reports the `ar` form as
# "Malformed package file", which is easy to miss because dpkg tools read it
# happily.
ipk=$work/ipk
mkdir -p "$ipk/control"
cat >"$ipk/control/control" <<EOF
Package: luci-app-iptv
Version: $version
Depends: rpcd-mod-file
Source: iptv-media-server
SourceName: luci-app-iptv
License: GPL-3.0-or-later
Section: luci
SourceDateEpoch: 0
Architecture: $arch
Installed-Size: $installed
Description: LuCI support and the server binary for the IPTV media server
EOF
printf '2.0\n' >"$ipk/debian-binary"
# conffiles keeps a router's edits to /etc/config/iptv across upgrades.
printf '/etc/config/iptv\n' >"$ipk/control/conffiles"
tar -czf "$ipk/control.tar.gz" -C "$ipk/control" .
tar -czf "$ipk/data.tar.gz" -C "$pkg" .
tar -czf "$out/luci-app-iptv_${version}_${arch}.ipk" -C "$ipk" \
    ./debian-binary ./control.tar.gz ./data.tar.gz

# --- .apk (apk-tools 3, OpenWrt 25.x) ---------------------------------------
"$apk_mkpkg" mkpkg --compat 3.0 \
    --info "name:luci-app-iptv" \
    --info "version:$version-r0" \
    --info "arch:$arch" \
    --info "description:LuCI support and the server binary for the IPTV media server" \
    --info "url:https://github.com/llc1123/iptv" \
    --info "license:GPL-3.0-or-later" \
    --info "depends:rpcd-mod-file" \
    --files "$pkg" \
    --output "$out/luci-app-iptv_${version}_${arch}.apk"

printf 'built %s/luci-app-iptv_%s_%s.{ipk,apk}\n' "$out" "$version" "$arch"
