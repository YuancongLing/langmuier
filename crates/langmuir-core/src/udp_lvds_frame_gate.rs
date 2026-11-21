//! Bounded duplicate suppression shared by all structurally accepted types.

use std::collections::VecDeque;
use std::time::Instant;

use crate::{DEFAULT_UDP_LVDS_DEDUP_CAPACITY, DEFAULT_UDP_LVDS_DEDUP_TTL, UdpLvdsDisposition};

#[derive(Debug)]
struct AcceptedFrame {
    sequence: u32,
    bytes: Box<[u8]>,
    accepted_at: Instant,
}

/// One cache belongs to one source endpoint and receiver session. The caller
/// validates the common structure before observing it. Original full bytes,
/// including TYPE and CRC, determine duplicates and same-sequence conflicts.
#[derive(Debug, Default)]
pub struct UdpLvdsFrameGate {
    accepted: VecDeque<AcceptedFrame>,
}

impl UdpLvdsFrameGate {
    /// Observes a complete validated frame. Duplicates and conflicts preserve
    /// the first accepted bytes, timestamp, and FIFO age. Keys use all 32 bits.
    pub fn observe_at(&mut self, sequence: u32, bytes: &[u8], now: Instant) -> UdpLvdsDisposition {
        self.expire_at(now);
        if let Some(previous) = self
            .accepted
            .iter()
            .find(|entry| entry.sequence == sequence)
        {
            return if previous.bytes.as_ref() == bytes {
                UdpLvdsDisposition::Duplicate
            } else {
                UdpLvdsDisposition::SequenceConflict
            };
        }
        if self.accepted.len() == DEFAULT_UDP_LVDS_DEDUP_CAPACITY {
            self.accepted.pop_front();
        }
        self.accepted.push_back(AcceptedFrame {
            sequence,
            bytes: bytes.into(),
            accepted_at: now,
        });
        UdpLvdsDisposition::Accepted
    }

    /// Expires records five seconds after their first acceptance. Earlier
    /// injected clocks never expire a later record.
    pub fn expire_at(&mut self, now: Instant) {
        self.accepted.retain(|entry| {
            now.checked_duration_since(entry.accepted_at)
                .is_none_or(|age| age < DEFAULT_UDP_LVDS_DEDUP_TTL)
        });
    }
}
