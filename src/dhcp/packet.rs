use std::{collections::HashMap, fmt, net::Ipv4Addr};
use thiserror::Error;
pub const PARAMETER_REQUEST_LIST: [u8; 10] = [1, 121, 33, 3, 6, 15, 28, 51, 58, 59];
const BOOTP: usize = 236;
const COOKIE: [u8; 4] = [99, 130, 83, 99];
const DHCP: usize = BOOTP + 4;
const UDP: usize = 8;
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
pub fn build_discover(
    source: MacAddress,
    xid: u32,
    option60: &[u8],
    hostname: &[u8],
) -> Result<Vec<u8>, PacketError> {
    build(
        source,
        xid,
        None,
        Ipv4Addr::BROADCAST,
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
        None,
        Ipv4Addr::BROADCAST,
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
/// A renewal REQUEST for the address already held: `ciaddr` carries it and the
/// requesting-server options are omitted, so the server renews that binding
/// instead of offering a free one.
pub fn build_renew(
    source: MacAddress,
    xid: u32,
    held: Ipv4Addr,
    server: Ipv4Addr,
    option60: &[u8],
    hostname: &[u8],
) -> Result<Vec<u8>, PacketError> {
    build(
        source,
        xid,
        Some(held),
        server,
        &[
            (53, &[3]),
            (60, option60),
            (12, hostname),
            (55, &PARAMETER_REQUEST_LIST),
        ],
    )
}

/// A broadcast DHCP client frame with `fields` as options, in order.
fn build(
    source: MacAddress,
    xid: u32,
    ciaddr: Option<Ipv4Addr>,
    destination: Ipv4Addr,
    fields: &[(u8, &[u8])],
) -> Result<Vec<u8>, PacketError> {
    let mut payload = vec![0; DHCP];
    payload[..3].copy_from_slice(&[1, 1, 6]);
    payload[4..8].copy_from_slice(&xid.to_be_bytes());
    if let Some(address) = ciaddr {
        payload[12..16].copy_from_slice(&address.octets());
    }
    payload[28..34].copy_from_slice(&source.octets());
    payload[BOOTP..DHCP].copy_from_slice(&COOKIE);
    for (code, value) in fields {
        let len = u8::try_from(value.len()).map_err(|_| PacketError::TooLarge)?;
        payload.extend_from_slice(&[*code, len]);
        payload.extend_from_slice(value);
    }
    payload.push(255);
    frame(source, ciaddr, destination, &payload, 68, 67)
}

/// Wraps a UDP payload in IPv4 and Ethernet. Renewal addresses the server that
/// granted the lease, as the set-top box does and as RFC 2131 §4.1 prescribes;
/// everything else is broadcast. Ethernet stays broadcast either way, because
/// the server's MAC is unknown.
fn frame(
    source: MacAddress,
    source_ip: Option<Ipv4Addr>,
    destination_ip: Ipv4Addr,
    payload: &[u8],
    source_port: u16,
    destination_port: u16,
) -> Result<Vec<u8>, PacketError> {
    let source_ip = source_ip.unwrap_or(Ipv4Addr::UNSPECIFIED).octets();
    let destination_ip = destination_ip.octets();
    let udp_len = u16::try_from(UDP + payload.len()).map_err(|_| PacketError::TooLarge)?;
    let ip_len = udp_len + 20;
    let mut out = Vec::with_capacity(ETH + usize::from(ip_len));
    out.extend_from_slice(&[0xff; 6]);
    out.extend_from_slice(&source.octets());
    out.extend_from_slice(&[0x08, 0x00]);
    let ip = out.len();
    out.extend_from_slice(&[0x45, 0]);
    out.extend_from_slice(&ip_len.to_be_bytes());
    out.extend_from_slice(&[0, 1, 0, 0, 64, 17, 0, 0]);
    out.extend_from_slice(&source_ip);
    out.extend_from_slice(&destination_ip);
    let ip_checksum = checksum(&out[ip..]);
    out[ip + 10..ip + 12].copy_from_slice(&ip_checksum.to_be_bytes());
    let udp = out.len();
    out.extend_from_slice(&source_port.to_be_bytes());
    out.extend_from_slice(&destination_port.to_be_bytes());
    out.extend_from_slice(&udp_len.to_be_bytes());
    out.extend_from_slice(&[0, 0]);
    out.extend_from_slice(payload);
    let mut pseudo = source_ip.to_vec();
    pseudo.extend_from_slice(&destination_ip);
    pseudo.extend_from_slice(&[0, 17]);
    pseudo.extend_from_slice(&udp_len.to_be_bytes());
    pseudo.extend_from_slice(&out[udp..]);
    out[udp + 6..udp + 8].copy_from_slice(&checksum(&pseudo).to_be_bytes());
    Ok(out)
}

/// Decodes a server reply to this client's transaction. Frames that are not
/// one (other traffic on the interface) are `Unexpected` or `Malformed`; a
/// NAK for this transaction is `Nak`. Checksums are not verified, as in
/// Python's Scapy client: Ethernet's FCS already covers the frame.
pub fn decode_reply(
    frame: &[u8],
    expected_xid: u32,
    expected_mac: MacAddress,
    expected_kind: ReplyKind,
) -> Result<DhcpReply, PacketError> {
    let ihl = usize::from(frame.get(ETH).ok_or(PacketError::Malformed)? & 15) * 4;
    let udp = ETH + ihl;
    if frame.get(12..14) != Some(&[8, 0])
        || frame.get(ETH + 9) != Some(&17)
        || read_u16(frame, udp)? != 67
        || read_u16(frame, udp + 2)? != 68
    {
        return Err(PacketError::Malformed);
    }
    let udp_end = (udp + usize::from(read_u16(frame, udp + 4)?)).min(frame.len());
    let dhcp = frame
        .get(udp + UDP..udp_end)
        .ok_or(PacketError::Malformed)?;
    if dhcp.len() < DHCP
        || dhcp[0..3] != [2, 1, 6]
        || read_u32(dhcp, 4)? != expected_xid
        || dhcp[28..34] != expected_mac.octets()
        || dhcp[BOOTP..DHCP] != COOKIE
    {
        return Err(PacketError::Unexpected);
    }
    let opts = options(&dhcp[DHCP..])?;
    let kind = match option(&opts, 53)?.first() {
        Some(2) => ReplyKind::Offer,
        Some(5) => ReplyKind::Ack,
        Some(6) => return Err(PacketError::Nak),
        _ => return Err(PacketError::Unexpected),
    };
    if kind != expected_kind {
        return Err(PacketError::Unexpected);
    }
    Ok(DhcpReply {
        xid: expected_xid,
        kind,
        address: Ipv4Addr::from(read_array(dhcp, 16)?),
        source: Ipv4Addr::from(read_array(frame, ETH + 12)?),
        server: ipv4(&opts, 54)?,
        lease_seconds: number(&opts, 51)?,
        vendor_specific: opts.get(&125).cloned(),
        subnet_mask: ipv4(&opts, 1)?,
        router: ipv4(&opts, 3)?,
    })
}

/// Options up to END; the first occurrence of a code wins, as in Python.
fn options(raw: &[u8]) -> Result<Options, PacketError> {
    let mut result = Options::new();
    let mut cursor = 0;
    loop {
        match *raw.get(cursor).ok_or(PacketError::Malformed)? {
            0 => cursor += 1,
            255 => return Ok(result),
            code => {
                let len = usize::from(*raw.get(cursor + 1).ok_or(PacketError::Malformed)?);
                let value = raw
                    .get(cursor + 2..cursor + 2 + len)
                    .ok_or(PacketError::Malformed)?;
                result.entry(code).or_insert_with(|| value.to_vec());
                cursor += 2 + len;
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

fn four_bytes(options: &Options, code: u8) -> Result<Option<[u8; 4]>, PacketError> {
    options
        .get(&code)
        .map(|value| {
            <[u8; 4]>::try_from(value.as_slice()).map_err(|_| PacketError::InvalidOption(code))
        })
        .transpose()
}

fn ipv4(options: &Options, code: u8) -> Result<Option<Ipv4Addr>, PacketError> {
    Ok(four_bytes(options, code)?.map(Ipv4Addr::from))
}

fn number(options: &Options, code: u8) -> Result<Option<u32>, PacketError> {
    Ok(four_bytes(options, code)?.map(u32::from_be_bytes))
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
    let (chunks, remainder) = bytes.as_chunks::<2>();
    for chunk in chunks {
        sum = sum.saturating_add(u32::from(u16::from_be_bytes([chunk[0], chunk[1]])));
    }
    if let Some(byte) = remainder.first() {
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
        &frame[ETH + 20 + UDP + DHCP..]
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
        frame(MAC, None, Ipv4Addr::BROADCAST, &payload, 67, 68).unwrap_or_default()
    }

    #[test]
    fn renewal_carries_the_held_address_and_no_requesting_server_options() {
        let renewal = build_renew(
            MAC,
            9,
            Ipv4Addr::new(192, 0, 2, 44),
            Ipv4Addr::new(192, 0, 2, 1),
            &[0xaa],
            b"STB",
        )
        .unwrap_or_default();
        // ciaddr, not option 50, is what asks the server to extend this binding.
        let ciaddr = ETH + 20 + UDP + 12;
        assert_eq!(&renewal[ciaddr..ciaddr + 4], &[192, 0, 2, 44]);
        // Addressed to the server that granted the lease, as the set-top box
        // does, so no other server on the segment can answer the renewal.
        let destination = ETH + 16;
        assert_eq!(&renewal[destination..destination + 4], &[192, 0, 2, 1]);
        assert_eq!(
            option_bytes(&renewal),
            &[
                53, 1, 3, 60, 1, 0xaa, 12, 3, b'S', b'T', b'B', 55, 10, 1, 121, 33, 3, 6, 15, 28,
                51, 58, 59, 255
            ]
        );
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
    fn ack_accepts_unicast_destination_and_zero_checksum() {
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
        frame[ETH + 16..ETH + 20].copy_from_slice(&[192, 0, 2, 44]);
        frame[ETH + 20 + 6..ETH + 20 + 8].fill(0);
        let reply = decode_reply(&frame, 7, MAC, ReplyKind::Ack).expect("unicast ACK");
        assert_eq!(reply.server, Some(Ipv4Addr::new(192, 0, 2, 1)));
    }

    #[test]
    fn replies_to_other_transactions_are_unexpected() {
        let fields = [
            (53, &[5][..]),
            (51, &[0, 0, 14, 16][..]),
            (125, &[16][..]),
            (1, &[255, 255, 255, 0][..]),
            (3, &[192, 0, 2, 1][..]),
            (54, &[192, 0, 2, 1][..]),
        ];
        assert_eq!(
            decode_reply(&reply_frame(8, &fields), 7, MAC, ReplyKind::Ack),
            Err(PacketError::Unexpected)
        );
        assert!(decode_reply(&reply_frame(7, &fields), 7, MAC, ReplyKind::Ack).is_ok());
        assert_eq!(
            decode_reply(&reply_frame(7, &[(53, &[6][..])]), 7, MAC, ReplyKind::Ack),
            Err(PacketError::Nak)
        );
    }
}
