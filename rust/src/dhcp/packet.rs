//! DHCP Ethernet, IPv4, UDP, BOOTP, and option wire formats.

#![allow(clippy::missing_errors_doc)]

use std::collections::HashMap;
use std::fmt;
use std::net::Ipv4Addr;
use std::str::FromStr;

use dhcproto::{Decodable, v4};
use etherparse::{IpHeaders, IpNumber, Ipv4Extensions, Ipv4Header, PacketBuilder};
use thiserror::Error;

/// The DHCP parameter request list required by the IPTV deployment.
pub const PARAMETER_REQUEST_LIST: [u8; 10] = [1, 121, 33, 3, 6, 15, 28, 51, 58, 59];
const BOOTP_HEADER_LEN: usize = 236;
const DHCP_COOKIE: [u8; 4] = [99, 130, 83, 99];
const DHCP_MIN_PAYLOAD: usize = BOOTP_HEADER_LEN + DHCP_COOKIE.len();
const UDP_HEADER_LEN: usize = 8;
const ETHERNET_HEADER_LEN: usize = 14;
const IPV4_HEADER_LEN: usize = 20;

/// A six-byte Ethernet address.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct MacAddress([u8; 6]);

impl MacAddress {
    /// Creates an address from its six octets.
    #[must_use]
    pub const fn new(octets: [u8; 6]) -> Self {
        Self(octets)
    }

    /// Returns the address octets.
    #[must_use]
    pub const fn octets(self) -> [u8; 6] {
        self.0
    }

    /// Returns uppercase hexadecimal octets without separators.
    #[must_use]
    pub fn compact_upper(self) -> String {
        hex::encode_upper(self.0)
    }
}

impl FromStr for MacAddress {
    type Err = PacketError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let mut octets = [0_u8; 6];
        let mut parts = value.split(':');
        for octet in &mut octets {
            let part = parts.next().ok_or(PacketError::InvalidMac)?;
            if part.len() != 2 {
                return Err(PacketError::InvalidMac);
            }
            *octet = u8::from_str_radix(part, 16).map_err(|_| PacketError::InvalidMac)?;
        }
        if parts.next().is_some() {
            return Err(PacketError::InvalidMac);
        }
        Ok(Self(octets))
    }
}

impl fmt::Display for MacAddress {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (index, octet) in self.0.iter().enumerate() {
            if index != 0 {
                formatter.write_str(":")?;
            }
            write!(formatter, "{octet:02x}")?;
        }
        Ok(())
    }
}

/// The DHCP reply type required by a receive stage.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReplyKind {
    /// A DHCPOFFER reply.
    Offer,
    /// A DHCPACK reply.
    Ack,
}

/// The strictly decoded fields needed by DHCP acquisition.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DhcpReply {
    /// Reply transaction identifier.
    pub xid: u32,
    /// Reply type.
    pub kind: ReplyKind,
    /// Offered or acknowledged address.
    pub address: Ipv4Addr,
    /// IPv4 source of the received datagram.
    pub source: Ipv4Addr,
    /// DHCP option 54, when present.
    pub server: Option<Ipv4Addr>,
    /// DHCP option 51, when present.
    pub lease_seconds: Option<u32>,
    /// DHCP option 43, when present.
    pub vendor_specific: Option<Vec<u8>>,
    /// DHCP option 1, when present.
    pub subnet_mask: Option<Ipv4Addr>,
    /// DHCP option 3, when present.
    pub router: Option<Ipv4Addr>,
}

/// Packet construction and validation failures.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum PacketError {
    /// The input does not contain a complete Ethernet/IP/UDP/DHCP frame.
    #[error("DHCP frame is malformed or truncated")]
    MalformedFrame,
    /// The Ethernet address is not six hexadecimal octets.
    #[error("DHCP interface address is malformed")]
    InvalidMac,
    /// The DHCP BOOTP cookie is not the required value.
    #[error("DHCP magic cookie is invalid")]
    InvalidCookie,
    /// DHCP option framing is invalid.
    #[error("DHCP options are malformed")]
    MalformedOptions,
    /// dhcproto rejected the DHCP message.
    #[error("DHCP message cannot be decoded")]
    DecodeFailed,
    /// The frame has non-DHCP UDP ports.
    #[error("DHCP UDP ports are invalid")]
    InvalidUdpPorts,
    /// The transaction identifier did not match the acquisition.
    #[error("DHCP transaction does not match")]
    WrongTransaction,
    /// The BOOTP operation is not a reply.
    #[error("DHCP reply is not a BOOTP reply")]
    NotBootReply,
    /// The requested reply type was not received.
    #[error("DHCP reply type is unexpected")]
    UnexpectedMessageType,
    /// A negative DHCP response was received.
    #[error("DHCP server rejected the request")]
    Nak,
    /// A required option is absent or has the wrong length.
    #[error("DHCP reply lacks a required option")]
    MissingOption,
    /// The generated packet could not be represented.
    #[error("DHCP packet is too large")]
    PacketTooLarge,
}

/// Builds the byte-compatible DHCPDISCOVER frame.
pub fn build_discover(
    source: MacAddress,
    xid: u32,
    option60: &[u8],
    hostname: &[u8],
) -> Result<Vec<u8>, PacketError> {
    let mut payload = bootp_payload(
        source,
        xid,
        &[
            (53, &[1]),
            (60, option60),
            (12, hostname),
            (55, &PARAMETER_REQUEST_LIST),
        ],
    )?;
    payload.push(255);
    ethernet_frame(source, &payload)
}

/// Builds the byte-compatible DHCPREQUEST frame.
pub fn build_request(
    source: MacAddress,
    xid: u32,
    requested: Ipv4Addr,
    server: Ipv4Addr,
    option60: &[u8],
    hostname: &[u8],
) -> Result<Vec<u8>, PacketError> {
    let requested_octets = requested.octets();
    let server_octets = server.octets();
    let mut payload = bootp_payload(
        source,
        xid,
        &[
            (53, &[3]),
            (50, &requested_octets),
            (54, &server_octets),
            (60, option60),
            (12, hostname),
            (55, &PARAMETER_REQUEST_LIST),
        ],
    )?;
    payload.push(255);
    ethernet_frame(source, &payload)
}

fn bootp_payload(
    source: MacAddress,
    xid: u32,
    options: &[(u8, &[u8])],
) -> Result<Vec<u8>, PacketError> {
    let options_len = options
        .iter()
        .try_fold(0_usize, |length, (_, value)| {
            let next = length.checked_add(2)?.checked_add(value.len())?;
            if value.len() > usize::from(u8::MAX) {
                return None;
            }
            Some(next)
        })
        .ok_or(PacketError::PacketTooLarge)?;
    let payload_len = DHCP_MIN_PAYLOAD
        .checked_add(options_len)
        .ok_or(PacketError::PacketTooLarge)?;
    let mut payload = vec![0_u8; payload_len];
    payload[0] = 1;
    payload[1] = 1;
    payload[2] = 6;
    payload[4..8].copy_from_slice(&xid.to_be_bytes());
    payload[28..34].copy_from_slice(&source.octets());
    payload[236..240].copy_from_slice(&DHCP_COOKIE);
    let mut cursor = DHCP_MIN_PAYLOAD;
    for (code, value) in options {
        payload[cursor] = *code;
        payload[cursor + 1] = u8::try_from(value.len()).map_err(|_| PacketError::PacketTooLarge)?;
        let end = cursor
            .checked_add(2)
            .and_then(|start| start.checked_add(value.len()))
            .ok_or(PacketError::PacketTooLarge)?;
        payload[cursor + 2..end].copy_from_slice(value);
        cursor = end;
    }
    Ok(payload)
}

fn ethernet_frame(source: MacAddress, payload: &[u8]) -> Result<Vec<u8>, PacketError> {
    ethernet_frame_with_ips(source, [0, 0, 0, 0], [255, 255, 255, 255], payload)
}

fn ethernet_frame_with_ips(
    source: MacAddress,
    source_ip: [u8; 4],
    destination_ip: [u8; 4],
    payload: &[u8],
) -> Result<Vec<u8>, PacketError> {
    ethernet_frame_with_ips_and_ports(source, source_ip, destination_ip, 68, 67, payload)
}

fn ethernet_frame_with_ips_and_ports(
    source: MacAddress,
    source_ip: [u8; 4],
    destination_ip: [u8; 4],
    source_port: u16,
    destination_port: u16,
    payload: &[u8],
) -> Result<Vec<u8>, PacketError> {
    let ip_payload_len = UDP_HEADER_LEN
        .checked_add(payload.len())
        .ok_or(PacketError::PacketTooLarge)?;
    let ip_len = u16::try_from(ip_payload_len).map_err(|_| PacketError::PacketTooLarge)?;
    let mut ip = Ipv4Header::new(ip_len, 64, IpNumber::UDP, source_ip, destination_ip)
        .map_err(|_| PacketError::PacketTooLarge)?;
    ip.identification = 1;
    ip.dont_fragment = false;
    let builder = PacketBuilder::ethernet2(source.octets(), [255, 255, 255, 255, 255, 255])
        .ip(IpHeaders::Ipv4(ip, Ipv4Extensions::default()))
        .udp(source_port, destination_port);
    let mut frame = Vec::with_capacity(builder.size(payload.len()));
    builder
        .write(&mut frame, payload)
        .map_err(|_| PacketError::PacketTooLarge)?;
    Ok(frame)
}

/// Strictly decodes one received Ethernet frame.
pub fn decode_reply(
    frame: &[u8],
    expected_xid: u32,
    expected_mac: MacAddress,
    expected_kind: ReplyKind,
) -> Result<DhcpReply, PacketError> {
    let (source, dhcp) = decode_udp(frame)?;
    if dhcp.len() < DHCP_MIN_PAYLOAD {
        return Err(PacketError::MalformedFrame);
    }
    if dhcp.first().copied() != Some(2)
        || dhcp.get(1).copied() != Some(1)
        || dhcp.get(2).copied() != Some(6)
    {
        return Err(PacketError::NotBootReply);
    }
    let xid = read_u32(dhcp, 4)?;
    if xid != expected_xid {
        return Err(PacketError::WrongTransaction);
    }
    if dhcp.get(28..34) != Some(&expected_mac.octets()) {
        return Err(PacketError::WrongTransaction);
    }
    if dhcp.get(236..240) != Some(&DHCP_COOKIE) {
        return Err(PacketError::InvalidCookie);
    }
    // dhcproto 0.14 validates the message shape, but its BTreeMap option
    // decoder is last-occurrence-wins. Keep the Python client's first-
    // occurrence semantics in parse_options below instead of using its
    // decoded option values for compatibility decisions.
    let _decoded = v4::Message::from_bytes(dhcp).map_err(|_| PacketError::DecodeFailed)?;
    let options = parse_options(
        dhcp.get(DHCP_MIN_PAYLOAD..)
            .ok_or(PacketError::MalformedFrame)?,
    )?;
    let message_type_bytes = option_bytes(&options, 53)?;
    if message_type_bytes.len() != 1 {
        return Err(PacketError::MalformedOptions);
    }
    let message_type = message_type_bytes[0];
    let kind = match message_type {
        2 => ReplyKind::Offer,
        5 => ReplyKind::Ack,
        6 => return Err(PacketError::Nak),
        _ => return Err(PacketError::UnexpectedMessageType),
    };
    if kind != expected_kind {
        return Err(PacketError::UnexpectedMessageType);
    }
    let address = Ipv4Addr::from(read_array::<4>(dhcp, 16)?);
    let server = option_ipv4_optional(&options, 54)?;
    let lease_seconds = option_u32_optional(&options, 51)?;
    let vendor_specific = option_bytes_optional(&options, 43).map(ToOwned::to_owned);
    let subnet_mask = option_ipv4_optional(&options, 1)?;
    let router = option_ipv4_optional(&options, 3)?;
    if expected_kind == ReplyKind::Ack
        && (server.is_none()
            || lease_seconds.is_none()
            || vendor_specific.is_none()
            || subnet_mask.is_none()
            || router.is_none())
    {
        return Err(PacketError::MissingOption);
    }
    Ok(DhcpReply {
        xid,
        kind,
        address,
        source,
        server,
        lease_seconds,
        vendor_specific,
        subnet_mask,
        router,
    })
}

fn decode_udp(frame: &[u8]) -> Result<(Ipv4Addr, &[u8]), PacketError> {
    let ip_start = ETHERNET_HEADER_LEN;
    let udp_start = ip_start
        .checked_add(IPV4_HEADER_LEN)
        .ok_or(PacketError::MalformedFrame)?;
    let min_len = udp_start
        .checked_add(UDP_HEADER_LEN)
        .ok_or(PacketError::MalformedFrame)?;
    if frame.len() < min_len || frame.get(12..14) != Some(&[0x08, 0x00]) {
        return Err(PacketError::MalformedFrame);
    }
    let version_ihl = frame
        .get(ip_start)
        .copied()
        .ok_or(PacketError::MalformedFrame)?;
    if version_ihl >> 4 != 4 {
        return Err(PacketError::MalformedFrame);
    }
    let ihl = version_ihl & 0x0f;
    if ihl != 5 {
        return Err(PacketError::MalformedFrame);
    }
    let ip_end = ip_start
        .checked_add(usize::from(ihl) * 4)
        .ok_or(PacketError::MalformedFrame)?;
    if frame.len() < ip_end || frame.get(ip_start + 9).copied() != Some(17) {
        return Err(PacketError::MalformedFrame);
    }
    let total_len = usize::from(read_u16(frame, ip_start + 2)?);
    if total_len < usize::from(ihl) * 4 + UDP_HEADER_LEN || frame.len() < ip_start + total_len {
        return Err(PacketError::MalformedFrame);
    }
    if read_u16(frame, ip_end)? != 67 || read_u16(frame, ip_end + 2)? != 68 {
        return Err(PacketError::InvalidUdpPorts);
    }
    if checksum(
        frame
            .get(ip_start..ip_end)
            .ok_or(PacketError::MalformedFrame)?,
    ) != 0
    {
        return Err(PacketError::MalformedFrame);
    }
    let udp_len = usize::from(read_u16(frame, ip_end + 4)?);
    if udp_len < UDP_HEADER_LEN || udp_len != total_len - usize::from(ihl) * 4 {
        return Err(PacketError::MalformedFrame);
    }
    let udp_end = ip_end
        .checked_add(udp_len)
        .ok_or(PacketError::MalformedFrame)?;
    let source = Ipv4Addr::from(read_array::<4>(frame, ip_start + 12)?);
    let destination = read_array::<4>(frame, ip_start + 16)?;
    let mut pseudo = Vec::with_capacity(12 + udp_len);
    pseudo.extend_from_slice(&source.octets());
    pseudo.extend_from_slice(&destination);
    pseudo.extend_from_slice(&[0, 17]);
    pseudo.extend_from_slice(
        &u16::try_from(udp_len)
            .map_err(|_| PacketError::MalformedFrame)?
            .to_be_bytes(),
    );
    pseudo.extend_from_slice(
        frame
            .get(ip_end..udp_end)
            .ok_or(PacketError::MalformedFrame)?,
    );
    let udp_checksum = read_u16(frame, ip_end + 6)?;
    if udp_checksum != 0 && checksum(&pseudo) != 0 {
        return Err(PacketError::MalformedFrame);
    }
    Ok((
        source,
        frame
            .get(ip_end + UDP_HEADER_LEN..udp_end)
            .ok_or(PacketError::MalformedFrame)?,
    ))
}

fn parse_options(raw: &[u8]) -> Result<HashMap<u8, Vec<u8>>, PacketError> {
    let mut options = HashMap::new();
    let mut cursor = 0_usize;
    let mut ended = false;
    while cursor < raw.len() {
        let code = *raw.get(cursor).ok_or(PacketError::MalformedOptions)?;
        cursor += 1;
        match code {
            0 => {}
            255 => {
                ended = true;
                if raw
                    .get(cursor..)
                    .is_some_and(|tail| tail.iter().any(|byte| *byte != 0))
                {
                    return Err(PacketError::MalformedOptions);
                }
                break;
            }
            code => {
                let length = usize::from(*raw.get(cursor).ok_or(PacketError::MalformedOptions)?);
                cursor += 1;
                let end = cursor
                    .checked_add(length)
                    .ok_or(PacketError::MalformedOptions)?;
                let value = raw
                    .get(cursor..end)
                    .ok_or(PacketError::MalformedOptions)?
                    .to_vec();
                options.entry(code).or_insert(value);
                cursor = end;
            }
        }
    }
    if !ended {
        return Err(PacketError::MalformedOptions);
    }
    Ok(options)
}

fn option_bytes(options: &HashMap<u8, Vec<u8>>, code: u8) -> Result<&[u8], PacketError> {
    option_bytes_optional(options, code).ok_or(PacketError::MissingOption)
}

fn option_bytes_optional(options: &HashMap<u8, Vec<u8>>, code: u8) -> Option<&[u8]> {
    options.get(&code).map(Vec::as_slice)
}

fn option_ipv4_optional(
    options: &HashMap<u8, Vec<u8>>,
    code: u8,
) -> Result<Option<Ipv4Addr>, PacketError> {
    let Some(value) = option_bytes_optional(options, code) else {
        return Ok(None);
    };
    if value.len() != 4 {
        return Err(PacketError::MissingOption);
    }
    Ok(Some(Ipv4Addr::from([
        value[0], value[1], value[2], value[3],
    ])))
}

fn option_u32_optional(
    options: &HashMap<u8, Vec<u8>>,
    code: u8,
) -> Result<Option<u32>, PacketError> {
    let Some(value) = option_bytes_optional(options, code) else {
        return Ok(None);
    };
    if value.len() != 4 {
        return Err(PacketError::MissingOption);
    }
    Ok(Some(u32::from_be_bytes([
        value[0], value[1], value[2], value[3],
    ])))
}

fn read_u16(bytes: &[u8], offset: usize) -> Result<u16, PacketError> {
    Ok(u16::from_be_bytes(read_array(bytes, offset)?))
}

fn read_u32(bytes: &[u8], offset: usize) -> Result<u32, PacketError> {
    Ok(u32::from_be_bytes(read_array(bytes, offset)?))
}

fn read_array<const N: usize>(bytes: &[u8], offset: usize) -> Result<[u8; N], PacketError> {
    let end = offset.checked_add(N).ok_or(PacketError::MalformedFrame)?;
    let slice = bytes.get(offset..end).ok_or(PacketError::MalformedFrame)?;
    slice.try_into().map_err(|_| PacketError::MalformedFrame)
}

fn checksum(bytes: &[u8]) -> u16 {
    let mut sum = 0_u32;
    for chunk in bytes.chunks_exact(2) {
        sum = sum.saturating_add(u32::from(u16::from_be_bytes([chunk[0], chunk[1]])));
    }
    if let Some(byte) = bytes.chunks_exact(2).remainder().first() {
        sum = sum.saturating_add(u32::from(*byte) << 8);
    }
    while sum > u32::from(u16::MAX) {
        sum = (sum & u32::from(u16::MAX)).saturating_add(sum >> 16);
    }
    match u16::try_from(sum) {
        Ok(value) => !value,
        Err(_) => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAC: MacAddress = MacAddress::new([0x02, 0x00, 0x00, 0x00, 0x00, 0x01]);

    fn option_bytes(frame: &[u8]) -> &[u8] {
        &frame[ETHERNET_HEADER_LEN + IPV4_HEADER_LEN + UDP_HEADER_LEN + DHCP_MIN_PAYLOAD..]
    }

    fn reply_frame(xid: u32, options: &[(u8, &[u8])]) -> Vec<u8> {
        let mut payload = vec![0_u8; DHCP_MIN_PAYLOAD];
        payload[0] = 2;
        payload[1] = 1;
        payload[2] = 6;
        payload[4..8].copy_from_slice(&xid.to_be_bytes());
        payload[16..20].copy_from_slice(&[192, 0, 2, 44]);
        payload[28..34].copy_from_slice(&MAC.octets());
        payload[236..240].copy_from_slice(&DHCP_COOKIE);
        for (code, value) in options {
            payload.push(*code);
            payload.push(u8::try_from(value.len()).unwrap_or_default());
            payload.extend_from_slice(value);
        }
        payload.push(255);
        ethernet_frame_with_ips_and_ports(
            MAC,
            [192, 0, 2, 1],
            [255, 255, 255, 255],
            67,
            68,
            &payload,
        )
        .unwrap_or_default()
    }

    #[test]
    fn discover_has_exact_option_order_and_bootp_defaults() {
        let frame = build_discover(MAC, 0x0102_0304, &[0xaa, 0xbb], b"STB").unwrap_or_default();
        assert_eq!(&frame[..6], &[0xff; 6]);
        assert_eq!(&frame[6..12], &MAC.octets());
        assert_eq!(frame[14], 0x45);
        assert_eq!(frame[15], 0);
        assert_eq!(&frame[18..20], &[0, 1]);
        assert_eq!(frame[20] & 0x40, 0);
        assert_eq!(frame[22], 64);
        assert_eq!(&frame[42..46], &[1, 1, 6, 0]);
        assert_eq!(&frame[46..50], &[1, 2, 3, 4]);
        assert_eq!(&frame[70..76], &MAC.octets());
        assert_eq!(&frame[278..282], &DHCP_COOKIE);
        assert_eq!(
            option_bytes(&frame),
            &[
                53, 1, 1, 60, 2, 0xaa, 0xbb, 12, 3, b'S', b'T', b'B', 55, 10, 1, 121, 33, 3, 6, 15,
                28, 51, 58, 59, 255
            ]
        );
        assert_eq!(&frame[34..38], &[0, 68, 0, 67]);
    }

    #[test]
    fn request_has_exact_option_order() {
        let frame = build_request(
            MAC,
            7,
            Ipv4Addr::new(192, 0, 2, 44),
            Ipv4Addr::new(192, 0, 2, 1),
            &[0xaa],
            b"STB",
        )
        .unwrap_or_default();
        assert_eq!(
            option_bytes(&frame),
            &[
                53, 1, 3, 50, 4, 192, 0, 2, 44, 54, 4, 192, 0, 2, 1, 60, 1, 0xaa, 12, 3, b'S',
                b'T', b'B', 55, 10, 1, 121, 33, 3, 6, 15, 28, 51, 58, 59, 255
            ]
        );
    }

    #[test]
    fn ack_requires_fields_and_rejects_adversarial_replies() {
        let lease = 3600_u32.to_be_bytes();
        let mask = [255, 255, 255, 0];
        let router = [192, 0, 2, 1];
        let server = [192, 0, 2, 1];
        let valid = reply_frame(
            7,
            &[
                (53, &[5]),
                (51, &lease),
                (43, &[0x10]),
                (1, &mask),
                (3, &router),
                (54, &server),
            ],
        );
        let decoded = decode_reply(&valid, 7, MAC, ReplyKind::Ack);
        assert_eq!(
            decoded.as_ref().map(|reply| reply.source),
            Ok(Ipv4Addr::new(192, 0, 2, 1))
        );
        assert_eq!(
            decoded.as_ref().map(|reply| reply.server),
            Ok(Some(Ipv4Addr::new(192, 0, 2, 1)))
        );
        let mut zero_udp_checksum = valid.clone();
        zero_udp_checksum[ip_end_for_test() + 6] = 0;
        zero_udp_checksum[ip_end_for_test() + 7] = 0;
        assert!(decode_reply(&zero_udp_checksum, 7, MAC, ReplyKind::Ack).is_ok());
        let checksum_offset = ip_end_for_test() + 6;
        let original_checksum = read_u16(&valid, checksum_offset).unwrap_or_default();
        let invalid_checksum = match original_checksum {
            0x1234 => 0x4321,
            u16::MAX => 0x1234,
            value => value.saturating_add(1),
        };
        let mut invalid_udp_checksum = valid.clone();
        invalid_udp_checksum[checksum_offset..checksum_offset + 2]
            .copy_from_slice(&invalid_checksum.to_be_bytes());
        assert_eq!(
            decode_reply(&invalid_udp_checksum, 7, MAC, ReplyKind::Ack),
            Err(PacketError::MalformedFrame)
        );
        assert_eq!(
            decode_reply(&valid, 8, MAC, ReplyKind::Ack),
            Err(PacketError::WrongTransaction)
        );
        let mut wrong_udp = valid.clone();
        wrong_udp[37] = 0;
        assert_eq!(
            decode_reply(&wrong_udp, 7, MAC, ReplyKind::Ack),
            Err(PacketError::InvalidUdpPorts)
        );
        let nak = reply_frame(7, &[(53, &[6])]);
        assert_eq!(
            decode_reply(&nak, 7, MAC, ReplyKind::Ack),
            Err(PacketError::Nak)
        );
        let missing = reply_frame(
            7,
            &[
                (53, &[5]),
                (51, &lease),
                (1, &mask),
                (3, &router),
                (54, &server),
            ],
        );
        assert_eq!(
            decode_reply(&missing, 7, MAC, ReplyKind::Ack),
            Err(PacketError::MissingOption)
        );
        assert_eq!(
            decode_reply(&valid[..valid.len() - 1], 7, MAC, ReplyKind::Ack),
            Err(PacketError::MalformedFrame)
        );
        let mut wrong_version = valid;
        wrong_version[ETHERNET_HEADER_LEN] = 0x65;
        wrong_version[ETHERNET_HEADER_LEN + 10] = 0;
        wrong_version[ETHERNET_HEADER_LEN + 11] = 0;
        let header_checksum =
            checksum(&wrong_version[ETHERNET_HEADER_LEN..ETHERNET_HEADER_LEN + IPV4_HEADER_LEN]);
        wrong_version[ETHERNET_HEADER_LEN + 10..ETHERNET_HEADER_LEN + 12]
            .copy_from_slice(&header_checksum.to_be_bytes());
        assert_eq!(
            decode_reply(&wrong_version, 7, MAC, ReplyKind::Ack),
            Err(PacketError::MalformedFrame)
        );
    }

    const fn ip_end_for_test() -> usize {
        ETHERNET_HEADER_LEN + IPV4_HEADER_LEN
    }

    #[test]
    fn ack_duplicate_server_options_use_first_occurrence() {
        let lease = 3600_u32.to_be_bytes();
        let mask = [255, 255, 255, 0];
        let router = [192, 0, 2, 1];
        let first_server = [192, 0, 2, 1];
        let later_server = [198, 51, 100, 1];
        let frame = reply_frame(
            7,
            &[
                (53, &[5]),
                (51, &lease),
                (43, &[0x10]),
                (1, &mask),
                (3, &router),
                (54, &first_server),
                (54, &later_server),
            ],
        );

        let decoded = decode_reply(&frame, 7, MAC, ReplyKind::Ack);

        assert_eq!(
            decoded.and_then(|reply| reply.server.ok_or(PacketError::MissingOption)),
            Ok(Ipv4Addr::new(192, 0, 2, 1))
        );
    }
}
