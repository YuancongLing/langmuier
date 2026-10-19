use crate::{FRAME_HEADER, FrameError, Protocol, crc16_ccitt_false};

pub const LVDS_FRAME_LEN: usize = 269;
pub const LVDS_PAYLOAD_LEN: usize = 263;
pub const LVDS_DATA_TYPE: u8 = 0xAA;
pub const LVDS_SENDER_ADDRESS: u8 = 0x55;
pub const LVDS_RECEIVER_ADDRESS: u8 = 0xAA;
pub const LVDS_DEVICE_ID: u8 = LVDS_SENDER_ADDRESS;
pub const SAMPLE_COUNT: usize = 125;
pub const SAMPLE_INTERVAL_MICROS: u32 = 400;
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
const PAYLOAD_LENGTH_FIELD: u16 = 0x0107;

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
    pub sequence: u16,
    pub channel_1: Box<[u32; SAMPLE_COUNT]>,
    pub channel_2: Box<[u32; SAMPLE_COUNT]>,
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

    #[must_use]
    /// # Panics
    ///
    /// Panics only if a manually constructed [`ScientificFrame`] contains a
    /// channel-1 first sample greater than `u16::MAX`. Frames decoded by this
    /// crate always satisfy that bound.
    pub fn raw_hex_prefix(&self) -> [u8; 2] {
        u16::try_from(self.channel_1[0])
            .expect("decoded LVDS sample always fits in 16 bits")
            .to_be_bytes()
    }
}

/// Diagnostic-only reconstruction of a frame whose received CRC is invalid.
///
/// This distinct type never represents an accepted scientific frame. Its
/// timestamp and ADC ranges have been checked, but its bytes are unverified.
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

/// Inspection result that leaves strict decoding and the received bytes intact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScientificFrameDiagnosis {
    /// Big-endian CRC field, available only for an exactly 269-byte candidate.
    pub received_crc: Option<u16>,
    /// CRC16 over the existing, exclusive `frame[7..267]` input range.
    pub calculated_crc: Option<u16>,
    /// The error from strict decoding, with its original validation precedence.
    pub error: Option<FrameError>,
    /// Time or ADC error discovered behind an earlier strict CRC rejection.
    ///
    /// This does not replace `error` or relax normal decoding.
    pub secondary_error: Option<FrameError>,
    /// Present only for a CRC mismatch with all other validation checks passing.
    pub preview: Option<UnverifiedScientificFrame>,
}

/// Inspects a candidate for a separate, explicitly unverified diagnostic view.
///
/// Valid candidates have no error and no diagnostic preview. A CRC mismatch can
/// yield a preview only after all framing, fixed-field, timestamp and ADC range
/// checks pass. Invalid time or ADC data suppress the preview even when the
/// strict error is the earlier CRC mismatch. No bytes are changed or repaired.
#[must_use]
pub fn diagnose_scientific_frame(frame: &[u8], sequence: u16) -> ScientificFrameDiagnosis {
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
    let error = decode_scientific_frame_with_sequence(frame, sequence).err();
    let (preview, secondary_error) = if matches!(error, Some(FrameError::ChecksumMismatch { .. })) {
        // A strict CRC error is reached only after structure validation. The
        // same body decoder still enforces every timestamp and ADC invariant.
        match decode_scientific_body(frame, sequence) {
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
/// a CRC16 mismatch, invalid timestamp offsets, or an out-of-range sample.
///
/// # Panics
///
/// Does not panic for arbitrary input. Fixed-offset reads are reached only
/// after the frame length has been validated.
pub fn decode_scientific_frame(frame: &[u8]) -> Result<ScientificFrame, FrameError> {
    decode_scientific_frame_with_sequence(frame, 0)
}

/// Decodes one LVDS scientific frame and associates it with the UDP converter's
/// frame sequence. The LVDS body itself does not carry a packet-sequence field.
///
/// The last two bytes are a big-endian CRC16 over the declared valid-data range.
/// The lower computer has confirmed the CRC16-CCITT algorithm: polynomial
/// `0x1021`, initial value `0xFFFF`, no reflection, and no final xor.
///
/// CRC validation is mandatory before decoding time or ADC data. Its input is
/// the 260 bytes `frame[7..267]` (UTC, microseconds, and scientific DATA), using
/// the current interpretation of the updated table's "valid data". There is no
/// fallback to another range. The manufacturer's actual `data` pointer and
/// `length` arguments remain a protocol confirmation item.
///
/// Signed differences retain the existing two's-complement `i8` interpretation;
/// the manufacturer's negative-difference encoding still requires confirmation.
///
/// # Errors
///
/// Returns [`FrameError`] for malformed framing, unexpected fixed fields,
/// a CRC16 mismatch, invalid timestamp offsets, or an out-of-range sample.
///
/// # Panics
///
/// Does not panic for arbitrary input. Fixed-offset reads are reached only
/// after the frame length has been validated.
pub fn decode_scientific_frame_with_sequence(
    frame: &[u8],
    sequence: u16,
) -> Result<ScientificFrame, FrameError> {
    validate_scientific_frame_structure(frame)?;
    validate_scientific_frame_crc16(frame)?;
    decode_scientific_body(frame, sequence)
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

    Ok(())
}

fn decode_scientific_body(frame: &[u8], sequence: u16) -> Result<ScientificFrame, FrameError> {
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

    let mut channel_1 = Box::new([0_u32; SAMPLE_COUNT]);
    let mut channel_2 = Box::new([0_u32; SAMPLE_COUNT]);
    channel_1[0] = u32::from(u16::from_be_bytes([
        frame[SAMPLE_DATA_OFFSET],
        frame[SAMPLE_DATA_OFFSET + 1],
    ]));
    channel_2[0] = u32::from(u16::from_be_bytes([
        frame[SAMPLE_DATA_OFFSET + 2],
        frame[SAMPLE_DATA_OFFSET + 3],
    ]));
    for index in 1..SAMPLE_COUNT {
        let offset = SAMPLE_DATA_OFFSET + 4 + (index - 1) * 2;
        channel_1[index] = reconstruct_sample(1, index, channel_1[index - 1], frame[offset])?;
        channel_2[index] = reconstruct_sample(2, index, channel_2[index - 1], frame[offset + 1])?;
    }

    Ok(ScientificFrame {
        timestamp,
        sequence,
        channel_1,
        channel_2,
    })
}

/// Validates the CRC16 field with the lower computer's `Calculate_CRC16`
/// implementation: polynomial `0x1021`, initial value `0xFFFF`, no reflection,
/// and no final xor.
///
/// The CRC covers exactly 260 valid-data bytes `frame[7..267]`: UTC,
/// microseconds, and scientific DATA. TYPE and the CRC field are excluded.
/// This range is the working interpretation of the updated table, pending the
/// manufacturer's confirmation of its actual function arguments. No alternate
/// range is accepted on a mismatch.
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

fn reconstruct_sample(
    channel: u8,
    index: usize,
    previous: u32,
    encoded_delta: u8,
) -> Result<u32, FrameError> {
    let value = i64::from(previous) + i64::from(i8::from_be_bytes([encoded_delta]));
    if !(0_i64..=i64::from(u16::MAX)).contains(&value) {
        return Err(FrameError::SampleOutOfRange {
            protocol: Protocol::Lvds,
            channel,
            index,
            value,
        });
    }
    // Convert through a checked path even though the range check above already
    // establishes the invariant, keeping malformed input free of panic paths.
    u32::try_from(value).map_err(|_| FrameError::SampleOutOfRange {
        protocol: Protocol::Lvds,
        channel,
        index,
        value,
    })
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
pub(crate) mod test_support {
    use super::{
        LVDS_CRC_CALC_END, LVDS_CRC_CALC_START, LVDS_CRC_FIELD_OFFSET, LVDS_DATA_TYPE,
        LVDS_FRAME_LEN, LVDS_RECEIVER_ADDRESS, LVDS_SENDER_ADDRESS, PAYLOAD_LENGTH_FIELD,
        PAYLOAD_OFFSET, SAMPLE_COUNT, SAMPLE_DATA_OFFSET,
    };
    use crate::{FRAME_HEADER, checksum::test_support::manufacturer_crc16};

    pub(crate) fn refresh_crc16(frame: &mut [u8; LVDS_FRAME_LEN]) {
        let crc = manufacturer_crc16(&frame[LVDS_CRC_CALC_START..LVDS_CRC_CALC_END]);
        frame[LVDS_CRC_FIELD_OFFSET..LVDS_CRC_FIELD_OFFSET + 2].copy_from_slice(&crc.to_be_bytes());
    }

    pub(crate) fn golden_frame() -> [u8; LVDS_FRAME_LEN] {
        // Complete fixed wire vector: the following 19-byte prefix, 124 copies
        // of 01 FF, then 44 C8. The CRC is a fixed result from the separately
        // compiled manufacturer's C function, not generated by production code.
        let mut frame = [0_u8; LVDS_FRAME_LEN];
        frame[..19].copy_from_slice(&[
            0xEB, 0x90, 0x55, 0xAA, 0x01, 0x07, 0xAA, 0x65, 0x00, 0x00, 0x00, 0x00, 0x0F, 0x41,
            0x78, 0x12, 0x34, 0xAB, 0xCD,
        ]);
        for offset in (19..267).step_by(2) {
            frame[offset] = 0x01;
            frame[offset + 1] = 0xFF;
        }
        frame[267..269].copy_from_slice(&[0x44, 0xC8]);
        frame
    }

    pub(crate) fn valid_frame() -> Box<[u8; LVDS_FRAME_LEN]> {
        let mut frame = Box::new([0_u8; LVDS_FRAME_LEN]);
        frame[..2].copy_from_slice(&FRAME_HEADER);
        frame[2] = LVDS_SENDER_ADDRESS;
        frame[3] = LVDS_RECEIVER_ADDRESS;
        frame[4..6].copy_from_slice(&PAYLOAD_LENGTH_FIELD.to_be_bytes());
        frame[PAYLOAD_OFFSET] = LVDS_DATA_TYPE;
        frame[7..11].copy_from_slice(&1_700_000_000_u32.to_be_bytes());
        frame[11..15].copy_from_slice(&123_456_u32.to_be_bytes());
        frame[SAMPLE_DATA_OFFSET..SAMPLE_DATA_OFFSET + 2].copy_from_slice(&2048_u16.to_be_bytes());
        frame[SAMPLE_DATA_OFFSET + 2..SAMPLE_DATA_OFFSET + 4]
            .copy_from_slice(&20_000_u16.to_be_bytes());
        for index in 1..SAMPLE_COUNT {
            let offset = SAMPLE_DATA_OFFSET + 4 + (index - 1) * 2;
            frame[offset] = if index % 2 == 0 { 0xFF } else { 1 };
            frame[offset + 1] = if index % 3 == 0 { 0xFE } else { 2 };
        }
        refresh_crc16(&mut frame);
        frame
    }
}

#[cfg(test)]
mod tests {
    use super::{
        LVDS_CRC_CALC_END, LVDS_CRC_CALC_START, LVDS_CRC_FIELD_OFFSET, LVDS_CRC_INPUT_LEN,
        LVDS_FRAME_LEN, SAMPLE_COUNT, decode_scientific_frame,
        decode_scientific_frame_with_sequence, diagnose_scientific_frame,
        test_support::{golden_frame, refresh_crc16, valid_frame},
        validate_scientific_frame_crc16,
    };
    use crate::{FrameError, Protocol, checksum::test_support::manufacturer_crc16};

    #[test]
    fn fixed_golden_frame_has_independent_crc_and_expected_samples() {
        let frame = golden_frame();
        assert_eq!(frame.len(), 269);
        assert_eq!(&frame[..7], &[0xEB, 0x90, 0x55, 0xAA, 0x01, 0x07, 0xAA]);
        assert_eq!(LVDS_CRC_CALC_START, 7);
        assert_eq!(LVDS_CRC_CALC_END, 267);
        assert_eq!(LVDS_CRC_FIELD_OFFSET, 267);
        assert_eq!(LVDS_CRC_INPUT_LEN, 260);
        assert_eq!(manufacturer_crc16(&frame[7..267]), 0x44C8);
        assert_eq!(&frame[267..269], &[0x44, 0xC8]);
        assert_eq!(validate_scientific_frame_crc16(&frame), Ok(()));

        let decoded = decode_scientific_frame(&frame).unwrap();
        assert_eq!(decoded.timestamp.utc_seconds, 1_694_498_816);
        assert_eq!(decoded.timestamp.microseconds, 999_800);
        assert_eq!(decoded.sequence, 0);
        assert_eq!(decoded.channel_1.len(), 125);
        assert_eq!(decoded.channel_2.len(), 125);
        for index in 0..SAMPLE_COUNT {
            let index_u32 = u32::try_from(index).unwrap();
            assert_eq!(decoded.channel_1[index], 4660 + index_u32);
            assert_eq!(decoded.channel_2[index], 43_981 - index_u32);
        }
        assert_eq!(decoded.channel_1[124], 4784);
        assert_eq!(decoded.channel_2[124], 43_857);
        assert_eq!(
            decoded.channel_1_base_seconds(),
            decoded.channel_2_base_seconds()
        );
        assert!((decoded.channel_1_sample_time(1).unwrap() - 1_694_498_817.000_2).abs() < 1e-6);
        assert!((decoded.channel_1_sample_time(124).unwrap() - 1_694_498_817.049_4).abs() < 1e-6);
    }

    #[test]
    fn rejects_crc_changes_and_accepts_independently_recalculated_frames() {
        let valid = golden_frame();
        // UTC, valid microseconds, a bounded scientific delta, and both CRC
        // bytes are changed individually to exercise the CRC gate itself.
        for offset in [7, 14, 100, 267, 268] {
            let mut corrupt = valid;
            corrupt[offset] ^= 1;
            let expected = manufacturer_crc16(&corrupt[7..267]);
            let actual = u16::from_be_bytes([corrupt[267], corrupt[268]]);
            assert_eq!(
                decode_scientific_frame(&corrupt),
                Err(FrameError::ChecksumMismatch {
                    protocol: Protocol::Lvds,
                    expected,
                    actual,
                }),
                "offset {offset} must fail at the CRC gate"
            );
            refresh_crc16(&mut corrupt);
            assert!(decode_scientific_frame(&corrupt).is_ok(), "offset {offset}");
        }
    }

    #[test]
    fn accepts_only_the_declared_crc_range_and_big_endian_field() {
        let golden = golden_frame();
        // Also fixed outputs from the separately compiled manufacturer C.
        assert_eq!(manufacturer_crc16(&golden[6..267]), 0xDC4E);
        assert_eq!(manufacturer_crc16(&golden[15..267]), 0x0A62);
        for incorrect_crc in [0xDC4E_u16, 0x0A62, 0xC844] {
            let mut frame = golden;
            frame[267..269].copy_from_slice(&incorrect_crc.to_be_bytes());
            assert_eq!(
                decode_scientific_frame(&frame),
                Err(FrameError::ChecksumMismatch {
                    protocol: Protocol::Lvds,
                    expected: 0x44C8,
                    actual: incorrect_crc,
                })
            );
        }
    }

    #[test]
    fn consecutive_golden_frames_keep_independent_bases_and_device_time() {
        let first_wire = golden_frame();
        let mut second_wire = first_wire;
        second_wire[7..11].copy_from_slice(&1_694_498_817_u32.to_be_bytes());
        second_wire[11..15].copy_from_slice(&49_800_u32.to_be_bytes());
        // Independently obtained from the manufacturer's compiled C function.
        second_wire[267..269].copy_from_slice(&[0x5E, 0xF9]);
        assert_eq!(manufacturer_crc16(&second_wire[7..267]), 0x5EF9);
        let first = decode_scientific_frame_with_sequence(&first_wire, 12).unwrap();
        let second = decode_scientific_frame_with_sequence(&second_wire, 13).unwrap();
        assert_eq!(second.channel_1[0], 4660);
        assert_eq!(second.channel_2[0], 43_981);
        assert_eq!(second.channel_1, first.channel_1);
        assert_eq!(second.channel_2, first.channel_2);
        assert!(
            (second.channel_1_base_seconds() - first.channel_1_base_seconds() - 0.05).abs() < 1e-6
        );
        assert_eq!(first.sequence, 12);
        assert_eq!(second.sequence, 13);
    }

    #[test]
    fn decodes_timestamp_sequence_and_signed_deltas() {
        let decoded = decode_scientific_frame_with_sequence(valid_frame().as_slice(), 42).unwrap();
        assert_eq!(decoded.timestamp.utc_seconds, 1_700_000_000);
        assert_eq!(decoded.timestamp.microseconds, 123_456);
        assert_eq!(decoded.sequence, 42);
        assert_eq!(decoded.channel_1[0], 2048);
        assert_eq!(decoded.channel_1[1], 2049);
        assert_eq!(decoded.channel_1[2], 2048);
        assert_eq!(decoded.channel_2[0], 20_000);
        assert_eq!(decoded.channel_2[1], 20_002);
        assert_eq!(decoded.channel_2[3], 20_002);
        assert_eq!(decoded.channel_1.len(), SAMPLE_COUNT);
        assert_eq!(decoded.raw_hex_prefix(), [0x08, 0x00]);
        assert!((decoded.channel_1_base_seconds() - 1_700_000_000.123_456).abs() < 1e-9);
        assert!((decoded.channel_1_sample_time(124).unwrap() - 1_700_000_000.173_056).abs() < 1e-9);
        assert_eq!(decoded.channel_1_sample_time(SAMPLE_COUNT), None);
    }

    #[test]
    fn rejects_length_and_structure_errors() {
        let valid = valid_frame();
        for length in 0..LVDS_FRAME_LEN {
            assert!(matches!(
                decode_scientific_frame(&valid[..length]),
                Err(FrameError::UnexpectedLength { actual, .. }) if actual == length
            ));
        }
        let mut oversized = valid.to_vec();
        oversized.push(0);
        assert!(matches!(
            decode_scientific_frame(&oversized),
            Err(FrameError::UnexpectedLength { actual: 270, .. })
        ));
        let mut frame = valid_frame();
        frame[0] ^= 1;
        assert!(matches!(
            decode_scientific_frame(frame.as_slice()),
            Err(FrameError::InvalidHeader { .. })
        ));
        let mut frame = valid_frame();
        frame[4] = 0;
        assert!(matches!(
            decode_scientific_frame(frame.as_slice()),
            Err(FrameError::InvalidDeclaredLength { .. })
        ));
        let decoded_without_transport_sequence = decode_scientific_frame(valid.as_slice()).unwrap();
        assert_eq!(decoded_without_transport_sequence.sequence, 0);
        for (offset, field) in [
            (2, "sender address"),
            (3, "receiver address"),
            (6, "data type"),
        ] {
            let mut frame = valid_frame();
            frame[offset] = 0;
            assert!(matches!(
                decode_scientific_frame(frame.as_slice()),
                Err(FrameError::UnexpectedField { field: actual_field, .. }) if actual_field == field
            ));
        }
    }

    #[test]
    fn validates_lower_computer_crc16_implementation() {
        let mut frame = valid_frame();
        assert_eq!(validate_scientific_frame_crc16(frame.as_slice()), Ok(()));
        frame[100] ^= 1;
        assert!(matches!(
            validate_scientific_frame_crc16(frame.as_slice()),
            Err(FrameError::ChecksumMismatch {
                protocol: Protocol::Lvds,
                ..
            })
        ));
        refresh_crc16(&mut frame);
        assert_eq!(validate_scientific_frame_crc16(frame.as_slice()), Ok(()));
    }

    #[test]
    fn rejects_wrong_addresses_and_sample_underflow() {
        let mut frame = valid_frame();
        frame[2] = 0;
        assert!(matches!(
            decode_scientific_frame(frame.as_slice()),
            Err(FrameError::UnexpectedField {
                field: "sender address",
                ..
            })
        ));

        let mut frame = valid_frame();
        frame[15..17].copy_from_slice(&0_u16.to_be_bytes());
        frame[19] = 0x80;
        assert!(matches!(
            decode_scientific_frame(frame.as_slice()),
            Err(FrameError::ChecksumMismatch { .. })
        ));
        refresh_crc16(&mut frame);
        assert!(matches!(
            decode_scientific_frame(frame.as_slice()),
            Err(FrameError::SampleOutOfRange {
                channel: 1,
                index: 1,
                ..
            })
        ));

        let mut frame = valid_frame();
        frame[17..19].copy_from_slice(&u16::MAX.to_be_bytes());
        frame[20] = 1;
        refresh_crc16(&mut frame);
        assert!(matches!(
            decode_scientific_frame(frame.as_slice()),
            Err(FrameError::SampleOutOfRange {
                channel: 2,
                index: 1,
                ..
            })
        ));
    }

    #[test]
    fn rejects_invalid_microseconds() {
        let mut frame = valid_frame();
        frame[11..15].copy_from_slice(&1_000_000_u32.to_be_bytes());
        assert!(matches!(
            decode_scientific_frame(frame.as_slice()),
            Err(FrameError::ChecksumMismatch { .. })
        ));
        refresh_crc16(&mut frame);
        assert!(matches!(
            decode_scientific_frame(frame.as_slice()),
            Err(FrameError::FieldOutOfRange {
                field: "microseconds",
                ..
            })
        ));
    }

    #[test]
    fn arbitrary_wire_input_returns_errors_without_panicking() {
        for length in 0..=LVDS_FRAME_LEN + 8 {
            let _ = decode_scientific_frame(&vec![0xEB; length]);
        }
        let mut state = 0x1357_9BDF_u32;
        for _ in 0..128 {
            let mut frame = golden_frame();
            for byte in &mut frame[7..267] {
                state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                *byte = state.to_be_bytes()[0];
            }
            refresh_crc16(&mut frame);
            let _ = decode_scientific_frame(&frame);
        }
    }

    #[test]
    fn diagnostic_valid_frame_reports_crc_without_creating_unverified_preview() {
        let frame = golden_frame();
        let result = diagnose_scientific_frame(&frame, 42);
        assert_eq!(result.received_crc, Some(0x44C8));
        assert_eq!(result.calculated_crc, Some(0x44C8));
        assert_eq!(result.error, None);
        assert_eq!(result.secondary_error, None);
        assert_eq!(result.preview, None);
    }

    #[test]
    fn diagnostic_crc_preview_preserves_samples_and_never_repairs_wire() {
        let valid = golden_frame();
        let decoded = decode_scientific_frame_with_sequence(&valid, 42).unwrap();
        for invalid_crc in [0x44C9_u16, 0xDC4E, 0x0A62, 0xC844] {
            let mut frame = valid;
            frame[267..269].copy_from_slice(&invalid_crc.to_be_bytes());
            let unchanged = frame;
            let result = diagnose_scientific_frame(&frame, 42);
            assert_eq!(result.received_crc, Some(invalid_crc));
            assert_eq!(result.calculated_crc, Some(0x44C8));
            assert_eq!(
                result.error,
                Some(FrameError::ChecksumMismatch {
                    protocol: Protocol::Lvds,
                    expected: 0x44C8,
                    actual: invalid_crc,
                })
            );
            assert_eq!(result.preview.unwrap().frame(), &decoded);
            assert_eq!(result.secondary_error, None);
            assert_eq!(frame, unchanged);
            assert!(decode_scientific_frame(&frame).is_err());
        }
    }

    #[test]
    fn diagnostic_structure_failures_never_produce_preview() {
        let valid = golden_frame();
        for length in 0..LVDS_FRAME_LEN {
            let result = diagnose_scientific_frame(&valid[..length], 0);
            assert_eq!(result.received_crc, None);
            assert_eq!(result.calculated_crc, None);
            assert_eq!(
                result.error,
                decode_scientific_frame(&valid[..length]).err()
            );
            assert_eq!(result.preview, None);
            assert_eq!(result.secondary_error, None);
        }
        let mut oversized = valid.to_vec();
        oversized.push(0);
        let result = diagnose_scientific_frame(&oversized, 0);
        assert_eq!(result.received_crc, None);
        assert_eq!(result.calculated_crc, None);
        assert_eq!(result.error, decode_scientific_frame(&oversized).err());
        assert_eq!(result.preview, None);

        for offset in [0, 1, 2, 3, 4, 5, 6] {
            let mut malformed = valid;
            malformed[offset] ^= 1;
            // Also corrupt CRC, so it cannot accidentally permit inspection
            // before the framing and fixed fields have been checked.
            malformed[267] ^= 1;
            let result = diagnose_scientific_frame(&malformed, 0);
            assert_eq!(result.error, decode_scientific_frame(&malformed).err());
            assert_eq!(result.preview, None, "offset {offset}");
            assert_eq!(result.secondary_error, None);
            assert_eq!(result.calculated_crc, Some(0x44C8));
        }
    }

    #[test]
    fn diagnostic_bad_microseconds_suppress_preview_with_valid_or_bad_crc() {
        for microseconds in [1_000_000_u32, u32::MAX] {
            let mut frame = golden_frame();
            frame[11..15].copy_from_slice(&microseconds.to_be_bytes());
            let result = diagnose_scientific_frame(&frame, 3);
            assert!(matches!(
                result.error,
                Some(FrameError::ChecksumMismatch { .. })
            ));
            assert!(matches!(
                result.secondary_error,
                Some(FrameError::FieldOutOfRange {
                    field: "microseconds",
                    ..
                })
            ));
            assert_eq!(result.preview, None);
            refresh_crc16(&mut frame);
            let result = diagnose_scientific_frame(&frame, 3);
            assert!(matches!(
                result.error,
                Some(FrameError::FieldOutOfRange {
                    field: "microseconds",
                    ..
                })
            ));
            assert_eq!(result.preview, None);
        }
    }

    #[test]
    fn diagnostic_checks_entire_both_channels_before_allowing_crc_preview() {
        for channel in [1_u8, 2] {
            for index in [1_usize, SAMPLE_COUNT - 1] {
                for (base, delta) in [(0_u16, 0xFF_u8), (u16::MAX, 1)] {
                    let mut frame = golden_frame();
                    frame[19..267].fill(0);
                    let base_offset = 15 + usize::from(channel - 1) * 2;
                    frame[base_offset..base_offset + 2].copy_from_slice(&base.to_be_bytes());
                    let delta_offset = 19 + (index - 1) * 2 + usize::from(channel - 1);
                    frame[delta_offset] = delta;
                    let result = diagnose_scientific_frame(&frame, 9);
                    assert!(matches!(
                        result.error,
                        Some(FrameError::ChecksumMismatch { .. })
                    ));
                    assert!(matches!(
                        result.secondary_error,
                        Some(FrameError::SampleOutOfRange { channel: actual_channel, index: actual_index, .. })
                        if actual_channel == channel && actual_index == index
                    ));
                    assert_eq!(result.preview, None);
                    refresh_crc16(&mut frame);
                    let result = diagnose_scientific_frame(&frame, 9);
                    assert!(matches!(
                        result.error,
                        Some(FrameError::SampleOutOfRange { channel: actual_channel, index: actual_index, .. })
                        if actual_channel == channel && actual_index == index
                    ));
                    assert_eq!(result.preview, None);
                }
            }
        }
    }

    #[test]
    fn diagnostic_arbitrary_candidates_return_without_panicking() {
        for length in 0..=LVDS_FRAME_LEN + 8 {
            let _ = diagnose_scientific_frame(&vec![0xEB; length], 0);
        }
        let mut state = 0x2468_ACE0_u32;
        for _ in 0..128 {
            let mut frame = golden_frame();
            for byte in &mut frame[7..267] {
                state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                *byte = state.to_be_bytes()[0];
            }
            let result = diagnose_scientific_frame(&frame, u16::MAX);
            assert_eq!(
                result.error,
                decode_scientific_frame_with_sequence(&frame, u16::MAX).err()
            );
        }
    }
}
