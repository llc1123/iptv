from __future__ import annotations

import os
import secrets
import subprocess
from ipaddress import IPv4Address

from scapy.arch import get_if_hwaddr
from scapy.config import conf
from scapy.layers.dhcp import BOOTP, DHCP
from scapy.layers.inet import IP, UDP
from scapy.layers.l2 import Ether
from scapy.packet import Packet
from scapy.sendrecv import srp1

from .credentials import DEFAULT_DHCP_PASSWORD, DEFAULT_DHCP_USER
from .ctc_dhcp import make_option60
from .errors import IptvError
from .models import DhcpLease

PARAMETER_REQUEST_LIST = bytes([1, 121, 33, 3, 6, 15, 28, 51, 58, 59])
STB_ID_PREFIX = os.environ.get("IPTV_STB_ID_PREFIX", "")


def interface_mac(interface: str) -> str:
    """Return the selected interface MAC in canonical uppercase form."""
    return get_if_hwaddr(interface).upper()


def stb_id_for_mac(mac: str) -> str:
    """Build the observed STB identifier from an interface MAC."""
    return f"{STB_ID_PREFIX}{mac.replace(':', '').upper()}"


def _packet_base(mac: str, xid: int) -> Packet:
    return (
        Ether(src=mac, dst="ff:ff:ff:ff:ff:ff")
        / IP(src="0.0.0.0", dst="255.255.255.255")
        / UDP(sport=68, dport=67)
        / BOOTP(
            op=1,
            htype=1,
            hlen=6,
            xid=xid,
            flags=0,
            chaddr=bytes.fromhex(mac.replace(":", "")),
        )
    )


def _dhcp_options(packet: Packet) -> list[tuple[str, str | int | bytes | list[str]]]:
    options = packet[DHCP].options
    return [option for option in options if isinstance(option, tuple)]


def _option(packet: Packet, name: str | int) -> str | int | bytes | list[str] | None:
    for key, value in _dhcp_options(packet):
        if key == name:
            return value
    return None


def _ipv4_option(packet: Packet, name: str) -> IPv4Address:
    value = _option(packet, name)
    if not isinstance(value, str):
        raise IptvError("dhcp", f"missing IPv4 option {name}")
    return IPv4Address(value)


def _lease_from_ack(packet: Packet) -> DhcpLease:
    lease_seconds = _option(packet, "lease_time")
    vendor_specific = _option(packet, "vendor_specific_information")
    if not isinstance(lease_seconds, int) or not isinstance(vendor_specific, bytes):
        raise IptvError("dhcp", "ACK lacks lease time or vendor-specific information")
    return DhcpLease(
        address=IPv4Address(packet[BOOTP].yiaddr),
        router=_ipv4_option(packet, "router"),
        server=_ipv4_option(packet, "server_id"),
        subnet_mask=_ipv4_option(packet, "subnet_mask"),
        lease_seconds=lease_seconds,
        vendor_specific=vendor_specific,
    )


def _receive(interface: str, packet: Packet, timeout: float) -> Packet:
    conf.checkIPaddr = False
    try:
        answer = srp1(packet, iface=interface, timeout=timeout, verbose=0)
    except OSError as error:
        raise IptvError("dhcp", f"cannot use {interface}: {error}") from error
    if answer is None:
        raise IptvError("dhcp", f"no reply on {interface} within {timeout:.1f}s")
    return answer


def acquire_lease(
    interface: str,
    timeout: float = 10.0,
    dhcp_password: str = DEFAULT_DHCP_PASSWORD,
    dhcp_user: str = DEFAULT_DHCP_USER,
) -> DhcpLease:
    mac = interface_mac(interface)
    xid = secrets.randbits(32)
    hostname = stb_id_for_mac(mac).encode("ascii")
    discover_vendor_class = make_option60(dhcp_password, dhcp_user)
    discover = _packet_base(mac, xid) / DHCP(
        options=[
            ("message-type", "discover"),
            (60, discover_vendor_class),
            (12, hostname),
            (55, PARAMETER_REQUEST_LIST),
            "end",
        ]
    )
    offer = _receive(interface, discover, timeout)
    if not offer.haslayer(BOOTP) or not offer.haslayer(IP):
        raise IptvError("dhcp", "reply was not a BOOTP offer")
    requested = IPv4Address(offer[BOOTP].yiaddr).packed
    server = IPv4Address(offer[IP].src).packed
    request_vendor_class = make_option60(dhcp_password, dhcp_user)
    request = _packet_base(mac, xid) / DHCP(
        options=[
            ("message-type", "request"),
            (50, requested),
            (54, server),
            (60, request_vendor_class),
            (12, hostname),
            (55, PARAMETER_REQUEST_LIST),
            "end",
        ]
    )
    ack = _receive(interface, request, timeout)
    if not ack.haslayer(BOOTP):
        raise IptvError("dhcp", "reply was not a BOOTP ACK")
    return _lease_from_ack(ack)


def configure_interface(interface: str, lease: DhcpLease) -> None:
    commands = [
        [
            "ip",
            "addr",
            "replace",
            f"{lease.address}/{lease.prefix_length}",
            "dev",
            interface,
        ],
        [
            "ip",
            "route",
            "replace",
            "10.223.0.0/16",
            "via",
            str(lease.router),
            "dev",
            interface,
            "src",
            str(lease.address),
        ],
        [
            "ip",
            "route",
            "replace",
            "20.248.0.0/13",
            "via",
            str(lease.router),
            "dev",
            interface,
            "src",
            str(lease.address),
        ],
    ]
    for command in commands:
        subprocess.run(command, check=True, capture_output=True, text=True)
