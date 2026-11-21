use std::time::SystemTime;

use langmuir_core::{GNSS_PRESETS, GnssPoint, StarTime, encode_gnss_broadcast};

pub const BROADCAST_INTERVAL_MIN_MILLIS: u32 = 100;
pub const BROADCAST_INTERVAL_MAX_MILLIS: u32 = 60_000;

#[derive(Debug, Clone, PartialEq)]
pub struct StarSimulatorConfig {
    pub enabled: [bool; 3],
    pub interval_millis: [u32; 3],
    pub point: GnssPoint,
}

impl Default for StarSimulatorConfig {
    fn default() -> Self {
        Self {
            enabled: [false; 3],
            interval_millis: [1_000; 3],
            point: GNSS_PRESETS[0],
        }
    }
}

impl StarSimulatorConfig {
    pub fn validate(&self) -> Result<(), String> {
        if !self.enabled.iter().any(|enabled| *enabled) {
            return Err("请至少选择一类广播".to_owned());
        }
        if self.interval_millis.iter().any(|value| {
            !(BROADCAST_INTERVAL_MIN_MILLIS..=BROADCAST_INTERVAL_MAX_MILLIS).contains(value)
        }) {
            return Err("广播周期必须为 100–60000 ms".to_owned());
        }
        encode_gnss_broadcast(
            StarTime {
                seconds: 0,
                microseconds: 0,
            },
            self.point,
            0,
        )
        .map_err(|error| error.to_string())?;
        Ok(())
    }
}

/// Measured at the single Writer, independently for each broadcast kind.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BroadcastTiming {
    pub samples: u64,
    pub last_interval_micros: u64,
    pub max_lateness_micros: u64,
    pub total_lateness_micros: u64,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct StarSimulatorStatus {
    pub active: bool,
    pub config: StarSimulatorConfig,
    pub sent: [u64; 3],
    pub skipped: [u64; 3],
    pub failed: [u64; 3],
    pub queue_depth: usize,
    pub last_sent_utc: Option<SystemTime>,
    pub last_frames: [Vec<u8>; 3],
    pub last_error: Option<String>,
    pub timing: [BroadcastTiming; 3],
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_simulator_cannot_start_without_selecting_broadcasts() {
        let config = StarSimulatorConfig::default();
        assert!(!StarSimulatorStatus::default().active);
        assert!(config.validate().is_err());
    }

    #[test]
    fn rejects_unbounded_periods_and_non_finite_positions() {
        let mut config = StarSimulatorConfig {
            enabled: [true; 3],
            ..Default::default()
        };
        assert!(config.validate().is_ok());
        for interval in [0, 99, 60_001, u32::MAX] {
            config.interval_millis[1] = interval;
            assert!(config.validate().is_err());
        }
        config.interval_millis[1] = 1_000;
        config.point.latitude_deg = f64::NAN;
        assert!(config.validate().is_err());
    }

    #[test]
    fn rejects_points_that_cannot_be_encoded_before_starting() {
        for field in 0..3 {
            let mut config = StarSimulatorConfig {
                enabled: [true; 3],
                ..Default::default()
            };
            match field {
                0 => config.point.height_m = 30_000_000.,
                1 => config.point.velocity_ecef_mps[0] = f64::NAN,
                _ => config.point.gps_satellites = 16,
            }
            assert!(
                config.validate().is_err(),
                "invalid field {field} must fail before Start"
            );
        }
    }
}
