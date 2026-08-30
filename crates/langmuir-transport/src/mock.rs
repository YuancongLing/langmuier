//! Explicit, deterministic no-hardware transports and protocol packets.

use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::sync::Arc;
use std::time::Duration;

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender, TrySendError, bounded};
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
pub fn rs422_packet(sequence: u8, timestamp_seconds: u32) -> [u8; 64] {
    let mut frame = [0_u8; 64];
    frame[0..3].copy_from_slice(&[0xEB, 0x90, 0x3A]);
    let effective = &mut frame[3..61];
    effective[0] = 0x04;
    effective[1] = 0x12;
    effective[2] = 0x10 | (sequence & 0x7F);
    effective[3..7].copy_from_slice(&timestamp_seconds.to_be_bytes());
    effective[7..11].copy_from_slice(&u32::from(sequence).to_be_bytes());
    effective[11] = [0x55, 0xAA, 0xBB][usize::from(sequence % 3)];
    effective[12] = sequence % 100 + 1;
    effective[13..15].copy_from_slice(&(u16::from(sequence) * 10).to_be_bytes());
    effective[15] = sequence;
    effective[16] = 0;
    effective[17] = 35;
    effective[18..20].copy_from_slice(&(25_000_u16 + u16::from(sequence)).to_be_bytes());
    effective[20..22].copy_from_slice(&(25_100_u16 + u16::from(sequence)).to_be_bytes());
    effective[22..24].copy_from_slice(&(25_200_u16 + u16::from(sequence)).to_be_bytes());
    effective[24] = 30;
    effective[25..31].fill(1);
    frame[61] = checksum8(&frame[3..61]);
    frame[62..64].copy_from_slice(&[0x09, 0xD7]);
    frame
}

#[must_use]
pub fn lvds_packet(frame_index: u32, timestamp_seconds: u32) -> Vec<u8> {
    const FRAME_LEN: usize = 270;
    let mut frame = vec![0_u8; FRAME_LEN];
    frame[0..6].copy_from_slice(&[0xEB, 0x90, 0x55, 0xAA, 0x01, 0x07]);
    frame[6] = 0xAA;
    frame[7..11].copy_from_slice(&timestamp_seconds.to_be_bytes());
    frame[11..15].copy_from_slice(&(frame_index % 1_000_000).to_be_bytes());
    frame[15..17].copy_from_slice(&(frame_index as u16).to_be_bytes());

    let channel_1 = 2_048 + (frame_index % 100) as u16;
    let channel_2 = 20_000 + (frame_index % 100) as u16 * 2;
    frame[17..19].copy_from_slice(&channel_1.to_be_bytes());
    frame[19..21].copy_from_slice(&channel_2.to_be_bytes());
    for sample in 1..125_u16 {
        let offset = 17 + 4 + usize::from(sample - 1) * 2;
        frame[offset] = 1;
        frame[offset + 1] = 3;
    }
    frame[269] = checksum8(&frame[6..269]);
    frame
}

#[must_use]
pub fn checksum8(bytes: &[u8]) -> u8 {
    bytes.iter().fold(0_u8, |sum, byte| sum.wrapping_add(*byte))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_packets_match_python_layout() {
        let telemetry = rs422_packet(7, 1_700_000_000);
        assert_eq!(telemetry.len(), 64);
        assert_eq!(&telemetry[..3], &[0xEB, 0x90, 0x3A]);
        assert_eq!(telemetry[61], checksum8(&telemetry[3..61]));
        assert_eq!(&telemetry[62..], &[0x09, 0xD7]);

        let scientific = lvds_packet(9, 1_700_000_000);
        assert_eq!(scientific.len(), 270);
        assert_eq!(&scientific[..6], &[0xEB, 0x90, 0x55, 0xAA, 0x01, 0x07]);
        assert_eq!(scientific[269], checksum8(&scientific[6..269]));
    }
}
