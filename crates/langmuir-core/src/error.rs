/// Wire protocol associated with a decode error or metric.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protocol {
    Rs422,
    Lvds,
}

impl std::fmt::Display for Protocol {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Rs422 => formatter.write_str("RS422"),
            Self::Lvds => formatter.write_str("LVDS"),
        }
    }
}

/// A precise reason why a complete frame candidate was rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FrameError {
    UnexpectedLength {
        protocol: Protocol,
        expected: usize,
        actual: usize,
    },
    InvalidHeader {
        protocol: Protocol,
        actual: [u8; 2],
    },
    InvalidDeclaredLength {
        protocol: Protocol,
        expected: u16,
        actual: u16,
    },
    InvalidTail {
        protocol: Protocol,
        actual: [u8; 2],
    },
    UnexpectedField {
        protocol: Protocol,
        field: &'static str,
        expected: u8,
        actual: u8,
    },
    ChecksumMismatch {
        protocol: Protocol,
        expected: u16,
        actual: u16,
    },
    FieldOutOfRange {
        protocol: Protocol,
        field: &'static str,
        actual: u64,
        valid_range: &'static str,
    },
}

impl std::fmt::Display for FrameError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnexpectedLength {
                protocol,
                expected,
                actual,
            } => write!(
                formatter,
                "{protocol} frame length must be {expected} bytes, got {actual}"
            ),
            Self::InvalidHeader { protocol, actual } => write!(
                formatter,
                "{protocol} frame header must be EB 90, got {actual:02X?}"
            ),
            Self::InvalidDeclaredLength {
                protocol,
                expected,
                actual,
            } => write!(
                formatter,
                "{protocol} declared payload length must be {expected} bytes, got {actual}"
            ),
            Self::InvalidTail { protocol, actual } => write!(
                formatter,
                "{protocol} frame tail must be 09 D7, got {actual:02X?}"
            ),
            Self::UnexpectedField {
                protocol,
                field,
                expected,
                actual,
            } => write!(
                formatter,
                "{protocol} {field} must be 0x{expected:02X}, got 0x{actual:02X}"
            ),
            Self::ChecksumMismatch {
                protocol,
                expected,
                actual,
            } => write!(
                formatter,
                "{protocol} checksum mismatch: frame contains 0x{actual:04X}, calculated 0x{expected:04X}"
            ),
            Self::FieldOutOfRange {
                protocol,
                field,
                actual,
                valid_range,
            } => write!(
                formatter,
                "{protocol} {field} value {actual} is outside {valid_range}"
            ),
        }
    }
}

impl std::error::Error for FrameError {}
