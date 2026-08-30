use crate::{FRAME_HEADER, additive_checksum};

pub const COMMAND_FRAME_LEN: usize = 12;
const COMMAND_LENGTH: [u8; 2] = [0x00, 0x06];
const SENDER_ADDRESS: u8 = 0xF0;
const RECEIVER_ADDRESS: u8 = 0x55;
const FILL_BYTE: u8 = 0x9B;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    RequestTelemetry,
    ConfigureFrequency(u8),
    ConfigureMode(u8),
    ConfigureVoltage(u16),
    SetLvdsEnabled(bool),
}

impl Command {
    #[must_use]
    pub fn encode(self) -> CommandFrame {
        let mut bytes = [FILL_BYTE; COMMAND_FRAME_LEN];
        bytes[..2].copy_from_slice(&FRAME_HEADER);
        bytes[2] = SENDER_ADDRESS;
        bytes[3] = RECEIVER_ADDRESS;
        bytes[4..6].copy_from_slice(&COMMAND_LENGTH);

        match self {
            Self::RequestTelemetry => bytes[6] = 0x11,
            Self::ConfigureFrequency(frequency) => {
                bytes[6] = 0x21;
                bytes[7] = 0x55;
                bytes[8] = frequency;
            }
            Self::ConfigureMode(mode) => {
                bytes[6] = mode;
            }
            Self::ConfigureVoltage(voltage) => {
                bytes[6] = 0xBB;
                bytes[7..9].copy_from_slice(&voltage.to_be_bytes());
            }
            Self::SetLvdsEnabled(enabled) => {
                bytes[6] = if enabled { 0x31 } else { 0xAA };
                if enabled {
                    bytes[7] = 0x55;
                }
            }
        }

        bytes[11] = additive_checksum(&bytes[6..11]);
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
}

impl AsRef<[u8]> for CommandFrame {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::{Command, FILL_BYTE};
    use crate::additive_checksum;

    #[test]
    fn telemetry_command_matches_protocol_v2() {
        assert_eq!(
            Command::RequestTelemetry.into_bytes(),
            [
                0xEB, 0x90, 0xF0, 0x55, 0x00, 0x06, 0x11, FILL_BYTE, FILL_BYTE, FILL_BYTE,
                FILL_BYTE, 0x7D,
            ]
        );
    }

    #[test]
    fn encodes_every_config_variant() {
        let cases = [
            (
                Command::ConfigureFrequency(100),
                [0x21, 0x55, 100, FILL_BYTE, FILL_BYTE],
            ),
            (
                Command::ConfigureMode(0xAA),
                [0xAA, FILL_BYTE, FILL_BYTE, FILL_BYTE, FILL_BYTE],
            ),
            (
                Command::ConfigureVoltage(0x1388),
                [0xBB, 0x13, 0x88, FILL_BYTE, FILL_BYTE],
            ),
            (
                Command::SetLvdsEnabled(true),
                [0x31, 0x55, FILL_BYTE, FILL_BYTE, FILL_BYTE],
            ),
            (
                Command::SetLvdsEnabled(false),
                [0xAA, FILL_BYTE, FILL_BYTE, FILL_BYTE, FILL_BYTE],
            ),
        ];

        for (command, expected) in cases {
            let frame = command.encode().into_bytes();
            assert_eq!(frame[..6], [0xEB, 0x90, 0xF0, 0x55, 0x00, 0x06]);
            assert_eq!(frame[6..11], expected);
            assert_eq!(frame[11], additive_checksum(&frame[6..11]));
        }
    }

    #[test]
    fn preserves_numeric_boundaries() {
        for value in [u8::MIN, u8::MAX] {
            assert_eq!(
                Command::ConfigureFrequency(value).encode().as_bytes()[8],
                value
            );
            assert_eq!(Command::ConfigureMode(value).encode().as_bytes()[6], value);
        }
        for value in [u16::MIN, u16::MAX] {
            assert_eq!(
                Command::ConfigureVoltage(value).encode().as_bytes()[7..9],
                value.to_be_bytes()
            );
        }
    }

    trait IntoBytes {
        fn into_bytes(self) -> [u8; 12];
    }

    impl IntoBytes for Command {
        fn into_bytes(self) -> [u8; 12] {
            self.encode().into_bytes()
        }
    }
}
