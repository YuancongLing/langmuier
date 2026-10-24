//! Explicit, deterministic no-hardware transports and protocol packets.

use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::sync::Arc;
use std::time::Duration;

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender, TrySendError, bounded};
use langmuir_core::LVDS_FRAME_LEN;
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

    let phase = frame_index % 100;
    for (channel, block) in frame[15..265]
        .as_chunks_mut::<125>()
        .0
        .iter_mut()
        .enumerate()
    {
        let mut sample = 0_u32;
        for bytes in block.as_chunks_mut::<5>().0 {
            let mut packed = 0_u64;
            for _ in 0..4 {
                let code10 = if channel == 0 {
                    512 + phase + sample
                } else {
                    1_023 - phase * 2 - sample
                };
                packed = (packed << 10) | u64::from(code10);
                sample += 1;
            }
            bytes.copy_from_slice(&packed.to_be_bytes()[3..]);
        }
    }
    frame[265] = frame_index.to_be_bytes()[3];
    // DATA251 and the unchecked CRC trailer remain zero in this fixture.
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
        UdpLvdsScienceGate, decode_scientific_frame, parse_udp_lvds_datagram,
    };

    use super::*;

    #[test]
    fn deterministic_packets_match_protocol_layout() {
        let telemetry = rs422_packet(7, 1_700_000_000);
        assert_eq!(telemetry.len(), 34);
        assert_eq!(&telemetry[..6], &[0xEB, 0x90, 0x55, 0xF0, 0x00, 0x1C]);
        assert_eq!(telemetry[33], checksum8(&telemetry[6..33]));

        let scientific = lvds_packet(9, 1_700_000_000);
        assert_eq!(scientific.len(), 269);
        assert_eq!(&scientific[..6], &[0xEB, 0x90, 0x55, 0xAA, 0x01, 0x07]);
        assert_eq!(&lvds_packet(0, 0)[15..20], &[0x80, 0x20, 0x18, 0x0A, 0x03]);
        assert_eq!(
            &lvds_packet(0, 0)[140..145],
            &[0xFF, 0xFF, 0xEF, 0xF7, 0xFC]
        );
        assert_eq!(&scientific[265..267], &[9, 0]);
        let decoded = decode_scientific_frame(&scientific).unwrap();
        assert_eq!(decoded.channel_1[0], 521);
        assert_eq!(decoded.channel_1[99], 620);
        assert_eq!(decoded.channel_2[0], 1_005);
        assert_eq!(decoded.channel_2[99], 906);
    }

    fn golden_frame(seconds: u32, microseconds: u32, frame_seq_u8: u8) -> Vec<u8> {
        let mut frame = vec![0_u8; LVDS_FRAME_LEN];
        frame[..7].copy_from_slice(&[0xEB, 0x90, 0x55, 0xAA, 0x01, 0x07, 0xAA]);
        frame[7..11].copy_from_slice(&seconds.to_be_bytes());
        frame[11..15].copy_from_slice(&microseconds.to_be_bytes());
        for group in frame[15..140].as_chunks_mut::<5>().0 {
            group.copy_from_slice(&[0xFF, 0xC0, 0x08, 0x00, 0x01]);
        }
        for group in frame[140..265].as_chunks_mut::<5>().0 {
            group.copy_from_slice(&[0x00, 0x3F, 0xF0, 0x03, 0xFF]);
        }
        frame[265] = frame_seq_u8;
        frame[267..].copy_from_slice(&[0x12, 0x34]);
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
        let first = golden_frame(1_694_498_816, 999_800, 255);
        let second = golden_frame(1_694_498_817, 49_800, 0);
        let mut corrupt = first.clone();
        corrupt[267] ^= 0x01;
        let datagrams = [
            udp_datagram(65_535, &first),
            udp_datagram(65_536, &corrupt),
            udp_datagram(65_537, &second),
        ];
        let mut reassembler = UdpLvdsReassembler::default();
        let mut science_gate = UdpLvdsScienceGate::default();
        let mut normal_frames = Vec::new();
        let mut outer_sequences = Vec::new();
        let mut errors = Vec::new();
        for datagram in &datagrams {
            let parsed = parse_udp_lvds_datagram(datagram).unwrap();
            assert_eq!(parsed.payload.len(), LVDS_FRAME_LEN);
            let packet = reassembler.push(datagram).unwrap().unwrap();
            outer_sequences.push(packet.frame_sequence);
            match science_gate.accept_at(&packet, std::time::Instant::now()) {
                Ok(result) => normal_frames.push(result.into_frame().unwrap()),
                Err(error) => errors.push(error),
            }
        }
        assert_eq!(normal_frames.len(), 3);
        assert_eq!(errors.len(), 0);
        for frame in &normal_frames {
            assert_eq!(frame.channel_1.len(), 100);
            assert_eq!(frame.channel_2.len(), 100);
            for index in (0..SAMPLE_COUNT).step_by(4) {
                assert_eq!(&frame.channel_1[index..index + 4], &[1023, 0, 512, 1]);
                assert_eq!(&frame.channel_2[index..index + 4], &[0, 1023, 0, 1023]);
            }
        }
        assert_eq!(outer_sequences, [65_535, 65_536, 65_537]);
        assert_eq!(normal_frames[0].frame_seq_u8, 255);
        assert_eq!(normal_frames[1].frame_seq_u8, 255);
        assert_eq!(normal_frames[2].frame_seq_u8, 0);
        assert_eq!(science_gate.metrics().duplicate_frames, 0);
        assert_eq!(science_gate.metrics().sequence_conflicts, 0);
        let first = normal_frames[0].sample_time_us(0).unwrap();
        let last = normal_frames[2].sample_time_us(0).unwrap();
        assert_eq!(last - first, 50_000);
        assert_eq!(SAMPLE_INTERVAL_MICROS, 500);
    }
}
