use std::collections::BTreeMap;
use std::time::{Duration, Instant};

/// Bytes preceding every LVDS fragment carried in one UDP datagram.
pub const UDP_LVDS_HEADER_LEN: usize = 24;
/// Fixed raw-frame length declared by the deployed LVDS-to-UDP converter.
pub const DEPLOYED_LVDS_RAW_FRAME_LEN: usize = 269;
pub const UDP_LVDS_MAGIC: [u8; 2] = [0x55, 0xAA];
pub const UDP_LVDS_VERSION: u8 = 0x01;
pub const UDP_LVDS_TYPE: u8 = 0xA5;
/// Software resource-recovery policy, measured from a partial frame's first
/// fragment. At 20 Hz this allows about 40 frame periods of network jitter.
pub const DEFAULT_UDP_LVDS_REASSEMBLY_TTL: Duration = Duration::from_secs(2);

/// Parsed 24-byte header that wraps a fragment of one LVDS frame.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UdpLvdsHeader {
    pub frame_sequence: u32,
    pub frame_length: u32,
    pub segment_index: u16,
    pub segment_count: u16,
    pub byte_offset: u32,
    pub segment_length: u16,
    /// The sender has not yet supplied bit definitions for this field.
    pub flags: u16,
}

/// One validated UDP fragment. `payload` borrows the source datagram.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UdpLvdsDatagram<'a> {
    pub header: UdpLvdsHeader,
    pub payload: &'a [u8],
}

/// A fully reassembled raw LVDS byte sequence. Deployed frames are 269 bytes;
/// science-field decoding is intentionally a separate protocol stage.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReassembledLvdsFrame {
    pub frame_sequence: u32,
    pub bytes: Vec<u8>,
}

/// Reassembly-layer diagnostics, independent of inner LVDS science validation.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct UdpLvdsReassemblyMetrics {
    pub in_flight_frames: usize,
    /// Declared payload buffers currently reserved by partial frames. This
    /// excludes coverage/index metadata and is not a total heap estimate.
    pub buffered_payload_bytes: usize,
    pub expired_frames: u64,
    pub capacity_rejections: u64,
    pub completed_frames: u64,
    pub fragment_errors: u64,
}

/// Errors detected in the UDP wrapper before LVDS science-data decoding.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum UdpLvdsError {
    DatagramTooShort {
        actual: usize,
    },
    InvalidMagic {
        actual: [u8; 2],
    },
    UnsupportedVersion {
        actual: u8,
    },
    UnexpectedType {
        actual: u8,
    },
    ZeroSegmentCount,
    InvalidSegmentIndex {
        index: u16,
        count: u16,
    },
    SegmentLengthMismatch {
        declared: u16,
        actual: usize,
    },
    SegmentOutsideFrame {
        offset: u32,
        length: u16,
        frame_length: u32,
    },
    FrameTooLarge {
        actual: u32,
        maximum: usize,
    },
    TooManySegments {
        actual: u16,
        maximum: usize,
    },
    TooManyInFlightFrames {
        maximum: usize,
    },
    FrameMetadataConflict {
        frame_sequence: u32,
    },
    SegmentIndexConflict {
        frame_sequence: u32,
        segment_index: u16,
    },
    ConflictingSegmentData {
        frame_sequence: u32,
        offset: usize,
    },
    IncompleteCoverage {
        frame_sequence: u32,
    },
    InvalidConfiguration {
        field: &'static str,
    },
}

impl std::fmt::Display for UdpLvdsError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DatagramTooShort { actual } => write!(
                formatter,
                "UDP LVDS datagram must contain at least {UDP_LVDS_HEADER_LEN} bytes, got {actual}"
            ),
            Self::InvalidMagic { actual } => {
                write!(formatter, "UDP LVDS magic must be 55 AA, got {actual:02X?}")
            }
            Self::UnsupportedVersion { actual } => {
                write!(
                    formatter,
                    "UDP LVDS version must be 0x{UDP_LVDS_VERSION:02X}, got 0x{actual:02X}"
                )
            }
            Self::UnexpectedType { actual } => {
                write!(
                    formatter,
                    "UDP LVDS type must be 0x{UDP_LVDS_TYPE:02X}, got 0x{actual:02X}"
                )
            }
            Self::ZeroSegmentCount => {
                formatter.write_str("UDP LVDS segment count must be non-zero")
            }
            Self::InvalidSegmentIndex { index, count } => {
                write!(
                    formatter,
                    "UDP LVDS segment index {index} is outside 0..{count}"
                )
            }
            Self::SegmentLengthMismatch { declared, actual } => write!(
                formatter,
                "UDP LVDS segment length declares {declared} bytes, datagram carries {actual}"
            ),
            Self::SegmentOutsideFrame {
                offset,
                length,
                frame_length,
            } => write!(
                formatter,
                "UDP LVDS segment {offset}..{} is outside frame length {frame_length}",
                offset.saturating_add(u32::from(*length))
            ),
            Self::FrameTooLarge { actual, maximum } => write!(
                formatter,
                "UDP LVDS frame length {actual} exceeds configured maximum {maximum}"
            ),
            Self::TooManySegments { actual, maximum } => write!(
                formatter,
                "UDP LVDS segment count {actual} exceeds configured maximum {maximum}"
            ),
            Self::TooManyInFlightFrames { maximum } => {
                write!(formatter, "UDP LVDS exceeds {maximum} in-flight frames")
            }
            Self::FrameMetadataConflict { frame_sequence } => write!(
                formatter,
                "UDP LVDS frame {frame_sequence} changed its declared length or segment count"
            ),
            Self::SegmentIndexConflict {
                frame_sequence,
                segment_index,
            } => write!(
                formatter,
                "UDP LVDS frame {frame_sequence} reused segment index {segment_index} with another range"
            ),
            Self::ConflictingSegmentData {
                frame_sequence,
                offset,
            } => write!(
                formatter,
                "UDP LVDS frame {frame_sequence} has conflicting data at byte offset {offset}"
            ),
            Self::IncompleteCoverage { frame_sequence } => write!(
                formatter,
                "UDP LVDS frame {frame_sequence} received every segment index without covering its full byte range"
            ),
            Self::InvalidConfiguration { field } => {
                write!(
                    formatter,
                    "UDP LVDS reassembler configuration {field} must be non-zero"
                )
            }
        }
    }
}

impl std::error::Error for UdpLvdsError {}

/// Parses and validates one UDP wrapper. The caller must preserve UDP
/// datagram boundaries and pass exactly one datagram to this function.
///
/// # Errors
///
/// Returns an error for an invalid wrapper, declared length, or segment range.
pub fn parse_udp_lvds_datagram(datagram: &[u8]) -> Result<UdpLvdsDatagram<'_>, UdpLvdsError> {
    if datagram.len() < UDP_LVDS_HEADER_LEN {
        return Err(UdpLvdsError::DatagramTooShort {
            actual: datagram.len(),
        });
    }
    let magic = [datagram[0], datagram[1]];
    if magic != UDP_LVDS_MAGIC {
        return Err(UdpLvdsError::InvalidMagic { actual: magic });
    }
    if datagram[2] != UDP_LVDS_VERSION {
        return Err(UdpLvdsError::UnsupportedVersion {
            actual: datagram[2],
        });
    }
    if datagram[3] != UDP_LVDS_TYPE {
        return Err(UdpLvdsError::UnexpectedType {
            actual: datagram[3],
        });
    }

    let header = UdpLvdsHeader {
        frame_sequence: u32::from_be_bytes([datagram[4], datagram[5], datagram[6], datagram[7]]),
        frame_length: u32::from_be_bytes([datagram[8], datagram[9], datagram[10], datagram[11]]),
        segment_index: u16::from_be_bytes([datagram[12], datagram[13]]),
        segment_count: u16::from_be_bytes([datagram[14], datagram[15]]),
        byte_offset: u32::from_be_bytes([datagram[16], datagram[17], datagram[18], datagram[19]]),
        segment_length: u16::from_be_bytes([datagram[20], datagram[21]]),
        flags: u16::from_be_bytes([datagram[22], datagram[23]]),
    };
    if header.segment_count == 0 {
        return Err(UdpLvdsError::ZeroSegmentCount);
    }
    if header.segment_index >= header.segment_count {
        return Err(UdpLvdsError::InvalidSegmentIndex {
            index: header.segment_index,
            count: header.segment_count,
        });
    }

    let payload = &datagram[UDP_LVDS_HEADER_LEN..];
    if payload.len() != usize::from(header.segment_length) {
        return Err(UdpLvdsError::SegmentLengthMismatch {
            declared: header.segment_length,
            actual: payload.len(),
        });
    }
    let end = header
        .byte_offset
        .checked_add(u32::from(header.segment_length));
    if end.is_none_or(|end| end > header.frame_length) {
        return Err(UdpLvdsError::SegmentOutsideFrame {
            offset: header.byte_offset,
            length: header.segment_length,
            frame_length: header.frame_length,
        });
    }

    Ok(UdpLvdsDatagram { header, payload })
}

/// Bounded reassembler for UDP LVDS fragments from one trusted source endpoint.
#[derive(Debug)]
pub struct UdpLvdsReassembler {
    maximum_frame_bytes: usize,
    maximum_segments: usize,
    maximum_in_flight_frames: usize,
    ttl: Duration,
    frames: BTreeMap<u32, PendingFrame>,
    counters: UdpLvdsReassemblyMetrics,
}

impl Default for UdpLvdsReassembler {
    fn default() -> Self {
        Self::new(1_048_576, 1_024, 64).expect("fixed UDP LVDS defaults are valid")
    }
}

impl UdpLvdsReassembler {
    /// Creates a bounded reassembler. `maximum_frame_bytes` should be chosen
    /// from the transport contract, not directly from an untrusted datagram.
    ///
    /// # Errors
    ///
    /// Returns an error if any bound is zero.
    pub fn new(
        maximum_frame_bytes: usize,
        maximum_segments: usize,
        maximum_in_flight_frames: usize,
    ) -> Result<Self, UdpLvdsError> {
        Self::new_with_ttl(
            maximum_frame_bytes,
            maximum_segments,
            maximum_in_flight_frames,
            DEFAULT_UDP_LVDS_REASSEMBLY_TTL,
        )
    }

    /// Creates a bounded reassembler with a non-zero partial-frame lifetime.
    /// New or repeated fragments never extend a frame's initial deadline.
    ///
    /// # Errors
    ///
    /// Returns an error if any bound or the TTL is zero.
    pub fn new_with_ttl(
        maximum_frame_bytes: usize,
        maximum_segments: usize,
        maximum_in_flight_frames: usize,
        ttl: Duration,
    ) -> Result<Self, UdpLvdsError> {
        if maximum_frame_bytes == 0 {
            return Err(UdpLvdsError::InvalidConfiguration {
                field: "maximum_frame_bytes",
            });
        }
        if maximum_segments == 0 {
            return Err(UdpLvdsError::InvalidConfiguration {
                field: "maximum_segments",
            });
        }
        if maximum_in_flight_frames == 0 {
            return Err(UdpLvdsError::InvalidConfiguration {
                field: "maximum_in_flight_frames",
            });
        }
        if ttl.is_zero() {
            return Err(UdpLvdsError::InvalidConfiguration { field: "ttl" });
        }
        Ok(Self {
            maximum_frame_bytes,
            maximum_segments,
            maximum_in_flight_frames,
            ttl,
            frames: BTreeMap::new(),
            counters: UdpLvdsReassemblyMetrics::default(),
        })
    }

    /// Adds one UDP datagram and returns a frame only after every segment has
    /// arrived and its byte ranges cover the declared frame completely.
    ///
    /// # Errors
    ///
    /// Returns an error for malformed, conflicting, or out-of-bound segments.
    ///
    /// # Panics
    ///
    /// Panics only if an internal reassembly bookkeeping invariant is violated.
    pub fn push(&mut self, datagram: &[u8]) -> Result<Option<ReassembledLvdsFrame>, UdpLvdsError> {
        self.push_at(datagram, Instant::now())
    }

    /// Adds one datagram at an injected monotonic time. Expired partial frames
    /// are removed before parsing or checking capacity, even for bad input.
    ///
    /// # Errors
    ///
    /// Returns an error for malformed, conflicting, or out-of-bound segments.
    ///
    /// # Panics
    ///
    /// Panics only if an internal reassembly bookkeeping invariant is violated.
    pub fn push_at(
        &mut self,
        datagram: &[u8],
        now: Instant,
    ) -> Result<Option<ReassembledLvdsFrame>, UdpLvdsError> {
        self.expire_at(now);
        let result = self.push_inner(datagram, now);
        match &result {
            Ok(Some(_)) => {
                self.counters.completed_frames = self.counters.completed_frames.saturating_add(1);
            }
            Err(UdpLvdsError::TooManyInFlightFrames { .. }) => {
                self.counters.capacity_rejections =
                    self.counters.capacity_rejections.saturating_add(1);
            }
            Err(_) => {
                self.counters.fragment_errors = self.counters.fragment_errors.saturating_add(1);
            }
            Ok(None) => {}
        }
        result
    }

    fn push_inner(
        &mut self,
        datagram: &[u8],
        now: Instant,
    ) -> Result<Option<ReassembledLvdsFrame>, UdpLvdsError> {
        let datagram = parse_udp_lvds_datagram(datagram)?;
        let header = datagram.header;
        let frame_length =
            usize::try_from(header.frame_length).map_err(|_| UdpLvdsError::FrameTooLarge {
                actual: header.frame_length,
                maximum: self.maximum_frame_bytes,
            })?;
        if frame_length > self.maximum_frame_bytes {
            return Err(UdpLvdsError::FrameTooLarge {
                actual: header.frame_length,
                maximum: self.maximum_frame_bytes,
            });
        }
        if usize::from(header.segment_count) > self.maximum_segments {
            return Err(UdpLvdsError::TooManySegments {
                actual: header.segment_count,
                maximum: self.maximum_segments,
            });
        }

        if let Some(frame) = self.frames.get(&header.frame_sequence) {
            if frame.frame_length != frame_length || frame.segment_count != header.segment_count {
                self.frames.remove(&header.frame_sequence);
                return Err(UdpLvdsError::FrameMetadataConflict {
                    frame_sequence: header.frame_sequence,
                });
            }
        } else {
            if self.frames.len() >= self.maximum_in_flight_frames {
                return Err(UdpLvdsError::TooManyInFlightFrames {
                    maximum: self.maximum_in_flight_frames,
                });
            }
            self.frames.insert(
                header.frame_sequence,
                PendingFrame::new(frame_length, header.segment_count, now),
            );
        }

        let result = self
            .frames
            .get_mut(&header.frame_sequence)
            .expect("frame was inserted or already present")
            .push(header, datagram.payload);
        let complete = match result {
            Ok(complete) => complete,
            Err(error) => {
                self.frames.remove(&header.frame_sequence);
                return Err(error);
            }
        };
        if !complete {
            return Ok(None);
        }

        let frame = self
            .frames
            .remove(&header.frame_sequence)
            .expect("completed frame must remain in the reassembler");
        Ok(Some(ReassembledLvdsFrame {
            frame_sequence: header.frame_sequence,
            bytes: frame.bytes,
        }))
    }

    /// Removes partial frames whose age is greater than or equal to the TTL.
    /// The first-fragment timestamp is never renewed. Earlier injected times
    /// are treated as zero age, without subtraction or deadline overflow.
    pub fn expire_at(&mut self, now: Instant) -> usize {
        let before = self.frames.len();
        let ttl = self.ttl;
        self.frames.retain(|_, frame| {
            now.checked_duration_since(frame.created_at)
                .is_none_or(|age| age < ttl)
        });
        let expired = before - self.frames.len();
        self.counters.expired_frames = self
            .counters
            .expired_frames
            .saturating_add(u64::try_from(expired).unwrap_or(u64::MAX));
        expired
    }

    /// Removes expired partial frames using the current monotonic time.
    pub fn expire(&mut self) -> usize {
        self.expire_at(Instant::now())
    }

    #[must_use]
    pub fn metrics(&self) -> UdpLvdsReassemblyMetrics {
        UdpLvdsReassemblyMetrics {
            in_flight_frames: self.frames.len(),
            buffered_payload_bytes: self.frames.values().fold(0_usize, |total, frame| {
                total.saturating_add(frame.bytes.len())
            }),
            ..self.counters
        }
    }

    /// Explicitly drops one incomplete frame, without counting a TTL expiry.
    pub fn discard(&mut self, frame_sequence: u32) -> bool {
        self.frames.remove(&frame_sequence).is_some()
    }

    #[must_use]
    pub fn in_flight_frames(&self) -> usize {
        self.frames.len()
    }
}

#[derive(Debug)]
struct PendingFrame {
    created_at: Instant,
    frame_length: usize,
    segment_count: u16,
    bytes: Vec<u8>,
    covered: Vec<bool>,
    segments: BTreeMap<u16, SegmentRange>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct SegmentRange {
    offset: usize,
    length: usize,
}

impl PendingFrame {
    fn new(frame_length: usize, segment_count: u16, created_at: Instant) -> Self {
        Self {
            created_at,
            frame_length,
            segment_count,
            bytes: vec![0_u8; frame_length],
            covered: vec![false; frame_length],
            segments: BTreeMap::new(),
        }
    }

    fn push(&mut self, header: UdpLvdsHeader, payload: &[u8]) -> Result<bool, UdpLvdsError> {
        let offset =
            usize::try_from(header.byte_offset).map_err(|_| UdpLvdsError::SegmentOutsideFrame {
                offset: header.byte_offset,
                length: header.segment_length,
                frame_length: u32::try_from(self.frame_length).unwrap_or(u32::MAX),
            })?;
        let length = payload.len();
        let range = SegmentRange { offset, length };
        if let Some(existing) = self.segments.get(&header.segment_index)
            && *existing != range
        {
            return Err(UdpLvdsError::SegmentIndexConflict {
                frame_sequence: header.frame_sequence,
                segment_index: header.segment_index,
            });
        }

        for (index, byte) in payload.iter().copied().enumerate() {
            let frame_index = offset + index;
            if self.covered[frame_index] && self.bytes[frame_index] != byte {
                return Err(UdpLvdsError::ConflictingSegmentData {
                    frame_sequence: header.frame_sequence,
                    offset: frame_index,
                });
            }
        }
        for (index, byte) in payload.iter().copied().enumerate() {
            let frame_index = offset + index;
            self.bytes[frame_index] = byte;
            self.covered[frame_index] = true;
        }
        self.segments.insert(header.segment_index, range);

        if self.segments.len() != usize::from(self.segment_count) {
            return Ok(false);
        }
        if self.covered.iter().all(|covered| *covered) {
            return Ok(true);
        }
        Err(UdpLvdsError::IncompleteCoverage {
            frame_sequence: header.frame_sequence,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use crate::decode_scientific_frame;
    use crate::lvds::test_support::golden_frame;

    use super::{
        DEFAULT_UDP_LVDS_REASSEMBLY_TTL, DEPLOYED_LVDS_RAW_FRAME_LEN, UDP_LVDS_HEADER_LEN,
        UdpLvdsError, UdpLvdsReassembler, parse_udp_lvds_datagram,
    };

    fn datagram(
        frame_sequence: u32,
        frame_length: u32,
        segment_index: u16,
        segment_count: u16,
        byte_offset: u32,
        payload: &[u8],
    ) -> Vec<u8> {
        let mut datagram = Vec::with_capacity(UDP_LVDS_HEADER_LEN + payload.len());
        datagram.extend_from_slice(&[0x55, 0xAA, 0x01, 0xA5]);
        datagram.extend_from_slice(&frame_sequence.to_be_bytes());
        datagram.extend_from_slice(&frame_length.to_be_bytes());
        datagram.extend_from_slice(&segment_index.to_be_bytes());
        datagram.extend_from_slice(&segment_count.to_be_bytes());
        datagram.extend_from_slice(&byte_offset.to_be_bytes());
        datagram.extend_from_slice(&u16::try_from(payload.len()).unwrap().to_be_bytes());
        datagram.extend_from_slice(&0_u16.to_be_bytes());
        datagram.extend_from_slice(payload);
        datagram
    }

    fn golden_datagram(sequence: u32) -> Vec<u8> {
        datagram(sequence, 269, 0, 1, 0, &golden_frame())
    }

    fn golden_fragments(sequence: u32) -> [Vec<u8>; 3] {
        let frame = golden_frame();
        [
            datagram(sequence, 269, 0, 3, 0, &frame[..89]),
            datagram(sequence, 269, 1, 3, 89, &frame[89..179]),
            datagram(sequence, 269, 2, 3, 179, &frame[179..]),
        ]
    }

    fn bounded_reassembler() -> UdpLvdsReassembler {
        UdpLvdsReassembler::new(269, 16, 64).unwrap()
    }

    #[test]
    fn ttl_t1_single_fragment_preserves_bytes_without_pending_state() {
        let now = Instant::now();
        let mut reassembler = bounded_reassembler();
        let complete = reassembler
            .push_at(&golden_datagram(1), now)
            .unwrap()
            .unwrap();
        assert_eq!(complete.bytes, golden_frame());
        assert_eq!(complete.frame_sequence, 1);
        assert_eq!(
            reassembler.expire_at(now + DEFAULT_UDP_LVDS_REASSEMBLY_TTL),
            0
        );
        let metrics = reassembler.metrics();
        assert_eq!(metrics.completed_frames, 1);
        assert_eq!(metrics.expired_frames, 0);
        assert_eq!(metrics.in_flight_frames, 0);
        assert_eq!(metrics.buffered_payload_bytes, 0);
    }

    #[test]
    fn ttl_t2_multiple_fragments_reassemble_in_order() {
        let now = Instant::now();
        let mut reassembler = bounded_reassembler();
        let fragments = golden_fragments(2);
        assert_eq!(reassembler.push_at(&fragments[0], now).unwrap(), None);
        assert_eq!(reassembler.push_at(&fragments[1], now).unwrap(), None);
        let complete = reassembler.push_at(&fragments[2], now).unwrap().unwrap();
        assert_eq!(complete.bytes, golden_frame());
        assert_eq!(reassembler.metrics().completed_frames, 1);
        assert_eq!(reassembler.metrics().buffered_payload_bytes, 0);
    }

    #[test]
    fn ttl_t3_interleaved_sequences_reassemble_reversed_fragments() {
        let now = Instant::now();
        let mut reassembler = bounded_reassembler();
        let first = golden_fragments(3);
        let second = golden_fragments(4);
        for index in [2, 1] {
            assert_eq!(reassembler.push_at(&first[index], now).unwrap(), None);
            assert_eq!(reassembler.push_at(&second[index], now).unwrap(), None);
        }
        assert_eq!(reassembler.metrics().in_flight_frames, 2);
        assert_eq!(reassembler.metrics().buffered_payload_bytes, 538);
        for fragment in [&second[0], &first[0]] {
            let complete = reassembler.push_at(fragment, now).unwrap().unwrap();
            assert_eq!(complete.bytes, golden_frame());
        }
        assert_eq!(reassembler.metrics().completed_frames, 2);
        assert_eq!(reassembler.metrics().expired_frames, 0);
    }

    #[test]
    fn ttl_t4_incomplete_frame_expires_once_and_releases_its_buffers() {
        let now = Instant::now();
        let mut reassembler = bounded_reassembler();
        reassembler.push_at(&golden_fragments(5)[0], now).unwrap();
        assert_eq!(reassembler.metrics().buffered_payload_bytes, 269);
        let later = now + DEFAULT_UDP_LVDS_REASSEMBLY_TTL + Duration::from_millis(1);
        assert_eq!(reassembler.expire_at(later), 1);
        assert_eq!(reassembler.expire_at(later), 0);
        let metrics = reassembler.metrics();
        assert_eq!(metrics.expired_frames, 1);
        assert_eq!(metrics.in_flight_frames, 0);
        assert_eq!(metrics.buffered_payload_bytes, 0);
        assert_eq!(metrics.fragment_errors, 0);
    }

    #[test]
    fn ttl_t5_boundary_uses_first_fragment_and_handles_earlier_times() {
        let now = Instant::now();
        let ttl = DEFAULT_UDP_LVDS_REASSEMBLY_TTL;
        for elapsed in [
            ttl.checked_sub(Duration::from_nanos(1)).unwrap(),
            ttl,
            ttl + Duration::from_nanos(1),
        ] {
            let mut reassembler = bounded_reassembler();
            let fragments = golden_fragments(6);
            reassembler.push_at(&fragments[0], now).unwrap();
            let earlier = now.checked_sub(Duration::from_secs(1)).unwrap();
            assert_eq!(reassembler.expire_at(earlier), 0);
            // Neither a duplicate nor a new valid fragment renews creation time.
            let almost_due = (now + ttl).checked_sub(Duration::from_millis(1)).unwrap();
            reassembler.push_at(&fragments[0], almost_due).unwrap();
            reassembler.push_at(&fragments[1], almost_due).unwrap();
            let expected = usize::from(elapsed >= ttl);
            assert_eq!(reassembler.expire_at(now + elapsed), expected);
            assert_eq!(
                reassembler.metrics().expired_frames,
                u64::try_from(expected).unwrap()
            );
        }
        assert!(matches!(
            UdpLvdsReassembler::new_with_ttl(269, 16, 64, Duration::ZERO),
            Err(UdpLvdsError::InvalidConfiguration { field: "ttl" })
        ));
    }

    #[test]
    fn ttl_t6_full_capacity_rejects_new_sequence_without_evicting_live_frames() {
        let now = Instant::now();
        let mut reassembler = bounded_reassembler();
        for sequence in 0..64 {
            reassembler
                .push_at(&golden_fragments(sequence)[0], now)
                .unwrap();
        }
        assert_eq!(reassembler.metrics().in_flight_frames, 64);
        assert_eq!(reassembler.metrics().buffered_payload_bytes, 64 * 269);
        assert_eq!(
            reassembler.push_at(&golden_datagram(64), now),
            Err(UdpLvdsError::TooManyInFlightFrames { maximum: 64 })
        );
        assert_eq!(reassembler.metrics().in_flight_frames, 64);
        assert_eq!(reassembler.metrics().capacity_rejections, 1);
        assert_eq!(reassembler.metrics().fragment_errors, 0);
        let existing = golden_fragments(0);
        reassembler.push_at(&existing[1], now).unwrap();
        assert_eq!(
            reassembler
                .push_at(&existing[2], now)
                .unwrap()
                .unwrap()
                .bytes,
            golden_frame()
        );
        assert_eq!(reassembler.metrics().in_flight_frames, 63);
        assert_eq!(reassembler.metrics().expired_frames, 0);
    }

    #[test]
    fn ttl_t7_full_capacity_automatically_recovers_on_next_datagram() {
        let now = Instant::now();
        let mut reassembler = bounded_reassembler();
        for sequence in 0..64 {
            reassembler
                .push_at(&golden_fragments(sequence)[0], now)
                .unwrap();
        }
        let complete = reassembler
            .push_at(&golden_datagram(65), now + DEFAULT_UDP_LVDS_REASSEMBLY_TTL)
            .unwrap()
            .unwrap();
        assert_eq!(complete.frame_sequence, 65);
        assert_eq!(complete.bytes, golden_frame());
        let metrics = reassembler.metrics();
        assert_eq!(metrics.expired_frames, 64);
        assert_eq!(metrics.completed_frames, 1);
        assert_eq!(metrics.capacity_rejections, 0);
        assert_eq!(metrics.in_flight_frames, 0);
        assert_eq!(metrics.buffered_payload_bytes, 0);
    }

    #[test]
    fn ttl_t8_late_fragment_cannot_reuse_expired_bytes_or_pollute_other_sequences() {
        let now = Instant::now();
        let mut reassembler = bounded_reassembler();
        let old = golden_fragments(7);
        reassembler.push_at(&old[0], now).unwrap();
        reassembler.push_at(&old[1], now).unwrap();
        let later = now + DEFAULT_UDP_LVDS_REASSEMBLY_TTL;
        assert_eq!(reassembler.push_at(&old[2], later).unwrap(), None);
        let complete = reassembler
            .push_at(&golden_datagram(8), later)
            .unwrap()
            .unwrap();
        assert_eq!(complete.frame_sequence, 8);
        assert_eq!(complete.bytes, golden_frame());
        assert_eq!(reassembler.metrics().expired_frames, 1);
        assert_eq!(reassembler.metrics().completed_frames, 1);
        assert_eq!(reassembler.metrics().in_flight_frames, 1);
    }

    #[test]
    fn ttl_t9_twenty_hertz_reception_is_lossless_and_has_no_residual_growth() {
        let now = Instant::now();
        let mut reassembler = bounded_reassembler();
        for sequence in 0..400 {
            let arrival = now + Duration::from_millis(u64::from(sequence) * 50);
            let complete = reassembler
                .push_at(&golden_datagram(sequence), arrival)
                .unwrap()
                .unwrap();
            assert_eq!(complete.frame_sequence, sequence);
            assert_eq!(complete.bytes, golden_frame());
            let science = decode_scientific_frame(&complete.bytes).unwrap();
            assert_eq!(science.channel_1.len(), 125);
            assert_eq!(science.channel_2.len(), 125);
            assert_eq!(reassembler.metrics().in_flight_frames, 0);
            assert_eq!(reassembler.metrics().buffered_payload_bytes, 0);
        }
        assert_eq!(reassembler.metrics().completed_frames, 400);
        assert_eq!(reassembler.metrics().expired_frames, 0);
        assert_eq!(reassembler.metrics().capacity_rejections, 0);
    }

    #[test]
    fn ttl_t10_duplicates_are_idempotent_and_conflicts_release_the_frame() {
        let now = Instant::now();
        let mut reassembler = bounded_reassembler();
        let first = golden_fragments(9);
        reassembler.push_at(&first[0], now).unwrap();
        reassembler.push_at(&first[0], now).unwrap();
        assert_eq!(reassembler.metrics().in_flight_frames, 1);
        let mut conflict = first[0].clone();
        conflict[UDP_LVDS_HEADER_LEN] ^= 1;
        assert!(matches!(
            reassembler.push_at(&conflict, now),
            Err(UdpLvdsError::ConflictingSegmentData { .. })
        ));
        assert_eq!(reassembler.metrics().in_flight_frames, 0);
        assert_eq!(reassembler.metrics().fragment_errors, 1);
        assert_eq!(reassembler.metrics().expired_frames, 0);
    }

    #[test]
    fn ttl_t11_unchecked_science_trailer_survives_reassembly_and_decoding() {
        let now = Instant::now();
        let mut reassembler = bounded_reassembler();
        let mut bad = golden_frame();
        bad[268] ^= 1;
        let complete = reassembler
            .push_at(&datagram(10, 269, 0, 1, 0, &bad), now)
            .unwrap()
            .unwrap();
        assert_eq!(complete.bytes, bad);
        assert_eq!(
            decode_scientific_frame(&complete.bytes),
            decode_scientific_frame(&golden_frame())
        );
        let metrics = reassembler.metrics();
        assert_eq!(metrics.completed_frames, 1);
        assert_eq!(metrics.expired_frames, 0);
        assert_eq!(metrics.fragment_errors, 0);
    }

    #[test]
    fn ttl_t12_repeated_capacity_pressure_keeps_resources_bounded_and_recovers() {
        let start = Instant::now();
        let mut reassembler = bounded_reassembler();
        for cycle in 0..128 {
            let now = start + DEFAULT_UDP_LVDS_REASSEMBLY_TTL * cycle;
            for sequence in 0..64 {
                reassembler
                    .push_at(&golden_fragments(sequence)[0], now)
                    .unwrap();
                let metrics = reassembler.metrics();
                assert!(metrics.in_flight_frames <= 64);
                assert!(metrics.buffered_payload_bytes <= 64 * 269);
            }
            assert!(matches!(
                reassembler.push_at(&golden_datagram(64), now),
                Err(UdpLvdsError::TooManyInFlightFrames { .. })
            ));
            let complete = reassembler
                .push_at(&golden_datagram(65), now + DEFAULT_UDP_LVDS_REASSEMBLY_TTL)
                .unwrap()
                .unwrap();
            assert_eq!(complete.bytes, golden_frame());
            assert_eq!(reassembler.metrics().in_flight_frames, 0);
            assert_eq!(reassembler.metrics().buffered_payload_bytes, 0);
        }
        let metrics = reassembler.metrics();
        assert_eq!(metrics.expired_frames, 128 * 64);
        assert_eq!(metrics.completed_frames, 128);
        assert_eq!(metrics.capacity_rejections, 128);
        assert_eq!(metrics.fragment_errors, 0);
    }

    #[test]
    fn ttl_cleanup_precedes_bad_datagram_parsing_and_counters_saturate() {
        let now = Instant::now();
        let mut reassembler = bounded_reassembler();
        reassembler.push_at(&golden_fragments(1)[0], now).unwrap();
        reassembler.counters.expired_frames = u64::MAX;
        reassembler.counters.fragment_errors = u64::MAX;
        let later = now + DEFAULT_UDP_LVDS_REASSEMBLY_TTL;
        assert!(reassembler.push_at(&[], later).is_err());
        assert_eq!(reassembler.metrics().expired_frames, u64::MAX);
        assert_eq!(reassembler.metrics().fragment_errors, u64::MAX);
        assert_eq!(reassembler.metrics().in_flight_frames, 0);
        reassembler.counters.completed_frames = u64::MAX;
        reassembler.push_at(&golden_datagram(2), later).unwrap();
        assert_eq!(reassembler.metrics().completed_frames, u64::MAX);
        for sequence in 0..64 {
            reassembler
                .push_at(&golden_fragments(sequence)[0], later)
                .unwrap();
        }
        reassembler.counters.capacity_rejections = u64::MAX;
        assert!(reassembler.push_at(&golden_datagram(65), later).is_err());
        assert_eq!(reassembler.metrics().capacity_rejections, u64::MAX);
    }

    #[test]
    fn parses_the_observed_single_segment_wrapper() {
        let payload = vec![0x5A; DEPLOYED_LVDS_RAW_FRAME_LEN];
        let deployed_frame_length =
            u32::try_from(DEPLOYED_LVDS_RAW_FRAME_LEN).expect("fixed deployed length fits in u32");
        let datagram = datagram(0xB537, deployed_frame_length, 0, 1, 0, &payload);
        let parsed = parse_udp_lvds_datagram(&datagram).unwrap();
        assert_eq!(parsed.header.frame_sequence, 0xB537);
        assert_eq!(parsed.header.frame_length, deployed_frame_length);
        assert_eq!(parsed.header.segment_count, 1);
        assert_eq!(
            usize::from(parsed.header.segment_length),
            DEPLOYED_LVDS_RAW_FRAME_LEN
        );
        assert_eq!(parsed.payload, payload);
    }

    #[test]
    fn reassembles_out_of_order_fragments_by_offset() {
        let mut reassembler = UdpLvdsReassembler::default();
        let second = datagram(42, 9, 1, 3, 3, &[4, 5, 6]);
        let third = datagram(42, 9, 2, 3, 6, &[7, 8, 9]);
        let first = datagram(42, 9, 0, 3, 0, &[1, 2, 3]);

        assert_eq!(reassembler.push(&second).unwrap(), None);
        assert_eq!(reassembler.push(&third).unwrap(), None);
        let complete = reassembler.push(&first).unwrap().unwrap();
        assert_eq!(complete.frame_sequence, 42);
        assert_eq!(complete.bytes, [1, 2, 3, 4, 5, 6, 7, 8, 9]);
        assert_eq!(reassembler.in_flight_frames(), 0);
    }

    #[test]
    fn accepts_identical_duplicate_before_completion() {
        let mut reassembler = UdpLvdsReassembler::default();
        let first = datagram(7, 4, 0, 2, 0, &[1, 2]);
        let second = datagram(7, 4, 1, 2, 2, &[3, 4]);

        assert_eq!(reassembler.push(&first).unwrap(), None);
        assert_eq!(reassembler.push(&first).unwrap(), None);
        assert_eq!(
            reassembler.push(&second).unwrap().unwrap().bytes,
            [1, 2, 3, 4]
        );
    }

    #[test]
    fn rejects_conflicting_duplicate_and_drops_partial_frame() {
        let mut reassembler = UdpLvdsReassembler::default();
        let first = datagram(8, 4, 0, 2, 0, &[1, 2]);
        let conflict = datagram(8, 4, 0, 2, 0, &[1, 9]);

        assert_eq!(reassembler.push(&first).unwrap(), None);
        assert!(matches!(
            reassembler.push(&conflict),
            Err(UdpLvdsError::ConflictingSegmentData {
                frame_sequence: 8,
                offset: 1
            })
        ));
        assert_eq!(reassembler.in_flight_frames(), 0);
    }

    #[test]
    fn rejects_incomplete_ranges_when_every_segment_index_arrived() {
        let mut reassembler = UdpLvdsReassembler::default();
        let first = datagram(9, 5, 0, 2, 0, &[1, 2]);
        let second = datagram(9, 5, 1, 2, 3, &[4, 5]);

        assert_eq!(reassembler.push(&first).unwrap(), None);
        assert!(matches!(
            reassembler.push(&second),
            Err(UdpLvdsError::IncompleteCoverage { frame_sequence: 9 })
        ));
    }
}
