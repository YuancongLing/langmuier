use langmuir_core::Timestamp;

pub const LEGACY_SAMPLE_COUNT_V2: usize = 125;
pub const LEGACY_SAMPLE_INTERVAL_MICROS_V2: u32 = 400;

/// Historical 125-point differential-decoder representation.
///
/// Only the unchanged `.lpd` v2 and legacy HDF5 writers accept this model.
/// New 100-point code10 frames must use CSV or explicitly versioned v3 files;
/// no conversion or padding from the live scientific model is provided.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LegacyScientificFrameV2 {
    pub timestamp: Timestamp,
    pub sequence: u16,
    pub channel_1: Box<[u32; LEGACY_SAMPLE_COUNT_V2]>,
    pub channel_2: Box<[u32; LEGACY_SAMPLE_COUNT_V2]>,
}

impl LegacyScientificFrameV2 {
    #[must_use]
    pub fn channel_1_base_seconds(&self) -> f64 {
        self.timestamp.as_seconds_f64()
    }
}
