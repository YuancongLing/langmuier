//! Explicit, deterministic no-hardware transports and protocol packets.

use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::sync::Arc;
use std::time::Duration;

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender, TrySendError, bounded};
use langmuir_core::{
    LVDS_CRC_CALC_END, LVDS_CRC_CALC_START, LVDS_CRC_FIELD_OFFSET, LVDS_FRAME_LEN,
    crc16_ccitt_false,
};
use parking_lot::Mutex;

use crate::{Transport, TransportError};

pub struct MemoryTransport {
    endpoint: String,
    input: Receiver<Vec<u8>>,
    current: VecDeque<u8>,
    writes: Arc<Mutex<Vec<u8>>>,
    read_timeout: Duration,
}

pub struct MemoryInput {
    sender: Option<Sender<Vec<u8>>>,
    writes: Arc<Mutex<Vec<u8>>>,
}

impl MemoryInput {
    /// Adds one deterministic byte chunk to the bounded test input.
    ///
    /// # Errors
    ///
    /// Returns the original bytes when the test queue is full or closed.
    pub fn feed(&self, bytes: Vec<u8>) -> Result<(), MemoryFeedError> {
        if bytes.is_empty() {
            return Err(MemoryFeedError::Empty);
        }
        let Some(sender) = &self.sender else {
            return Err(MemoryFeedError::Closed(bytes));
        };
        sender.try_send(bytes).map_err(|error| match error {
            TrySendError::Full(bytes) => MemoryFeedError::Full(bytes),
            TrySendError::Disconnected(bytes) => MemoryFeedError::Closed(bytes),
        })
    }

    pub fn close(&mut self) {
        self.sender.take();
    }

    #[must_use]
    pub fn written_bytes(&self) -> Vec<u8> {
        self.writes.lock().clone()
    }
}

#[derive(Debug)]
pub enum MemoryFeedError {
    Empty,
    Full(Vec<u8>),
    Closed(Vec<u8>),
}

/// Creates an explicitly selected, bounded in-memory transport.
///
/// # Errors
///
/// Returns an error when capacity or timeout is zero.
pub fn memory_transport(
    capacity: usize,
    read_timeout: Duration,
) -> Result<(MemoryTransport, MemoryInput), TransportError> {
    if capacity == 0 {
        return Err(TransportError::InvalidConfiguration(
            "memory transport capacity must be non-zero".to_owned(),
        ));
    }
    if read_timeout.is_zero() {
        return Err(TransportError::InvalidConfiguration(
            "memory transport timeout must be non-zero".to_owned(),
        ));
    }
    let (sender, input) = bounded(capacity);
    let writes = Arc::new(Mutex::new(Vec::new()));
    Ok((
        MemoryTransport {
            endpoint: "memory://explicit-mock".to_owned(),
            input,
            current: VecDeque::new(),
            writes: Arc::clone(&writes),
            read_timeout,
        },
        MemoryInput {
            sender: Some(sender),
            writes,
        },
    ))
}

impl Read for MemoryTransport {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if output.is_empty() {
            return Ok(0);
        }
        if self.current.is_empty() {
            match self.input.recv_timeout(self.read_timeout) {
                Ok(chunk) => self.current.extend(chunk),
                Err(RecvTimeoutError::Timeout) => {
                    return Err(io::Error::new(io::ErrorKind::TimedOut, "mock read timeout"));
                }
                Err(RecvTimeoutError::Disconnected) => return Ok(0),
            }
        }
        let count = output.len().min(self.current.len());
        for slot in &mut output[..count] {
            *slot = self.current.pop_front().expect("length was checked");
        }
        Ok(count)
    }
}

impl Write for MemoryTransport {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.writes.lock().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Transport for MemoryTransport {
    fn endpoint(&self) -> &str {
        &self.endpoint
    }

    fn set_read_timeout(&mut self, timeout: Duration) -> Result<(), TransportError> {
        if timeout.is_zero() {
            return Err(TransportError::InvalidConfiguration(
                "mock read timeout must be non-zero".to_owned(),
            ));
        }
        self.read_timeout = timeout;
        Ok(())
    }
}

#[must_use]
pub fn rs422_packet(sequence: u8, timestamp_seconds: u32) -> [u8; 34] {
    let mut frame = [0_u8; 34];
    frame[0..6].copy_from_slice(&[0xEB, 0x90, 0x55, 0xF0, 0x00, 0x1C]);
    let effective = &mut frame[6..33];
    effective[0] = 0x1A;
    effective[1] = sequence;
    effective[2] = sequence;
    effective[3] = 0;
    effective[4..8].copy_from_slice(&timestamp_seconds.to_be_bytes());
    effective[8..12].copy_from_slice(&u32::from(sequence).to_be_bytes());
    effective[12] = [0x55, 0xAA, 0xBB][usize::from(sequence % 3)];
    effective[13] = sequence % 100 + 1;
    effective[14..16].copy_from_slice(&(u16::from(sequence) * 10).to_be_bytes());
    effective[16] = 35;
    effective[17..22].fill(1);
    effective[22..27].copy_from_slice(&[0x21, 0x55, sequence, 0x9B, 0x9B]);
    frame[33] = checksum8(&frame[6..33]);
    frame
}

#[must_use]
pub fn lvds_packet(frame_index: u32, timestamp_seconds: u32) -> Vec<u8> {
    let mut frame = vec![0_u8; LVDS_FRAME_LEN];
    frame[0..6].copy_from_slice(&[0xEB, 0x90, 0x55, 0xAA, 0x01, 0x07]);
    frame[6] = 0xAA;
    frame[7..11].copy_from_slice(&timestamp_seconds.to_be_bytes());
    frame[11..15].copy_from_slice(&(frame_index % 1_000_000).to_be_bytes());

    let channel_1 = 2_048 + (frame_index % 100) as u16;
    let channel_2 = 20_000 + (frame_index % 100) as u16 * 2;
    frame[15..17].copy_from_slice(&channel_1.to_be_bytes());
    frame[17..19].copy_from_slice(&channel_2.to_be_bytes());
    for sample in 1..125_u16 {
        let offset = 15 + 4 + usize::from(sample - 1) * 2;
        frame[offset] = 1;
        frame[offset + 1] = 3;
    }
    let checksum = crc16_ccitt_false(&frame[LVDS_CRC_CALC_START..LVDS_CRC_CALC_END]);
    frame[LVDS_CRC_FIELD_OFFSET..].copy_from_slice(&checksum.to_be_bytes());
    frame
}

#[must_use]
pub fn checksum8(bytes: &[u8]) -> u8 {
    bytes.iter().fold(0_u8, |sum, byte| sum.wrapping_add(*byte))
}

#[cfg(test)]
mod tests {
    use langmuir_core::{
        SAMPLE_COUNT, SAMPLE_INTERVAL_MICROS, UDP_LVDS_HEADER_LEN, UdpLvdsReassembler,
        decode_scientific_frame, decode_scientific_frame_with_sequence, parse_udp_lvds_datagram,
    };

    use super::*;

    #[test]
    fn deterministic_packets_match_python_layout() {
        let telemetry = rs422_packet(7, 1_700_000_000);
        assert_eq!(telemetry.len(), 34);
        assert_eq!(&telemetry[..6], &[0xEB, 0x90, 0x55, 0xF0, 0x00, 0x1C]);
        assert_eq!(telemetry[33], checksum8(&telemetry[6..33]));

        let scientific = lvds_packet(9, 1_700_000_000);
        assert_eq!(scientific.len(), 269);
        assert_eq!(&scientific[..6], &[0xEB, 0x90, 0x55, 0xAA, 0x01, 0x07]);
        let decoded = decode_scientific_frame(&scientific).unwrap();
        assert_eq!(decoded.channel_1[0], 2_057);
        assert_eq!(decoded.channel_2[124], 20_390);
    }

    fn golden_frame(seconds: u32, microseconds: u32, checksum: u16) -> Vec<u8> {
        let mut frame = vec![0_u8; LVDS_FRAME_LEN];
        frame[..7].copy_from_slice(&[0xEB, 0x90, 0x55, 0xAA, 0x01, 0x07, 0xAA]);
        frame[7..11].copy_from_slice(&seconds.to_be_bytes());
        frame[11..15].copy_from_slice(&microseconds.to_be_bytes());
        frame[15..19].copy_from_slice(&[0x12, 0x34, 0xAB, 0xCD]);
        for offset in (19..267).step_by(2) {
            frame[offset] = 0x01;
            frame[offset + 1] = 0xFF;
        }
        frame[267..].copy_from_slice(&checksum.to_be_bytes());
        frame
    }

    fn udp_datagram(sequence: u32, frame: &[u8]) -> Vec<u8> {
        let mut datagram = vec![0_u8; UDP_LVDS_HEADER_LEN];
        datagram[..4].copy_from_slice(&[0x55, 0xAA, 0x01, 0xA5]);
        datagram[4..8].copy_from_slice(&sequence.to_be_bytes());
        datagram[8..12].copy_from_slice(&u32::try_from(frame.len()).unwrap().to_be_bytes());
        datagram[14..16].copy_from_slice(&1_u16.to_be_bytes());
        datagram[20..22].copy_from_slice(&u16::try_from(frame.len()).unwrap().to_be_bytes());
        datagram.extend_from_slice(frame);
        datagram
    }

    #[test]
    fn protocol_mock_delivers_both_channels_without_crc_acceptance_gate() {
        // CRC values independently calculated with the manufacturer's C function.
        let first = golden_frame(1_694_498_816, 999_800, 0x44C8);
        let second = golden_frame(1_694_498_817, 49_800, 0x5EF9);
        let mut corrupt = first.clone();
        corrupt[267] ^= 0x01;
        let datagrams = [
            udp_datagram(41, &first),
            udp_datagram(42, &corrupt),
            udp_datagram(43, &second),
        ];
        let mut reassembler = UdpLvdsReassembler::default();
        let mut normal_frames = Vec::new();
        let mut errors = Vec::new();
        for datagram in &datagrams {
            let parsed = parse_udp_lvds_datagram(datagram).unwrap();
            assert_eq!(parsed.payload.len(), LVDS_FRAME_LEN);
            let packet = reassembler.push(datagram).unwrap().unwrap();
            match decode_scientific_frame_with_sequence(
                &packet.bytes,
                u16::try_from(packet.frame_sequence).unwrap(),
            ) {
                Ok(frame) => normal_frames.push(frame),
                Err(error) => errors.push(error),
            }
        }
        assert_eq!(normal_frames.len(), 3);
        assert_eq!(errors.len(), 0);
        for frame in &normal_frames {
            assert_eq!(frame.channel_1.len(), SAMPLE_COUNT);
            assert_eq!(frame.channel_2.len(), SAMPLE_COUNT);
            for index in 0..SAMPLE_COUNT {
                let offset = u32::try_from(index).unwrap();
                assert_eq!(frame.channel_1[index], 4_660 + offset);
                assert_eq!(frame.channel_2[index], 43_981 - offset);
            }
        }
        assert_eq!(normal_frames[0].sequence, 41);
        assert_eq!(normal_frames[1].sequence, 42);
        assert_eq!(normal_frames[2].sequence, 43);
        let elapsed = normal_frames[2].timestamp.as_seconds_f64()
            - normal_frames[0].timestamp.as_seconds_f64();
        assert!((elapsed - 0.05).abs() < 0.000_001);
        assert_eq!(SAMPLE_INTERVAL_MICROS, 400);
    }
}
