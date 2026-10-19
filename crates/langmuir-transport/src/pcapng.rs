//! Bounded, read-only PCAPNG packet extraction for offline replay.
//!
//! Timestamps are capture timestamps, truncated to nanoseconds. This module does
//! not open sockets, decode science frames, or repair incomplete packet captures.

use std::io::{self, Read};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::time::Duration;

use thiserror::Error;

const SECTION_HEADER: [u8; 4] = [0x0a, 0x0d, 0x0d, 0x0a];
const MAX_BLOCK_BYTES: usize = 16 * 1024 * 1024;
const MAX_INTERFACES: usize = 4096;
const MAX_METADATA_BLOCKS_WITHOUT_PACKET: usize = 4096;
const MAX_METADATA_BYTES_WITHOUT_PACKET: usize = 64 * 1024 * 1024;

#[derive(Debug, Error)]
pub enum CaptureError {
    #[error("capture read failed: {0}")]
    Io(#[from] io::Error),
    #[error("malformed PCAPNG capture: {0}")]
    Malformed(&'static str),
    #[error("unsupported PCAPNG capture: {0}")]
    Unsupported(&'static str),
    #[error("unsupported PCAPNG link type {0}; Ethernet is required")]
    UnknownLinkType(u16),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CapturedUdp {
    pub source: SocketAddr,
    pub destination: SocketAddr,
    pub payload: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CapturePacket {
    pub timestamp: Duration,
    /// A valid non-UDP packet still produces a packet with `None` here.
    pub udp: Option<CapturedUdp>,
}

#[derive(Clone, Copy)]
enum Endian {
    Little,
    Big,
}

impl Endian {
    fn u16(self, bytes: &[u8]) -> u16 {
        let bytes = [bytes[0], bytes[1]];
        match self {
            Self::Little => u16::from_le_bytes(bytes),
            Self::Big => u16::from_be_bytes(bytes),
        }
    }

    fn u32(self, bytes: &[u8]) -> u32 {
        let bytes = [bytes[0], bytes[1], bytes[2], bytes[3]];
        match self {
            Self::Little => u32::from_le_bytes(bytes),
            Self::Big => u32::from_be_bytes(bytes),
        }
    }

    fn i64(self, bytes: &[u8]) -> i64 {
        let bytes = [
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        ];
        match self {
            Self::Little => i64::from_le_bytes(bytes),
            Self::Big => i64::from_be_bytes(bytes),
        }
    }
}

struct Interface {
    snaplen: u32,
    resolution: u8,
    offset_seconds: i64,
}

/// Streaming PCAPNG reader with bounded block and interface allocations.
///
/// Ethernet EPBs and obsolete Packet Blocks are supported. Simple Packet Blocks
/// have no timestamp and are rejected explicitly rather than assigned a made-up
/// replay time. Each new section resets its interface table and byte order.
pub struct PcapNgReader<R> {
    reader: R,
    endian: Option<Endian>,
    interfaces: Vec<Interface>,
    section_remaining: Option<u64>,
}

impl<R: Read> PcapNgReader<R> {
    pub const fn new(reader: R) -> Self {
        Self {
            reader,
            endian: None,
            interfaces: Vec::new(),
            section_remaining: None,
        }
    }

    /// Read the next packet, including non-UDP packets, or the clean end of file.
    ///
    /// # Errors
    /// Returns an error for incomplete blocks/packets, unsupported link types,
    /// timestamp-free packets, fragmented IP, and malformed protocol lengths.
    pub fn next_packet(&mut self) -> Result<Option<CapturePacket>, CaptureError> {
        let mut metadata_blocks = 0;
        let mut metadata_bytes = 0;
        loop {
            let Some((kind, body, endian)) = self.next_block()? else {
                if self.endian.is_none() {
                    return Err(CaptureError::Malformed("missing section header"));
                }
                return Ok(None);
            };
            if !matches!(kind, 2 | 3 | 6) {
                metadata_blocks += 1;
                metadata_bytes += body.len() + 12;
                if metadata_blocks > MAX_METADATA_BLOCKS_WITHOUT_PACKET
                    || metadata_bytes > MAX_METADATA_BYTES_WITHOUT_PACKET
                {
                    return Err(CaptureError::Unsupported(
                        "excessive metadata without a packet",
                    ));
                }
            }
            match kind {
                0x0a0d_0d0a => self.section(&body, endian)?,
                1 => self.interface(&body, endian)?,
                2 | 6 => return self.packet(kind, &body, endian).map(Some),
                3 => {
                    return Err(CaptureError::Unsupported(
                        "Simple Packet Block has no capture timestamp",
                    ));
                }
                // Metadata and unknown extension blocks are length-checked.
                _ => {}
            }
        }
    }

    fn next_block(&mut self) -> Result<Option<(u32, Vec<u8>, Endian)>, CaptureError> {
        let mut header = [0; 12];
        loop {
            match self.reader.read(&mut header[..1]) {
                Ok(0) => {
                    if self
                        .section_remaining
                        .is_some_and(|remaining| remaining != 0)
                    {
                        return Err(CaptureError::Malformed("truncated declared section"));
                    }
                    return Ok(None);
                }
                Ok(_) => break,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => return Err(error.into()),
            }
        }
        read_complete(&mut self.reader, &mut header[1..8])?;
        let is_section = header[..4] == SECTION_HEADER;
        if is_section
            && self
                .section_remaining
                .is_some_and(|remaining| remaining != 0)
        {
            return Err(CaptureError::Malformed(
                "section ended before declared length",
            ));
        }
        let endian = if is_section {
            read_complete(&mut self.reader, &mut header[8..12])?;
            match header[8..12] {
                [0x4d, 0x3c, 0x2b, 0x1a] => Endian::Little,
                [0x1a, 0x2b, 0x3c, 0x4d] => Endian::Big,
                _ => return Err(CaptureError::Malformed("invalid byte-order magic")),
            }
        } else {
            self.endian
                .ok_or(CaptureError::Malformed("packet before section header"))?
        };
        let length = usize::try_from(endian.u32(&header[4..8]))
            .map_err(|_| CaptureError::Malformed("block length exceeds address space"))?;
        if !(12..=MAX_BLOCK_BYTES).contains(&length) || length % 4 != 0 {
            return Err(CaptureError::Malformed("invalid or excessive block length"));
        }
        if is_section && length < 28 {
            return Err(CaptureError::Malformed("short section header"));
        }
        if !is_section && let Some(remaining) = &mut self.section_remaining {
            *remaining = remaining
                .checked_sub(u64::try_from(length).unwrap_or(u64::MAX))
                .ok_or(CaptureError::Malformed("block exceeds declared section"))?;
        }
        let consumed = if is_section { 12 } else { 8 };
        let mut remainder = vec![0; length - consumed];
        read_complete(&mut self.reader, &mut remainder)?;
        let trailer = remainder.len() - 4;
        if endian.u32(&remainder[trailer..]) != u32::try_from(length).unwrap_or(u32::MAX) {
            return Err(CaptureError::Malformed("block length trailer disagrees"));
        }
        remainder.truncate(trailer);
        let body = if is_section {
            let mut body = Vec::with_capacity(remainder.len() + 4);
            body.extend_from_slice(&header[8..12]);
            body.extend_from_slice(&remainder);
            body
        } else {
            remainder
        };
        Ok(Some((endian.u32(&header[..4]), body, endian)))
    }

    fn section(&mut self, body: &[u8], endian: Endian) -> Result<(), CaptureError> {
        if body.len() < 16 {
            return Err(CaptureError::Malformed("short section header"));
        }
        if endian.u16(&body[4..6]) != 1 || endian.u16(&body[6..8]) != 0 {
            return Err(CaptureError::Unsupported("PCAPNG version is not 1.0"));
        }
        options(&body[16..], endian, |_, _| Ok(()))?;
        self.endian = Some(endian);
        self.interfaces.clear();
        self.section_remaining = match endian.i64(&body[8..16]) {
            -1 => None,
            length if length >= 0 => Some(
                u64::try_from(length)
                    .map_err(|_| CaptureError::Malformed("invalid section length"))?,
            ),
            _ => return Err(CaptureError::Malformed("negative section length")),
        };
        Ok(())
    }

    fn interface(&mut self, body: &[u8], endian: Endian) -> Result<(), CaptureError> {
        if body.len() < 8 {
            return Err(CaptureError::Malformed("short interface description"));
        }
        let link = endian.u16(body);
        if link != 1 {
            return Err(CaptureError::UnknownLinkType(link));
        }
        if endian.u16(&body[2..4]) != 0 {
            return Err(CaptureError::Malformed("nonzero reserved interface field"));
        }
        if self.interfaces.len() == MAX_INTERFACES {
            return Err(CaptureError::Malformed("too many interfaces"));
        }
        let mut interface = Interface {
            snaplen: endian.u32(&body[4..8]),
            resolution: 6,
            offset_seconds: 0,
        };
        options(&body[8..], endian, |code, value| {
            match code {
                9 if value.len() == 1 => interface.resolution = value[0],
                14 if value.len() == 8 => interface.offset_seconds = endian.i64(value),
                9 | 14 => return Err(CaptureError::Malformed("bad timestamp option size")),
                _ => {}
            }
            Ok(())
        })?;
        self.interfaces.push(interface);
        Ok(())
    }

    fn packet(
        &self,
        kind: u32,
        body: &[u8],
        endian: Endian,
    ) -> Result<CapturePacket, CaptureError> {
        if body.len() < 20 {
            return Err(CaptureError::Malformed("short packet block"));
        }
        let interface_id = if kind == 2 {
            u32::from(endian.u16(body))
        } else {
            endian.u32(body)
        };
        let interface = usize::try_from(interface_id)
            .ok()
            .and_then(|id| self.interfaces.get(id))
            .ok_or(CaptureError::Malformed("unknown packet interface"))?;
        let captured = usize::try_from(endian.u32(&body[12..16]))
            .map_err(|_| CaptureError::Malformed("packet length exceeds address space"))?;
        let original = endian.u32(&body[16..20]);
        if captured != usize::try_from(original).unwrap_or(usize::MAX) {
            return Err(CaptureError::Unsupported(
                "packet was truncated by capture snaplen",
            ));
        }
        if interface.snaplen != 0 && original > interface.snaplen {
            return Err(CaptureError::Malformed("packet exceeds interface snaplen"));
        }
        let padded = captured
            .checked_add(3)
            .map(|value| value & !3)
            .ok_or(CaptureError::Malformed("packet padding overflow"))?;
        if padded > body.len() - 20 {
            return Err(CaptureError::Malformed("packet bytes exceed block"));
        }
        options(&body[20 + padded..], endian, |_, _| Ok(()))?;
        let ticks =
            (u64::from(endian.u32(&body[4..8])) << 32) | u64::from(endian.u32(&body[8..12]));
        Ok(CapturePacket {
            timestamp: timestamp(ticks, interface)?,
            udp: ethernet(&body[20..20 + captured])?,
        })
    }
}

fn read_complete(reader: &mut impl Read, bytes: &mut [u8]) -> Result<(), CaptureError> {
    reader.read_exact(bytes).map_err(|error| {
        if error.kind() == io::ErrorKind::UnexpectedEof {
            CaptureError::Malformed("truncated block")
        } else {
            CaptureError::Io(error)
        }
    })
}

fn options(
    mut bytes: &[u8],
    endian: Endian,
    mut visit: impl FnMut(u16, &[u8]) -> Result<(), CaptureError>,
) -> Result<(), CaptureError> {
    while !bytes.is_empty() {
        if bytes.len() < 4 {
            return Err(CaptureError::Malformed("short option header"));
        }
        let code = endian.u16(bytes);
        let length = usize::from(endian.u16(&bytes[2..4]));
        if code == 0 {
            if length != 0 || bytes[4..].iter().any(|byte| *byte != 0) {
                return Err(CaptureError::Malformed("invalid option terminator"));
            }
            return Ok(());
        }
        let padded = (length + 3) & !3;
        if padded > bytes.len() - 4 {
            return Err(CaptureError::Malformed("option exceeds block"));
        }
        visit(code, &bytes[4..4 + length])?;
        bytes = &bytes[4 + padded..];
    }
    Ok(())
}

fn timestamp(ticks: u64, interface: &Interface) -> Result<Duration, CaptureError> {
    let resolution = interface.resolution;
    let divisor = if resolution & 0x80 == 0 {
        10_u128.checked_pow(u32::from(resolution))
    } else {
        1_u128.checked_shl(u32::from(resolution & 0x7f))
    };
    // A decimal resolution beyond u128 makes even u64::MAX ticks < 1ns.
    let nanos = divisor.map_or(0, |value| u128::from(ticks) * 1_000_000_000 / value);
    let nanos = i128::try_from(nanos).map_err(|_| CaptureError::Malformed("timestamp overflow"))?
        + i128::from(interface.offset_seconds) * 1_000_000_000;
    let nanos = u128::try_from(nanos)
        .map_err(|_| CaptureError::Unsupported("capture timestamp is before Unix epoch"))?;
    let seconds = u64::try_from(nanos / 1_000_000_000)
        .map_err(|_| CaptureError::Malformed("timestamp overflow"))?;
    let subsecond = u32::try_from(nanos % 1_000_000_000)
        .map_err(|_| CaptureError::Malformed("timestamp fraction overflow"))?;
    Ok(Duration::new(seconds, subsecond))
}

fn network_u16(bytes: &[u8]) -> u16 {
    u16::from_be_bytes([bytes[0], bytes[1]])
}

fn ethernet(bytes: &[u8]) -> Result<Option<CapturedUdp>, CaptureError> {
    if bytes.len() < 14 {
        return Err(CaptureError::Malformed("short Ethernet frame"));
    }
    let mut protocol = network_u16(&bytes[12..14]);
    let mut offset = 14;
    let mut tags = 0;
    while matches!(protocol, 0x8100 | 0x88a8 | 0x9100) {
        tags += 1;
        if tags > 4 {
            return Err(CaptureError::Unsupported("more than four VLAN tags"));
        }
        if bytes.len() < offset + 4 {
            return Err(CaptureError::Malformed("short Ethernet VLAN tag"));
        }
        protocol = network_u16(&bytes[offset + 2..offset + 4]);
        offset += 4;
    }
    match protocol {
        0x0800 => ipv4(&bytes[offset..]),
        0x86dd => ipv6(&bytes[offset..]),
        _ => Ok(None),
    }
}

fn ipv4(bytes: &[u8]) -> Result<Option<CapturedUdp>, CaptureError> {
    if bytes.len() < 20 || bytes[0] >> 4 != 4 {
        return Err(CaptureError::Malformed("invalid IPv4 header"));
    }
    let header = usize::from(bytes[0] & 0x0f) * 4;
    let total = usize::from(network_u16(&bytes[2..4]));
    if header < 20 || header > total || total > bytes.len() {
        return Err(CaptureError::Malformed("invalid IPv4 packet length"));
    }
    if network_u16(&bytes[6..8]) & 0x3fff != 0 {
        return Err(CaptureError::Unsupported("fragmented IPv4 packet"));
    }
    if bytes[9] != 17 {
        return Ok(None);
    }
    let source = IpAddr::V4(Ipv4Addr::new(bytes[12], bytes[13], bytes[14], bytes[15]));
    let destination = IpAddr::V4(Ipv4Addr::new(bytes[16], bytes[17], bytes[18], bytes[19]));
    udp(&bytes[header..total], source, destination).map(Some)
}

fn ipv6(bytes: &[u8]) -> Result<Option<CapturedUdp>, CaptureError> {
    if bytes.len() < 40 || bytes[0] >> 4 != 6 {
        return Err(CaptureError::Malformed("invalid IPv6 header"));
    }
    let payload_length = usize::from(network_u16(&bytes[4..6]));
    if payload_length == 0 {
        if bytes[6] == 59 {
            return Ok(None);
        }
        return Err(CaptureError::Unsupported("IPv6 jumbogram or empty payload"));
    }
    let total = 40 + payload_length;
    if total > bytes.len() {
        return Err(CaptureError::Malformed("invalid IPv6 packet length"));
    }
    let source_bytes = <[u8; 16]>::try_from(&bytes[8..24])
        .map_err(|_| CaptureError::Malformed("invalid IPv6 source"))?;
    let destination_bytes = <[u8; 16]>::try_from(&bytes[24..40])
        .map_err(|_| CaptureError::Malformed("invalid IPv6 destination"))?;
    let mut next = bytes[6];
    let mut offset = 40;
    for _ in 0..8 {
        match next {
            17 => {
                return udp(
                    &bytes[offset..total],
                    IpAddr::V6(Ipv6Addr::from(source_bytes)),
                    IpAddr::V6(Ipv6Addr::from(destination_bytes)),
                )
                .map(Some);
            }
            44 => return Err(CaptureError::Unsupported("fragmented IPv6 packet")),
            0 | 43 | 60 | 51 => {
                if total - offset < 2 {
                    return Err(CaptureError::Malformed("short IPv6 extension"));
                }
                let length = if next == 51 {
                    (usize::from(bytes[offset + 1]) + 2) * 4
                } else {
                    (usize::from(bytes[offset + 1]) + 1) * 8
                };
                if length > total - offset {
                    return Err(CaptureError::Malformed("invalid IPv6 extension length"));
                }
                next = bytes[offset];
                offset += length;
            }
            _ => return Ok(None),
        }
    }
    Err(CaptureError::Unsupported("too many IPv6 extensions"))
}

fn udp(bytes: &[u8], source: IpAddr, destination: IpAddr) -> Result<CapturedUdp, CaptureError> {
    if bytes.len() < 8 {
        return Err(CaptureError::Malformed("short UDP header"));
    }
    let length = usize::from(network_u16(&bytes[4..6]));
    if length < 8 || length != bytes.len() {
        return Err(CaptureError::Malformed(
            "UDP length disagrees with IP payload",
        ));
    }
    Ok(CapturedUdp {
        source: SocketAddr::new(source, network_u16(&bytes[..2])),
        destination: SocketAddr::new(destination, network_u16(&bytes[2..4])),
        payload: bytes[8..].to_vec(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn word(value: u32, endian: Endian) -> [u8; 4] {
        match endian {
            Endian::Little => value.to_le_bytes(),
            Endian::Big => value.to_be_bytes(),
        }
    }

    fn half(value: u16, endian: Endian) -> [u8; 2] {
        match endian {
            Endian::Little => value.to_le_bytes(),
            Endian::Big => value.to_be_bytes(),
        }
    }

    fn block(kind: u32, body: &[u8], endian: Endian) -> Vec<u8> {
        let length = u32::try_from(body.len() + 12).unwrap();
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&word(kind, endian));
        bytes.extend_from_slice(&word(length, endian));
        bytes.extend_from_slice(body);
        bytes.extend_from_slice(&word(length, endian));
        bytes
    }

    fn section(endian: Endian) -> Vec<u8> {
        let mut body = Vec::new();
        body.extend_from_slice(&word(0x1a2b_3c4d, endian));
        body.extend_from_slice(&half(1, endian));
        body.extend_from_slice(&half(0, endian));
        body.extend_from_slice(&[0xff; 8]);
        block(0x0a0d_0d0a, &body, endian)
    }

    fn interface(endian: Endian, link: u16, resolution: Option<u8>) -> Vec<u8> {
        let mut body = Vec::new();
        body.extend_from_slice(&half(link, endian));
        body.extend_from_slice(&half(0, endian));
        body.extend_from_slice(&word(65535, endian));
        if let Some(resolution) = resolution {
            body.extend_from_slice(&half(9, endian));
            body.extend_from_slice(&half(1, endian));
            body.extend_from_slice(&[resolution, 0, 0, 0]);
            body.extend_from_slice(&[0; 4]);
        }
        block(1, &body, endian)
    }

    fn packet(endian: Endian, data: &[u8], ticks: u64) -> Vec<u8> {
        let mut body = Vec::new();
        body.extend_from_slice(&word(0, endian));
        body.extend_from_slice(&word(u32::try_from(ticks >> 32).unwrap(), endian));
        body.extend_from_slice(&word(u32::try_from(ticks & 0xffff_ffff).unwrap(), endian));
        let length = u32::try_from(data.len()).unwrap();
        body.extend_from_slice(&word(length, endian));
        body.extend_from_slice(&word(length, endian));
        body.extend_from_slice(data);
        body.resize((body.len() + 3) & !3, 0);
        block(6, &body, endian)
    }

    fn ethernet_udp(payload: &[u8]) -> Vec<u8> {
        let mut bytes = vec![0; 14 + 20 + 8];
        bytes[12..14].copy_from_slice(&0x0800_u16.to_be_bytes());
        bytes[14] = 0x45;
        let length = u16::try_from(20 + 8 + payload.len()).unwrap();
        bytes[16..18].copy_from_slice(&length.to_be_bytes());
        bytes[23] = 17;
        bytes[26..30].copy_from_slice(&[192, 168, 1, 4]);
        bytes[30..34].copy_from_slice(&[192, 168, 1, 5]);
        bytes[34..36].copy_from_slice(&8080_u16.to_be_bytes());
        bytes[36..38].copy_from_slice(&9000_u16.to_be_bytes());
        bytes[38..40].copy_from_slice(&u16::try_from(8 + payload.len()).unwrap().to_be_bytes());
        bytes.extend_from_slice(payload);
        bytes
    }

    fn capture(endian: Endian, data: &[u8], ticks: u64) -> Vec<u8> {
        let mut bytes = section(endian);
        bytes.extend(interface(endian, 1, None));
        bytes.extend(packet(endian, data, ticks));
        bytes
    }

    #[test]
    fn endian_sections_preserve_udp_addresses_bytes_and_timestamps() {
        let payload = [0, 55, 255, 13, 10];
        let ethernet = ethernet_udp(&payload);
        let mut bytes = capture(Endian::Little, &ethernet, 1_234_567);
        bytes.extend(capture(Endian::Big, &ethernet, 2_345_678));
        let mut reader = PcapNgReader::new(Cursor::new(bytes));
        for micros in [1_234_567, 2_345_678] {
            let packet = reader.next_packet().unwrap().unwrap();
            assert_eq!(packet.timestamp, Duration::from_micros(micros));
            let udp = packet.udp.unwrap();
            assert_eq!(udp.source, "192.168.1.4:8080".parse().unwrap());
            assert_eq!(udp.destination, "192.168.1.5:9000".parse().unwrap());
            assert_eq!(udp.payload, payload);
        }
        assert!(reader.next_packet().unwrap().is_none());
    }

    #[test]
    fn binary_resolution_and_signed_timestamp_offset_are_honored() {
        let endian = Endian::Little;
        let mut bytes = section(endian);
        let mut body = Vec::new();
        body.extend_from_slice(&half(1, endian));
        body.extend_from_slice(&half(0, endian));
        body.extend_from_slice(&word(65535, endian));
        body.extend_from_slice(&[9, 0, 1, 0, 0x8a, 0, 0, 0]);
        body.extend_from_slice(&[14, 0, 8, 0]);
        body.extend_from_slice(&(-1_i64).to_le_bytes());
        body.extend_from_slice(&[0; 4]);
        bytes.extend(block(1, &body, endian));
        bytes.extend(packet(endian, &ethernet_udp(&[7]), 1536));
        let packet = PcapNgReader::new(Cursor::new(bytes))
            .next_packet()
            .unwrap()
            .unwrap();
        assert_eq!(packet.timestamp, Duration::from_millis(500));
    }

    #[test]
    fn truncated_blocks_bad_lengths_and_unknown_links_fail_explicitly() {
        let valid = capture(Endian::Little, &ethernet_udp(&[1, 2]), 0);
        for length in [0, 1, 7, 11, 27, valid.len() - 1] {
            assert!(
                PcapNgReader::new(Cursor::new(&valid[..length]))
                    .next_packet()
                    .is_err()
            );
        }
        let mut unknown = section(Endian::Little);
        unknown.extend(interface(Endian::Little, 101, None));
        assert!(matches!(
            PcapNgReader::new(Cursor::new(unknown)).next_packet(),
            Err(CaptureError::UnknownLinkType(101))
        ));
        let mut bad_trailer = valid;
        *bad_trailer.last_mut().unwrap() = 255;
        assert!(
            PcapNgReader::new(Cursor::new(bad_trailer))
                .next_packet()
                .is_err()
        );
        let mut excessive = section(Endian::Little);
        excessive.extend_from_slice(&[6, 0, 0, 0]);
        excessive.extend_from_slice(&u32::MAX.to_le_bytes());
        assert!(
            PcapNgReader::new(Cursor::new(excessive))
                .next_packet()
                .is_err()
        );
    }

    #[test]
    fn vlan_non_udp_and_ipv6_udp_are_distinguished() {
        let original = ethernet_udp(&[4, 5, 6]);
        let mut vlan = original[..12].to_vec();
        vlan.extend_from_slice(&[0x81, 0, 0, 9]);
        vlan.extend_from_slice(&original[12..]);
        assert_eq!(ethernet(&vlan).unwrap().unwrap().payload, [4, 5, 6]);
        let mut arp = vec![0; 42];
        arp[12..14].copy_from_slice(&0x0806_u16.to_be_bytes());
        assert!(ethernet(&arp).unwrap().is_none());
        let mut ipv6 = vec![0; 14 + 40];
        ipv6[12..14].copy_from_slice(&0x86dd_u16.to_be_bytes());
        ipv6[14] = 0x60;
        ipv6[18..20].copy_from_slice(&11_u16.to_be_bytes());
        ipv6[20] = 17;
        ipv6[37] = 1;
        ipv6[53] = 2;
        ipv6.extend_from_slice(&original[34..]);
        let udp = ethernet(&ipv6).unwrap().unwrap();
        assert_eq!(udp.source, "[::1]:8080".parse().unwrap());
        assert_eq!(udp.destination, "[::2]:9000".parse().unwrap());
        assert_eq!(udp.payload, [4, 5, 6]);
    }

    #[test]
    fn fragment_snaplen_and_timestamp_free_packets_are_not_silently_skipped() {
        let mut fragmented = ethernet_udp(&[1]);
        fragmented[20..22].copy_from_slice(&0x2000_u16.to_be_bytes());
        assert!(matches!(
            ethernet(&fragmented),
            Err(CaptureError::Unsupported(_))
        ));
        let mut bytes = section(Endian::Little);
        bytes.extend(interface(Endian::Little, 1, None));
        let mut truncated = packet(Endian::Little, &ethernet_udp(&[1]), 0);
        truncated[24..28].copy_from_slice(&999_u32.to_le_bytes());
        bytes.extend(truncated);
        assert!(PcapNgReader::new(Cursor::new(bytes)).next_packet().is_err());
        let mut simple = section(Endian::Little);
        simple.extend(interface(Endian::Little, 1, None));
        simple.extend(block(3, &[0; 4], Endian::Little));
        assert!(matches!(
            PcapNgReader::new(Cursor::new(simple)).next_packet(),
            Err(CaptureError::Unsupported(_))
        ));
    }

    #[test]
    fn all_actual_packets_are_returned_even_when_not_udp() {
        let mut arp = vec![0; 42];
        arp[12..14].copy_from_slice(&0x0806_u16.to_be_bytes());
        let mut bytes = capture(Endian::Little, &arp, 0);
        bytes.extend(packet(Endian::Little, &ethernet_udp(&[9]), 50_000));
        let mut reader = PcapNgReader::new(Cursor::new(bytes));
        assert!(reader.next_packet().unwrap().unwrap().udp.is_none());
        assert_eq!(
            reader.next_packet().unwrap().unwrap().timestamp,
            Duration::from_millis(50)
        );
        assert!(reader.next_packet().unwrap().is_none());
    }

    #[test]
    fn excessive_metadata_cannot_keep_one_packet_read_scanning_without_a_bound() {
        let mut bytes = section(Endian::Little);
        bytes.extend(interface(Endian::Little, 1, None));
        for _ in 0..MAX_METADATA_BLOCKS_WITHOUT_PACKET {
            bytes.extend(block(0x1234, &[], Endian::Little));
        }
        assert!(matches!(
            PcapNgReader::new(Cursor::new(bytes)).next_packet(),
            Err(CaptureError::Unsupported(
                "excessive metadata without a packet"
            ))
        ));
    }

    #[test]
    fn finite_section_lengths_and_legacy_packet_blocks_are_validated() {
        let endian = Endian::Little;
        let ethernet = ethernet_udp(&[8]);
        let mut legacy = packet(endian, &ethernet, 123);
        legacy[..4].copy_from_slice(&word(2, endian));
        // An obsolete Packet Block uses a u16 interface and u16 drop count.
        legacy[10..12].copy_from_slice(&half(17, endian));
        let interface = interface(endian, 1, None);
        let mut bytes = section(endian);
        let length = i64::try_from(interface.len() + legacy.len()).unwrap();
        bytes[16..24].copy_from_slice(&length.to_le_bytes());
        bytes.extend(interface);
        bytes.extend(legacy);
        let mut reader = PcapNgReader::new(Cursor::new(&bytes));
        assert_eq!(
            reader.next_packet().unwrap().unwrap().timestamp,
            Duration::from_micros(123)
        );
        assert!(reader.next_packet().unwrap().is_none());

        bytes[16..24].copy_from_slice(&(length + 4).to_le_bytes());
        let mut truncated = PcapNgReader::new(Cursor::new(&bytes));
        assert!(truncated.next_packet().unwrap().is_some());
        assert!(truncated.next_packet().is_err());

        bytes[16..24].copy_from_slice(&(length - 4).to_le_bytes());
        assert!(PcapNgReader::new(Cursor::new(bytes)).next_packet().is_err());
    }
}
