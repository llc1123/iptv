#!/usr/bin/env bash
# Local gate: Rust fmt/clippy/tests/build, frozen-fixture integrity, shell and
# LuCI script checks. Runs offline; prepare a fresh host with `cargo fetch`.
set -Eeuo pipefail

cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.."

run() {
    printf '+ %s\n' "$*"
    "$@"
}

cargo=(rustup run 1.99.1 cargo)
# The local gate runs offline against a populated cache; CI sets
# VERIFY_ALLOW_NETWORK=1 so cargo may fetch the crates it needs.
offline=(--offline --locked)
[ "${VERIFY_ALLOW_NETWORK:-0}" = 1 ] && offline=(--locked)
openhwrt_scripts=(
    deploy/openwrt/luci-app-iptv/root/etc/init.d/iptv-media-server
    deploy/openwrt/luci-app-iptv/root/usr/libexec/iptv-lease-report
    deploy/openwrt/luci-app-iptv/root/usr/libexec/iptv-log
    deploy/openwrt/luci-app-iptv/root/usr/libexec/iptv-status
    deploy/openwrt/luci-app-iptv/root/lib/netifd/proto/iptv.sh
)

run "${cargo[@]}" fmt --all -- --check
run "${cargo[@]}" clippy "${offline[@]}" --all-targets --all-features -- -D warnings
run "${cargo[@]}" test "${offline[@]}" --tests --bins --all-features
run "${cargo[@]}" build "${offline[@]}" --release --bin iptv-media-server

# The fixtures are frozen; this fails if one was edited by hand.
run "${cargo[@]}" test "${offline[@]}" --test fixture_integrity

# Shell carries the packaging logic, so lint it. Only errors are enforced: the
# OpenWrt scripts legitimately use `local`, `[[ ]]` and procd variables that a
# warning-level pass flags as non-POSIX or unused-but-exported.
run shellcheck --severity=error --external-sources "${openhwrt_scripts[@]}" scripts/*.sh
run shfmt -d -i 4 -ci -s scripts/*.sh
run shfmt -d -i 0 -ci -s "${openhwrt_scripts[@]}"

# LuCI views are plain scripts that the LuCI loader wraps in a function, so they
# are neither ES modules nor runnable files; a parse check is what fits them.
for view in deploy/openwrt/luci-app-iptv/htdocs/luci-static/resources/view/iptv/*.js \
    deploy/openwrt/luci-app-iptv/htdocs/luci-static/resources/protocol/*.js; do
    run node --check "$view"
done
# The menu, ACL and views must agree, or a page renders nothing or cannot run
# the helper it calls.
run node scripts/check-luci.mjs

printf 'PASS: local verification completed\n'
