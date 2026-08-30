use crate::{FRAME_HEADER, FRAME_TAIL, additive_checksum};

pub const COMMAND_FRAME_LEN: usize = 16;
const COMMAND_PAYLOAD_LEN: u8 = 10;
const FILL_BYTE: u8 = 0x9B;
const DEVICE_ID: u8 = 0x04;

/// All command variants exposed by the Python control panel.
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
        bytes[2] = COMMAND_PAYLOAD_LEN;
        bytes[3] = DEVICE_ID;
        bytes[14..].copy_from_slice(&FRAME_TAIL);

        match self {
            Self::RequestTelemetry => bytes[4] = 0x11,
            Self::ConfigureFrequency(frequency) => {
                bytes[4] = 0x22;
                bytes[5] = 0x55;
                bytes[6] = frequency;
            }
            Self::ConfigureMode(mode) => {
                bytes[4] = 0x22;
                bytes[5] = 0xAA;
                bytes[6] = mode;
            }
            Self::ConfigureVoltage(voltage) => {
                bytes[4] = 0x22;
                bytes[5] = 0xBB;
                bytes[6..8].copy_from_slice(&voltage.to_be_bytes());
            }
            Self::SetLvdsEnabled(enabled) => {
                bytes[4] = 0x33;
                bytes[5] = if enabled { 0x55 } else { 0xAA };
            }
        }

        bytes[13] = additive_checksum(&bytes[3..13]);
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
    fn telemetry_command_matches_python_default() {
        assert_eq!(
            Command::RequestTelemetry.into_bytes(),
            [
                0xEB, 0x90, 0x0A, 0x04, 0x11, 0x9B, 0x9B, 0x9B, 0x9B, 0x9B, 0x9B, 0x9B, 0x9B, 0xED,
                0x09, 0xD7,
            ]
        );
    }

    #[test]
    fn encodes_every_config_variant() {
        let cases = [
            (
                Command::ConfigureFrequency(100),
                [0x22, 0x55, 100, FILL_BYTE],
            ),
            (Command::ConfigureMode(0xAA), [0x22, 0xAA, 0xAA, FILL_BYTE]),
            (Command::ConfigureVoltage(0x1388), [0x22, 0xBB, 0x13, 0x88]),
            (
                Command::SetLvdsEnabled(true),
                [0x33, 0x55, FILL_BYTE, FILL_BYTE],
            ),
            (
                Command::SetLvdsEnabled(false),
                [0x33, 0xAA, FILL_BYTE, FILL_BYTE],
            ),
        ];

        for (command, expected) in cases {
            let frame = command.encode().into_bytes();
            assert_eq!(frame[..4], [0xEB, 0x90, 0x0A, 0x04]);
            assert_eq!(frame[4..8], expected);
            assert_eq!(frame[13], additive_checksum(&frame[3..13]));
            assert_eq!(frame[14..], [0x09, 0xD7]);
        }
    }

    #[test]
    fn preserves_numeric_boundaries() {
        for value in [u8::MIN, u8::MAX] {
            assert_eq!(
                Command::ConfigureFrequency(value).encode().as_bytes()[6],
                value
            );
            assert_eq!(Command::ConfigureMode(value).encode().as_bytes()[6], value);
        }
        for value in [u16::MIN, u16::MAX] {
            assert_eq!(
                Command::ConfigureVoltage(value).encode().as_bytes()[6..8],
                value.to_be_bytes()
            );
        }
    }

    trait IntoBytes {
        fn into_bytes(self) -> [u8; 16];
    }

    impl IntoBytes for Command {
        fn into_bytes(self) -> [u8; 16] {
            self.encode().into_bytes()
        }
    }
}
