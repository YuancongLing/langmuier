use std::fmt;

use crate::{FRAME_HEADER, additive_checksum};

pub const COMMAND_FRAME_LEN: usize = 12;
const COMMAND_LENGTH: [u8; 2] = [0x00, 0x06];
const SENDER_ADDRESS: u8 = 0xF0;
const RECEIVER_ADDRESS: u8 = 0x55;
const FILL_BYTE: u8 = 0x9B;
pub const MAX_SCAN_FREQUENCY: u8 = 0x64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandError {
    ScanFrequencyOutOfRange { value: u8 },
}

impl fmt::Display for CommandError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ScanFrequencyOutOfRange { value } => write!(
                formatter,
                "scan frequency 0x{value:02X} is outside the command range 0x00..=0x{MAX_SCAN_FREQUENCY:02X}"
            ),
        }
    }
}

impl std::error::Error for CommandError {}

/// A structural or semantic error in a manually edited 12-byte command frame.
///
/// Typed [`Command`] values cannot produce these errors: the encoder emits a
/// complete protocol frame. This error is intended for the expert/raw editor,
/// where every byte is user controlled and must be checked before sending.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandFrameError {
    InvalidHeader {
        actual: [u8; 2],
    },
    InvalidSenderAddress {
        actual: u8,
    },
    InvalidReceiverAddress {
        actual: u8,
    },
    InvalidDeclaredLength {
        actual: u16,
    },
    UnsupportedType {
        actual: u8,
    },
    InvalidDataByte {
        field: &'static str,
        actual: u8,
        expected: &'static str,
    },
    ScanFrequencyOutOfRange {
        value: u8,
    },
    ChecksumMismatch {
        expected: u8,
        actual: u8,
    },
}

impl fmt::Display for CommandFrameError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidHeader { actual } => {
                write!(formatter, "command header must be EB 90, got {actual:02X?}")
            }
            Self::InvalidSenderAddress { actual } => {
                write!(
                    formatter,
                    "command sender address must be F0, got 0x{actual:02X}"
                )
            }
            Self::InvalidReceiverAddress { actual } => {
                write!(
                    formatter,
                    "command receiver address must be 55, got 0x{actual:02X}"
                )
            }
            Self::InvalidDeclaredLength { actual } => {
                write!(
                    formatter,
                    "command declared length must be 0x0006, got 0x{actual:04X}"
                )
            }
            Self::UnsupportedType { actual } => {
                write!(formatter, "unsupported command TYPE 0x{actual:02X}")
            }
            Self::InvalidDataByte {
                field,
                actual,
                expected,
            } => write!(formatter, "{field} must be {expected}, got 0x{actual:02X}"),
            Self::ScanFrequencyOutOfRange { value } => write!(
                formatter,
                "scan frequency 0x{value:02X} is outside 0x00..=0x{MAX_SCAN_FREQUENCY:02X}"
            ),
            Self::ChecksumMismatch { expected, actual } => write!(
                formatter,
                "command SUM mismatch: frame contains 0x{actual:02X}, calculated 0x{expected:02X}"
            ),
        }
    }
}

impl std::error::Error for CommandFrameError {}

/// A validated scan-frequency setting from the command table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScanFrequency(u8);

impl ScanFrequency {
    pub const MIN: u8 = 0x00;
    pub const MAX: u8 = MAX_SCAN_FREQUENCY;

    /// Creates a frequency after checking the command-table range.
    ///
    /// # Errors
    ///
    /// Returns [`CommandError::ScanFrequencyOutOfRange`] when `value` exceeds
    /// the command-table maximum of `0x64`.
    pub const fn new(value: u8) -> Result<Self, CommandError> {
        if value <= Self::MAX {
            Ok(Self(value))
        } else {
            Err(CommandError::ScanFrequencyOutOfRange { value })
        }
    }

    #[must_use]
    pub const fn value(self) -> u8 {
        self.0
    }
}

impl TryFrom<u8> for ScanFrequency {
    type Error = CommandError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    RequestTelemetry,
    ConfigureTriangle(ScanFrequency),
    ConfigureSine(ScanFrequency),
    ConfigureDc(u16),
    SetLvdsEnabled(bool),
}

impl Command {
    /// Builds a triangle-wave command after validating its scan frequency.
    ///
    /// # Errors
    ///
    /// Returns [`CommandError::ScanFrequencyOutOfRange`] when `frequency` is
    /// greater than `0x64`.
    pub fn configure_triangle(frequency: u8) -> Result<Self, CommandError> {
        ScanFrequency::new(frequency).map(Self::ConfigureTriangle)
    }

    /// Builds a sine-wave command after validating its scan frequency.
    ///
    /// # Errors
    ///
    /// Returns [`CommandError::ScanFrequencyOutOfRange`] when `frequency` is
    /// greater than `0x64`.
    pub fn configure_sine(frequency: u8) -> Result<Self, CommandError> {
        ScanFrequency::new(frequency).map(Self::ConfigureSine)
    }

    #[must_use]
    pub fn encode(self) -> CommandFrame {
        let mut bytes = [FILL_BYTE; COMMAND_FRAME_LEN];
        bytes[..2].copy_from_slice(&FRAME_HEADER);
        bytes[2] = SENDER_ADDRESS;
        bytes[3] = RECEIVER_ADDRESS;
        bytes[4..6].copy_from_slice(&COMMAND_LENGTH);

        match self {
            Self::RequestTelemetry => bytes[6] = 0x11,
            Self::ConfigureTriangle(frequency) => {
                bytes[6] = 0x21;
                bytes[7] = 0x55;
                bytes[8] = frequency.value();
            }
            Self::ConfigureSine(frequency) => {
                bytes[6] = 0x21;
                bytes[7] = 0xAA;
                bytes[8] = frequency.value();
            }
            Self::ConfigureDc(voltage) => {
                bytes[6] = 0x21;
                bytes[7] = 0xBB;
                bytes[8..10].copy_from_slice(&voltage.to_be_bytes());
                bytes[10] = FILL_BYTE;
            }
            Self::SetLvdsEnabled(enabled) => {
                bytes[6] = 0x31;
                bytes[7] = if enabled { 0x55 } else { 0xAA };
            }
        }

        bytes[11] = command_checksum(&bytes);
        CommandFrame(bytes)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandFrame([u8; COMMAND_FRAME_LEN]);

impl CommandFrame {
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; COMMAND_FRAME_LEN] {
        &self.0
    }

    #[must_use]
    pub const fn into_bytes(self) -> [u8; COMMAND_FRAME_LEN] {
        self.0
    }

    /// Returns the SUM calculated from TYPE and DATA1..DATA4.
    #[must_use]
    pub fn expected_checksum(bytes: &[u8; COMMAND_FRAME_LEN]) -> u8 {
        command_checksum(bytes)
    }

    /// Recalculates only the SUM byte in a raw command frame.
    ///
    /// Header, addresses, length, TYPE, and DATA bytes are intentionally left
    /// untouched so an expert editor can inspect and correct structural errors
    /// separately.
    pub fn recalculate_checksum(bytes: &mut [u8; COMMAND_FRAME_LEN]) {
        bytes[11] = command_checksum(bytes);
    }
}

impl AsRef<[u8]> for CommandFrame {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

/// Computes the command SUM from TYPE and DATA1..DATA4.
///
/// This is the single checksum source used by both the typed encoder and the
/// raw-frame editor. The caller must provide a 12-byte command-shaped array;
/// fixed-field validation is performed by [`validate_command_frame`].
#[must_use]
pub fn command_checksum(bytes: &[u8; COMMAND_FRAME_LEN]) -> u8 {
    additive_checksum(&bytes[6..11])
}

/// Validates a manually edited RS422 command frame.
///
/// The protocol uses one fixed 12-byte layout. In addition to the header,
/// addresses, and declared payload length, this checks that TYPE/DATA bytes
/// describe one of the documented telemetry, waveform/DC, or LVDS commands,
/// and that SUM matches TYPE + DATA1..DATA4 modulo 256.
///
/// # Errors
///
/// Returns [`CommandFrameError`] when a fixed field, TYPE/DATA combination, or
/// checksum is invalid.
pub fn validate_command_frame(bytes: &[u8; COMMAND_FRAME_LEN]) -> Result<(), CommandFrameError> {
    if bytes[..2] != FRAME_HEADER {
        return Err(CommandFrameError::InvalidHeader {
            actual: [bytes[0], bytes[1]],
        });
    }
    if bytes[2] != SENDER_ADDRESS {
        return Err(CommandFrameError::InvalidSenderAddress { actual: bytes[2] });
    }
    if bytes[3] != RECEIVER_ADDRESS {
        return Err(CommandFrameError::InvalidReceiverAddress { actual: bytes[3] });
    }
    let declared_length = u16::from_be_bytes([bytes[4], bytes[5]]);
    if declared_length != 0x0006 {
        return Err(CommandFrameError::InvalidDeclaredLength {
            actual: declared_length,
        });
    }

    match bytes[6] {
        0x11 => {
            require_fill_byte("DATA1", bytes[7])?;
            require_fill_byte("DATA2", bytes[8])?;
            require_fill_byte("DATA3", bytes[9])?;
            require_fill_byte("DATA4", bytes[10])?;
        }
        0x21 => match bytes[7] {
            0x55 | 0xAA => {
                if bytes[8] > MAX_SCAN_FREQUENCY {
                    return Err(CommandFrameError::ScanFrequencyOutOfRange { value: bytes[8] });
                }
                require_fill_byte("DATA3", bytes[9])?;
                require_fill_byte("DATA4", bytes[10])?;
            }
            0xBB => {
                // DATA2/DATA3 are the big-endian raw voltage value. The
                // protocol does not define a narrower downlink range.
                require_fill_byte("DATA4", bytes[10])?;
            }
            actual => {
                return Err(CommandFrameError::InvalidDataByte {
                    field: "DATA1",
                    actual,
                    expected: "0x55, 0xAA, or 0xBB",
                });
            }
        },
        0x31 => {
            if !matches!(bytes[7], 0x55 | 0xAA) {
                return Err(CommandFrameError::InvalidDataByte {
                    field: "DATA1",
                    actual: bytes[7],
                    expected: "0x55 or 0xAA",
                });
            }
            require_fill_byte("DATA2", bytes[8])?;
            require_fill_byte("DATA3", bytes[9])?;
            require_fill_byte("DATA4", bytes[10])?;
        }
        actual => return Err(CommandFrameError::UnsupportedType { actual }),
    }

    let expected = command_checksum(bytes);
    if bytes[11] != expected {
        return Err(CommandFrameError::ChecksumMismatch {
            expected,
            actual: bytes[11],
        });
    }
    Ok(())
}

fn require_fill_byte(field: &'static str, actual: u8) -> Result<(), CommandFrameError> {
    if actual == FILL_BYTE {
        Ok(())
    } else {
        Err(CommandFrameError::InvalidDataByte {
            field,
            actual,
            expected: "0x9B",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Command, CommandError, CommandFrame, CommandFrameError, ScanFrequency, command_checksum,
        validate_command_frame,
    };

    const TELEMETRY: [u8; 12] = [
        0xEB, 0x90, 0xF0, 0x55, 0x00, 0x06, 0x11, 0x9B, 0x9B, 0x9B, 0x9B, 0x7D,
    ];
    const TRIANGLE_50: [u8; 12] = [
        0xEB, 0x90, 0xF0, 0x55, 0x00, 0x06, 0x21, 0x55, 0x32, 0x9B, 0x9B, 0xDE,
    ];
    const SINE_50: [u8; 12] = [
        0xEB, 0x90, 0xF0, 0x55, 0x00, 0x06, 0x21, 0xAA, 0x32, 0x9B, 0x9B, 0x33,
    ];
    const DC_1000: [u8; 12] = [
        0xEB, 0x90, 0xF0, 0x55, 0x00, 0x06, 0x21, 0xBB, 0x03, 0xE8, 0x9B, 0x62,
    ];
    const LVDS_ON: [u8; 12] = [
        0xEB, 0x90, 0xF0, 0x55, 0x00, 0x06, 0x31, 0x55, 0x9B, 0x9B, 0x9B, 0x57,
    ];
    const LVDS_OFF: [u8; 12] = [
        0xEB, 0x90, 0xF0, 0x55, 0x00, 0x06, 0x31, 0xAA, 0x9B, 0x9B, 0x9B, 0xAC,
    ];

    #[test]
    fn encodes_all_protocol_golden_vectors() {
        assert_eq!(Command::RequestTelemetry.encode().into_bytes(), TELEMETRY);
        assert_eq!(
            Command::configure_triangle(50)
                .unwrap()
                .encode()
                .into_bytes(),
            TRIANGLE_50
        );
        assert_eq!(
            Command::configure_sine(50).unwrap().encode().into_bytes(),
            SINE_50
        );
        assert_eq!(Command::ConfigureDc(1_000).encode().into_bytes(), DC_1000);
        assert_eq!(Command::SetLvdsEnabled(true).encode().into_bytes(), LVDS_ON);
        assert_eq!(
            Command::SetLvdsEnabled(false).encode().into_bytes(),
            LVDS_OFF
        );
    }

    #[test]
    fn validates_scan_frequency_boundaries() {
        assert!(ScanFrequency::new(0).is_ok());
        assert!(ScanFrequency::new(0x64).is_ok());
        assert_eq!(
            ScanFrequency::new(0x65),
            Err(CommandError::ScanFrequencyOutOfRange { value: 0x65 })
        );
        assert_eq!(
            Command::configure_triangle(0xFF),
            Err(CommandError::ScanFrequencyOutOfRange { value: 0xFF })
        );
        assert_eq!(
            Command::configure_sine(0x65),
            Err(CommandError::ScanFrequencyOutOfRange { value: 0x65 })
        );
    }

    #[test]
    fn encodes_dc_voltage_as_big_endian_without_inventing_a_unit_conversion() {
        let frame = Command::ConfigureDc(0x1234).encode().into_bytes();
        assert_eq!(&frame[6..11], &[0x21, 0xBB, 0x12, 0x34, 0x9B]);
    }

    #[test]
    fn command_modes_have_distinct_frames() {
        let sine = Command::configure_sine(50).unwrap().encode().into_bytes();
        let lvds_off = Command::SetLvdsEnabled(false).encode().into_bytes();
        assert_ne!(sine, lvds_off);
    }

    #[test]
    fn encoded_frames_pass_raw_frame_validation() {
        for frame in [TELEMETRY, TRIANGLE_50, SINE_50, DC_1000, LVDS_ON, LVDS_OFF] {
            assert_eq!(validate_command_frame(&frame), Ok(()));
            assert_eq!(frame[11], command_checksum(&frame));
        }
    }

    #[test]
    fn raw_checksum_error_is_visible_until_explicit_recalculation() {
        let mut frame = TRIANGLE_50;
        frame[11] ^= 0x01;
        assert_eq!(
            validate_command_frame(&frame),
            Err(CommandFrameError::ChecksumMismatch {
                expected: TRIANGLE_50[11],
                actual: TRIANGLE_50[11] ^ 0x01,
            })
        );

        let before_data = frame[..11].to_owned();
        CommandFrame::recalculate_checksum(&mut frame);
        assert_eq!(&frame[..11], &before_data);
        assert_eq!(validate_command_frame(&frame), Ok(()));
    }

    #[test]
    fn raw_data_change_with_stale_checksum_is_rejected() {
        let mut frame = TRIANGLE_50;
        let original_checksum = frame[11];
        frame[8] = 0x33;

        assert_eq!(
            validate_command_frame(&frame),
            Err(CommandFrameError::ChecksumMismatch {
                expected: command_checksum(&frame),
                actual: original_checksum,
            })
        );

        CommandFrame::recalculate_checksum(&mut frame);
        assert_eq!(validate_command_frame(&frame), Ok(()));
    }

    #[test]
    fn raw_validation_rejects_structure_and_data_errors() {
        let mut bad_header = TELEMETRY;
        bad_header[0] = 0;
        assert_eq!(
            validate_command_frame(&bad_header),
            Err(CommandFrameError::InvalidHeader { actual: [0, 0x90] })
        );

        let mut bad_length = TELEMETRY;
        bad_length[5] = 0x05;
        assert_eq!(
            validate_command_frame(&bad_length),
            Err(CommandFrameError::InvalidDeclaredLength { actual: 5 })
        );

        let mut bad_frequency = TRIANGLE_50;
        bad_frequency[8] = 0x65;
        CommandFrame::recalculate_checksum(&mut bad_frequency);
        assert_eq!(
            validate_command_frame(&bad_frequency),
            Err(CommandFrameError::ScanFrequencyOutOfRange { value: 0x65 })
        );

        let mut bad_data = LVDS_ON;
        bad_data[8] = 0;
        CommandFrame::recalculate_checksum(&mut bad_data);
        assert_eq!(
            validate_command_frame(&bad_data),
            Err(CommandFrameError::InvalidDataByte {
                field: "DATA2",
                actual: 0,
                expected: "0x9B",
            })
        );
    }
}
