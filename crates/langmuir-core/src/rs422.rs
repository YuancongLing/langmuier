use crate::{FRAME_HEADER, FrameError, Protocol, additive_checksum};

pub const RS422_FRAME_LEN: usize = 34;
pub const RS422_PAYLOAD_LEN: usize = 28;
pub const RS422_SENDER_ADDRESS: u8 = 0x55;
pub const RS422_RECEIVER_ADDRESS: u8 = 0xF0;
pub const RS422_DEVICE_ID: u8 = RS422_SENDER_ADDRESS;
pub const RS422_TELEMETRY_TYPE: u8 = 0x1A;

const LENGTH_OFFSET: usize = 4;
const PAYLOAD_OFFSET: usize = 6;
const CHECKSUM_OFFSET: usize = 33;
const PAYLOAD_LENGTH_FIELD: u16 = 0x001C;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MagneticField {
    pub x: u16,
    pub y: u16,
    pub z: u16,
}

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
    pub lvds_status: u8,
    pub last_injected_command: [u8; 9],
    pub reserved: [u8; 18],
}

impl Rs422Telemetry {
    #[must_use]
    pub const fn adc_status(&self) -> u8 {
        self.adc1_status
    }

    #[must_use]
    pub fn raw_type_hex(&self) -> String {
        format!("{:02X}{:02X}", self.instrument_id, self.command_type)
    }
}

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

    let declared_length = u16::from_be_bytes([frame[LENGTH_OFFSET], frame[LENGTH_OFFSET + 1]]);
    if declared_length != PAYLOAD_LENGTH_FIELD {
        return Err(FrameError::InvalidDeclaredLength {
            protocol: Protocol::Rs422,
            expected: PAYLOAD_LENGTH_FIELD,
            actual: declared_length,
        });
    }
    if frame[2] != RS422_SENDER_ADDRESS {
        return Err(FrameError::UnexpectedField {
            protocol: Protocol::Rs422,
            field: "sender address",
            expected: RS422_SENDER_ADDRESS,
            actual: frame[2],
        });
    }
    if frame[3] != RS422_RECEIVER_ADDRESS {
        return Err(FrameError::UnexpectedField {
            protocol: Protocol::Rs422,
            field: "receiver address",
            expected: RS422_RECEIVER_ADDRESS,
            actual: frame[3],
        });
    }

    let payload = &frame[PAYLOAD_OFFSET..CHECKSUM_OFFSET];
    if payload[0] != RS422_TELEMETRY_TYPE {
        return Err(FrameError::UnexpectedField {
            protocol: Protocol::Rs422,
            field: "telemetry type",
            expected: RS422_TELEMETRY_TYPE,
            actual: payload[0],
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
    last_injected_command[..5].copy_from_slice(&payload[22..27]);
    Ok(Rs422Telemetry {
        instrument_id: frame[2],
        command_type: payload[0],
        version_heartbeat: payload[1],
        absolute_time_seconds: u32::from_be_bytes(payload[4..8].try_into().unwrap()),
        local_time_seconds: u32::from_be_bytes(payload[8..12].try_into().unwrap()),
        work_mode: payload[12],
        scan_frequency: payload[13],
        scan_voltage: u16::from_be_bytes([payload[14], payload[15]]),
        accepted_command_count: payload[2],
        rejected_command_count: payload[3],
        cpu_temperature: payload[16],
        magnetic_field: MagneticField { x: 0, y: 0, z: 0 },
        pcb_temperature: 0,
        dds1_status: payload[17],
        dds2_status: 0,
        dac_status: payload[18],
        auxiliary_status: payload[21],
        adc1_status: payload[19],
        adc2_status: payload[20],
        lvds_status: payload[21],
        last_injected_command,
        reserved: [0_u8; 18],
    })
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::{
        CHECKSUM_OFFSET, PAYLOAD_OFFSET, RS422_FRAME_LEN, RS422_RECEIVER_ADDRESS,
        RS422_SENDER_ADDRESS, RS422_TELEMETRY_TYPE,
    };
    use crate::{FRAME_HEADER, additive_checksum};

    pub(crate) fn valid_frame() -> [u8; RS422_FRAME_LEN] {
        let mut frame = [0_u8; RS422_FRAME_LEN];
        frame[..2].copy_from_slice(&FRAME_HEADER);
        frame[2] = RS422_SENDER_ADDRESS;
        frame[3] = RS422_RECEIVER_ADDRESS;
        frame[4..6].copy_from_slice(&0x001C_u16.to_be_bytes());
        let payload = &mut frame[PAYLOAD_OFFSET..CHECKSUM_OFFSET];
        payload[0] = RS422_TELEMETRY_TYPE;
        payload[1] = 9;
        payload[2] = 201;
        payload[3] = 7;
        payload[4..8].copy_from_slice(&1_700_000_000_u32.to_be_bytes());
        payload[8..12].copy_from_slice(&123_456_u32.to_be_bytes());
        payload[12] = 0xAA;
        payload[13] = 100;
        payload[14..16].copy_from_slice(&5_000_u16.to_be_bytes());
        payload[16] = 55;
        payload[17] = 1;
        payload[18] = 2;
        payload[19] = 3;
        payload[20] = 4;
        payload[21] = 5;
        payload[22..27].copy_from_slice(&[1, 2, 3, 4, 5]);
        frame[CHECKSUM_OFFSET] = additive_checksum(payload);
        frame
    }
}

#[cfg(test)]
mod tests {
    use super::{RS422_FRAME_LEN, decode_rs422_telemetry, test_support::valid_frame};
    use crate::{FrameError, Protocol};

    #[test]
    fn decodes_protocol_v2_telemetry() {
        let decoded = decode_rs422_telemetry(&valid_frame()).unwrap();
        assert_eq!(decoded.instrument_id, 0x55);
        assert_eq!(decoded.command_type, 0x1A);
        assert_eq!(decoded.version_heartbeat, 9);
        assert_eq!(decoded.absolute_time_seconds, 1_700_000_000);
        assert_eq!(decoded.local_time_seconds, 123_456);
        assert_eq!(decoded.scan_frequency, 100);
        assert_eq!(decoded.scan_voltage, 5_000);
        assert_eq!(decoded.accepted_command_count, 201);
        assert_eq!(decoded.rejected_command_count, 7);
        assert_eq!(decoded.dds1_status, 1);
        assert_eq!(decoded.dac_status, 2);
        assert_eq!(decoded.adc1_status, 3);
        assert_eq!(decoded.adc2_status, 4);
        assert_eq!(decoded.lvds_status, 5);
        assert_eq!(&decoded.last_injected_command[..5], [1, 2, 3, 4, 5]);
    }

    #[test]
    fn rejects_invalid_length_and_fields() {
        let valid = valid_frame();
        for length in 0..RS422_FRAME_LEN {
            assert!(matches!(
                decode_rs422_telemetry(&valid[..length]),
                Err(FrameError::UnexpectedLength { actual, .. }) if actual == length
            ));
        }
        let mut frame = valid_frame();
        frame[5] = 0;
        assert!(matches!(
            decode_rs422_telemetry(&frame),
            Err(FrameError::InvalidDeclaredLength { .. })
        ));
        let mut frame = valid_frame();
        frame[2] = 0;
        assert!(matches!(
            decode_rs422_telemetry(&frame),
            Err(FrameError::UnexpectedField {
                field: "sender address",
                ..
            })
        ));
        let mut frame = valid_frame();
        frame[33] ^= 1;
        assert!(matches!(
            decode_rs422_telemetry(&frame),
            Err(FrameError::ChecksumMismatch {
                protocol: Protocol::Rs422,
                ..
            })
        ));
    }
}
