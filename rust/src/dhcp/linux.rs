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
pub const STAGE_TIMEOUT: Duration = Duration::from_secs(10);
const COMMAND_POLL: Duration = Duration::from_millis(10);
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
            .field("stb_id_prefix", &"[REDACTED]")
            .finish()
    }
}
#[derive(Debug, Error, PartialEq, Eq)]
pub enum LinuxError {
    #[error("DHCP interface name is invalid")]
    InvalidInterface,
    #[error("required Linux network capability is unavailable: {0}")]
    MissingCapability(&'static str),
    #[error("Linux DHCP operation timed out")]
    Timeout,
    #[error("Linux DHCP platform I/O failed")]
    Io,
    #[error("DHCP server rejected the request (NAK)")]
    Nak,
    #[error("Linux interface setup command failed")]
    Command,
    #[error("DHCP route override is invalid")]
    InvalidRoute,
}
pub fn acquire_lease(
    interface: &str,
    config: &ClientConfig<'_>,
) -> Result<(super::DhcpLease, MacAddress), DhcpError> {
    let platform = LinuxPlatform::new(interface)?;
    LinuxPlatform::preflight()?;
    let mac = platform.interface_mac()?;
    let hostname = format!("{}{}", config.stb_id_prefix, mac.compact_upper());
    let xid = random_u32().map_err(|_| LinuxError::Io)?;
    let discover_option60 = option60(config)?;
    let discover = packet::build_discover(mac, xid, &discover_option60, hostname.as_bytes())?;
    let offer_frame = platform.exchange(&discover, xid, mac, ReplyKind::Offer, STAGE_TIMEOUT)?;
    let offer = packet::decode_reply(&offer_frame, xid, mac, ReplyKind::Offer)?;
    let request_option60 = option60(config)?;
    let request = packet::build_request(
        mac,
        xid,
        offer.address,
        offer.server.unwrap_or(offer.source),
        &request_option60,
        hostname.as_bytes(),
    )?;
    let acquired_at = Instant::now();
    let ack_frame = platform.exchange(&request, xid, mac, ReplyKind::Ack, STAGE_TIMEOUT)?;
    let ack = packet::decode_reply(&ack_frame, xid, mac, ReplyKind::Ack)?;
    Ok((lease::from_ack(ack, acquired_at)?, mac))
}
pub fn configure_interface(
    interface: &str,
    lease: &super::DhcpLease,
    routes: &[String],
) -> Result<(), DhcpError> {
    LinuxPlatform::new(interface)?;
    LinuxPlatform::preflight()?;
    for command in route_commands(interface, lease, routes)? {
        run_ip_command(&command, STAGE_TIMEOUT)?;
    }
    Ok(())
}
fn route_commands(
    interface: &str,
    lease: &super::DhcpLease,
    routes: &[String],
) -> Result<Vec<Vec<String>>, LinuxError> {
    if interface.is_empty() {
        return Err(LinuxError::InvalidInterface);
    }
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
fn random_u32() -> io::Result<u32> {
    let mut bytes = [0; 4];
    fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(u32::from_ne_bytes(bytes))
}
fn option60(config: &ClientConfig<'_>) -> Result<Vec<u8>, DhcpError> {
    let random = (u64::from(random_u32().map_err(|_| LinuxError::Io)?) & 0x7fff_ffff).to_le_bytes();
    let origin = OPTION60_ORIGIN.get_or_init(Instant::now);
    let monotonic_ms = u64::try_from(origin.elapsed().as_millis())
        .unwrap_or(u64::MAX)
        .to_le_bytes();
    ctc_option60::encode(&Option60Input {
        password: config.password,
        login: config.user,
        random,
        monotonic_ms,
    })
    .map_err(DhcpError::from)
}
struct LinuxPlatform {
    interface: String,
}
impl LinuxPlatform {
    fn new(interface: &str) -> Result<Self, LinuxError> {
        if interface.is_empty()
            || !interface
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"_.-".contains(&byte))
        {
            return Err(LinuxError::InvalidInterface);
        }
        Ok(Self {
            interface: interface.to_owned(),
        })
    }

    fn interface_mac(&self) -> Result<MacAddress, LinuxError> {
        fs::read_to_string(format!("/sys/class/net/{}/address", self.interface))
            .map_err(|_| LinuxError::Io)?
            .trim()
            .parse()
            .map_err(|_| LinuxError::Io)
    }

    fn preflight() -> Result<(), LinuxError> {
        let status = fs::read_to_string("/proc/self/status").map_err(|_| LinuxError::Io)?;
        let effective = status
            .lines()
            .find_map(|line| line.strip_prefix("CapEff:\t"))
            .ok_or(LinuxError::Io)?;
        let effective = u64::from_str_radix(effective.trim(), 16).map_err(|_| LinuxError::Io)?;
        if effective & (1_u64 << 13) == 0 {
            return Err(LinuxError::MissingCapability("CAP_NET_RAW"));
        }
        if effective & (1_u64 << 12) == 0 {
            return Err(LinuxError::MissingCapability("CAP_NET_ADMIN"));
        }
        Ok(())
    }

    fn exchange(
        &self,
        frame: &[u8],
        xid: u32,
        mac: MacAddress,
        kind: ReplyKind,
        timeout: Duration,
    ) -> Result<Vec<u8>, LinuxError> {
        let index = interface_index(&self.interface)?;
        let socket = RawPacketSocket::open(index)?;
        socket.send(frame, index)?;
        let deadline = Instant::now()
            .checked_add(timeout)
            .ok_or(LinuxError::Timeout)?;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(LinuxError::Timeout);
            }
            let reply = socket.receive(remaining)?;
            match packet::matching_reply(&reply, xid, mac, kind) {
                Ok(true) => return Ok(reply),
                Err(packet::PacketError::Nak) => return Err(LinuxError::Nak),
                Ok(false) | Err(_) => {}
            }
        }
    }
}
fn interface_index(interface: &str) -> Result<i32, LinuxError> {
    let name = CString::new(interface).map_err(|_| LinuxError::InvalidInterface)?;
    // SAFETY: `name` is a NUL-terminated interface name owned by this call.
    let index = unsafe { libc::if_nametoindex(name.as_ptr()) };
    i32::try_from(index)
        .ok()
        .filter(|index| *index > 0)
        .ok_or(LinuxError::Io)
}
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
                libc::ETH_P_IP.to_be(),
            )
        };
        if fd < 0 {
            return Err(LinuxError::Io);
        }
        let socket = Self { fd };
        let address = libc::sockaddr_ll {
            sll_family: u16::try_from(libc::AF_PACKET).map_err(|_| LinuxError::Io)?,
            sll_protocol: u16::try_from(libc::ETH_P_IP)
                .map_err(|_| LinuxError::Io)?
                .to_be(),
            sll_ifindex: index,
            sll_hatype: 0,
            sll_pkttype: 0,
            sll_halen: 0,
            sll_addr: [0; 8],
        };
        let length = libc::socklen_t::try_from(std::mem::size_of::<libc::sockaddr_ll>())
            .map_err(|_| LinuxError::Io)?;
        // SAFETY: `address` is initialized and `length` is its exact size.
        if unsafe { libc::bind(socket.fd, std::ptr::from_ref(&address).cast(), length) } < 0 {
            return Err(LinuxError::Io);
        }
        Ok(socket)
    }

    fn send(&self, frame: &[u8], index: i32) -> Result<(), LinuxError> {
        let address = libc::sockaddr_ll {
            sll_family: u16::try_from(libc::AF_PACKET).map_err(|_| LinuxError::Io)?,
            sll_protocol: u16::try_from(libc::ETH_P_IP)
                .map_err(|_| LinuxError::Io)?
                .to_be(),
            sll_ifindex: index,
            sll_hatype: 1,
            sll_pkttype: 0,
            sll_halen: 6,
            sll_addr: [255; 8],
        };
        let length = libc::socklen_t::try_from(std::mem::size_of::<libc::sockaddr_ll>())
            .map_err(|_| LinuxError::Io)?;
        // SAFETY: `frame` and `address` remain valid for the synchronous syscall.
        let sent = unsafe {
            libc::sendto(
                self.fd,
                frame.as_ptr().cast(),
                frame.len(),
                0,
                std::ptr::from_ref(&address).cast(),
                length,
            )
        };
        if sent < 0 || usize::try_from(sent).ok() != Some(frame.len()) {
            return Err(LinuxError::Io);
        }
        Ok(())
    }

    fn receive(&self, timeout: Duration) -> Result<Vec<u8>, LinuxError> {
        let millis = i32::try_from(timeout.as_millis()).map_err(|_| LinuxError::Timeout)?;
        loop {
            let mut poll = libc::pollfd {
                fd: self.fd,
                events: libc::POLLIN,
                revents: 0,
            };
            // SAFETY: `poll` receives a valid one-element descriptor array.
            let ready = unsafe { libc::poll(&raw mut poll, 1, millis) };
            if ready == 0 {
                return Err(LinuxError::Timeout);
            }
            if ready < 0 {
                if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(LinuxError::Io);
            }
            let mut buffer = vec![0; 2048];
            // SAFETY: `buffer` is valid writable storage for its declared length.
            let received =
                unsafe { libc::recv(self.fd, buffer.as_mut_ptr().cast(), buffer.len(), 0) };
            if received < 0 && io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
                continue;
            }
            let received = usize::try_from(received).map_err(|_| LinuxError::Io)?;
            buffer.truncate(received);
            return Ok(buffer);
        }
    }
}
impl Drop for RawPacketSocket {
    fn drop(&mut self) {
        // SAFETY: this object owns the descriptor and closes it exactly once.
        if unsafe { libc::close(self.fd) } < 0 {
            tracing::debug!("DHCP raw socket close failed");
        }
    }
}
fn run_ip_command(argv: &[String], timeout: Duration) -> Result<(), LinuxError> {
    let executable = argv.first().ok_or(LinuxError::Command)?;
    let mut child = Command::new(executable)
        .args(&argv[1..])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| LinuxError::Io)?;
    let deadline = Instant::now()
        .checked_add(timeout)
        .ok_or(LinuxError::Timeout)?;
    loop {
        match child.try_wait().map_err(|_| LinuxError::Io)? {
            Some(status) if status.success() => return Ok(()),
            Some(_) => return Err(LinuxError::Command),
            None if Instant::now() >= deadline => {
                child.kill().map_err(|_| LinuxError::Io)?;
                child.wait().map_err(|_| LinuxError::Io)?;
                return Err(LinuxError::Timeout);
            }
            None => std::thread::sleep(COMMAND_POLL),
        }
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
