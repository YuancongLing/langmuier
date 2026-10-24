use crate::{FRAME_HEADER, FrameError, Protocol, crc16_ccitt_false};

#[cfg(test)]
mod migration_golden_tests {
    use super::decode_scientific_frame;

    #[test]
    fn manufacturer_10bit_literal_groups_decode_distinct_channel_boundaries() {
        let mut wire = [0_u8; 269];
        wire[..15].copy_from_slice(&[
            0xEB, 0x90, 0x55, 0xAA, 0x01, 0x07, 0xAA, 0x65, 0, 0, 0, 0, 0x0F, 0x41, 0x78,
        ]);
        for group in wire[15..140].as_chunks_mut::<5>().0 {
            group.copy_from_slice(&[0xFF, 0xC0, 0x08, 0, 1]);
        }
        for group in wire[140..265].as_chunks_mut::<5>().0 {
            group.copy_from_slice(&[0, 0x3F, 0xF0, 3, 0xFF]);
        }
        wire[265] = 255;
        wire[267..269].copy_from_slice(&[0x12, 0x34]);
        let decoded = decode_scientific_frame(&wire).unwrap();
        assert_eq!(decoded.channel_1.len(), 100);
        assert_eq!(decoded.channel_2.len(), 100);
        assert_eq!(&decoded.channel_1[..8], &[1023, 0, 512, 1, 1023, 0, 512, 1]);
        assert_eq!(
            &decoded.channel_2[..8],
            &[0, 1023, 0, 1023, 0, 1023, 0, 1023]
        );
        assert_eq!((decoded.channel_1[99], decoded.channel_2[99]), (1, 1023));
    }

    #[test]
    fn literal_unpack_vectors_and_signed_reconstruction_preserve_extremes() {
        assert_eq!(
            super::unpack_4x10(&[0xFF, 0xC0, 8, 0, 1]),
            [1023, 0, 512, 1]
        );
        assert_eq!(
            super::unpack_4x10(&[0, 0x3F, 0xF0, 3, 0xFF]),
            [0, 1023, 0, 1023]
        );
        assert_eq!(super::unpack_4x10(&[0; 5]), [0; 4]);
        assert_eq!(super::unpack_4x10(&[0xFF; 5]), [1023; 4]);
        assert_eq!(super::reconstruct_adc16(0), -32768);
        assert_eq!(super::reconstruct_adc16(512), 0);
        assert_eq!(super::reconstruct_adc16(1023), 32704);
    }
}

pub const LVDS_FRAME_LEN: usize = 269;
pub const LVDS_PAYLOAD_LEN: usize = 263;
pub const LVDS_DATA_TYPE: u8 = 0xAA;
pub const LVDS_SENDER_ADDRESS: u8 = 0x55;
pub const LVDS_RECEIVER_ADDRESS: u8 = 0xAA;
pub const LVDS_DEVICE_ID: u8 = LVDS_SENDER_ADDRESS;
pub const SAMPLE_COUNT: usize = 100;
pub const SAMPLE_INTERVAL_MICROS: u32 = 500;
/// Start of the CRC input: the UTC timestamp, excluding TYPE.
///
/// This is the current interpretation of the updated table's "valid data".
/// The manufacturer's CRC function establishes the algorithm; its actual
/// `data` pointer and `length` arguments still require confirmation.
pub const LVDS_CRC_CALC_START: usize = 7;
/// Exclusive end of the CRC input, immediately after scientific DATA.
pub const LVDS_CRC_CALC_END: usize = 267;
/// Offset of the two-byte, big-endian CRC field; separate from the input range.
pub const LVDS_CRC_FIELD_OFFSET: usize = 267;
pub const LVDS_CRC_INPUT_LEN: usize = LVDS_CRC_CALC_END - LVDS_CRC_CALC_START;

const LENGTH_OFFSET: usize = 4;
const PAYLOAD_OFFSET: usize = 6;
const TIMESTAMP_OFFSET: usize = 7;
const MICROSECONDS_OFFSET: usize = 11;
const SAMPLE_DATA_OFFSET: usize = 15;
const CHANNEL_PACKED_BYTES: usize = 125;
const FRAME_SEQUENCE_OFFSET: usize = 265;
const RESERVED_OFFSET: usize = 266;
const PAYLOAD_LENGTH_FIELD: u16 = 0x0107;

const _: () = assert!(1 + 4 + 4 + 252 + 2 == LVDS_PAYLOAD_LEN);
const _: () = assert!(CHANNEL_PACKED_BYTES / 5 * 4 == SAMPLE_COUNT);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timestamp {
    pub utc_seconds: u32,
    pub microseconds: u32,
}

impl Timestamp {
    #[must_use]
    pub fn as_seconds_f64(self) -> f64 {
        f64::from(self.utc_seconds) + f64::from(self.microseconds) / 1_000_000.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScientificFrame {
    pub timestamp: Timestamp,
    pub frame_seq_u8: u8,
    /// Exact received 10bit codes; no physical-voltage calibration is implied.
    pub channel_1: Box<[u16; SAMPLE_COUNT]>,
    pub channel_2: Box<[u16; SAMPLE_COUNT]>,
}

impl ScientificFrame {
    #[must_use]
    pub fn channel_1_base_seconds(&self) -> f64 {
        self.timestamp.as_seconds_f64()
    }

    #[must_use]
    pub fn channel_2_base_seconds(&self) -> f64 {
        self.timestamp.as_seconds_f64()
    }

    #[must_use]
    pub fn channel_1_sample_time(&self, index: usize) -> Option<f64> {
        if index >= SAMPLE_COUNT {
            return None;
        }
        let index = u32::try_from(index).ok()?;
        Some(
            self.channel_1_base_seconds()
                + f64::from(index) * f64::from(SAMPLE_INTERVAL_MICROS) / 1_000_000.0,
        )
    }

    /// Device time of an output sample, without assuming a calendar epoch.
    /// The transmitted timestamp already belongs to the first averaged sample.
    #[must_use]
    pub fn sample_time_us(&self, index: usize) -> Option<u64> {
        if index >= SAMPLE_COUNT {
            return None;
        }
        u64::from(self.timestamp.utc_seconds)
            .checked_mul(1_000_000)?
            .checked_add(u64::from(self.timestamp.microseconds))?
            .checked_add(
                u64::try_from(index)
                    .ok()?
                    .checked_mul(u64::from(SAMPLE_INTERVAL_MICROS))?,
            )
    }
}

/// Offline CRC-reference reconstruction of a frame whose received CRC is invalid.
///
/// This type is retained for reference tooling. Production scientific decoding
/// does not calculate or compare CRC and does not require this wrapper.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnverifiedScientificFrame {
    frame: ScientificFrame,
}

impl UnverifiedScientificFrame {
    /// Returns diagnostic sample values without granting accepted-frame status.
    #[must_use]
    pub fn frame(&self) -> &ScientificFrame {
        &self.frame
    }
}

/// Offline CRC inspection result that leaves the received bytes intact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScientificFrameDiagnosis {
    /// Big-endian CRC field, available only for an exactly 269-byte candidate.
    pub received_crc: Option<u16>,
    /// CRC16 over the existing, exclusive `frame[7..267]` input range.
    pub calculated_crc: Option<u16>,
    /// The error from offline structure/CRC/body inspection, in that order.
    pub error: Option<FrameError>,
    /// Time or ADC error discovered behind an earlier strict CRC rejection.
    ///
    /// This does not replace `error` or change production decoding.
    pub secondary_error: Option<FrameError>,
    /// Present only for a CRC mismatch with all other validation checks passing.
    pub preview: Option<UnverifiedScientificFrame>,
}

/// Inspects a candidate against the historical CRC reference rule.
///
/// This is an explicit offline API, not a production acceptance gate.
/// Valid candidates have no error and no diagnostic preview. A CRC mismatch can
/// yield a preview only after all framing, fixed-field, timestamp and ADC range
/// checks pass. Invalid time or ADC data suppress the preview even when the
/// strict error is the earlier CRC mismatch. No bytes are changed or repaired.
#[must_use]
pub fn diagnose_scientific_frame(frame: &[u8]) -> ScientificFrameDiagnosis {
    let (received_crc, calculated_crc) = if frame.len() == LVDS_FRAME_LEN {
        (
            Some(u16::from_be_bytes([
                frame[LVDS_CRC_FIELD_OFFSET],
                frame[LVDS_CRC_FIELD_OFFSET + 1],
            ])),
            Some(crc16_ccitt_false(
                &frame[LVDS_CRC_CALC_START..LVDS_CRC_CALC_END],
            )),
        )
    } else {
        (None, None)
    };
    let error = validate_scientific_frame_structure(frame)
        .and_then(|()| validate_scientific_frame_crc16(frame))
        .and_then(|()| decode_scientific_body(frame).map(|_| ()))
        .err();
    let (preview, secondary_error) = if matches!(error, Some(FrameError::ChecksumMismatch { .. })) {
        // A strict CRC error is reached only after structure validation. The
        // same body decoder still enforces every timestamp and ADC invariant.
        match decode_scientific_body(frame) {
            Ok(frame) => (Some(UnverifiedScientificFrame { frame }), None),
            Err(error) => (None, Some(error)),
        }
    } else {
        (None, None)
    };
    ScientificFrameDiagnosis {
        received_crc,
        calculated_crc,
        error,
        secondary_error,
        preview,
    }
}

/// # Errors
///
/// Returns [`FrameError`] for malformed framing, unexpected fixed fields,
/// invalid timestamp offsets, or an out-of-range sample. The two trailer bytes
/// are preserved in the wire format but are not checked for CRC integrity.
///
/// # Panics
///
/// Does not panic for arbitrary input. Fixed-offset reads are reached only
/// after the frame length has been validated.
pub fn decode_scientific_frame(frame: &[u8]) -> Result<ScientificFrame, FrameError> {
    validate_scientific_frame_structure(frame)?;
    decode_scientific_body(frame)
}
fn validate_scientific_frame_structure(frame: &[u8]) -> Result<(), FrameError> {
    if frame.len() != LVDS_FRAME_LEN {
        return Err(FrameError::UnexpectedLength {
            protocol: Protocol::Lvds,
            expected: LVDS_FRAME_LEN,
            actual: frame.len(),
        });
    }
    if frame[..2] != FRAME_HEADER {
        return Err(FrameError::InvalidHeader {
            protocol: Protocol::Lvds,
            actual: [frame[0], frame[1]],
        });
    }

    let declared_length = u16::from_be_bytes([frame[LENGTH_OFFSET], frame[LENGTH_OFFSET + 1]]);
    if usize::from(declared_length) != LVDS_PAYLOAD_LEN {
        return Err(FrameError::InvalidDeclaredLength {
            protocol: Protocol::Lvds,
            expected: PAYLOAD_LENGTH_FIELD,
            actual: declared_length,
        });
    }
    if frame[2] != LVDS_SENDER_ADDRESS {
        return Err(FrameError::UnexpectedField {
            protocol: Protocol::Lvds,
            field: "sender address",
            expected: LVDS_SENDER_ADDRESS,
            actual: frame[2],
        });
    }
    if frame[3] != LVDS_RECEIVER_ADDRESS {
        return Err(FrameError::UnexpectedField {
            protocol: Protocol::Lvds,
            field: "receiver address",
            expected: LVDS_RECEIVER_ADDRESS,
            actual: frame[3],
        });
    }
    if frame[PAYLOAD_OFFSET] != LVDS_DATA_TYPE {
        return Err(FrameError::UnexpectedField {
            protocol: Protocol::Lvds,
            field: "data type",
            expected: LVDS_DATA_TYPE,
            actual: frame[PAYLOAD_OFFSET],
        });
    }
    if frame[RESERVED_OFFSET] != 0 {
        return Err(FrameError::UnexpectedField {
            protocol: Protocol::Lvds,
            field: "DATA251 reserved byte",
            expected: 0,
            actual: frame[RESERVED_OFFSET],
        });
    }

    Ok(())
}

fn decode_scientific_body(frame: &[u8]) -> Result<ScientificFrame, FrameError> {
    let timestamp = Timestamp {
        utc_seconds: u32::from_be_bytes([
            frame[TIMESTAMP_OFFSET],
            frame[TIMESTAMP_OFFSET + 1],
            frame[TIMESTAMP_OFFSET + 2],
            frame[TIMESTAMP_OFFSET + 3],
        ]),
        microseconds: u32::from_be_bytes([
            frame[MICROSECONDS_OFFSET],
            frame[MICROSECONDS_OFFSET + 1],
            frame[MICROSECONDS_OFFSET + 2],
            frame[MICROSECONDS_OFFSET + 3],
        ]),
    };
    validate_microseconds(timestamp.microseconds)?;

    let channel_1 =
        decode_channel(&frame[SAMPLE_DATA_OFFSET..SAMPLE_DATA_OFFSET + CHANNEL_PACKED_BYTES]);
    let channel_2 =
        decode_channel(&frame[SAMPLE_DATA_OFFSET + CHANNEL_PACKED_BYTES..FRAME_SEQUENCE_OFFSET]);
    Ok(ScientificFrame {
        timestamp,
        frame_seq_u8: frame[FRAME_SEQUENCE_OFFSET],
        channel_1,
        channel_2,
    })
}

fn decode_channel(packed: &[u8]) -> Box<[u16; SAMPLE_COUNT]> {
    let mut samples = Box::new([0; SAMPLE_COUNT]);
    for (group, values) in packed
        .as_chunks::<5>()
        .0
        .iter()
        .zip(samples.as_chunks_mut::<4>().0)
    {
        *values = unpack_4x10(group);
    }
    samples
}
/// Explicit offline reference check of the CRC16 field with `Calculate_CRC16`
/// implementation: polynomial `0x1021`, initial value `0xFFFF`, no reflection,
/// and no final xor.
///
/// The CRC covers exactly 260 valid-data bytes `frame[7..267]`: UTC,
/// microseconds, and scientific DATA. TYPE and the CRC field are excluded.
/// This range is the working interpretation of the updated table, pending the
/// manufacturer's confirmation of its actual function arguments. No alternate
/// range is accepted on a mismatch.
/// This reference API is not called by production scientific decoding.
///
/// # Errors
///
/// Returns [`FrameError`] when the frame length is invalid or the calculated
/// CRC16 does not equal the big-endian trailer.
pub fn validate_scientific_frame_crc16(frame: &[u8]) -> Result<(), FrameError> {
    if frame.len() != LVDS_FRAME_LEN {
        return Err(FrameError::UnexpectedLength {
            protocol: Protocol::Lvds,
            expected: LVDS_FRAME_LEN,
            actual: frame.len(),
        });
    }
    let expected = crc16_ccitt_false(&frame[LVDS_CRC_CALC_START..LVDS_CRC_CALC_END]);
    let actual = u16::from_be_bytes([
        frame[LVDS_CRC_FIELD_OFFSET],
        frame[LVDS_CRC_FIELD_OFFSET + 1],
    ]);
    if actual != expected {
        return Err(FrameError::ChecksumMismatch {
            protocol: Protocol::Lvds,
            expected,
            actual,
        });
    }
    Ok(())
}

/// Unpacks four consecutive big-endian 10bit codes from a five-byte group.
#[must_use]
pub fn unpack_4x10(bytes: &[u8; 5]) -> [u16; 4] {
    let [b0, b1, b2, b3, b4] = bytes.map(u16::from);
    [
        (b0 << 2) | (b1 >> 6),
        ((b1 & 0x3F) << 4) | (b2 >> 4),
        ((b2 & 0x0F) << 6) | (b3 >> 2),
        ((b3 & 0x03) << 8) | b4,
    ]
}

/// Reconstructs a signed, uncalibrated ADC estimate from a received code.
/// Quantization discarded the lower bits; this does not recover lossless ADC16.
#[must_use]
pub fn reconstruct_adc16(code10: u16) -> i32 {
    (i32::from(code10) << 6) - 32_768
}
fn validate_microseconds(value: u32) -> Result<(), FrameError> {
    if value >= 1_000_000 {
        return Err(FrameError::FieldOutOfRange {
            protocol: Protocol::Lvds,
            field: "microseconds",
            actual: u64::from(value),
            valid_range: "0..=999999",
        });
    }
    Ok(())
}

#[cfg(test)]
#[path = "lvds_test_support.rs"]
pub(crate) mod test_support;

#[cfg(test)]
#[path = "lvds_protocol_tests.rs"]
mod tests;
