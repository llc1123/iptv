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
/// Routes to the provider's networks when none are configured, as in Python.
const DEFAULT_ROUTES: [(Ipv4Addr, u8); 2] = [
    (Ipv4Addr::new(10, 223, 0, 0), 16),
    (Ipv4Addr::new(20, 248, 0, 0), 13),
];
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
    #[error("DHCP route override is invalid")]
    InvalidRoute,
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
    let config_datalink = pnet_datalink::Config {
        read_timeout: Some(RECEIVE_POLL),
        promiscuous: false,
        ..pnet_datalink::Config::default()
    };
    let (mut tx, mut rx) = match pnet_datalink::channel(&interface, config_datalink) {
        Ok(Channel::Ethernet(tx, rx)) => (tx, rx),
        Ok(_) => return Err(LinuxError::Io("unsupported datalink channel".to_owned()).into()),
        Err(error) => {
            return Err(LinuxError::Io(format!("open {}: {error}", interface.name)).into());
        }
    };
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

/// Assigns the leased address and the provider routes to `interface`, like
/// `ip addr replace` and `ip route replace ... via ... dev ... src ...`.
pub fn configure_interface(
    interface: &str,
    lease: &DhcpLease,
    routes: &[String],
) -> Result<(), DhcpError> {
    let routes = lease_routes(lease, routes)?;
    let index = find_interface(interface)?.index;
    let mut netlink = Netlink::open()?;
    let address = format!("{}/{}", lease.address, lease.prefix_length());
    netlink
        .request(RTM_NEWADDR, NLM_F_REPLACE, &address_message(index, lease))
        .map_err(|error| LinuxError::Netlink(format!("add {address}: {error}")))?;
    for route in routes {
        netlink
            .request(RTM_NEWROUTE, NLM_F_REPLACE, &route_message(index, &route))
            .map_err(|error| {
                LinuxError::Netlink(format!(
                    "route {}/{}: {error}",
                    route.destination, route.prefix
                ))
            })?;
    }
    Ok(())
}

/// Removes a lease's address from the interface, e.g. after the server handed
/// out a different one on renewal.
pub fn remove_address(interface: &str, lease: &DhcpLease) -> Result<(), DhcpError> {
    let index = find_interface(interface)?.index;
    Netlink::open()?
        .request(RTM_DELADDR, 0, &address_message(index, lease))
        .map_err(|error| LinuxError::Netlink(format!("delete {}: {error}", lease.address)))?;
    Ok(())
}

fn find_interface(name: &str) -> Result<NetworkInterface, LinuxError> {
    pnet_datalink::interfaces()
        .into_iter()
        .find(|interface| interface.name == name)
        .ok_or_else(|| LinuxError::Io(format!("interface {name} not found")))
}

#[derive(Debug, PartialEq, Eq)]
struct Route {
    destination: Ipv4Addr,
    prefix: u8,
    gateway: Option<Ipv4Addr>,
    source: Option<Ipv4Addr>,
}

/// The configured routes (`CIDR[,via=IP][,src=IP]`), or the provider
/// defaults; a route naming neither gateway nor source uses the lease's.
fn lease_routes(lease: &DhcpLease, routes: &[String]) -> Result<Vec<Route>, LinuxError> {
    let via_lease = |destination, prefix| Route {
        destination,
        prefix,
        gateway: Some(lease.router),
        source: Some(lease.address),
    };
    if routes.is_empty() {
        return Ok(DEFAULT_ROUTES
            .iter()
            .map(|&(destination, prefix)| via_lease(destination, prefix))
            .collect());
    }
    routes
        .iter()
        .map(|value| {
            let mut parts = value.split(',');
            let (destination, prefix) = parts
                .next()
                .and_then(|cidr| cidr.split_once('/'))
                .and_then(|(address, prefix)| Some((address.parse().ok()?, prefix.parse().ok()?)))
                .filter(|&(_, prefix): &(Ipv4Addr, u8)| prefix <= 32)
                .ok_or(LinuxError::InvalidRoute)?;
            let mut route = Route {
                destination,
                prefix,
                gateway: None,
                source: None,
            };
            for part in parts {
                let (key, address) = part.split_once('=').ok_or(LinuxError::InvalidRoute)?;
                let address = Some(address.parse().map_err(|_| LinuxError::InvalidRoute)?);
                match key {
                    "via" => route.gateway = address,
                    "src" => route.source = address,
                    _ => return Err(LinuxError::InvalidRoute),
                }
            }
            if route.gateway.is_none() && route.source.is_none() {
                route = via_lease(destination, prefix);
            }
            Ok(route)
        })
        .collect()
}

const RTM_NEWADDR: u16 = 20;
const RTM_DELADDR: u16 = 21;
const RTM_NEWROUTE: u16 = 24;
const NLMSG_ERROR: u16 = 2;
const NLM_F_REQUEST: u16 = 0x1;
const NLM_F_ACK: u16 = 0x4;
/// `NLM_F_REPLACE | NLM_F_CREATE`: what `ip ... replace` sends.
const NLM_F_REPLACE: u16 = 0x100 | 0x400;
const AF_INET: u8 = 2;
const IFA_ADDRESS: u16 = 1;
const IFA_LOCAL: u16 = 2;
const RTA_DST: u16 = 1;
const RTA_OIF: u16 = 4;
const RTA_GATEWAY: u16 = 5;
const RTA_PREFSRC: u16 = 7;
const RT_TABLE_MAIN: u8 = 254;
const RTPROT_BOOT: u8 = 3;
const RT_SCOPE_UNIVERSE: u8 = 0;
const RT_SCOPE_LINK: u8 = 253;
const RTN_UNICAST: u8 = 1;

/// Appends a 4-byte route attribute (`struct rtattr` plus payload).
fn attribute(message: &mut Vec<u8>, kind: u16, value: [u8; 4]) {
    message.extend_from_slice(&8_u16.to_ne_bytes());
    message.extend_from_slice(&kind.to_ne_bytes());
    message.extend_from_slice(&value);
}

/// `struct ifaddrmsg` and its attributes for the lease's address.
fn address_message(index: u32, lease: &DhcpLease) -> Vec<u8> {
    let prefix = u8::try_from(lease.prefix_length()).unwrap_or(32);
    let mut message = vec![AF_INET, prefix, 0, RT_SCOPE_UNIVERSE];
    message.extend_from_slice(&index.to_ne_bytes());
    attribute(&mut message, IFA_LOCAL, lease.address.octets());
    attribute(&mut message, IFA_ADDRESS, lease.address.octets());
    message
}

/// `struct rtmsg` and its attributes for a main-table unicast route.
fn route_message(index: u32, route: &Route) -> Vec<u8> {
    let mut message = vec![
        AF_INET,
        route.prefix,
        0,
        0,
        RT_TABLE_MAIN,
        RTPROT_BOOT,
        // A route without a gateway is directly connected, as `ip` infers.
        if route.gateway.is_some() {
            RT_SCOPE_UNIVERSE
        } else {
            RT_SCOPE_LINK
        },
        RTN_UNICAST,
        0,
        0,
        0,
        0,
    ];
    attribute(&mut message, RTA_DST, route.destination.octets());
    if let Some(gateway) = route.gateway {
        attribute(&mut message, RTA_GATEWAY, gateway.octets());
    }
    attribute(&mut message, RTA_OIF, index.to_ne_bytes());
    if let Some(source) = route.source {
        attribute(&mut message, RTA_PREFSRC, source.octets());
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn route(
        destination: [u8; 4],
        prefix: u8,
        gateway: Option<[u8; 4]>,
        source: Option<[u8; 4]>,
    ) -> Route {
        Route {
            destination: Ipv4Addr::from(destination),
            prefix,
            gateway: gateway.map(Ipv4Addr::from),
            source: source.map(Ipv4Addr::from),
        }
    }

    #[test]
    fn default_routes_match_python() {
        let lease = lease::test_lease();
        let via = Some([192, 0, 2, 1]);
        let src = Some([192, 0, 2, 44]);
        assert_eq!(
            lease_routes(&lease, &[]).unwrap(),
            [
                route([10, 223, 0, 0], 16, via, src),
                route([20, 248, 0, 0], 13, via, src)
            ]
        );
    }

    #[test]
    fn route_overrides_keep_given_fields_and_reject_unknown_keys() {
        let lease = lease::test_lease();
        let routes = [
            "198.51.100.0/24".to_owned(),
            "203.0.113.0/24,via=192.0.2.9".to_owned(),
        ];
        assert_eq!(
            lease_routes(&lease, &routes).unwrap(),
            [
                route(
                    [198, 51, 100, 0],
                    24,
                    Some([192, 0, 2, 1]),
                    Some([192, 0, 2, 44])
                ),
                route([203, 0, 113, 0], 24, Some([192, 0, 2, 9]), None),
            ]
        );
        for bad in [
            "10.0.0.0/33",
            "10.0.0.0",
            "10.0.0.0/8,dev=eth0",
            "10.0.0.0/8,via=x",
        ] {
            assert_eq!(
                lease_routes(&lease, &[bad.to_owned()]),
                Err(LinuxError::InvalidRoute)
            );
        }
    }
}
