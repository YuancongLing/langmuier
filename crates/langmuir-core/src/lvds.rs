use crate::{FRAME_HEADER, FRAME_TAIL, FrameError, Protocol, crc16_ccitt_false};

pub const LVDS_FRAME_LEN: usize = 6_026;
pub const LVDS_PAYLOAD_LEN: usize = 6_018;
pub const LVDS_DATA_TYPE: u8 = 0xAA;
pub const LVDS_DEVICE_ID: u8 = 0x04;
pub const SAMPLE_COUNT: usize = 1_000;
pub const SAMPLE_INTERVAL_MICROS: u32 = 10;

const PAYLOAD_OFFSET: usize = 4;
const PAYLOAD_LENGTH_FIELD: u16 = 6_018;
const DATA_TYPE_OFFSET: usize = 4;
const DEVICE_ID_OFFSET: usize = 5;
const TIMESTAMP_OFFSET: usize = 6;
const SAMPLE_DATA_OFFSET: usize = 22;
const CRC_OFFSET: usize = 6_022;
const TAIL_OFFSET: usize = 6_024;

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

/// Decoded scientific samples. Samples are owned so transport buffers can be
/// immediately reused; each channel is allocated exactly once.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScientificFrame {
    pub channel_1_time: Timestamp,
    pub channel_2_time: Timestamp,
    pub channel_1: Box<[u32; SAMPLE_COUNT]>,
    pub channel_2: Box<[u32; SAMPLE_COUNT]>,
}

impl ScientificFrame {
    #[must_use]
    pub fn channel_1_base_seconds(&self) -> f64 {
        self.channel_1_time.as_seconds_f64()
    }

    /// Time in seconds for a channel-1 sample. Returns `None` outside the frame.
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
    pub fn raw_hex_prefix(&self) -> [u8; 3] {
        let value = self.channel_1[0];
        [
            ((value >> 16) & 0xFF) as u8,
            ((value >> 8) & 0xFF) as u8,
            (value & 0xFF) as u8,
        ]
    }
}

/// Decodes and validates one complete LVDS scientific frame.
///
/// # Errors
///
/// Returns [`FrameError`] when the input is not exactly one scientific frame,
/// a structural or discriminator field is invalid, either microsecond field is
/// out of range, or the CRC does not match.
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

    let declared_length = u16::from_be_bytes([frame[2], frame[3]]);
    if usize::from(declared_length) != LVDS_PAYLOAD_LEN {
        return Err(FrameError::InvalidDeclaredLength {
            protocol: Protocol::Lvds,
            expected: PAYLOAD_LENGTH_FIELD,
            actual: declared_length,
        });
    }
    if frame[TAIL_OFFSET..] != FRAME_TAIL {
        return Err(FrameError::InvalidTail {
            protocol: Protocol::Lvds,
            actual: [frame[TAIL_OFFSET], frame[TAIL_OFFSET + 1]],
        });
    }
    if frame[DATA_TYPE_OFFSET] != LVDS_DATA_TYPE {
        return Err(FrameError::UnexpectedField {
            protocol: Protocol::Lvds,
            field: "data type",
            expected: LVDS_DATA_TYPE,
            actual: frame[DATA_TYPE_OFFSET],
        });
    }
    if frame[DEVICE_ID_OFFSET] != LVDS_DEVICE_ID {
        return Err(FrameError::UnexpectedField {
            protocol: Protocol::Lvds,
            field: "instrument id",
            expected: LVDS_DEVICE_ID,
            actual: frame[DEVICE_ID_OFFSET],
        });
    }

    let actual_crc = u16::from_be_bytes([frame[CRC_OFFSET], frame[CRC_OFFSET + 1]]);
    let expected_crc = crc16_ccitt_false(&frame[PAYLOAD_OFFSET..CRC_OFFSET]);
    if actual_crc != expected_crc {
        return Err(FrameError::ChecksumMismatch {
            protocol: Protocol::Lvds,
            expected: expected_crc,
            actual: actual_crc,
        });
    }

    let channel_1_time = timestamp_from(&frame[TIMESTAMP_OFFSET..TIMESTAMP_OFFSET + 8]);
    validate_microseconds("channel 1 microseconds", channel_1_time.microseconds)?;
    let channel_2_time = timestamp_from(&frame[TIMESTAMP_OFFSET + 8..SAMPLE_DATA_OFFSET]);
    validate_microseconds("channel 2 microseconds", channel_2_time.microseconds)?;

    let mut channel_1 = Box::new([0_u32; SAMPLE_COUNT]);
    let mut channel_2 = Box::new([0_u32; SAMPLE_COUNT]);
    for (index, sample) in frame[SAMPLE_DATA_OFFSET..CRC_OFFSET]
        .chunks_exact(6)
        .enumerate()
    {
        channel_1[index] = decode_u24(&sample[..3]);
        channel_2[index] = decode_u24(&sample[3..]);
    }

    Ok(ScientificFrame {
        channel_1_time,
        channel_2_time,
        channel_1,
        channel_2,
    })
}

fn timestamp_from(data: &[u8]) -> Timestamp {
    Timestamp {
        utc_seconds: u32::from_be_bytes(data[..4].try_into().unwrap()),
        microseconds: u32::from_be_bytes(data[4..8].try_into().unwrap()),
    }
}

fn validate_microseconds(field: &'static str, value: u32) -> Result<(), FrameError> {
    if value >= 1_000_000 {
        return Err(FrameError::FieldOutOfRange {
            protocol: Protocol::Lvds,
            field,
            actual: u64::from(value),
            valid_range: "0..=999999",
        });
    }
    Ok(())
}

fn decode_u24(bytes: &[u8]) -> u32 {
    (u32::from(bytes[0]) << 16) | (u32::from(bytes[1]) << 8) | u32::from(bytes[2])
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::{
        CRC_OFFSET, LVDS_DATA_TYPE, LVDS_DEVICE_ID, LVDS_FRAME_LEN, PAYLOAD_LENGTH_FIELD,
        PAYLOAD_OFFSET, SAMPLE_COUNT,
    };
    use crate::{FRAME_HEADER, FRAME_TAIL, crc16_ccitt_false};

    pub(crate) fn refresh_crc(frame: &mut [u8; LVDS_FRAME_LEN]) {
        let crc = crc16_ccitt_false(&frame[PAYLOAD_OFFSET..CRC_OFFSET]);
        frame[CRC_OFFSET..CRC_OFFSET + 2].copy_from_slice(&crc.to_be_bytes());
    }

    pub(crate) fn valid_frame() -> Box<[u8; LVDS_FRAME_LEN]> {
        let mut frame = Box::new([0_u8; LVDS_FRAME_LEN]);
        frame[..2].copy_from_slice(&FRAME_HEADER);
        frame[2..4].copy_from_slice(&PAYLOAD_LENGTH_FIELD.to_be_bytes());
        frame[4] = LVDS_DATA_TYPE;
        frame[5] = LVDS_DEVICE_ID;
        frame[6..10].copy_from_slice(&1_700_000_000_u32.to_be_bytes());
        frame[10..14].copy_from_slice(&123_456_u32.to_be_bytes());
        frame[14..18].copy_from_slice(&1_700_000_001_u32.to_be_bytes());
        frame[18..22].copy_from_slice(&654_321_u32.to_be_bytes());
        for index in 0..SAMPLE_COUNT {
            let ch1 = u32::try_from(index).unwrap();
            let ch2 = 0xFF_FFFF_u32 - ch1;
            let offset = 22 + index * 6;
            frame[offset..offset + 3].copy_from_slice(&ch1.to_be_bytes()[1..]);
            frame[offset + 3..offset + 6].copy_from_slice(&ch2.to_be_bytes()[1..]);
        }
        refresh_crc(&mut frame);
        frame[6024..].copy_from_slice(&FRAME_TAIL);
        frame
    }
}

#[cfg(test)]
mod tests {
    use super::{
        LVDS_FRAME_LEN, SAMPLE_COUNT, decode_scientific_frame,
        test_support::{refresh_crc, valid_frame},
    };
    use crate::{FrameError, Protocol};

    #[test]
    fn decodes_timestamps_and_unsigned_24_bit_samples() {
        let decoded = decode_scientific_frame(valid_frame().as_slice()).unwrap();
        assert_eq!(decoded.channel_1_time.utc_seconds, 1_700_000_000);
        assert_eq!(decoded.channel_1_time.microseconds, 123_456);
        assert_eq!(decoded.channel_2_time.utc_seconds, 1_700_000_001);
        assert_eq!(decoded.channel_2_time.microseconds, 654_321);
        assert_eq!(decoded.channel_1[0], 0);
        assert_eq!(decoded.channel_1[500], 500);
        assert_eq!(decoded.channel_1[SAMPLE_COUNT - 1], 999);
        assert_eq!(decoded.channel_2[0], 0xFF_FFFF);
        assert_eq!(decoded.channel_2[SAMPLE_COUNT - 1], 0xFF_FFFF - 999);
        assert_eq!(decoded.raw_hex_prefix(), [0, 0, 0]);
        assert!((decoded.channel_1_base_seconds() - 1_700_000_000.123_456).abs() < 1e-9);
        assert!((decoded.channel_1_sample_time(999).unwrap() - 1_700_000_000.133_446).abs() < 1e-9);
        assert_eq!(decoded.channel_1_sample_time(1_000), None);
    }

    #[test]
    fn rejects_every_short_length_and_oversize() {
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
            Err(FrameError::UnexpectedLength { actual: 6_027, .. })
        ));
    }

    #[test]
    fn rejects_corrupt_structure_and_crc() {
        let mut frame = valid_frame();
        frame[0] = 0;
        assert!(matches!(
            decode_scientific_frame(frame.as_slice()),
            Err(FrameError::InvalidHeader {
                protocol: Protocol::Lvds,
                ..
            })
        ));

        let mut frame = valid_frame();
        frame[3] ^= 1;
        assert!(matches!(
            decode_scientific_frame(frame.as_slice()),
            Err(FrameError::InvalidDeclaredLength { .. })
        ));

        let mut frame = valid_frame();
        frame[6025] = 0;
        assert!(matches!(
            decode_scientific_frame(frame.as_slice()),
            Err(FrameError::InvalidTail {
                protocol: Protocol::Lvds,
                ..
            })
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
    fn rejects_wrong_type_and_device() {
        let mut frame = valid_frame();
        frame[4] = 0x55;
        refresh_crc(&mut frame);
        assert!(matches!(
            decode_scientific_frame(frame.as_slice()),
            Err(FrameError::UnexpectedField {
                field: "data type",
                ..
            })
        ));

        let mut frame = valid_frame();
        frame[5] = 0x05;
        refresh_crc(&mut frame);
        assert!(matches!(
            decode_scientific_frame(frame.as_slice()),
            Err(FrameError::UnexpectedField {
                field: "instrument id",
                ..
            })
        ));
    }

    #[test]
    fn enforces_microsecond_bound_for_both_channels() {
        for (range, field) in [
            (10..14, "channel 1 microseconds"),
            (18..22, "channel 2 microseconds"),
        ] {
            let mut frame = valid_frame();
            frame[range].copy_from_slice(&1_000_000_u32.to_be_bytes());
            refresh_crc(&mut frame);
            assert!(matches!(
                decode_scientific_frame(frame.as_slice()),
                Err(FrameError::FieldOutOfRange {
                    field: actual_field,
                    actual: 1_000_000,
                    ..
                }) if actual_field == field
            ));
        }
    }
}
