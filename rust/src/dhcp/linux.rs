#![expect(
    unsafe_code,
    reason = "Linux AF_PACKET and interface syscalls are isolated here"
)]
use super::{
    DhcpError, lease,
    packet::{self, MacAddress, ReplyKind},
};
use crate::crypto::ctc_option60::{self, Option60Input};
use std::{
    ffi::CString,
    fmt, fs,
    io::{self, Read},
    process::{Command, Stdio},
    sync::OnceLock,
    time::{Duration, Instant},
};
use thiserror::Error;

/// How long each DISCOVER and REQUEST waits for its reply.
const STAGE_TIMEOUT: Duration = Duration::from_secs(10);
const AF_PACKET: u16 = 17;
const ETH_P_IP: u16 = 0x0800;
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
    #[error("`ip` command failed: {0}")]
    Command(String),
    #[error("DHCP route override is invalid")]
    InvalidRoute,
}

fn os_error(operation: &str) -> LinuxError {
    LinuxError::Io(format!("{operation}: {}", io::Error::last_os_error()))
}

/// Runs the DISCOVER/OFFER/REQUEST/ACK exchange on `interface`.
pub fn acquire_lease(
    interface: &str,
    config: &ClientConfig<'_>,
) -> Result<(super::DhcpLease, MacAddress), DhcpError> {
    check_capabilities()?;
    let mac: MacAddress = fs::read_to_string(format!("/sys/class/net/{interface}/address"))
        .map_err(|error| LinuxError::Io(format!("read {interface} MAC: {error}")))?
        .trim()
        .parse()
        .map_err(|_| LinuxError::Io(format!("parse {interface} MAC")))?;
    let index = interface_index(interface)?;
    let hostname = format!("{}{}", config.stb_id_prefix, mac.compact_upper());
    let xid = random_u32()?;
    let discover = packet::build_discover(mac, xid, &option60(config)?, hostname.as_bytes())?;
    let offer = exchange(index, &discover, xid, mac, ReplyKind::Offer)?;
    let request = packet::build_request(
        mac,
        xid,
        offer.address,
        offer.server.unwrap_or(offer.source),
        &option60(config)?,
        hostname.as_bytes(),
    )?;
    let acquired_at = Instant::now();
    let ack = exchange(index, &request, xid, mac, ReplyKind::Ack)?;
    Ok((lease::from_ack(ack, acquired_at)?, mac))
}

/// Assigns the leased address and the provider routes to `interface`.
pub fn configure_interface(
    interface: &str,
    lease: &super::DhcpLease,
    routes: &[String],
) -> Result<(), DhcpError> {
    for command in route_commands(interface, lease, routes)? {
        run_ip_command(&command)?;
    }
    Ok(())
}

/// Removes a lease's address from the interface, e.g. after the server handed
/// out a different one on renewal.
pub fn remove_address(interface: &str, lease: &super::DhcpLease) -> Result<(), DhcpError> {
    let address = format!("{}/{}", lease.address, lease.prefix_length());
    run_ip_command(&["ip", "addr", "del", &address, "dev", interface].map(str::to_owned))?;
    Ok(())
}

fn route_commands(
    interface: &str,
    lease: &super::DhcpLease,
    routes: &[String],
) -> Result<Vec<Vec<String>>, LinuxError> {
    let mut commands = vec![vec![
        "ip".into(),
        "addr".into(),
        "replace".into(),
        format!("{}/{}", lease.address, lease.prefix_length()),
        "dev".into(),
        interface.into(),
    ]];
    if routes.is_empty() {
        for cidr in ["10.223.0.0/16", "20.248.0.0/13"] {
            commands.push(vec![
                "ip".into(),
                "route".into(),
                "replace".into(),
                cidr.into(),
                "via".into(),
                lease.router.to_string(),
                "dev".into(),
                interface.into(),
                "src".into(),
                lease.address.to_string(),
            ]);
        }
    } else {
        commands.extend(
            routes
                .iter()
                .map(|route| parse_route(route, interface, lease))
                .collect::<Result<Vec<_>, _>>()?,
        );
    }
    Ok(commands)
}
fn parse_route(
    value: &str,
    interface: &str,
    lease: &super::DhcpLease,
) -> Result<Vec<String>, LinuxError> {
    let mut parts = value.split(',');
    let cidr = parts
        .next()
        .filter(|part| is_cidr(part))
        .ok_or(LinuxError::InvalidRoute)?;
    let mut command = vec!["ip".into(), "route".into(), "replace".into(), cidr.into()];
    for part in parts {
        let (key, value) = part.split_once('=').ok_or(LinuxError::InvalidRoute)?;
        if key != "via" && key != "src" {
            return Err(LinuxError::InvalidRoute);
        }
        value
            .parse::<std::net::Ipv4Addr>()
            .map_err(|_| LinuxError::InvalidRoute)?;
        command.extend([key.into(), value.into()]);
    }
    command.extend(["dev".into(), interface.into()]);
    if command.len() == 6 {
        command.extend([
            "via".into(),
            lease.router.to_string(),
            "src".into(),
            lease.address.to_string(),
        ]);
    }
    Ok(command)
}
fn is_cidr(value: &str) -> bool {
    let Some((address, prefix)) = value.split_once('/') else {
        return false;
    };
    address.parse::<std::net::Ipv4Addr>().is_ok()
        && prefix.parse::<u8>().is_ok_and(|value| value <= 32)
}
fn random_u32() -> Result<u32, LinuxError> {
    let mut bytes = [0; 4];
    fs::File::open("/dev/urandom")
        .and_then(|mut file| file.read_exact(&mut bytes))
        .map_err(|error| LinuxError::Io(format!("read /dev/urandom: {error}")))?;
    Ok(u32::from_ne_bytes(bytes))
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
    index: i32,
    frame: &[u8],
    xid: u32,
    mac: MacAddress,
    kind: ReplyKind,
) -> Result<packet::DhcpReply, DhcpError> {
    let socket = RawPacketSocket::open(index)?;
    socket.send(frame, index)?;
    let deadline = Instant::now() + STAGE_TIMEOUT;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(LinuxError::Timeout.into());
        }
        match packet::decode_reply(&socket.receive(remaining)?, xid, mac, kind) {
            Ok(reply) => return Ok(reply),
            Err(packet::PacketError::Nak) => return Err(LinuxError::Nak.into()),
            // Other traffic on the interface.
            Err(_) => {}
        }
    }
}

fn interface_index(interface: &str) -> Result<i32, LinuxError> {
    let name = CString::new(interface)
        .map_err(|_| LinuxError::Io(format!("interface name {interface:?}")))?;
    // SAFETY: `name` is a NUL-terminated interface name owned by this call.
    let index = unsafe { libc::if_nametoindex(name.as_ptr()) };
    i32::try_from(index)
        .ok()
        .filter(|index| *index > 0)
        .ok_or_else(|| os_error(&format!("find interface {interface}")))
}

fn link_address(index: i32, destination: Option<[u8; 8]>) -> libc::sockaddr_ll {
    libc::sockaddr_ll {
        sll_family: AF_PACKET,
        sll_protocol: ETH_P_IP.to_be(),
        sll_ifindex: index,
        sll_hatype: u16::from(destination.is_some()),
        sll_pkttype: 0,
        sll_halen: if destination.is_some() { 6 } else { 0 },
        sll_addr: destination.unwrap_or_default(),
    }
}

#[expect(clippy::cast_possible_truncation, reason = "sockaddr_ll is 20 bytes")]
const LINK_ADDRESS_LEN: libc::socklen_t =
    std::mem::size_of::<libc::sockaddr_ll>() as libc::socklen_t;

struct RawPacketSocket {
    fd: libc::c_int,
}

impl RawPacketSocket {
    fn open(index: i32) -> Result<Self, LinuxError> {
        // SAFETY: libc receives only constants and no borrowed Rust pointers.
        let fd = unsafe {
            libc::socket(
                libc::AF_PACKET,
                libc::SOCK_RAW | libc::SOCK_CLOEXEC,
                i32::from(ETH_P_IP.to_be()),
            )
        };
        if fd < 0 {
            return Err(os_error("open raw socket"));
        }
        let socket = Self { fd };
        let address = link_address(index, None);
        // SAFETY: `address` is initialized and `LINK_ADDRESS_LEN` is its exact size.
        if unsafe {
            libc::bind(
                socket.fd,
                std::ptr::from_ref(&address).cast(),
                LINK_ADDRESS_LEN,
            )
        } < 0
        {
            return Err(os_error("bind raw socket"));
        }
        Ok(socket)
    }

    fn send(&self, frame: &[u8], index: i32) -> Result<(), LinuxError> {
        let address = link_address(index, Some([0xff; 8]));
        // SAFETY: `frame` and `address` remain valid for the synchronous syscall.
        let sent = unsafe {
            libc::sendto(
                self.fd,
                frame.as_ptr().cast(),
                frame.len(),
                0,
                std::ptr::from_ref(&address).cast(),
                LINK_ADDRESS_LEN,
            )
        };
        if usize::try_from(sent).ok() != Some(frame.len()) {
            return Err(os_error("send"));
        }
        Ok(())
    }

    fn receive(&self, timeout: Duration) -> Result<Vec<u8>, LinuxError> {
        let millis = i32::try_from(timeout.as_millis()).unwrap_or(i32::MAX);
        loop {
            let mut poll = libc::pollfd {
                fd: self.fd,
                events: libc::POLLIN,
                revents: 0,
            };
            // SAFETY: `poll` is a valid one-element descriptor array.
            let ready = unsafe { libc::poll(&raw mut poll, 1, millis) };
            if ready == 0 {
                return Err(LinuxError::Timeout);
            }
            let mut buffer = vec![0; 2048];
            let received = if ready < 0 {
                -1
            } else {
                // SAFETY: `buffer` is valid writable storage for its declared length.
                unsafe { libc::recv(self.fd, buffer.as_mut_ptr().cast(), buffer.len(), 0) }
            };
            if let Ok(received) = usize::try_from(received) {
                buffer.truncate(received);
                return Ok(buffer);
            }
            if io::Error::last_os_error().kind() != io::ErrorKind::Interrupted {
                return Err(os_error("receive"));
            }
        }
    }
}

impl Drop for RawPacketSocket {
    fn drop(&mut self) {
        // SAFETY: this object owns the descriptor and closes it exactly once.
        unsafe { libc::close(self.fd) };
    }
}

fn run_ip_command(argv: &[String]) -> Result<(), LinuxError> {
    let status = Command::new(&argv[0])
        .args(&argv[1..])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|error| LinuxError::Command(format!("{}: {error}", argv.join(" "))))?;
    if status.success() {
        Ok(())
    } else {
        Err(LinuxError::Command(format!("{}: {status}", argv.join(" "))))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_routes_match_python_argv() {
        let lease = super::super::lease::test_lease();
        assert_eq!(
            route_commands("eth0", &lease, &[]).unwrap_or_default(),
            vec![
                vec!["ip", "addr", "replace", "192.0.2.44/24", "dev", "eth0"],
                vec![
                    "ip",
                    "route",
                    "replace",
                    "10.223.0.0/16",
                    "via",
                    "192.0.2.1",
                    "dev",
                    "eth0",
                    "src",
                    "192.0.2.44"
                ],
                vec![
                    "ip",
                    "route",
                    "replace",
                    "20.248.0.0/13",
                    "via",
                    "192.0.2.1",
                    "dev",
                    "eth0",
                    "src",
                    "192.0.2.44"
                ]
            ]
        );
    }
}
