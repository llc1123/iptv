use dhcproto::{v4, Decodable};
use etherparse::{IpHeaders, IpNumber, Ipv4Extensions, Ipv4Header, PacketBuilder};
use std::{collections::HashMap, fmt, net::Ipv4Addr, str::FromStr};
use thiserror::Error;
pub const PARAMETER_REQUEST_LIST: [u8; 10] = [1, 121, 33, 3, 6, 15, 28, 51, 58, 59];
const BOOTP: usize = 236;
const COOKIE: [u8; 4] = [99, 130, 83, 99];
const DHCP: usize = BOOTP + 4;
const UDP: usize = 8;
const IP: usize = 20;
const ETH: usize = 14;
type Options = HashMap<u8, Vec<u8>>;
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct MacAddress([u8; 6]);
impl MacAddress {
    pub const fn new(octets: [u8; 6]) -> Self {
        Self(octets)
    }
    pub const fn octets(self) -> [u8; 6] {
        self.0
    }
    pub fn compact_upper(self) -> String {
        hex::encode_upper(self.0)
    }
}
impl FromStr for MacAddress {
    type Err = PacketError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let mut octets = [0; 6];
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
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:02X}:{:02X}:{:02X}:{:02X}:{:02X}:{:02X}",
            self.0[0], self.0[1], self.0[2], self.0[3], self.0[4], self.0[5]
        )
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReplyKind {
    Offer,
    Ack,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DhcpReply {
    pub xid: u32,
    pub kind: ReplyKind,
    pub address: Ipv4Addr,
    pub source: Ipv4Addr,
    pub server: Option<Ipv4Addr>,
    pub lease_seconds: Option<u32>,
    pub vendor_specific: Option<Vec<u8>>,
    pub subnet_mask: Option<Ipv4Addr>,
    pub router: Option<Ipv4Addr>,
}
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum PacketError {
    #[error("malformed DHCP frame")]
    Malformed,
    #[error("invalid DHCP interface address")]
    InvalidMac,
    #[error("unexpected DHCP response")]
    Unexpected,
    #[error("DHCP NAK")]
    Nak,
    #[error("DHCP ACK lacks option {0}")]
    MissingOption(u8),
    #[error("DHCP option {0} has an invalid length")]
    InvalidOption(u8),
    #[error("DHCP packet is too large")]
    TooLarge,
}
pub(crate) fn matching_reply(
    frame: &[u8],
    xid: u32,
    mac: MacAddress,
    kind: ReplyKind,
) -> Result<bool, PacketError> {
    match decode_reply(frame, xid, mac, kind) {
        Ok(_) => Ok(true),
        Err(PacketError::Nak) => Err(PacketError::Nak),
        Err(_) => Ok(false),
    }
}
pub fn build_discover(
    source: MacAddress,
    xid: u32,
    option60: &[u8],
    hostname: &[u8],
) -> Result<Vec<u8>, PacketError> {
    build(
        source,
        xid,
        &[
            (53, &[1]),
            (60, option60),
            (12, hostname),
            (55, &PARAMETER_REQUEST_LIST),
        ],
    )
}
pub fn build_request(
    source: MacAddress,
    xid: u32,
    requested: Ipv4Addr,
    server: Ipv4Addr,
    option60: &[u8],
    hostname: &[u8],
) -> Result<Vec<u8>, PacketError> {
    let requested = requested.octets();
    let server = server.octets();
    build(
        source,
        xid,
        &[
            (53, &[3]),
            (50, &requested),
            (54, &server),
            (60, option60),
            (12, hostname),
            (55, &PARAMETER_REQUEST_LIST),
        ],
    )
}
fn build(source: MacAddress, xid: u32, fields: &[(u8, &[u8])]) -> Result<Vec<u8>, PacketError> {
    let options_len = fields
        .iter()
        .try_fold(0usize, |sum, (_, value)| sum.checked_add(2 + value.len()))
        .ok_or(PacketError::TooLarge)?;
    if fields
        .iter()
        .any(|(_, value)| value.len() > usize::from(u8::MAX))
    {
        return Err(PacketError::TooLarge);
    }
    let mut payload = vec![0; DHCP.checked_add(options_len).ok_or(PacketError::TooLarge)? + 1];
    payload[0] = 1;
    payload[1] = 1;
    payload[2] = 6;
    payload[4..8].copy_from_slice(&xid.to_be_bytes());
    payload[28..34].copy_from_slice(&source.octets());
    payload[BOOTP..DHCP].copy_from_slice(&COOKIE);
    let mut cursor = DHCP;
    for (code, value) in fields {
        payload[cursor] = *code;
        payload[cursor + 1] = u8::try_from(value.len()).map_err(|_| PacketError::TooLarge)?;
        let end = cursor
            .checked_add(2 + value.len())
            .ok_or(PacketError::TooLarge)?;
        payload[cursor + 2..end].copy_from_slice(value);
        cursor = end;
    }
    payload[cursor] = 255;
    frame(source, &payload, 68, 67)
}
fn frame(
    source: MacAddress,
    payload: &[u8],
    source_port: u16,
    destination_port: u16,
) -> Result<Vec<u8>, PacketError> {
    let payload_len = UDP
        .checked_add(payload.len())
        .ok_or(PacketError::TooLarge)?;
    let length = u16::try_from(payload_len).map_err(|_| PacketError::TooLarge)?;
    let mut header = Ipv4Header::new(length, 64, IpNumber::UDP, [0; 4], [255; 4])
        .map_err(|_| PacketError::TooLarge)?;
    header.identification = 1;
    let builder = PacketBuilder::ethernet2(source.octets(), [255; 6])
        .ip(IpHeaders::Ipv4(header, Ipv4Extensions::default()))
        .udp(source_port, destination_port);
    let mut output = Vec::with_capacity(builder.size(payload.len()));
    builder
        .write(&mut output, payload)
        .map_err(|_| PacketError::TooLarge)?;
    Ok(output)
}
pub fn decode_reply(
    frame: &[u8],
    expected_xid: u32,
    expected_mac: MacAddress,
    expected_kind: ReplyKind,
) -> Result<DhcpReply, PacketError> {
    let (source, dhcp, destination) = decode_udp(frame)?;
    if destination != Ipv4Addr::new(255, 255, 255, 255) || dhcp.len() < DHCP {
        return Err(PacketError::Malformed);
    }
    if dhcp.get(0..3) != Some(&[2, 1, 6])
        || read_u32(dhcp, 4)? != expected_xid
        || dhcp.get(28..34) != Some(&expected_mac.octets())
        || dhcp.get(BOOTP..DHCP) != Some(&COOKIE)
    {
        return Err(PacketError::Unexpected);
    }
    let parsed = v4::Message::from_bytes(dhcp).map_err(|_| PacketError::Malformed)?;
    let _ = parsed;
    let opts = options(dhcp)?;
    let kind = match option(&opts, 53)?.first() {
        Some(2) => ReplyKind::Offer,
        Some(5) => ReplyKind::Ack,
        Some(6) => return Err(PacketError::Nak),
        _ => return Err(PacketError::Unexpected),
    };
    if kind != expected_kind {
        return Err(PacketError::Unexpected);
    }
    let server = ipv4(&opts, 54)?;
    let lease_seconds = number(&opts, 51)?;
    let vendor_specific = opts.get(&125).cloned();
    let subnet_mask = ipv4(&opts, 1)?;
    let router = ipv4(&opts, 3)?;
    if expected_kind == ReplyKind::Ack {
        for code in [54, 51, 125, 1, 3] {
            if (code == 125 && vendor_specific.is_none())
                || (code != 125 && !opts.contains_key(&code))
            {
                return Err(PacketError::MissingOption(code));
            }
        }
    }
    Ok(DhcpReply {
        xid: expected_xid,
        kind,
        address: Ipv4Addr::from(read_array(dhcp, 16)?),
        source,
        server,
        lease_seconds,
        vendor_specific,
        subnet_mask,
        router,
    })
}
fn decode_udp(frame: &[u8]) -> Result<(Ipv4Addr, &[u8], Ipv4Addr), PacketError> {
    let ip = ETH;
    let udp = ip + IP;
    if frame.len() < udp + UDP
        || frame.get(12..14) != Some(&[8, 0])
        || frame.get(ip).is_none_or(|v| v >> 4 != 4 || v & 15 != 5)
        || frame.get(ip + 9) != Some(&17)
    {
        return Err(PacketError::Malformed);
    }
    let total = usize::from(read_u16(frame, ip + 2)?);
    if total < IP + UDP
        || frame.len() < ip + total
        || read_u16(frame, udp)? != 67
        || read_u16(frame, udp + 2)? != 68
    {
        return Err(PacketError::Malformed);
    }
    if checksum(frame.get(ip..udp).ok_or(PacketError::Malformed)?) != 0 {
        return Err(PacketError::Malformed);
    }
    let udp_len = usize::from(read_u16(frame, udp + 4)?);
    if udp_len < UDP || udp_len != total - IP || frame.len() < udp + udp_len {
        return Err(PacketError::Malformed);
    }
    let source = Ipv4Addr::from(read_array(frame, ip + 12)?);
    let destination = Ipv4Addr::from(read_array(frame, ip + 16)?);
    let checksum_value = read_u16(frame, udp + 6)?;
    if checksum_value != 0 {
        let mut pseudo = Vec::with_capacity(12 + udp_len);
        pseudo.extend_from_slice(&source.octets());
        pseudo.extend_from_slice(&destination.octets());
        pseudo.extend_from_slice(&[0, 17]);
        pseudo.extend_from_slice(
            &u16::try_from(udp_len)
                .map_err(|_| PacketError::Malformed)?
                .to_be_bytes(),
        );
        pseudo.extend_from_slice(
            frame
                .get(udp..udp + udp_len)
                .ok_or(PacketError::Malformed)?,
        );
        if checksum(&pseudo) != 0 {
            return Err(PacketError::Malformed);
        }
    }
    Ok((
        source,
        frame
            .get(udp + UDP..udp + udp_len)
            .ok_or(PacketError::Malformed)?,
        destination,
    ))
}
fn options(raw_frame: &[u8]) -> Result<Options, PacketError> {
    let raw = raw_frame.get(DHCP..).ok_or(PacketError::Malformed)?;
    let mut result = std::collections::HashMap::new();
    let mut cursor = 0;
    loop {
        let code = *raw.get(cursor).ok_or(PacketError::Malformed)?;
        cursor += 1;
        match code {
            0 => {}
            255 => {
                if raw[cursor..].iter().any(|byte| *byte != 0) {
                    return Err(PacketError::Malformed);
                }
                return Ok(result);
            }
            code => {
                let len = usize::from(*raw.get(cursor).ok_or(PacketError::Malformed)?);
                cursor += 1;
                let end = cursor.checked_add(len).ok_or(PacketError::Malformed)?;
                let value = raw.get(cursor..end).ok_or(PacketError::Malformed)?.to_vec();
                result.entry(code).or_insert(value);
                cursor = end;
            }
        }
    }
}
fn option(options: &Options, code: u8) -> Result<&[u8], PacketError> {
    options
        .get(&code)
        .map(Vec::as_slice)
        .ok_or(PacketError::MissingOption(code))
}
fn ipv4(options: &Options, code: u8) -> Result<Option<Ipv4Addr>, PacketError> {
    options.get(&code).map_or(Ok(None), |value| {
        if value.len() != 4 {
            return Err(PacketError::InvalidOption(code));
        }
        Ok(Some(Ipv4Addr::from([
            value[0], value[1], value[2], value[3],
        ])))
    })
}
fn number(options: &Options, code: u8) -> Result<Option<u32>, PacketError> {
    options.get(&code).map_or(Ok(None), |value| {
        if value.len() != 4 {
            return Err(PacketError::InvalidOption(code));
        }
        Ok(Some(u32::from_be_bytes([
            value[0], value[1], value[2], value[3],
        ])))
    })
}
fn read_u16(bytes: &[u8], offset: usize) -> Result<u16, PacketError> {
    Ok(u16::from_be_bytes(read_array(bytes, offset)?))
}
fn read_u32(bytes: &[u8], offset: usize) -> Result<u32, PacketError> {
    Ok(u32::from_be_bytes(read_array(bytes, offset)?))
}
fn read_array<const N: usize>(bytes: &[u8], offset: usize) -> Result<[u8; N], PacketError> {
    bytes
        .get(offset..offset + N)
        .ok_or(PacketError::Malformed)?
        .try_into()
        .map_err(|_| PacketError::Malformed)
}
fn checksum(bytes: &[u8]) -> u16 {
    let mut sum = 0u32;
    for chunk in bytes.chunks_exact(2) {
        sum = sum.saturating_add(u32::from(u16::from_be_bytes([chunk[0], chunk[1]])));
    }
    if let Some(byte) = bytes.chunks_exact(2).remainder().first() {
        sum = sum.saturating_add(u32::from(*byte) << 8);
    }
    while sum > u32::from(u16::MAX) {
        sum = (sum & u32::from(u16::MAX)) + (sum >> 16);
    }
    !u16::try_from(sum).unwrap_or(u16::MAX)
}
#[cfg(test)]
mod tests {
    use super::*;
    const MAC: MacAddress = MacAddress::new([2, 0, 0, 0, 0, 1]);

    fn option_bytes(frame: &[u8]) -> &[u8] {
        &frame[ETH + IP + UDP + DHCP..]
    }
    fn reply_frame(xid: u32, fields: &[(u8, &[u8])]) -> Vec<u8> {
        let mut payload = vec![0; DHCP];
        payload[..3].copy_from_slice(&[2, 1, 6]);
        payload[4..8].copy_from_slice(&xid.to_be_bytes());
        payload[16..20].copy_from_slice(&[192, 0, 2, 44]);
        payload[28..34].copy_from_slice(&MAC.octets());
        payload[BOOTP..DHCP].copy_from_slice(&COOKIE);
        for (code, value) in fields {
            payload.extend_from_slice(&[*code, u8::try_from(value.len()).unwrap_or(0)]);
            payload.extend_from_slice(value);
        }
        payload.push(255);
        frame(MAC, &payload, 67, 68).unwrap_or_default()
    }

    #[test]
    fn discover_and_request_pin_python_option_order() {
        let discover = build_discover(MAC, 0x0102_0304, &[0xaa, 0xbb], b"STB").unwrap_or_default();
        assert_eq!(
            option_bytes(&discover),
            &[
                53, 1, 1, 60, 2, 0xaa, 0xbb, 12, 3, b'S', b'T', b'B', 55, 10, 1, 121, 33, 3, 6, 15,
                28, 51, 58, 59, 255
            ]
        );
        let request = build_request(
            MAC,
            7,
            Ipv4Addr::new(192, 0, 2, 44),
            Ipv4Addr::new(192, 0, 2, 1),
            &[0xaa],
            b"STB",
        )
        .unwrap_or_default();
        assert_eq!(
            option_bytes(&request),
            &[
                53, 1, 3, 50, 4, 192, 0, 2, 44, 54, 4, 192, 0, 2, 1, 60, 1, 0xaa, 12, 3, b'S',
                b'T', b'B', 55, 10, 1, 121, 33, 3, 6, 15, 28, 51, 58, 59, 255
            ]
        );
    }

    #[test]
    fn ack_accepts_zero_checksum_and_first_duplicate_option() {
        let lease = 3600u32.to_be_bytes();
        let mask = [255, 255, 255, 0];
        let router = [192, 0, 2, 1];
        let server = [192, 0, 2, 1];
        let mut frame = reply_frame(
            7,
            &[
                (53, &[5]),
                (51, &lease),
                (125, &[16]),
                (1, &mask),
                (3, &router),
                (54, &server),
                (54, &[198, 51, 100, 1]),
            ],
        );
        frame[ETH + IP + 6..ETH + IP + 8].fill(0);
        let reply = decode_reply(&frame, 7, MAC, ReplyKind::Ack).unwrap_or(DhcpReply {
            xid: 0,
            kind: ReplyKind::Offer,
            address: Ipv4Addr::UNSPECIFIED,
            source: Ipv4Addr::UNSPECIFIED,
            server: None,
            lease_seconds: None,
            vendor_specific: None,
            subnet_mask: None,
            router: None,
        });
        assert_eq!(reply.server, Some(Ipv4Addr::new(192, 0, 2, 1)));
    }

    #[test]
    fn matching_reply_discards_unrelated_before_matching() {
        let fields = [
            (53, &[5][..]),
            (51, &[0, 0, 14, 16][..]),
            (125, &[16][..]),
            (1, &[255, 255, 255, 0][..]),
            (3, &[192, 0, 2, 1][..]),
            (54, &[192, 0, 2, 1][..]),
        ];
        assert!(!matching_reply(&reply_frame(8, &fields), 7, MAC, ReplyKind::Ack).unwrap_or(true));
        assert!(matching_reply(&reply_frame(7, &fields), 7, MAC, ReplyKind::Ack).unwrap_or(false));
    }
}
