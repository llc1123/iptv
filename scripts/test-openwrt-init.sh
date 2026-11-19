#!/usr/bin/env bash
set -Eeuo pipefail

root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

config_load() { :; }
config_get() {
    local key="TEST_$3"
    printf -v "$1" '%s' "${!key-${4-}}"
}
config_get_bool() { config_get "$@"; }
procd_open_instance() { :; }
procd_close_instance() { :; }
procd_append_param() { :; }
procd_set_param() {
    if [ "$1" = group ]; then
        service_group=$2
    fi
}

# shellcheck source=../deploy/openwrt/luci-app-iptv/root/etc/init.d/iptv-media-server
source "$root/deploy/openwrt/luci-app-iptv/root/etc/init.d/iptv-media-server"

TEST_dhcp_user=test
TEST_dhcp_password=test
TEST_epg_user=test
TEST_epg_password=test
TEST_guide_cache=$work/guide.json
lease_arguments() { printf '%s' '--lease-address 192.0.2.10'; }

for TEST_managed in 0 1; do
    for state in default enabled disabled; do
        unset TEST_bypass_proxy
        expected=nogroup
        case "$state" in
            enabled) TEST_bypass_proxy=1 ;;
            disabled) TEST_bypass_proxy=0 expected='' ;;
        esac
        service_group=''
        start_service
        if [ "$service_group" != "$expected" ]; then
            printf 'FAIL: managed=%s bypass=%s: group=%s, expected=%s\n' \
                "$TEST_managed" "$state" "$service_group" "$expected" >&2
            exit 1
        fi
    done
done

printf 'PASS: proxy bypass defaults and both states in standalone and managed modes\n'
