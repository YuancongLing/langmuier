//! Bounded, opt-in synchronization without modifying received input.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::lvds::decode_scientific_data_prefix;
use crate::{
    LVDS_CRC_CALC_END, LVDS_CRC_CALC_START, LVDS_FRAME_LEN, ScientificFrame, crc16_ccitt_false,
    decode_scientific_frame,
};

const HEADER: [u8; 7] = [0xEB, 0x90, 0x55, 0xAA, 0x01, 0x07, 0xAA];

pub const MAX_RECOVERY_PAYLOAD_BYTES: usize = 4096;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum FrameQuality {
    #[default]
    Native,
    #[serde(rename = "RECOVERED_1BIT")]
    Recovered1Bit,
    #[serde(rename = "RECOVERED_4BIT")]
    Recovered4Bit,
    RecoveryCandidate,
    Rejected,
}

impl FrameQuality {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Native => "NATIVE",
            Self::Recovered1Bit => "RECOVERED_1BIT",
            Self::Recovered4Bit => "RECOVERED_4BIT",
            Self::RecoveryCandidate => "RECOVERY_CANDIDATE",
            Self::Rejected => "REJECTED",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Last accepted native or confirmed frame from the same source and session.
pub struct RecoveryAnchor {
    pub udp_sequence: u32,
    pub frame_seq_u8: u8,
    pub sample_time_us: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
/// The caller owns source/session isolation and must clear `previous` at a
/// session boundary. Diagnostic candidates must never become anchors.
pub struct RecoveryContext {
    pub udp_sequence: u32,
    pub previous: Option<RecoveryAnchor>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub enum ReferenceCrcStatus {
    #[default]
    #[serde(rename = "NOT_EVALUATED")]
    NotEvaluated,
    #[serde(rename = "REFERENCE_CRC_MATCH")]
    Match,
    #[serde(rename = "REFERENCE_CRC_MISMATCH")]
    Mismatch,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct ReferenceCrc {
    pub status: ReferenceCrcStatus,
    pub received: Option<u16>,
    pub calculated: Option<u16>,
    pub input_range: [usize; 2],
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "independent audit checks are serialized separately, not mutually exclusive states"
)]
pub struct RecoveryValidation {
    pub structure: bool,
    pub unique: bool,
    pub full_bits: bool,
    pub outer_sequence: bool,
    pub inner_sequence: bool,
    pub device_time: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveryResult {
    pub quality: FrameQuality,
    pub frame: Option<ScientificFrame>,
    /// Only actual available re-aligned bytes. Never padded to a full frame.
    pub bytes: Vec<u8>,
    /// MSB-first bit offset from the start of this one complete payload.
    pub bit_offset: Option<usize>,
    /// Real frame bits available in this payload, capped at 269*8.
    pub available_bits: usize,
    /// Actual left realignment shift; 7 also describes a one-bit phase error.
    pub shift: u8,
    pub candidate_count: usize,
    pub validation: RecoveryValidation,
    pub crc_reference: ReferenceCrc,
    pub reason: String,
}

pub struct LvdsBitAlignmentRecoverer;

impl LvdsBitAlignmentRecoverer {
    /// Searches one bounded, reassembled payload. No adjacent record is read.
    /// Normal decoding always precedes searching and ignores the CRC trailer.
    #[must_use]
    pub fn inspect(payload: &[u8], enabled: bool, context: RecoveryContext) -> RecoveryResult {
        if payload.len() > MAX_RECOVERY_PAYLOAD_BYTES {
            return rejected("payload exceeds the 4096-byte synchronization bound", 0);
        }
        if let Ok(frame) = decode_scientific_frame(payload) {
            return native(payload, frame, 0);
        }
        let mut found = None;
        for (offset, bytes) in payload.windows(LVDS_FRAME_LEN).enumerate() {
            if bytes.starts_with(&HEADER)
                && let Ok(frame) = decode_scientific_frame(bytes)
            {
                if found.is_some() {
                    return rejected("multiple complete byte synchronization candidates", 2);
                }
                found = Some(native(bytes, frame, offset * 8));
            }
        }
        if let Some(result) = found {
            return result;
        }
        if !enabled {
            return rejected("no complete native frame; bit recovery is disabled", 0);
        }
        search_bits(payload, context)
    }
}

fn rejected(reason: &str, candidate_count: usize) -> RecoveryResult {
    RecoveryResult {
        quality: FrameQuality::Rejected,
        frame: None,
        bytes: Vec::new(),
        bit_offset: None,
        available_bits: 0,
        shift: 0,
        candidate_count,
        validation: RecoveryValidation::default(),
        crc_reference: ReferenceCrc {
            input_range: [LVDS_CRC_CALC_START, LVDS_CRC_CALC_END],
            ..ReferenceCrc::default()
        },
        reason: reason.to_owned(),
    }
}

fn native(bytes: &[u8], frame: ScientificFrame, bit_offset: usize) -> RecoveryResult {
    RecoveryResult {
        quality: FrameQuality::Native,
        frame: Some(frame),
        bytes: bytes.to_vec(),
        bit_offset: Some(bit_offset),
        available_bits: LVDS_FRAME_LEN * 8,
        shift: 0,
        candidate_count: 1,
        validation: RecoveryValidation {
            structure: true,
            unique: true,
            full_bits: true,
            ..RecoveryValidation::default()
        },
        crc_reference: ReferenceCrc {
            input_range: [LVDS_CRC_CALC_START, LVDS_CRC_CALC_END],
            ..ReferenceCrc::default()
        },
        reason: if bit_offset == 0 {
            "native 269-byte science frame"
        } else {
            "unique complete byte-aligned science frame"
        }
        .to_owned(),
    }
}

fn search_bits(payload: &[u8], context: RecoveryContext) -> RecoveryResult {
    let mut found = None;
    let Some(last_header) = (payload.len() * 8).checked_sub(HEADER.len() * 8) else {
        return rejected("not enough received bits for the fixed science header", 0);
    };
    for bit_offset in 0..=last_header {
        let shift = u8::try_from(bit_offset % 8).expect("phase is below eight");
        if !matches!(shift, 1 | 4 | 7) || !matches_header(payload, bit_offset) {
            continue;
        }
        let available_bits = (payload.len() * 8 - bit_offset).min(LVDS_FRAME_LEN * 8);
        let available = available_bits / 8;
        let bytes: Vec<u8> = (0..available)
            .map(|index| aligned_byte(payload, bit_offset + index * 8))
            .collect();
        let frame = if available >= 267 {
            let Ok(frame) = decode_scientific_data_prefix(&bytes) else {
                continue;
            };
            Some(frame)
        } else {
            None
        };
        if found.is_some() {
            return rejected("multiple plausible bit synchronization candidates", 2);
        }
        found = Some(bit_candidate(
            bytes,
            frame,
            bit_offset,
            available_bits,
            shift,
            context,
        ));
    }
    found.unwrap_or_else(|| rejected("no valid 1-bit or 4-bit fixed-header candidate", 0))
}

fn matches_header(payload: &[u8], bit_offset: usize) -> bool {
    HEADER
        .iter()
        .enumerate()
        .all(|(index, &expected)| aligned_byte(payload, bit_offset + index * 8) == expected)
}

fn aligned_byte(payload: &[u8], bit_offset: usize) -> u8 {
    let index = bit_offset / 8;
    let phase = bit_offset % 8;
    if phase == 0 {
        payload[index]
    } else {
        (payload[index] << phase) | (payload[index + 1] >> (8 - phase))
    }
}

fn bit_candidate(
    bytes: Vec<u8>,
    frame: Option<ScientificFrame>,
    bit_offset: usize,
    available_bits: usize,
    shift: u8,
    context: RecoveryContext,
) -> RecoveryResult {
    let mut validation = RecoveryValidation {
        structure: frame.is_some(),
        unique: true,
        full_bits: bytes.len() == LVDS_FRAME_LEN,
        ..RecoveryValidation::default()
    };
    if let (Some(previous), Some(frame)) = (context.previous, frame.as_ref()) {
        validation.outer_sequence = context.udp_sequence == previous.udp_sequence.wrapping_add(1);
        validation.inner_sequence = frame.frame_seq_u8 == previous.frame_seq_u8.wrapping_add(1);
        validation.device_time =
            previous.sample_time_us.checked_add(50_000) == frame.sample_time_us(0);
    }
    let crc_reference = reference_crc(&bytes);
    let confirmed = validation.structure
        && validation.full_bits
        && validation.outer_sequence
        && validation.inner_sequence
        && validation.device_time
        && crc_reference.status == ReferenceCrcStatus::Match;
    let quality = if confirmed {
        if shift == 4 {
            FrameQuality::Recovered4Bit
        } else {
            FrameQuality::Recovered1Bit
        }
    } else {
        FrameQuality::RecoveryCandidate
    };
    let reason = if !validation.full_bits {
        format!(
            "insufficient real tail bits: {available_bits} of 2152 frame bits available; no cross-record bits used"
        )
    } else if crc_reference.status != ReferenceCrcStatus::Match {
        "reference CRC does not match the complete candidate".to_owned()
    } else if !confirmed {
        "unique complete candidate lacks reliable outer/inner sequence or device-time continuity"
            .to_owned()
    } else {
        "unique complete real-bit alignment with sequence/time evidence and matching reference CRC"
            .to_owned()
    };
    RecoveryResult {
        quality,
        frame,
        bytes,
        bit_offset: Some(bit_offset),
        available_bits,
        shift,
        candidate_count: 1,
        validation,
        crc_reference,
        reason,
    }
}

fn reference_crc(bytes: &[u8]) -> ReferenceCrc {
    let mut reference = ReferenceCrc {
        input_range: [LVDS_CRC_CALC_START, LVDS_CRC_CALC_END],
        ..ReferenceCrc::default()
    };
    if bytes.len() >= LVDS_CRC_CALC_END {
        reference.calculated = Some(crc16_ccitt_false(
            &bytes[LVDS_CRC_CALC_START..LVDS_CRC_CALC_END],
        ));
    }
    if bytes.len() == LVDS_FRAME_LEN {
        let received = u16::from_be_bytes([bytes[267], bytes[268]]);
        reference.received = Some(received);
        reference.status = if reference.calculated == Some(received) {
            ReferenceCrcStatus::Match
        } else {
            ReferenceCrcStatus::Mismatch
        };
    }
    reference
}

#[must_use]
pub fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

#[cfg(test)]
#[path = "lvds_recovery_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "lvds_recovery_perf_tests.rs"]
mod perf_tests;
