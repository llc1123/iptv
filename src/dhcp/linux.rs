//! The Linux side of the DHCP client: layer-2 frames through `pnet_datalink`
//! (an `AF_PACKET` socket) and interface setup through rtnetlink, so no
//! external `ip` binary is needed (distroless images, `OpenWrt`).
use super::{
    DhcpError, DhcpLease, lease,
    packet::{self, MacAddress, ReplyKind},
};
use crate::crypto::ctc_option60::{self, Option60Input};
use netlink_sys::{Socket, SocketAddr, protocols::NETLINK_ROUTE};
use pnet_datalink::{Channel, DataLinkReceiver, DataLinkSender, NetworkInterface};
use std::{
    fmt, fs, io,
    net::Ipv4Addr,
    sync::OnceLock,
    time::{Duration, Instant},
};
use thiserror::Error;

/// How long each DISCOVER and REQUEST waits for its reply.
const STAGE_TIMEOUT: Duration = Duration::from_secs(10);
/// How often a quiet receive wakes up to check the deadline.
const RECEIVE_POLL: Duration = Duration::from_secs(1);
static OPTION60_ORIGIN: OnceLock<Instant> = OnceLock::new();

#[derive(Clone)]
pub struct ClientConfig<'a> {
    pub user: &'a str,
    pub password: &'a str,
    pub stb_id_prefix: &'a str,
}
impl fmt::Debug for ClientConfig<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClientConfig")
            .field("user", &"[REDACTED]")
            .field("password", &"[REDACTED]")
            .field("stb_id_prefix", &self.stb_id_prefix)
            .finish()
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum LinuxError {
    #[error("required Linux network capability is unavailable: {0}")]
    MissingCapability(&'static str),
    #[error("DHCP reply timed out")]
    Timeout,
    /// The failing operation and the OS error.
    #[error("DHCP {0}")]
    Io(String),
    #[error("DHCP server rejected the request (NAK)")]
    Nak,
    /// The failing interface change and the kernel's error.
    #[error("interface setup failed: {0}")]
    Netlink(String),
}

/// Runs the DISCOVER/OFFER/REQUEST/ACK exchange on `interface`.
pub fn acquire_lease(
    interface: &str,
    config: &ClientConfig<'_>,
) -> Result<(DhcpLease, MacAddress), DhcpError> {
    check_capabilities()?;
    let interface = find_interface(interface)?;
    let mac = interface
        .mac
        .map(|mac| MacAddress::new(mac.octets()))
        .ok_or_else(|| LinuxError::Io(format!("{} has no MAC address", interface.name)))?;
    let (mut tx, mut rx) = channel(&interface)?;
    let hostname = format!("{}{}", config.stb_id_prefix, mac.compact_upper());
    let xid = random_u32()?;
    let discover = packet::build_discover(mac, xid, &option60(config)?, hostname.as_bytes())?;
    let offer = exchange(&mut *tx, &mut *rx, &discover, xid, mac, ReplyKind::Offer)?;
    let request = packet::build_request(
        mac,
        xid,
        offer.address,
        offer.server.unwrap_or(offer.source),
        &option60(config)?,
        hostname.as_bytes(),
    )?;
    let acquired_at = Instant::now();
    let ack = exchange(&mut *tx, &mut *rx, &request, xid, mac, ReplyKind::Ack)?;
    Ok((lease::from_ack(ack, acquired_at)?, mac))
}

/// Renews `held` in place: one REQUEST carrying the address already held, with
/// no requesting-server options, so the server keeps the same binding instead
/// of handing out whatever DISCOVER would have offered.
///
/// # Errors
/// Returns the exchange error; the caller falls back to a full acquisition,
/// which is also what recovers from an expired lease.
pub fn renew_lease(
    interface: &str,
    config: &ClientConfig<'_>,
    held: &DhcpLease,
) -> Result<DhcpLease, DhcpError> {
    check_capabilities()?;
    let interface = find_interface(interface)?;
    let mac = interface
        .mac
        .map(|mac| MacAddress::new(mac.octets()))
        .ok_or_else(|| LinuxError::Io(format!("{} has no MAC address", interface.name)))?;
    let (mut tx, mut rx) = channel(&interface)?;
    let hostname = format!("{}{}", config.stb_id_prefix, mac.compact_upper());
    let xid = random_u32()?;
    let request = packet::build_renew(
        mac,
        xid,
        held.address,
        held.server,
        &option60(config)?,
        hostname.as_bytes(),
    )?;
    let renewed_at = Instant::now();
    let ack = exchange(&mut *tx, &mut *rx, &request, xid, mac, ReplyKind::Ack)?;
    Ok(lease::from_ack(ack, renewed_at)?)
}

/// The raw-L2 sender and receiver of one interface.
type Link = (Box<dyn DataLinkSender>, Box<dyn DataLinkReceiver>);

/// Opens the raw-L2 channel for `interface`.
fn channel(interface: &NetworkInterface) -> Result<Link, DhcpError> {
    let config_datalink = pnet_datalink::Config {
        read_timeout: Some(RECEIVE_POLL),
        promiscuous: false,
        ..pnet_datalink::Config::default()
    };
    match pnet_datalink::channel(interface, config_datalink) {
        Ok(Channel::Ethernet(tx, rx)) => Ok((tx, rx)),
        Ok(_) => Err(LinuxError::Io("unsupported datalink channel".to_owned()).into()),
        Err(error) => Err(LinuxError::Io(format!("open {}: {error}", interface.name)).into()),
    }
}

/// The MAC address of `interface`, used to derive the EPG client identity when
/// netifd owns the address and no DHCP exchange happened here.
pub fn interface_mac(interface: &str) -> Result<MacAddress, LinuxError> {
    let interface = find_interface(interface)?;
    interface
        .mac
        .map(|mac| MacAddress::new(mac.octets()))
        .ok_or_else(|| LinuxError::Io(format!("{} has no MAC address", interface.name)))
}

/// Default policy routing table for traffic sourced from the lease address.
/// Only that traffic reaches it, so the IPTV network never mixes with the
/// host's routes, even when their subnets overlap. ("IPTV" on a keypad.)
pub const DEFAULT_ROUTE_TABLE: u32 = 4788;

/// Puts the lease on `interface` without touching the main routing table:
/// the address is added without its prefix route, and the subnet plus a
/// default route via the lease's router go into `route_table`, selected by a
/// rule of the same number for the lease address (which keeps it ahead of
/// `main`). Sockets bound to that address (the EPG and HLS clients) use the
/// IPTV network; nothing else does.
pub fn configure_interface(
    interface: &str,
    lease: &DhcpLease,
    route_table: u32,
) -> Result<(), DhcpError> {
    let index = find_interface(interface)?.index;
    let mut netlink = Netlink::open()?;
    let address = format!("{}/{}", lease.address, lease.prefix_length());
    netlink
        .request(
            RTM_NEWADDR,
            NLM_F_REPLACE,
            &address_message(index, lease, true),
        )
        .map_err(|error| LinuxError::Netlink(format!("add {address}: {error}")))?;
    let mask = u32::from(lease.subnet_mask);
    let subnet = Ipv4Addr::from(u32::from(lease.address) & mask);
    for (destination, prefix, gateway) in [
        (subnet, lease.prefix_length(), None),
        (Ipv4Addr::UNSPECIFIED, 0, Some(lease.router)),
    ] {
        let route = route_message(
            index,
            lease.address,
            destination,
            prefix,
            gateway,
            route_table,
        );
        netlink
            .request(RTM_NEWROUTE, NLM_F_REPLACE, &route)
            .map_err(|error| {
                LinuxError::Netlink(format!("route {destination}/{prefix}: {error}"))
            })?;
    }
    remove_rules(&mut netlink, route_table)?;
    netlink
        .request(
            RTM_NEWRULE,
            NLM_F_CREATE,
            &rule_message(Some(lease.address), route_table),
        )
        .map_err(|error| LinuxError::Netlink(format!("rule from {}: {error}", lease.address)))?;
    Ok(())
}

/// Removes a lease's address, its routes (the kernel drops routes whose
/// source it was) and the routing rule, e.g. after the server handed out a
/// different address on renewal.
pub fn remove_address(
    interface: &str,
    lease: &DhcpLease,
    route_table: u32,
) -> Result<(), DhcpError> {
    let index = find_interface(interface)?.index;
    let mut netlink = Netlink::open()?;
    remove_rules(&mut netlink, route_table)?;
    netlink
        .request(RTM_DELADDR, 0, &address_message(index, lease, false))
        .map_err(|error| LinuxError::Netlink(format!("delete {}: {error}", lease.address)))?;
    Ok(())
}

/// Deletes every rule of `route_table`, including ones left by an earlier
/// run, so restarts do not stack duplicates.
fn remove_rules(netlink: &mut Netlink, route_table: u32) -> Result<(), LinuxError> {
    loop {
        match netlink.request(RTM_DELRULE, 0, &rule_message(None, route_table)) {
            Ok(()) => {}
            Err(error) if error.raw_os_error() == Some(ENOENT) => return Ok(()),
            Err(error) => return Err(LinuxError::Netlink(format!("delete rule: {error}"))),
        }
    }
}

fn find_interface(name: &str) -> Result<NetworkInterface, LinuxError> {
    pnet_datalink::interfaces()
        .into_iter()
        .find(|interface| interface.name == name)
        .ok_or_else(|| LinuxError::Io(format!("interface {name} not found")))
}

const RTM_NEWADDR: u16 = 20;
const RTM_DELADDR: u16 = 21;
const RTM_NEWROUTE: u16 = 24;
const RTM_NEWRULE: u16 = 32;
const RTM_DELRULE: u16 = 33;
const NLMSG_ERROR: u16 = 2;
const NLM_F_REQUEST: u16 = 0x1;
const NLM_F_ACK: u16 = 0x4;
const NLM_F_CREATE: u16 = 0x400;
/// `NLM_F_REPLACE | NLM_F_CREATE`: what `ip ... replace` sends.
const NLM_F_REPLACE: u16 = 0x100 | NLM_F_CREATE;
const ENOENT: i32 = 2;
const AF_INET: u8 = 2;
const IFA_ADDRESS: u16 = 1;
const IFA_LOCAL: u16 = 2;
const IFA_FLAGS: u16 = 8;
const IFA_F_NOPREFIXROUTE: u32 = 0x200;
const RTA_DST: u16 = 1;
const RTA_OIF: u16 = 4;
const RTA_GATEWAY: u16 = 5;
const RTA_PREFSRC: u16 = 7;
const RTA_TABLE: u16 = 15;
const FRA_SRC: u16 = 2;
const FRA_PRIORITY: u16 = 6;
const FRA_TABLE: u16 = 15;
const FR_ACT_TO_TBL: u8 = 1;
const RTPROT_BOOT: u8 = 3;
const RT_SCOPE_UNIVERSE: u8 = 0;
const RT_SCOPE_LINK: u8 = 253;
const RTN_UNICAST: u8 = 1;

/// Appends a 4-byte netlink attribute (`struct rtattr` plus payload).
fn attribute(message: &mut Vec<u8>, kind: u16, value: [u8; 4]) {
    message.extend_from_slice(&8_u16.to_ne_bytes());
    message.extend_from_slice(&kind.to_ne_bytes());
    message.extend_from_slice(&value);
}

/// `struct ifaddrmsg` and its attributes for the lease's address.
fn address_message(index: u32, lease: &DhcpLease, no_prefix_route: bool) -> Vec<u8> {
    let prefix = u8::try_from(lease.prefix_length()).unwrap_or(32);
    let mut message = vec![AF_INET, prefix, 0, RT_SCOPE_UNIVERSE];
    message.extend_from_slice(&index.to_ne_bytes());
    attribute(&mut message, IFA_LOCAL, lease.address.octets());
    attribute(&mut message, IFA_ADDRESS, lease.address.octets());
    if no_prefix_route {
        attribute(&mut message, IFA_FLAGS, IFA_F_NOPREFIXROUTE.to_ne_bytes());
    }
    message
}

/// `struct rtmsg` and its attributes for a unicast route in `route_table`;
/// a route without a gateway is directly connected, as `ip` infers.
fn route_message(
    index: u32,
    source: Ipv4Addr,
    destination: Ipv4Addr,
    prefix: u32,
    gateway: Option<Ipv4Addr>,
    route_table: u32,
) -> Vec<u8> {
    let scope = if gateway.is_some() {
        RT_SCOPE_UNIVERSE
    } else {
        RT_SCOPE_LINK
    };
    // The table travels in RTA_TABLE; the 8-bit header field stays unset.
    let prefix = u8::try_from(prefix).unwrap_or(32);
    let mut message = vec![AF_INET, prefix, 0, 0, 0, RTPROT_BOOT, scope, RTN_UNICAST];
    message.extend_from_slice(&0_u32.to_ne_bytes());
    attribute(&mut message, RTA_TABLE, route_table.to_ne_bytes());
    if prefix > 0 {
        attribute(&mut message, RTA_DST, destination.octets());
    }
    if let Some(gateway) = gateway {
        attribute(&mut message, RTA_GATEWAY, gateway.octets());
    }
    attribute(&mut message, RTA_OIF, index.to_ne_bytes());
    attribute(&mut message, RTA_PREFSRC, source.octets());
    message
}

/// `struct fib_rule_hdr` and its attributes for a `route_table` rule; without
/// a source it selects every such rule (for deletion).
fn rule_message(source: Option<Ipv4Addr>, route_table: u32) -> Vec<u8> {
    let source_len = if source.is_some() { 32 } else { 0 };
    let mut message = vec![AF_INET, 0, source_len, 0, 0, 0, 0, FR_ACT_TO_TBL];
    message.extend_from_slice(&0_u32.to_ne_bytes());
    if let Some(source) = source {
        attribute(&mut message, FRA_SRC, source.octets());
    }
    attribute(&mut message, FRA_PRIORITY, route_table.to_ne_bytes());
    attribute(&mut message, FRA_TABLE, route_table.to_ne_bytes());
    message
}

/// A route-netlink socket for sequential acknowledged requests.
struct Netlink {
    socket: Socket,
    sequence: u32,
}

impl Netlink {
    fn open() -> Result<Self, LinuxError> {
        let socket = Socket::new(NETLINK_ROUTE)
            .and_then(|socket| socket.connect(&SocketAddr::new(0, 0)).map(|()| socket))
            .map_err(|error| LinuxError::Netlink(format!("open netlink socket: {error}")))?;
        Ok(Self {
            socket,
            sequence: 0,
        })
    }

    /// Sends one request and returns the kernel's acknowledgement.
    fn request(&mut self, kind: u16, flags: u16, body: &[u8]) -> io::Result<()> {
        self.sequence += 1;
        let length = u32::try_from(16 + body.len()).map_err(io::Error::other)?;
        let mut message = Vec::with_capacity(16 + body.len());
        message.extend_from_slice(&length.to_ne_bytes());
        message.extend_from_slice(&kind.to_ne_bytes());
        message.extend_from_slice(&(NLM_F_REQUEST | NLM_F_ACK | flags).to_ne_bytes());
        message.extend_from_slice(&self.sequence.to_ne_bytes());
        message.extend_from_slice(&0_u32.to_ne_bytes());
        message.extend_from_slice(body);
        self.socket.send(&message, 0)?;
        let mut reply = Vec::with_capacity(4096);
        self.socket.recv(&mut reply, 0)?;
        // The only reply to an acknowledged request is `nlmsgerr`: the header,
        // then the negated errno (zero for success).
        let error = reply
            .get(16..20)
            .filter(|_| reply.get(4..6) == Some(&NLMSG_ERROR.to_ne_bytes()[..]))
            .and_then(|code| code.try_into().ok())
            .map(i32::from_ne_bytes)
            .ok_or_else(|| io::Error::other("unexpected netlink reply"))?;
        match error {
            0 => Ok(()),
            error => Err(io::Error::from_raw_os_error(-error)),
        }
    }
}

fn random_u32() -> Result<u32, LinuxError> {
    getrandom::u32().map_err(|error| LinuxError::Io(format!("random number: {error}")))
}

fn option60(config: &ClientConfig<'_>) -> Result<Vec<u8>, DhcpError> {
    let random = (u64::from(random_u32()?) & 0x7fff_ffff).to_le_bytes();
    let origin = OPTION60_ORIGIN.get_or_init(Instant::now);
    let monotonic_ms = u64::try_from(origin.elapsed().as_millis())
        .unwrap_or(u64::MAX)
        .to_le_bytes();
    Ok(ctc_option60::encode(&Option60Input {
        password: config.password,
        login: config.user,
        random,
        monotonic_ms,
    })?)
}

/// Fails before any interface change when a needed capability is missing.
fn check_capabilities() -> Result<(), LinuxError> {
    let status = fs::read_to_string("/proc/self/status")
        .map_err(|error| LinuxError::Io(format!("read capabilities: {error}")))?;
    let effective = status
        .lines()
        .find_map(|line| line.strip_prefix("CapEff:\t"))
        .and_then(|value| u64::from_str_radix(value.trim(), 16).ok())
        .ok_or_else(|| LinuxError::Io("parse capabilities".to_owned()))?;
    for (bit, name) in [(13, "CAP_NET_RAW"), (12, "CAP_NET_ADMIN")] {
        if effective & (1 << bit) == 0 {
            return Err(LinuxError::MissingCapability(name));
        }
    }
    Ok(())
}

/// Broadcasts `frame` and waits for this transaction's reply.
fn exchange(
    tx: &mut dyn DataLinkSender,
    rx: &mut dyn DataLinkReceiver,
    frame: &[u8],
    xid: u32,
    mac: MacAddress,
    kind: ReplyKind,
) -> Result<packet::DhcpReply, DhcpError> {
    tx.send_to(frame, None)
        .unwrap_or_else(|| Err(io::Error::other("frame was not sent")))
        .map_err(|error| LinuxError::Io(format!("send: {error}")))?;
    let deadline = Instant::now() + STAGE_TIMEOUT;
    while Instant::now() < deadline {
        match rx.next() {
            Ok(reply) => match packet::decode_reply(reply, xid, mac, kind) {
                Ok(reply) => return Ok(reply),
                Err(packet::PacketError::Nak) => return Err(LinuxError::Nak.into()),
                // Other traffic on the interface, including our own frames.
                Err(_) => {}
            },
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::TimedOut | io::ErrorKind::Interrupted
                ) => {}
            Err(error) => return Err(LinuxError::Io(format!("receive: {error}")).into()),
        }
    }
    Err(LinuxError::Timeout.into())
}
