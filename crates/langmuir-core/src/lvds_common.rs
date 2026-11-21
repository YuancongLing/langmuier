//! Shared LVDS wire structure, before any type-specific business decoding.

use crate::{
    FRAME_HEADER, FrameError, LVDS_FRAME_LEN, LVDS_PAYLOAD_LEN, LVDS_RECEIVER_ADDRESS,
    LVDS_SENDER_ADDRESS, MAX_RECOVERY_PAYLOAD_BYTES, Protocol,
};

pub(crate) const COMMON_HEADER: [u8; 6] = [0xEB, 0x90, 0x55, 0xAA, 0x01, 0x07];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LvdsPacketType {
    Science,
    Excitation,
    GnssAttitude,
    Unknown(u8),
}

impl LvdsPacketType {
    #[must_use]
    pub const fn code(self) -> u8 {
        match self {
            Self::Science => 0xAA,
            Self::Excitation => 0xAB,
            Self::GnssAttitude => 0xAC,
            Self::Unknown(code) => code,
        }
    }
}

/// An intact, structurally valid 269-byte wire frame. Its DATA and CRC have not
/// been validated or decoded; even an unknown TYPE retains its original bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LvdsCommonFrame {
    raw: [u8; LVDS_FRAME_LEN],
}

impl LvdsCommonFrame {
    #[must_use]
    pub const fn raw_bytes(&self) -> &[u8; LVDS_FRAME_LEN] {
        &self.raw
    }

    #[must_use]
    pub fn data(&self) -> &[u8; 260] {
        &self.raw[7..267].as_chunks::<260>().0[0]
    }

    #[must_use]
    pub const fn raw_crc(&self) -> [u8; 2] {
        [self.raw[267], self.raw[268]]
    }

    #[must_use]
    pub const fn packet_type(&self) -> LvdsPacketType {
        match self.raw[6] {
            0xAA => LvdsPacketType::Science,
            0xAB => LvdsPacketType::Excitation,
            0xAC => LvdsPacketType::GnssAttitude,
            code => LvdsPacketType::Unknown(code),
        }
    }

    #[must_use]
    pub const fn src(&self) -> u8 {
        self.raw[2]
    }

    #[must_use]
    pub const fn dst(&self) -> u8 {
        self.raw[3]
    }

    #[must_use]
    pub const fn frame_len(&self) -> usize {
        self.raw.len()
    }
}

/// Checks only fixed public framing. Unknown TYPE values are valid structures
/// for caller-specific routing and statistics. CRC bytes are never evaluated.
///
/// # Errors
///
/// Returns a framing error for a length other than 269, a marker other than
/// EB90, an address other than SRC55/DSTAA, or LEN other than 0107.
pub fn parse_common_frame(frame: &[u8]) -> Result<LvdsCommonFrame, FrameError> {
    let raw: [u8; LVDS_FRAME_LEN] = frame.try_into().map_err(|_| FrameError::UnexpectedLength {
        protocol: Protocol::Lvds,
        expected: LVDS_FRAME_LEN,
        actual: frame.len(),
    })?;
    if raw[..2] != FRAME_HEADER {
        return Err(FrameError::InvalidHeader {
            protocol: Protocol::Lvds,
            actual: [raw[0], raw[1]],
        });
    }
    let declared_length = u16::from_be_bytes([raw[4], raw[5]]);
    if usize::from(declared_length) != LVDS_PAYLOAD_LEN {
        return Err(FrameError::InvalidDeclaredLength {
            protocol: Protocol::Lvds,
            expected: 0x0107,
            actual: declared_length,
        });
    }
    for (field, actual, expected) in [
        ("sender address", raw[2], LVDS_SENDER_ADDRESS),
        ("receiver address", raw[3], LVDS_RECEIVER_ADDRESS),
    ] {
        if actual != expected {
            return Err(FrameError::UnexpectedField {
                protocol: Protocol::Lvds,
                field,
                expected,
                actual,
            });
        }
    }
    Ok(LvdsCommonFrame { raw })
}

/// Finds one complete public frame inside this one bounded payload. An exact
/// native frame, including unknown TYPE, takes priority. Byte synchronization
/// accepts only AA/AB/AC and requires a unique full fixed-field candidate.
/// No neighboring datagram or missing byte is consulted.
#[must_use]
pub fn find_common_frame(payload: &[u8]) -> Option<(usize, LvdsCommonFrame)> {
    if payload.len() > MAX_RECOVERY_PAYLOAD_BYTES {
        return None;
    }
    if let Ok(frame) = parse_common_frame(payload) {
        return Some((0, frame));
    }
    let mut found = None;
    for (offset, bytes) in payload.windows(LVDS_FRAME_LEN).enumerate() {
        if !bytes.starts_with(&COMMON_HEADER) {
            continue;
        }
        let Ok(frame) = parse_common_frame(bytes) else {
            continue;
        };
        if matches!(frame.packet_type(), LvdsPacketType::Unknown(_)) {
            continue;
        }
        if found.is_some() {
            return None;
        }
        found = Some((offset, frame));
    }
    found
}
