#!/bin/sh
# netifd protocol handler for the provider's authenticated DHCP.
#
# netifd owns the interface: the lease daemon started below acquires and renews
# the lease, and reports each one back through /usr/libexec/iptv-lease-report,
# which tells netifd the address and gateway. The media server then runs with
# `--network-managed` against that address. Set `option ip4table` to keep the
# IPTV routes out of the main table.

[ -x /usr/bin/iptv-media-server ] || exit 0
[ -x /usr/libexec/iptv-lease-report ] || exit 0

. /lib/functions.sh
. ../netifd-proto.sh
init_proto "$@"

proto_iptv_init_config() {
	renew_handler=1

	proto_config_add_string dhcp_user
	proto_config_add_string dhcp_password
	proto_config_add_string stb_id_prefix
}

proto_iptv_setup() {
	local interface="$1"
	local device="$2"
	local dhcp_user dhcp_password stb_id_prefix
	json_get_vars dhcp_user dhcp_password stb_id_prefix

	# The daemon reads its credentials from the environment, never from argv.
	proto_export "IPTV_DHCP_USER=$dhcp_user"
	proto_export "IPTV_DHCP_PASSWORD=$dhcp_password"
	proto_export "IPTV_DEVICE=$device"
	proto_export "INTERFACE=$interface"
	[ -n "$stb_id_prefix" ] && proto_export "IPTV_STB_ID_PREFIX=$stb_id_prefix"

	proto_run_command "$interface" /usr/bin/iptv-media-server \
		--lease-daemon \
		--interface "$device" \
		--lease-report /usr/libexec/iptv-lease-report
}

proto_iptv_renew() {
	local interface="$1"

	# A fresh daemon performs a full authenticated exchange; the previous one
	# is replaced rather than signalled, so a stuck exchange cannot linger.
	proto_kill_command "$interface"
	proto_iptv_setup "$@"
}

proto_iptv_teardown() {
	local interface="$1"

	# netifd withdraws the addresses and routes itself, as with udhcpc.
	proto_kill_command "$interface"
}

add_protocol iptv
