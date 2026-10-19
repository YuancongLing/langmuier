use std::collections::VecDeque;
use std::time::{Duration, Instant};

use crate::{
    FrameError, LVDS_FRAME_LEN, ReassembledLvdsFrame, ScientificFrame, UdpLvdsError,
    decode_scientific_frame_with_sequence,
};

/// Recent accepted-frame lifetime, measured from first acceptance. This is a
/// software duplicate-suppression policy, not a manufacturer sequence guarantee.
pub const DEFAULT_UDP_LVDS_DEDUP_TTL: Duration = Duration::from_secs(5);
/// Maximum accepted frames remembered by one source and receiver session.
pub const DEFAULT_UDP_LVDS_DEDUP_CAPACITY: usize = 256;

/// Result of validating and checking one complete UDP scientific frame.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UdpLvdsDisposition {
    Accepted,
    Duplicate,
    SequenceConflict,
}

/// Only an accepted result contains a frame that may enter normal processing.
#[derive(Debug, Eq, PartialEq)]
pub struct UdpLvdsScienceResult {
    disposition: UdpLvdsDisposition,
    frame: Option<ScientificFrame>,
}

impl UdpLvdsScienceResult {
    #[must_use]
    pub fn disposition(&self) -> UdpLvdsDisposition {
        self.disposition
    }

    #[must_use]
    pub fn into_frame(self) -> Option<ScientificFrame> {
        self.frame
    }
}

/// Counters cover complete science validation and the subsequent delivery gate.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct UdpLvdsDedupMetrics {
    /// Complete structure, CRC16, time, and ADC validation successes, including
    /// valid duplicates and conflicts. Invalid candidates do not increment it.
    pub validated_frames: u64,
    pub duplicate_frames: u64,
    pub sequence_conflicts: u64,
    /// Frames returned for normal processing, before caller-specific handling.
    pub accepted_frames: u64,
    pub cached_frames: usize,
    /// Stored complete wire bytes, excluding collection and entry metadata.
    pub cached_payload_bytes: usize,
}

#[derive(Debug)]
struct AcceptedFrame {
    sequence: u32,
    bytes: Box<[u8]>,
    accepted_at: Instant,
}

/// Validates scientific frames before suppressing recent repeated deliveries.
///
/// Each object belongs to exactly one filtered source endpoint and receiver
/// session. Create a fresh object after reconnecting or explicitly starting a
/// new session. The manufacturer has not defined reset semantics: sequence
/// decreases never clear this cache automatically, since they may be late UDP
/// traffic. Full `u32` keys are independent of the legacy scientific model's
/// `u16` sequence. Sequence wrap neither sorts nor resets accepted records.
///
/// Matching uses every original wire byte. A valid same-sequence conflict is
/// suppressed separately and leaves the first accepted record unchanged. Both
/// duplicates and conflicts retain the original acceptance time and FIFO age.
/// Expiry and bounded FIFO eviction permit eventual sequence reuse; duplicate
/// protection applies only while that record remains cached.
#[derive(Debug)]
pub struct UdpLvdsScienceGate {
    capacity: usize,
    ttl: Duration,
    accepted: VecDeque<AcceptedFrame>,
    counters: UdpLvdsDedupMetrics,
}

impl Default for UdpLvdsScienceGate {
    fn default() -> Self {
        Self {
            capacity: DEFAULT_UDP_LVDS_DEDUP_CAPACITY,
            ttl: DEFAULT_UDP_LVDS_DEDUP_TTL,
            accepted: VecDeque::new(),
            counters: UdpLvdsDedupMetrics::default(),
        }
    }
}

impl UdpLvdsScienceGate {
    /// Configures a bounded duplicate cache for one source and receiver session.
    ///
    /// # Errors
    ///
    /// Returns [`UdpLvdsError::InvalidConfiguration`] for zero capacity or TTL.
    pub fn new(capacity: usize, ttl: Duration) -> Result<Self, UdpLvdsError> {
        if capacity == 0 {
            return Err(UdpLvdsError::InvalidConfiguration {
                field: "dedup_capacity",
            });
        }
        if ttl.is_zero() {
            return Err(UdpLvdsError::InvalidConfiguration { field: "dedup_ttl" });
        }
        Ok(Self {
            capacity,
            ttl,
            accepted: VecDeque::new(),
            counters: UdpLvdsDedupMetrics::default(),
        })
    }

    /// Validates a complete scientific frame before inspecting or registering
    /// accepted state. Rejections cannot poison a later valid retransmission.
    ///
    /// # Errors
    ///
    /// Returns the strict scientific decoder's structural, CRC, time, or ADC
    /// error. Such candidates never enter the cache or validation counters.
    pub fn accept_at(
        &mut self,
        complete: &ReassembledLvdsFrame,
        now: Instant,
    ) -> Result<UdpLvdsScienceResult, FrameError> {
        self.expire_at(now);
        // Preserve the existing model's low 16 bits only after retaining the
        // full outer sequence for all cache lookups and conflict decisions.
        let [_, _, sequence_high, sequence_low] = complete.frame_sequence.to_be_bytes();
        let frame = decode_scientific_frame_with_sequence(
            &complete.bytes,
            u16::from_be_bytes([sequence_high, sequence_low]),
        )?;
        self.counters.validated_frames = self.counters.validated_frames.saturating_add(1);

        if let Some(previous) = self
            .accepted
            .iter()
            .find(|previous| previous.sequence == complete.frame_sequence)
        {
            let disposition = if previous.bytes.as_ref() == complete.bytes.as_slice() {
                self.counters.duplicate_frames = self.counters.duplicate_frames.saturating_add(1);
                UdpLvdsDisposition::Duplicate
            } else {
                self.counters.sequence_conflicts =
                    self.counters.sequence_conflicts.saturating_add(1);
                UdpLvdsDisposition::SequenceConflict
            };
            return Ok(UdpLvdsScienceResult {
                disposition,
                frame: None,
            });
        }

        if self.accepted.len() == self.capacity {
            self.accepted.pop_front();
        }
        self.accepted.push_back(AcceptedFrame {
            sequence: complete.frame_sequence,
            bytes: complete.bytes.clone().into_boxed_slice(),
            accepted_at: now,
        });
        self.counters.accepted_frames = self.counters.accepted_frames.saturating_add(1);
        Ok(UdpLvdsScienceResult {
            disposition: UdpLvdsDisposition::Accepted,
            frame: Some(frame),
        })
    }

    /// Releases records whose first acceptance is at least TTL old. An earlier
    /// injected clock value cannot expire a later accepted record.
    pub fn expire_at(&mut self, now: Instant) -> usize {
        let before = self.accepted.len();
        self.accepted.retain(|accepted| {
            now.checked_duration_since(accepted.accepted_at)
                .is_none_or(|age| age < self.ttl)
        });
        before - self.accepted.len()
    }

    #[must_use]
    pub fn metrics(&self) -> UdpLvdsDedupMetrics {
        UdpLvdsDedupMetrics {
            cached_frames: self.accepted.len(),
            cached_payload_bytes: self.accepted.len().saturating_mul(LVDS_FRAME_LEN),
            ..self.counters
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        DEFAULT_UDP_LVDS_DEDUP_CAPACITY, DEFAULT_UDP_LVDS_DEDUP_TTL, UdpLvdsDisposition,
        UdpLvdsScienceGate,
    };
    use crate::{
        FrameError, LVDS_FRAME_LEN, ReassembledLvdsFrame, UdpLvdsError,
        lvds::test_support::{golden_frame, refresh_crc16},
    };
    use std::time::{Duration, Instant};

    fn complete(sequence: u32) -> ReassembledLvdsFrame {
        ReassembledLvdsFrame {
            frame_sequence: sequence,
            bytes: golden_frame().to_vec(),
        }
    }

    fn changed_time(sequence: u32) -> ReassembledLvdsFrame {
        let mut bytes = golden_frame();
        bytes[10] = 1;
        refresh_crc16(&mut bytes);
        ReassembledLvdsFrame {
            frame_sequence: sequence,
            bytes: bytes.to_vec(),
        }
    }

    fn assert_disposition(
        gate: &mut UdpLvdsScienceGate,
        frame: &ReassembledLvdsFrame,
        now: Instant,
        expected: UdpLvdsDisposition,
    ) {
        let result = gate.accept_at(frame, now).unwrap();
        assert_eq!(result.disposition(), expected);
        assert_eq!(
            result.into_frame().is_some(),
            expected == UdpLvdsDisposition::Accepted
        );
    }

    #[test]
    fn first_valid_frame_is_delivered_and_exact_repeats_are_suppressed() {
        let now = Instant::now();
        let frame = complete(5);
        let mut gate = UdpLvdsScienceGate::default();
        let result = gate.accept_at(&frame, now).unwrap();
        assert_eq!(result.disposition(), UdpLvdsDisposition::Accepted);
        let scientific = result.into_frame().unwrap();
        assert_eq!(scientific.sequence, 5);
        assert_eq!(scientific.channel_1[0], 4660);
        assert_eq!(scientific.channel_2[124], 43857);
        for offset in 1..=10 {
            assert_disposition(
                &mut gate,
                &frame,
                now + Duration::from_millis(offset),
                UdpLvdsDisposition::Duplicate,
            );
        }
        let metrics = gate.metrics();
        assert_eq!(metrics.validated_frames, 11);
        assert_eq!(metrics.accepted_frames, 1);
        assert_eq!(metrics.duplicate_frames, 10);
        assert_eq!(metrics.sequence_conflicts, 0);
        assert_eq!(metrics.cached_frames, 1);
        assert_eq!(metrics.cached_payload_bytes, LVDS_FRAME_LEN);
    }

    #[test]
    fn bad_crc_does_not_register_or_replace_accepted_state() {
        let now = Instant::now();
        let good = complete(21);
        let mut bad = good.clone();
        bad.bytes[268] ^= 1;
        let mut gate = UdpLvdsScienceGate::default();
        assert!(matches!(
            gate.accept_at(&bad, now),
            Err(FrameError::ChecksumMismatch { .. })
        ));
        assert_eq!(gate.metrics().cached_frames, 0);
        assert_eq!(gate.metrics().validated_frames, 0);
        assert_disposition(&mut gate, &good, now, UdpLvdsDisposition::Accepted);
        assert!(matches!(
            gate.accept_at(&bad, now),
            Err(FrameError::ChecksumMismatch { .. })
        ));
        assert_disposition(&mut gate, &good, now, UdpLvdsDisposition::Duplicate);
        assert_eq!(gate.metrics().sequence_conflicts, 0);
    }

    #[test]
    fn legal_sequence_conflict_keeps_the_original_record_without_refreshing_it() {
        let now = Instant::now();
        let first = complete(30);
        let conflict = changed_time(30);
        let mut gate = UdpLvdsScienceGate::default();
        assert_disposition(&mut gate, &first, now, UdpLvdsDisposition::Accepted);
        assert_disposition(
            &mut gate,
            &conflict,
            now + Duration::from_secs(4),
            UdpLvdsDisposition::SequenceConflict,
        );
        assert_disposition(
            &mut gate,
            &first,
            now + Duration::from_secs(4),
            UdpLvdsDisposition::Duplicate,
        );
        assert_disposition(
            &mut gate,
            &conflict,
            now + DEFAULT_UDP_LVDS_DEDUP_TTL,
            UdpLvdsDisposition::Accepted,
        );
        let metrics = gate.metrics();
        assert_eq!(metrics.accepted_frames, 2);
        assert_eq!(metrics.sequence_conflicts, 1);
        assert_eq!(metrics.duplicate_frames, 1);
        assert_eq!(metrics.cached_frames, 1);
    }

    #[test]
    fn equal_content_with_distinct_full_u32_sequences_is_not_a_duplicate() {
        let now = Instant::now();
        let mut gate = UdpLvdsScienceGate::default();
        for sequence in [1, 65_537, 2, 65_538] {
            let result = gate.accept_at(&complete(sequence), now).unwrap();
            assert_eq!(result.disposition(), UdpLvdsDisposition::Accepted);
            assert!(result.into_frame().is_some());
        }
        assert_eq!(gate.metrics().accepted_frames, 4);
        assert_eq!(gate.metrics().duplicate_frames, 0);
        assert_eq!(gate.metrics().sequence_conflicts, 0);
    }

    #[test]
    fn four_hundred_frames_at_fifty_millisecond_intervals_are_all_delivered() {
        let now = Instant::now();
        let mut gate = UdpLvdsScienceGate::default();
        for sequence in 0..400_u32 {
            let at = now + Duration::from_millis(u64::from(sequence) * 50);
            assert_disposition(
                &mut gate,
                &complete(sequence),
                at,
                UdpLvdsDisposition::Accepted,
            );
            assert!(gate.metrics().cached_frames <= DEFAULT_UDP_LVDS_DEDUP_CAPACITY);
        }
        let metrics = gate.metrics();
        assert_eq!(metrics.accepted_frames, 400);
        assert_eq!(metrics.validated_frames, 400);
        assert_eq!(metrics.duplicate_frames, 0);
        assert_eq!(metrics.sequence_conflicts, 0);
        assert_eq!(metrics.cached_frames, 100);
        assert_eq!(metrics.cached_payload_bytes, 100 * LVDS_FRAME_LEN);
    }

    #[test]
    fn capacity_evicts_oldest_acceptance_and_duplicates_do_not_refresh_fifo() {
        let now = Instant::now();
        let mut gate = UdpLvdsScienceGate::new(2, Duration::from_secs(60)).unwrap();
        assert_disposition(&mut gate, &complete(1), now, UdpLvdsDisposition::Accepted);
        assert_disposition(&mut gate, &complete(2), now, UdpLvdsDisposition::Accepted);
        assert_disposition(&mut gate, &complete(1), now, UdpLvdsDisposition::Duplicate);
        assert_disposition(&mut gate, &complete(3), now, UdpLvdsDisposition::Accepted);
        assert_eq!(gate.metrics().cached_frames, 2);
        assert_eq!(gate.metrics().cached_payload_bytes, 2 * LVDS_FRAME_LEN);
        assert_disposition(&mut gate, &complete(2), now, UdpLvdsDisposition::Duplicate);
        assert_disposition(&mut gate, &complete(1), now, UdpLvdsDisposition::Accepted);
        assert_eq!(gate.metrics().cached_frames, 2);
    }

    #[test]
    fn default_capacity_remains_bounded_under_repeated_eviction() {
        let now = Instant::now();
        let mut gate = UdpLvdsScienceGate::default();
        for sequence in 0..1024 {
            assert_disposition(
                &mut gate,
                &complete(sequence),
                now,
                UdpLvdsDisposition::Accepted,
            );
            assert!(gate.metrics().cached_frames <= DEFAULT_UDP_LVDS_DEDUP_CAPACITY);
            assert!(
                gate.metrics().cached_payload_bytes
                    <= DEFAULT_UDP_LVDS_DEDUP_CAPACITY * LVDS_FRAME_LEN
            );
        }
        assert_eq!(
            gate.metrics().cached_frames,
            DEFAULT_UDP_LVDS_DEDUP_CAPACITY
        );
        assert_eq!(gate.metrics().accepted_frames, 1024);
    }

    #[test]
    fn ttl_boundary_is_exact_and_duplicate_does_not_extend_lifetime() {
        let now = Instant::now();
        let frame = complete(1);
        let mut gate = UdpLvdsScienceGate::default();
        assert_disposition(&mut gate, &frame, now, UdpLvdsDisposition::Accepted);
        let before = (now + DEFAULT_UDP_LVDS_DEDUP_TTL)
            .checked_sub(Duration::from_nanos(1))
            .unwrap();
        assert_disposition(&mut gate, &frame, before, UdpLvdsDisposition::Duplicate);
        assert_eq!(gate.expire_at(before), 0);
        let boundary = now + DEFAULT_UDP_LVDS_DEDUP_TTL;
        assert_eq!(gate.expire_at(boundary), 1);
        assert_eq!(gate.expire_at(boundary), 0);
        assert_disposition(&mut gate, &frame, boundary, UdpLvdsDisposition::Accepted);
        assert_eq!(
            gate.expire_at(boundary + DEFAULT_UDP_LVDS_DEDUP_TTL + Duration::from_nanos(1)),
            1
        );
    }

    #[test]
    fn earlier_time_is_safe_and_expiry_does_not_assume_timestamp_order() {
        let now = Instant::now();
        let later = now + Duration::from_secs(10);
        let mut gate = UdpLvdsScienceGate::default();
        assert_disposition(&mut gate, &complete(1), later, UdpLvdsDisposition::Accepted);
        assert_disposition(&mut gate, &complete(2), now, UdpLvdsDisposition::Accepted);
        assert_eq!(gate.expire_at(now + DEFAULT_UDP_LVDS_DEDUP_TTL), 1);
        assert_eq!(gate.metrics().cached_frames, 1);
        assert_disposition(&mut gate, &complete(1), now, UdpLvdsDisposition::Duplicate);
        assert_eq!(gate.expire_at(later + DEFAULT_UDP_LVDS_DEDUP_TTL), 1);
    }

    #[test]
    fn fresh_sessions_sequence_reset_and_u32_wrap_allow_new_science() {
        let now = Instant::now();
        let mut original = UdpLvdsScienceGate::default();
        for sequence in [0, u32::MAX - 1, u32::MAX] {
            assert_disposition(
                &mut original,
                &complete(sequence),
                now,
                UdpLvdsDisposition::Accepted,
            );
        }
        assert_disposition(
            &mut original,
            &complete(0),
            now,
            UdpLvdsDisposition::Duplicate,
        );
        assert_disposition(
            &mut original,
            &changed_time(0),
            now,
            UdpLvdsDisposition::SequenceConflict,
        );
        assert_disposition(
            &mut original,
            &changed_time(0),
            now + DEFAULT_UDP_LVDS_DEDUP_TTL,
            UdpLvdsDisposition::Accepted,
        );
        let mut reconnected = UdpLvdsScienceGate::default();
        assert_disposition(
            &mut reconnected,
            &changed_time(0),
            now,
            UdpLvdsDisposition::Accepted,
        );
        let mut wrapping = UdpLvdsScienceGate::default();
        for sequence in [u32::MAX - 1, u32::MAX, 0, 1] {
            assert_disposition(
                &mut wrapping,
                &complete(sequence),
                now,
                UdpLvdsDisposition::Accepted,
            );
        }
        assert_eq!(wrapping.metrics().sequence_conflicts, 0);
    }

    #[test]
    fn invalid_structure_time_and_adc_never_poison_the_cache() {
        let now = Instant::now();
        let mut gate = UdpLvdsScienceGate::default();
        let mut invalid_time = golden_frame();
        invalid_time[11..15].copy_from_slice(&1_000_000_u32.to_be_bytes());
        refresh_crc16(&mut invalid_time);
        let mut invalid_adc = golden_frame();
        invalid_adc[15..17].copy_from_slice(&u16::MAX.to_be_bytes());
        refresh_crc16(&mut invalid_adc);
        let mut invalid_header = golden_frame();
        invalid_header[0] = 0;
        for (sequence, bytes) in [invalid_time, invalid_adc, invalid_header]
            .into_iter()
            .enumerate()
        {
            let sequence = u32::try_from(sequence).unwrap();
            let invalid = ReassembledLvdsFrame {
                frame_sequence: sequence,
                bytes: bytes.to_vec(),
            };
            assert!(gate.accept_at(&invalid, now).is_err());
            assert_disposition(
                &mut gate,
                &complete(sequence),
                now,
                UdpLvdsDisposition::Accepted,
            );
        }
        assert_eq!(gate.metrics().accepted_frames, 3);
        assert_eq!(gate.metrics().validated_frames, 3);
        assert_eq!(gate.metrics().sequence_conflicts, 0);
        assert_eq!(gate.metrics().duplicate_frames, 0);
    }

    #[test]
    fn configuration_requires_nonzero_capacity_and_ttl_and_counters_saturate() {
        assert!(matches!(
            UdpLvdsScienceGate::new(0, Duration::from_secs(1)),
            Err(UdpLvdsError::InvalidConfiguration { .. })
        ));
        assert!(matches!(
            UdpLvdsScienceGate::new(1, Duration::ZERO),
            Err(UdpLvdsError::InvalidConfiguration { .. })
        ));
        let now = Instant::now();
        let mut gate = UdpLvdsScienceGate::default();
        gate.counters.accepted_frames = u64::MAX;
        gate.counters.validated_frames = u64::MAX;
        gate.counters.duplicate_frames = u64::MAX;
        gate.counters.sequence_conflicts = u64::MAX;
        assert_disposition(&mut gate, &complete(1), now, UdpLvdsDisposition::Accepted);
        assert_disposition(&mut gate, &complete(1), now, UdpLvdsDisposition::Duplicate);
        assert_disposition(
            &mut gate,
            &changed_time(1),
            now,
            UdpLvdsDisposition::SequenceConflict,
        );
        let metrics = gate.metrics();
        assert_eq!(metrics.accepted_frames, u64::MAX);
        assert_eq!(metrics.validated_frames, u64::MAX);
        assert_eq!(metrics.duplicate_frames, u64::MAX);
        assert_eq!(metrics.sequence_conflicts, u64::MAX);
    }
}
