use crate::{FRAME_HEADER, FRAME_TAIL, FrameError, Protocol, additive_checksum};

pub const RS422_FRAME_LEN: usize = 64;
pub const RS422_PAYLOAD_LEN: usize = 58;
pub const RS422_DEVICE_ID: u8 = 0x04;
pub const RS422_TELEMETRY_TYPE: u8 = 0x12;

const LENGTH_OFFSET: usize = 2;
const PAYLOAD_LENGTH_BYTE: u8 = 58;
const PAYLOAD_OFFSET: usize = 3;
const CHECKSUM_OFFSET: usize = 61;
const TAIL_OFFSET: usize = 62;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MagneticField {
    pub x: u16,
    pub y: u16,
    pub z: u16,
}

/// Strongly typed representation of one valid 64-byte telemetry frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rs422Telemetry {
    pub instrument_id: u8,
    pub command_type: u8,
    pub version_heartbeat: u8,
    pub absolute_time_seconds: u32,
    pub local_time_seconds: u32,
    pub work_mode: u8,
    pub scan_frequency: u8,
    pub scan_voltage: u16,
    pub accepted_command_count: u8,
    pub rejected_command_count: u8,
    pub cpu_temperature: u8,
    pub magnetic_field: MagneticField,
    pub pcb_temperature: u8,
    pub dds1_status: u8,
    pub dds2_status: u8,
    pub dac_status: u8,
    pub auxiliary_status: u8,
    pub adc1_status: u8,
    pub adc2_status: u8,
    pub last_injected_command: [u8; 9],
    pub reserved: [u8; 18],
}

impl Rs422Telemetry {
    /// Compatibility alias used by the Python UI, where `adc_val` meant ADC1.
    #[must_use]
    pub const fn adc_status(&self) -> u8 {
        self.adc1_status
    }

    /// Instrument and command bytes rendered like Python's `raw_type` field.
    #[must_use]
    pub fn raw_type_hex(&self) -> String {
        format!("{:02X}{:02X}", self.instrument_id, self.command_type)
    }
}

/// Decodes and validates one complete RS422 telemetry frame.
///
/// # Errors
///
/// Returns [`FrameError`] when the input is not exactly one telemetry frame or
/// when any structural field, device/type discriminator, or checksum is invalid.
pub fn decode_rs422_telemetry(frame: &[u8]) -> Result<Rs422Telemetry, FrameError> {
    if frame.len() != RS422_FRAME_LEN {
        return Err(FrameError::UnexpectedLength {
            protocol: Protocol::Rs422,
            expected: RS422_FRAME_LEN,
            actual: frame.len(),
        });
    }
    if frame[..2] != FRAME_HEADER {
        return Err(FrameError::InvalidHeader {
            protocol: Protocol::Rs422,
            actual: [frame[0], frame[1]],
        });
    }
    if usize::from(frame[LENGTH_OFFSET]) != RS422_PAYLOAD_LEN {
        return Err(FrameError::InvalidDeclaredLength {
            protocol: Protocol::Rs422,
            expected: u16::from(PAYLOAD_LENGTH_BYTE),
            actual: u16::from(frame[LENGTH_OFFSET]),
        });
    }
    if frame[TAIL_OFFSET..] != FRAME_TAIL {
        return Err(FrameError::InvalidTail {
            protocol: Protocol::Rs422,
            actual: [frame[TAIL_OFFSET], frame[TAIL_OFFSET + 1]],
        });
    }

    let payload = &frame[PAYLOAD_OFFSET..CHECKSUM_OFFSET];
    if payload[0] != RS422_DEVICE_ID {
        return Err(FrameError::UnexpectedField {
            protocol: Protocol::Rs422,
            field: "instrument id",
            expected: RS422_DEVICE_ID,
            actual: payload[0],
        });
    }
    if payload[1] != RS422_TELEMETRY_TYPE {
        return Err(FrameError::UnexpectedField {
            protocol: Protocol::Rs422,
            field: "telemetry type",
            expected: RS422_TELEMETRY_TYPE,
            actual: payload[1],
        });
    }
    let expected_checksum = additive_checksum(payload);
    let actual_checksum = frame[CHECKSUM_OFFSET];
    if actual_checksum != expected_checksum {
        return Err(FrameError::ChecksumMismatch {
            protocol: Protocol::Rs422,
            expected: u16::from(expected_checksum),
            actual: u16::from(actual_checksum),
        });
    }

    let mut last_injected_command = [0_u8; 9];
    last_injected_command.copy_from_slice(&payload[31..40]);
    let mut reserved = [0_u8; 18];
    reserved.copy_from_slice(&payload[40..58]);

    Ok(Rs422Telemetry {
        instrument_id: payload[0],
        command_type: payload[1],
        version_heartbeat: payload[2],
        absolute_time_seconds: u32::from_be_bytes([payload[3], payload[4], payload[5], payload[6]]),
        local_time_seconds: u32::from_be_bytes([payload[7], payload[8], payload[9], payload[10]]),
        work_mode: payload[11],
        scan_frequency: payload[12],
        scan_voltage: u16::from_be_bytes([payload[13], payload[14]]),
        accepted_command_count: payload[15],
        rejected_command_count: payload[16],
        cpu_temperature: payload[17],
        magnetic_field: MagneticField {
            x: u16::from_be_bytes([payload[18], payload[19]]),
            y: u16::from_be_bytes([payload[20], payload[21]]),
            z: u16::from_be_bytes([payload[22], payload[23]]),
        },
        pcb_temperature: payload[24],
        dds1_status: payload[25],
        dds2_status: payload[26],
        dac_status: payload[27],
        auxiliary_status: payload[28],
        adc1_status: payload[29],
        adc2_status: payload[30],
        last_injected_command,
        reserved,
    })
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::{CHECKSUM_OFFSET, PAYLOAD_LENGTH_BYTE, PAYLOAD_OFFSET, RS422_FRAME_LEN};
    use crate::{FRAME_HEADER, FRAME_TAIL, additive_checksum};

    pub(crate) fn valid_frame() -> [u8; RS422_FRAME_LEN] {
        let mut frame = [0_u8; RS422_FRAME_LEN];
        frame[..2].copy_from_slice(&FRAME_HEADER);
        frame[2] = PAYLOAD_LENGTH_BYTE;
        let payload = &mut frame[PAYLOAD_OFFSET..CHECKSUM_OFFSET];
        payload[0] = 0x04;
        payload[1] = 0x12;
        payload[2] = 0x91;
        payload[3..7].copy_from_slice(&1_700_000_000_u32.to_be_bytes());
        payload[7..11].copy_from_slice(&123_456_u32.to_be_bytes());
        payload[11] = 0xAA;
        payload[12] = 100;
        payload[13..15].copy_from_slice(&5_000_u16.to_be_bytes());
        payload[15] = 201;
        payload[16] = 7;
        payload[17] = 55;
        payload[18..20].copy_from_slice(&0x1234_u16.to_be_bytes());
        payload[20..22].copy_from_slice(&0xABCD_u16.to_be_bytes());
        payload[22..24].copy_from_slice(&0xFFFF_u16.to_be_bytes());
        payload[24] = 44;
        payload[25..31].copy_from_slice(&[1, 2, 1, 0, 1, 2]);
        payload[31..40].copy_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8, 9]);
        payload[40..58].fill(0x9B);
        frame[CHECKSUM_OFFSET] = additive_checksum(&frame[PAYLOAD_OFFSET..CHECKSUM_OFFSET]);
        frame[62..].copy_from_slice(&FRAME_TAIL);
        frame
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CHECKSUM_OFFSET, PAYLOAD_OFFSET, RS422_FRAME_LEN, decode_rs422_telemetry,
        test_support::valid_frame,
    };
    use crate::{FrameError, Protocol, additive_checksum};

    #[test]
    fn decodes_all_fields() {
        let decoded = decode_rs422_telemetry(&valid_frame()).unwrap();
        assert_eq!(decoded.instrument_id, 0x04);
        assert_eq!(decoded.command_type, 0x12);
        assert_eq!(decoded.version_heartbeat, 0x91);
        assert_eq!(decoded.absolute_time_seconds, 1_700_000_000);
        assert_eq!(decoded.local_time_seconds, 123_456);
        assert_eq!(decoded.work_mode, 0xAA);
        assert_eq!(decoded.scan_frequency, 100);
        assert_eq!(decoded.scan_voltage, 5_000);
        assert_eq!(decoded.accepted_command_count, 201);
        assert_eq!(decoded.rejected_command_count, 7);
        assert_eq!(decoded.cpu_temperature, 55);
        assert_eq!(decoded.magnetic_field.x, 0x1234);
        assert_eq!(decoded.magnetic_field.y, 0xABCD);
        assert_eq!(decoded.magnetic_field.z, 0xFFFF);
        assert_eq!(decoded.pcb_temperature, 44);
        assert_eq!(decoded.dds1_status, 1);
        assert_eq!(decoded.dds2_status, 2);
        assert_eq!(decoded.dac_status, 1);
        assert_eq!(decoded.auxiliary_status, 0);
        assert_eq!(decoded.adc1_status, 1);
        assert_eq!(decoded.adc2_status, 2);
        assert_eq!(decoded.adc_status(), 1);
        assert_eq!(decoded.last_injected_command, [1, 2, 3, 4, 5, 6, 7, 8, 9]);
        assert_eq!(decoded.reserved, [0x9B; 18]);
        assert_eq!(decoded.raw_type_hex(), "0412");
    }

    #[test]
    fn rejects_every_wrong_exact_length() {
        let valid = valid_frame();
        for length in 0..RS422_FRAME_LEN {
            assert!(matches!(
                decode_rs422_telemetry(&valid[..length]),
                Err(FrameError::UnexpectedLength { actual, .. }) if actual == length
            ));
        }
        let mut oversized = valid.to_vec();
        oversized.push(0);
        assert!(matches!(
            decode_rs422_telemetry(&oversized),
            Err(FrameError::UnexpectedLength { actual: 65, .. })
        ));
    }

    #[test]
    fn rejects_corrupt_structure_and_checksum() {
        let mut frame = valid_frame();
        frame[0] = 0;
        assert!(matches!(
            decode_rs422_telemetry(&frame),
            Err(FrameError::InvalidHeader {
                protocol: Protocol::Rs422,
                ..
            })
        ));

        let mut frame = valid_frame();
        frame[2] = 57;
        assert!(matches!(
            decode_rs422_telemetry(&frame),
            Err(FrameError::InvalidDeclaredLength { actual: 57, .. })
        ));

        let mut frame = valid_frame();
        frame[63] = 0;
        assert!(matches!(
            decode_rs422_telemetry(&frame),
            Err(FrameError::InvalidTail {
                protocol: Protocol::Rs422,
                ..
            })
        ));

        let mut frame = valid_frame();
        frame[20] ^= 1;
        assert!(matches!(
            decode_rs422_telemetry(&frame),
            Err(FrameError::ChecksumMismatch {
                protocol: Protocol::Rs422,
                ..
            })
        ));
    }

    #[test]
    fn rejects_wrong_instrument_and_telemetry_type() {
        for (offset, field) in [(3, "instrument id"), (4, "telemetry type")] {
            let mut frame = valid_frame();
            frame[offset] ^= 1;
            frame[CHECKSUM_OFFSET] = additive_checksum(&frame[PAYLOAD_OFFSET..CHECKSUM_OFFSET]);
            assert!(matches!(
                decode_rs422_telemetry(&frame),
                Err(FrameError::UnexpectedField {
                    protocol: Protocol::Rs422,
                    field: actual_field,
                    ..
                }) if actual_field == field
            ));
        }
    }
}
