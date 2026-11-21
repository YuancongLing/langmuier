//! TYPE AB excitation DATA, decoded after shared public LVDS framing.
use std::fmt;

use crate::{LvdsCommonFrame, LvdsPacketType, Timestamp};

pub const EXCITATION_SAMPLE_COUNT: usize = 100;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExcitationFrame {
    /// Device time in seconds from 2018-01-01 plus microseconds. This is the
    /// time of wave[99] only when a positive sample interval is supplied.
    pub timestamp: Timestamp,
    pub f_act_mhz: u32,
    pub interval_us: u16,
    pub points: u16,
    pub work_mode: u8,
    pub scan_freq: u8,
    pub scan_voltage_mv: u16,
    pub ad9833_status: u8,
    pub dac8563_status: u8,
    pub sci_frame_seq: u8,
    pub pa0_seq: u8,
    pub wave_mv: Box<[u16; EXCITATION_SAMPLE_COUNT]>,
    pub raw_crc: [u8; 2],
}

impl ExcitationFrame {
    /// Whether the raw waveform slots have a supplied sample interval.
    #[must_use]
    pub const fn has_sample_timing(&self) -> bool {
        self.interval_us > 0
    }

    /// Returns no time for an untimed status slot or an invalid timed sample.
    #[must_use]
    pub fn sample_time_us(&self, index: usize) -> Option<u64> {
        if !self.has_sample_timing()
            || index >= EXCITATION_SAMPLE_COUNT
            || self.timestamp.microseconds >= 1_000_000
        {
            return None;
        }
        let preceding = u64::try_from(EXCITATION_SAMPLE_COUNT - 1 - index).ok()?;
        let latest = u64::from(self.timestamp.utc_seconds) * 1_000_000
            + u64::from(self.timestamp.microseconds);
        latest.checked_sub(preceding * u64::from(self.interval_us))
    }

    #[must_use]
    pub fn actual_frequency_hz(&self) -> f64 {
        f64::from(self.f_act_mhz) / 1000.0
    }

    #[must_use]
    pub fn is_all_zero(&self) -> bool {
        self.wave_mv.iter().all(|sample| *sample == 0)
    }

    #[must_use]
    pub const fn integrity(&self) -> &'static str {
        "CRC_NOT_CHECKED"
    }
}

impl fmt::Display for ExcitationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WrongPacketType { actual } => {
                write!(formatter, "AB decoder received TYPE=0x{actual:02X}")
            }
            Self::InvalidPoints { actual } => {
                write!(formatter, "AB points must be 100, got {actual}")
            }
            Self::InvalidMicroseconds { actual } => {
                write!(formatter, "AB microseconds exceeds 999999: {actual}")
            }
            Self::InvalidMode { actual } => {
                write!(formatter, "AB work_mode is not 55/AA/BB: 0x{actual:02X}")
            }
            Self::InvalidScanFrequency { actual } => write!(
                formatter,
                "AB triangle/sine scan_freq exceeds 100: {actual}"
            ),
            Self::InvalidScanVoltage { work_mode, actual } => write!(
                formatter,
                "AB scan_voltage_mV={actual} is invalid for mode 0x{work_mode:02X}"
            ),
            Self::InvalidStatus { field, actual } => {
                write!(formatter, "AB {field} must be 0 or 1, got {actual}")
            }
            Self::NonZeroReserved { index, actual } => {
                write!(formatter, "AB reserved[{index}] must be zero, got {actual}")
            }
            Self::WaveOutOfRange { index, actual } => {
                write!(formatter, "AB wave[{index}] exceeds 3300mV: {actual}")
            }
            Self::TimestampUnderflow { latest_us, span_us } => write!(
                formatter,
                "AB latest device time {latest_us}us is before its {span_us}us sample window"
            ),
        }
    }
}

impl std::error::Error for ExcitationError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExcitationRecord {
    pub frame: ExcitationFrame,
    pub outer_sequence: u32,
    pub raw_record_id: u64,
    pub source: String,
    pub session: String,
    pub received_unix_nanos: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExcitationError {
    WrongPacketType { actual: u8 },
    InvalidPoints { actual: u16 },
    InvalidMicroseconds { actual: u32 },
    InvalidMode { actual: u8 },
    InvalidScanFrequency { actual: u8 },
    InvalidScanVoltage { work_mode: u8, actual: u16 },
    InvalidStatus { field: &'static str, actual: u8 },
    NonZeroReserved { index: usize, actual: u8 },
    WaveOutOfRange { index: usize, actual: u16 },
    TimestampUnderflow { latest_us: u64, span_us: u64 },
}

/// Decodes only AB business fields; CRC bytes remain retained and unchecked.
///
/// # Errors
/// Returns the specific invalid field or an unsafe device-time subtraction.
pub fn decode_excitation_frame(
    common: &LvdsCommonFrame,
) -> Result<ExcitationFrame, ExcitationError> {
    if common.packet_type() != LvdsPacketType::Excitation {
        return Err(ExcitationError::WrongPacketType {
            actual: common.packet_type().code(),
        });
    }
    let data = common.data();
    let word = |offset| u16::from_be_bytes([data[offset], data[offset + 1]]);
    let dword = |offset| {
        u32::from_be_bytes([
            data[offset],
            data[offset + 1],
            data[offset + 2],
            data[offset + 3],
        ])
    };
    let timestamp = Timestamp {
        utc_seconds: dword(0),
        microseconds: dword(4),
    };
    if timestamp.microseconds >= 1_000_000 {
        return Err(ExcitationError::InvalidMicroseconds {
            actual: timestamp.microseconds,
        });
    }
    let points = word(14);
    if points != 100 {
        return Err(ExcitationError::InvalidPoints { actual: points });
    }
    let work_mode = data[16];
    if !matches!(work_mode, 0x55 | 0xAA | 0xBB) {
        return Err(ExcitationError::InvalidMode { actual: work_mode });
    }
    let scan_freq = data[17];
    if work_mode != 0xBB && scan_freq > 100 {
        return Err(ExcitationError::InvalidScanFrequency { actual: scan_freq });
    }
    let scan_voltage_mv = word(18);
    if (work_mode == 0xBB && scan_voltage_mv > 5000) || (work_mode != 0xBB && scan_voltage_mv != 0)
    {
        return Err(ExcitationError::InvalidScanVoltage {
            work_mode,
            actual: scan_voltage_mv,
        });
    }
    for (field, actual) in [("ad9833_status", data[20]), ("dac8563_status", data[21])] {
        if actual > 1 {
            return Err(ExcitationError::InvalidStatus { field, actual });
        }
    }
    for (index, actual) in data[24..60].iter().copied().enumerate() {
        if actual != 0 {
            return Err(ExcitationError::NonZeroReserved { index, actual });
        }
    }
    let mut wave_mv = Box::new([0; EXCITATION_SAMPLE_COUNT]);
    for (index, (sample, bytes)) in wave_mv
        .iter_mut()
        .zip(data[60..].as_chunks::<2>().0)
        .enumerate()
    {
        let actual = u16::from_be_bytes(*bytes);
        if actual > 3300 {
            return Err(ExcitationError::WaveOutOfRange { index, actual });
        }
        *sample = actual;
    }
    let interval_us = word(12);
    let latest_us =
        u64::from(timestamp.utc_seconds) * 1_000_000 + u64::from(timestamp.microseconds);
    let span_us = 99 * u64::from(interval_us);
    if interval_us > 0 && latest_us < span_us {
        return Err(ExcitationError::TimestampUnderflow { latest_us, span_us });
    }
    Ok(ExcitationFrame {
        timestamp,
        f_act_mhz: dword(8),
        interval_us,
        points,
        work_mode,
        scan_freq,
        scan_voltage_mv,
        ad9833_status: data[20],
        dac8563_status: data[21],
        sci_frame_seq: data[22],
        pa0_seq: data[23],
        wave_mv,
        raw_crc: common.raw_crc(),
    })
}
