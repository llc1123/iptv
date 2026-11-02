//! Linux raw-L2 transport, capability preflight, and interface setup.

#![expect(
    unsafe_code,
    reason = "Linux AF_PACKET syscalls are isolated in this audited module"
)]
#![allow(clippy::missing_errors_doc)]

use std::ffi::CString;
use std::fs;
use std::io::{self, Read};
use std::net::Ipv4Addr;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use thiserror::Error;

use super::lease::{self, DhcpLease};
use super::packet::{self, MacAddress, ReplyKind};
use super::{DhcpError, LifecycleCause, LifecycleResult};
use crate::crypto::ctc_option60::{self, Option60Input};

/// Every DHCP and `ip` stage has this upper bound.
pub const STAGE_TIMEOUT: Duration = Duration::from_secs(10);

/// The capability needed before a raw transmit or interface mutation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Capability {
    /// Linux capability 13.
    NetRaw,
    /// Linux capability 12.
    NetAdmin,
}

/// Platform failures with redacted, stable messages.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum LinuxError {
    /// The selected interface name is empty or unsafe for a sysfs lookup.
    #[error("DHCP interface name is invalid")]
    InvalidInterface,
    /// A required Linux capability was not present.
    #[error("required Linux network capability is unavailable")]
    MissingCapability(Capability),
    /// The raw socket stage timed out.
    #[error("Linux DHCP stage timed out")]
    Timeout,
    /// A platform I/O operation failed.
    #[error("Linux DHCP platform I/O failed")]
    Io,
    /// An `ip` command failed without exposing command output.
    #[error("Linux interface setup command failed")]
    CommandFailed,
    /// The route grammar is not the supported operator grammar.
    #[error("DHCP route override is invalid")]
    InvalidRoute,
}

/// The setup stage that owns a command or receive operation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SetupStage {
    /// Address mutation.
    Address,
    /// A route mutation.
    Route,
}

/// Injectable platform boundary used by the client and unit tests.
pub trait Platform {
    /// Checks both capabilities before any packet or mutation.
    fn preflight(&mut self) -> Result<(), LinuxError>;
    /// Sends one frame and receives one bounded reply.
    fn exchange(&mut self, frame: &[u8], timeout: Duration) -> Result<Vec<u8>, LinuxError>;
    /// Runs one already-tokenized command without a shell.
    fn run_command(&mut self, argv: &[String], timeout: Duration) -> Result<(), LinuxError>;
}

/// Configuration for one DHCP transaction.
#[derive(Clone, Debug)]
pub struct ClientConfig<'a> {
    /// Interface Ethernet address.
    pub mac: MacAddress,
    /// CTC DHCP login.
    pub user: &'a str,
    /// CTC DHCP password.
    pub password: &'a str,
    /// BOOTP hostname derived from the interface identity.
    pub hostname: &'a [u8],
}

/// DHCP client over an injectable Linux platform.
pub struct DhcpClient<'a, P> {
    platform: P,
    config: ClientConfig<'a>,
}

impl<'a, P: Platform> DhcpClient<'a, P> {
    /// Creates a client. The caller owns the platform boundary.
    #[must_use]
    pub const fn new(platform: P, config: ClientConfig<'a>) -> Self {
        Self { platform, config }
    }

    /// Acquires one lease without renewal.
    pub fn acquire(&mut self) -> Result<DhcpLease, DhcpError> {
        self.platform.preflight()?;
        let xid = random_u32().map_err(|_| LinuxError::Io)?;
        let discover_option60 = option60(self.config.user, self.config.password)?;
        let discover = packet::build_discover(
            self.config.mac,
            xid,
            &discover_option60,
            self.config.hostname,
        )?;
        let offer_frame = self.platform.exchange(&discover, STAGE_TIMEOUT)?;
        let offer = packet::decode_reply(&offer_frame, xid, self.config.mac, ReplyKind::Offer)?;
        let server = offer.source;
        let request_option60 = option60(self.config.user, self.config.password)?;
        let request = packet::build_request(
            self.config.mac,
            xid,
            offer.address,
            server,
            &request_option60,
            self.config.hostname,
        )?;
        let ack_frame = self.platform.exchange(&request, STAGE_TIMEOUT)?;
        let ack = packet::decode_reply(&ack_frame, xid, self.config.mac, ReplyKind::Ack)?;
        lease::from_ack(ack, Instant::now()).map_err(DhcpError::from)
    }
}

/// Applies address and route mutations without rollback.
pub fn configure_interface<P: Platform>(
    platform: &mut P,
    interface: &str,
    lease: &DhcpLease,
    routes: &[String],
) -> Result<LifecycleResult, SetupError> {
    platform.preflight().map_err(SetupError::Platform)?;
    let commands = route_commands(interface, lease, routes).map_err(SetupError::Platform)?;
    let mut applied = false;
    for (index, command) in commands.iter().enumerate() {
        let stage = if index == 0 {
            SetupStage::Address
        } else {
            SetupStage::Route
        };
        match platform.run_command(command, STAGE_TIMEOUT) {
            Ok(()) => applied = true,
            Err(_error) if applied => {
                return Ok(LifecycleResult::Preserved {
                    lease: lease.clone(),
                    cause: LifecycleCause::StartupFailure,
                    failed_stage: Some(stage),
                });
            }
            Err(error) => return Err(SetupError::Platform(error)),
        }
    }
    Ok(LifecycleResult::Applied(lease.clone()))
}

/// Setup failures that occur before or after the first mutation.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum SetupError {
    /// Platform preflight or command failure.
    #[error(transparent)]
    Platform(#[from] LinuxError),
}

fn route_commands(
    interface: &str,
    lease: &DhcpLease,
    routes: &[String],
) -> Result<Vec<Vec<String>>, LinuxError> {
    if interface.is_empty() {
        return Err(LinuxError::InvalidInterface);
    }
    let mut commands = vec![vec![
        "ip".to_owned(),
        "addr".to_owned(),
        "replace".to_owned(),
        format!("{}/{}", lease.address, lease.prefix_length()),
        "dev".to_owned(),
        interface.to_owned(),
    ]];
    let specs = if routes.is_empty() {
        vec![RouteSpec::default_first(), RouteSpec::default_second()]
    } else {
        routes
            .iter()
            .map(|route| route.parse())
            .collect::<Result<Vec<_>, _>>()?
    };
    commands.extend(specs.into_iter().map(|route| route.argv(interface, lease)));
    Ok(commands)
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct RouteSpec {
    cidr: String,
    via: Option<Ipv4Addr>,
    src: Option<Ipv4Addr>,
    defaults: bool,
}

impl RouteSpec {
    fn default_first() -> Self {
        Self {
            cidr: "10.223.0.0/16".to_owned(),
            via: None,
            src: None,
            defaults: true,
        }
    }
    fn default_second() -> Self {
        Self {
            cidr: "20.248.0.0/13".to_owned(),
            via: None,
            src: None,
            defaults: true,
        }
    }
    fn argv(self, interface: &str, lease: &DhcpLease) -> Vec<String> {
        let mut argv = vec![
            "ip".to_owned(),
            "route".to_owned(),
            "replace".to_owned(),
            self.cidr,
        ];
        if let Some(via) = self.via.or_else(|| self.defaults.then_some(lease.router)) {
            argv.extend(["via".to_owned(), via.to_string()]);
        }
        argv.extend(["dev".to_owned(), interface.to_owned()]);
        if let Some(src) = self.src.or_else(|| self.defaults.then_some(lease.address)) {
            argv.extend(["src".to_owned(), src.to_string()]);
        }
        argv
    }
}

impl std::str::FromStr for RouteSpec {
    type Err = LinuxError;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let mut parts = value.split(',');
        let cidr = parts
            .next()
            .filter(|part| is_cidr(part))
            .ok_or(LinuxError::InvalidRoute)?
            .to_owned();
        let via = parts
            .next()
            .map(|part| parse_route_part(part, "via="))
            .transpose()?;
        let src = parts
            .next()
            .map(|part| parse_route_part(part, "src="))
            .transpose()?;
        if parts.next().is_some() {
            return Err(LinuxError::InvalidRoute);
        }
        Ok(Self {
            cidr,
            via,
            src,
            defaults: false,
        })
    }
}

fn parse_route_part(part: &str, prefix: &str) -> Result<Ipv4Addr, LinuxError> {
    let value = part.strip_prefix(prefix).ok_or(LinuxError::InvalidRoute)?;
    value.parse().map_err(|_| LinuxError::InvalidRoute)
}

fn is_cidr(value: &str) -> bool {
    let Some((address, prefix)) = value.split_once('/') else {
        return false;
    };
    address.parse::<Ipv4Addr>().is_ok() && prefix.parse::<u8>().is_ok_and(|value| value <= 32)
}

fn random_u32() -> io::Result<u32> {
    let mut bytes = [0_u8; 4];
    fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(u32::from_ne_bytes(bytes))
}

fn option60(user: &str, password: &str) -> Result<Vec<u8>, DhcpError> {
    let random = (u64::from(random_u32().map_err(|_| LinuxError::Io)?) & 0x7fff_ffff).to_le_bytes();
    let timestamp = monotonic_millis().to_le_bytes();
    ctc_option60::encode(&Option60Input {
        password,
        login: user,
        random,
        monotonic_ms: timestamp,
    })
    .map_err(DhcpError::from)
}

fn monotonic_millis() -> u64 {
    static BASE: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    let elapsed = BASE.get_or_init(Instant::now).elapsed();
    match u64::try_from(elapsed.as_millis()) {
        Ok(value) => value,
        Err(_) => u64::MAX,
    }
}

/// The real Linux platform implementation.
#[derive(Clone, Debug)]
pub struct LinuxPlatform {
    interface: String,
}

impl LinuxPlatform {
    /// Creates a platform bound to one interface name.
    pub fn new(interface: &str) -> Result<Self, LinuxError> {
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

    /// Reads the selected interface's Ethernet address without mutation.
    pub fn interface_mac(&self) -> Result<MacAddress, LinuxError> {
        let text = fs::read_to_string(format!("/sys/class/net/{}/address", self.interface))
            .map_err(|_| LinuxError::Io)?;
        text.trim().parse().map_err(|_| LinuxError::Io)
    }
}

impl Platform for LinuxPlatform {
    fn preflight(&mut self) -> Result<(), LinuxError> {
        let status = fs::read_to_string("/proc/self/status").map_err(|_| LinuxError::Io)?;
        let effective = status
            .lines()
            .find_map(|line| line.strip_prefix("CapEff:\t"))
            .ok_or(LinuxError::Io)?;
        let effective = u64::from_str_radix(effective.trim(), 16).map_err(|_| LinuxError::Io)?;
        if effective & (1_u64 << 13) == 0 {
            return Err(LinuxError::MissingCapability(Capability::NetRaw));
        }
        if effective & (1_u64 << 12) == 0 {
            return Err(LinuxError::MissingCapability(Capability::NetAdmin));
        }
        Ok(())
    }

    fn exchange(&mut self, frame: &[u8], timeout: Duration) -> Result<Vec<u8>, LinuxError> {
        let index = interface_index(&self.interface)?;
        let socket = RawPacketSocket::open(index)?;
        socket.send(frame, index)?;
        socket.receive(timeout)
    }

    fn run_command(&mut self, argv: &[String], timeout: Duration) -> Result<(), LinuxError> {
        run_ip_command(argv, timeout)
    }
}

fn interface_index(interface: &str) -> Result<i32, LinuxError> {
    let name = CString::new(interface).map_err(|_| LinuxError::InvalidInterface)?;
    // SAFETY: `name` is a NUL-terminated interface name owned by this call.
    let index = unsafe { libc::if_nametoindex(name.as_ptr()) };
    i32::try_from(index)
        .map_err(|_| LinuxError::Io)
        .and_then(|index| {
            if index > 0 {
                Ok(index)
            } else {
                Err(LinuxError::Io)
            }
        })
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
        // SAFETY: `address` is a fully initialized sockaddr_ll with its exact size.
        let result = unsafe { libc::bind(socket.fd, std::ptr::from_ref(&address).cast(), length) };
        if result < 0 {
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
            sll_addr: [0xff; 8],
        };
        let length = libc::socklen_t::try_from(std::mem::size_of::<libc::sockaddr_ll>())
            .map_err(|_| LinuxError::Io)?;
        // SAFETY: `frame` remains borrowed for the duration of the synchronous syscall.
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
        let mut poll = libc::pollfd {
            fd: self.fd,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: `poll` is a valid one-element pollfd array for this synchronous call.
        let ready = unsafe { libc::poll(&raw mut poll, 1, millis) };
        if ready == 0 {
            return Err(LinuxError::Timeout);
        }
        if ready < 0 {
            return Err(LinuxError::Io);
        }
        let mut buffer = vec![0_u8; 2048];
        // SAFETY: `buffer` is valid writable storage for the requested receive length.
        let received = unsafe { libc::recv(self.fd, buffer.as_mut_ptr().cast(), buffer.len(), 0) };
        let received = usize::try_from(received).map_err(|_| LinuxError::Io)?;
        buffer.truncate(received);
        Ok(buffer)
    }
}

impl Drop for RawPacketSocket {
    fn drop(&mut self) {
        // SAFETY: this Drop implementation owns the descriptor and closes it exactly once.
        let result = unsafe { libc::close(self.fd) };
        if result < 0 {
            tracing::debug!("DHCP raw socket close failed");
        }
    }
}

fn run_ip_command(argv: &[String], timeout: Duration) -> Result<(), LinuxError> {
    let executable = argv.first().ok_or(LinuxError::CommandFailed)?;
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
            Some(_) => return Err(LinuxError::CommandFailed),
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(LinuxError::Timeout);
            }
            None => thread::sleep(Duration::from_millis(10)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct MockPlatform {
        preflight_error: Option<LinuxError>,
        exchange_result: Option<Result<Vec<u8>, LinuxError>>,
        exchange_timeouts: Vec<Duration>,
        command_results: Vec<Result<(), LinuxError>>,
        commands: Vec<Vec<String>>,
    }

    impl Platform for MockPlatform {
        fn preflight(&mut self) -> Result<(), LinuxError> {
            self.preflight_error.take().map_or(Ok(()), Err)
        }

        fn exchange(&mut self, _frame: &[u8], timeout: Duration) -> Result<Vec<u8>, LinuxError> {
            self.exchange_timeouts.push(timeout);
            self.exchange_result
                .take()
                .map_or(Err(LinuxError::Io), |result| result)
        }

        fn run_command(&mut self, argv: &[String], _timeout: Duration) -> Result<(), LinuxError> {
            self.commands.push(argv.to_vec());
            if self.command_results.is_empty() {
                Ok(())
            } else {
                self.command_results.remove(0)
            }
        }
    }

    fn lease() -> DhcpLease {
        lease::test_lease()
    }

    #[test]
    fn default_routes_match_python_argv() {
        let commands = route_commands("eth0", &lease(), &[]).unwrap_or_default();
        assert_eq!(
            commands,
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
                ],
            ]
        );
    }

    #[test]
    fn supplied_routes_replace_defaults_and_reject_extra_syntax() {
        let commands = route_commands(
            "eth0",
            &lease(),
            &["203.0.113.0/24,via=192.0.2.1".to_owned()],
        )
        .unwrap_or_default();
        assert_eq!(
            commands,
            vec![
                vec!["ip", "addr", "replace", "192.0.2.44/24", "dev", "eth0"],
                vec![
                    "ip",
                    "route",
                    "replace",
                    "203.0.113.0/24",
                    "via",
                    "192.0.2.1",
                    "dev",
                    "eth0"
                ],
            ]
        );
        assert_eq!(
            route_commands(
                "eth0",
                &lease(),
                &["203.0.113.0/24,foo=192.0.2.1".to_owned()]
            ),
            Err(LinuxError::InvalidRoute)
        );
        assert_eq!(
            route_commands(
                "eth0",
                &lease(),
                &["203.0.113.0/24,via=192.0.2.1,src=192.0.2.2,extra=x".to_owned()]
            ),
            Err(LinuxError::InvalidRoute)
        );
    }

    #[test]
    fn capability_failure_precedes_exchange_and_setup_mutation() {
        let mut client = DhcpClient::new(
            MockPlatform {
                preflight_error: Some(LinuxError::MissingCapability(Capability::NetRaw)),
                ..MockPlatform::default()
            },
            ClientConfig {
                mac: MacAddress::new([2, 0, 0, 0, 0, 1]),
                user: "user",
                password: "password",
                hostname: b"STB",
            },
        );
        let acquisition = client.acquire();
        assert_eq!(
            acquisition,
            Err(DhcpError::Platform(LinuxError::MissingCapability(
                Capability::NetRaw
            )))
        );
        let mut setup = MockPlatform {
            preflight_error: Some(LinuxError::MissingCapability(Capability::NetAdmin)),
            ..MockPlatform::default()
        };
        assert_eq!(
            configure_interface(&mut setup, "eth0", &lease(), &[]),
            Err(SetupError::Platform(LinuxError::MissingCapability(
                Capability::NetAdmin
            )))
        );
        assert!(setup.commands.is_empty());
    }

    #[test]
    fn stage_timeout_is_injected_and_post_mutation_state_is_preserved() {
        let mut client = DhcpClient::new(
            MockPlatform {
                exchange_result: Some(Err(LinuxError::Timeout)),
                ..MockPlatform::default()
            },
            ClientConfig {
                mac: MacAddress::new([2, 0, 0, 0, 0, 1]),
                user: "user",
                password: "password",
                hostname: b"STB",
            },
        );
        let result = client.acquire();
        assert_eq!(result, Err(DhcpError::Platform(LinuxError::Timeout)));
        let mut setup = MockPlatform {
            command_results: vec![Ok(()), Err(LinuxError::CommandFailed)],
            ..MockPlatform::default()
        };
        let result = configure_interface(&mut setup, "eth0", &lease(), &[]);
        assert!(matches!(
            result,
            Ok(LifecycleResult::Preserved {
                cause: LifecycleCause::StartupFailure,
                failed_stage: Some(SetupStage::Route),
                ..
            })
        ));
    }

    #[test]
    fn platform_errors_do_not_render_credentials() {
        let error = DhcpError::Platform(LinuxError::Io).to_string();
        assert!(!error.contains("password"));
        assert!(!error.contains("user"));
    }
}
