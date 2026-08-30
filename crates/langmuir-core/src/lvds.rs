use crate::{FRAME_HEADER, FrameError, Protocol, additive_checksum};

pub const LVDS_FRAME_LEN: usize = 270;
pub const LVDS_PAYLOAD_LEN: usize = 263;
pub const LVDS_DATA_TYPE: u8 = 0xAA;
pub const LVDS_SENDER_ADDRESS: u8 = 0x55;
pub const LVDS_RECEIVER_ADDRESS: u8 = 0xAA;
pub const LVDS_DEVICE_ID: u8 = LVDS_SENDER_ADDRESS;
pub const SAMPLE_COUNT: usize = 125;
pub const SAMPLE_INTERVAL_MICROS: u32 = 400;

const LENGTH_OFFSET: usize = 4;
const PAYLOAD_OFFSET: usize = 6;
const TIMESTAMP_OFFSET: usize = 7;
const MICROSECONDS_OFFSET: usize = 11;
const SEQUENCE_OFFSET: usize = 15;
const SAMPLE_DATA_OFFSET: usize = 17;
const CHECKSUM_OFFSET: usize = 269;
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

/// # Errors
///
/// Returns [`FrameError`] for malformed framing, unexpected fixed fields,
/// invalid timestamp offsets, a checksum mismatch, or an out-of-range sample.
///
/// # Panics
///
/// Does not panic for arbitrary input. Internal fixed-size conversions are
/// reached only after the frame length has been validated.
pub fn decode_scientific_frame(frame: &[u8]) -> Result<ScientificFrame, FrameError> {
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

    let timestamp = Timestamp {
        utc_seconds: u32::from_be_bytes(
            frame[TIMESTAMP_OFFSET..TIMESTAMP_OFFSET + 4]
                .try_into()
                .unwrap(),
        ),
        microseconds: u32::from_be_bytes(
            frame[MICROSECONDS_OFFSET..MICROSECONDS_OFFSET + 4]
                .try_into()
                .unwrap(),
        ),
    };
    validate_microseconds(timestamp.microseconds)?;

    let expected_checksum = u16::from(additive_checksum(&frame[PAYLOAD_OFFSET..CHECKSUM_OFFSET]));
    let actual_checksum = u16::from(frame[CHECKSUM_OFFSET]);
    if actual_checksum != expected_checksum {
        return Err(FrameError::ChecksumMismatch {
            protocol: Protocol::Lvds,
            expected: expected_checksum,
            actual: actual_checksum,
        });
    }

    let sequence = u16::from_be_bytes([frame[SEQUENCE_OFFSET], frame[SEQUENCE_OFFSET + 1]]);
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
    Ok(u32::try_from(value).expect("sample range was checked"))
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
        CHECKSUM_OFFSET, LVDS_DATA_TYPE, LVDS_FRAME_LEN, LVDS_RECEIVER_ADDRESS,
        LVDS_SENDER_ADDRESS, PAYLOAD_LENGTH_FIELD, PAYLOAD_OFFSET, SAMPLE_COUNT,
        SAMPLE_DATA_OFFSET,
    };
    use crate::{FRAME_HEADER, additive_checksum};

    pub(crate) fn refresh_checksum(frame: &mut [u8; LVDS_FRAME_LEN]) {
        frame[CHECKSUM_OFFSET] = additive_checksum(&frame[PAYLOAD_OFFSET..CHECKSUM_OFFSET]);
    }

    pub(crate) fn valid_frame() -> Box<[u8; LVDS_FRAME_LEN]> {
        let mut frame = Box::new([0_u8; LVDS_FRAME_LEN]);
        frame[..2].copy_from_slice(&FRAME_HEADER);
        frame[2] = LVDS_SENDER_ADDRESS;
        frame[3] = LVDS_RECEIVER_ADDRESS;
        frame[4..6].copy_from_slice(&PAYLOAD_LENGTH_FIELD.to_be_bytes());
        frame[6] = LVDS_DATA_TYPE;
        frame[7..11].copy_from_slice(&1_700_000_000_u32.to_be_bytes());
        frame[11..15].copy_from_slice(&123_456_u32.to_be_bytes());
        frame[15..17].copy_from_slice(&42_u16.to_be_bytes());
        frame[SAMPLE_DATA_OFFSET..SAMPLE_DATA_OFFSET + 2].copy_from_slice(&2048_u16.to_be_bytes());
        frame[SAMPLE_DATA_OFFSET + 2..SAMPLE_DATA_OFFSET + 4]
            .copy_from_slice(&20_000_u16.to_be_bytes());
        for index in 1..SAMPLE_COUNT {
            let offset = SAMPLE_DATA_OFFSET + 4 + (index - 1) * 2;
            frame[offset] = if index % 2 == 0 { 0xFF } else { 1 };
            frame[offset + 1] = if index % 3 == 0 { 0xFE } else { 2 };
        }
        refresh_checksum(&mut frame);
        frame
    }
}

#[cfg(test)]
mod tests {
    use super::{
        LVDS_FRAME_LEN, SAMPLE_COUNT, decode_scientific_frame,
        test_support::{refresh_checksum, valid_frame},
    };
    use crate::{FrameError, Protocol};

    #[test]
    fn decodes_timestamp_sequence_and_signed_deltas() {
        let decoded = decode_scientific_frame(valid_frame().as_slice()).unwrap();
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
    fn rejects_length_structure_and_checksum_errors() {
        let valid = valid_frame();
        for length in 0..LVDS_FRAME_LEN {
            assert!(matches!(
                decode_scientific_frame(&valid[..length]),
                Err(FrameError::UnexpectedLength { actual, .. }) if actual == length
            ));
        }
        let mut frame = valid_frame();
        frame[4] = 0;
        assert!(matches!(
            decode_scientific_frame(frame.as_slice()),
            Err(FrameError::InvalidDeclaredLength { .. })
        ));
        let mut frame = valid_frame();
        frame[100] ^= 1;
        assert!(matches!(
            decode_scientific_frame(frame.as_slice()),
            Err(FrameError::ChecksumMismatch {
                protocol: Protocol::Lvds,
                ..
            })
        ));
    }

    #[test]
    fn rejects_wrong_addresses_and_sample_underflow() {
        let mut frame = valid_frame();
        frame[2] = 0;
        refresh_checksum(&mut frame);
        assert!(matches!(
            decode_scientific_frame(frame.as_slice()),
            Err(FrameError::UnexpectedField {
                field: "sender address",
                ..
            })
        ));

        let mut frame = valid_frame();
        frame[17..19].copy_from_slice(&0_u16.to_be_bytes());
        frame[21] = 0x80;
        refresh_checksum(&mut frame);
        assert!(matches!(
            decode_scientific_frame(frame.as_slice()),
            Err(FrameError::SampleOutOfRange {
                channel: 1,
                index: 1,
                ..
            })
        ));
    }

    #[test]
    fn rejects_invalid_microseconds() {
        let mut frame = valid_frame();
        frame[11..15].copy_from_slice(&1_000_000_u32.to_be_bytes());
        refresh_checksum(&mut frame);
        assert!(matches!(
            decode_scientific_frame(frame.as_slice()),
            Err(FrameError::FieldOutOfRange {
                field: "microseconds",
                ..
            })
        ));
    }
}
