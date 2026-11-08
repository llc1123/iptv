#!/bin/sh
# netifd protocol handler for the provider's authenticated DHCP.
#
# netifd owns the interface: this handler asks the server for one lease
# (`--lease-only`), reports the address, gateway and DNS through netifd, and
# lets netifd place the routes. Set `option ip4table` on the interface to keep
# the IPTV routes out of the main table.

[ -x /usr/bin/iptv-media-server ] || exit 0

. /lib/functions.sh
. ../netifd-proto.sh
init_proto "$@"

proto_iptv_init_config() {
	renew_handler=1

	proto_config_add_string 'dhcp_user:dhcp_user'
	proto_config_add_string 'dhcp_password:dhcp_password'
	proto_config_add_string 'stb_id_prefix:stb_id_prefix'
	proto_config_add_string 'epg_mac:macaddr'
	proto_config_add_string 'authentication_url:url'
}

proto_iptv_setup() {
	local interface="$1"
	local device="$2"
	local dhcp_user dhcp_password stb_id_prefix epg_mac authentication_url
	json_get_vars dhcp_user dhcp_password stb_id_prefix epg_mac authentication_url

	# The server reads its credentials from the environment, never from argv.
	export IPTV_DHCP_USER="$dhcp_user"
	export IPTV_DHCP_PASSWORD="$dhcp_password"
	[ -n "$stb_id_prefix" ] && export IPTV_STB_ID_PREFIX="$stb_id_prefix"
	[ -n "$epg_mac" ] && export IPTV_EPG_MAC="$epg_mac"
	[ -n "$authentication_url" ] && export IPTV_AUTH_URL="$authentication_url"

	local report reason
	if ! report="$(/usr/bin/iptv-media-server --lease-only --interface "$device" 2>/tmp/iptv-lease.err)"; then
		reason="$(tail -n 1 /tmp/iptv-lease.err 2>/dev/null)"
		proto_notify_error "$interface" "${reason:-lease-failed}"
		proto_block_restart "$interface"
		return 1
	fi

	eval "$report"

	proto_init_update "$device" 1
	proto_add_ipv4_address "$IPTV_ADDRESS" "$IPTV_PREFIX"
	proto_add_ipv4_route 0.0.0.0 0 "$IPTV_GATEWAY"
	proto_send_update "$interface"
}

proto_iptv_renew() {
	proto_iptv_setup "$@"
}

proto_iptv_teardown() {
	local interface="$1"
	# The address and routes belong to netifd; nothing to clean up here.
	proto_init_update "*" 0
	proto_send_update "$interface"
}

add_protocol iptv
