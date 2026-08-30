use crate::{Rs422Telemetry, SAMPLE_COUNT, StreamMetrics, Timestamp};

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum LinkState {
    #[default]
    Disconnected,
    Connecting,
    Connected {
        endpoint: String,
    },
    Faulted {
        message: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum AcquisitionState {
    #[default]
    Idle,
    Running,
    Stopping,
    Faulted {
        message: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CommandCounters {
    pub accepted: u8,
    pub rejected: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScientificFrameSummary {
    pub channel_1_time: Timestamp,
    pub channel_2_time: Timestamp,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DeviceState {
    pub rs422_link: LinkState,
    pub lvds_link: LinkState,
    pub acquisition: AcquisitionState,
    pub latest_telemetry: Option<Rs422Telemetry>,
    pub latest_scientific: Option<ScientificFrameSummary>,
    pub command_counters: CommandCounters,
}

impl DeviceState {
    pub fn apply_telemetry(&mut self, telemetry: Rs422Telemetry) {
        self.command_counters = CommandCounters {
            accepted: telemetry.accepted_command_count,
            rejected: telemetry.rejected_command_count,
        };
        self.latest_telemetry = Some(telemetry);
    }

    pub fn apply_scientific_summary(&mut self, summary: ScientificFrameSummary) {
        self.latest_scientific = Some(summary);
    }
}

/// Aggregate counters intended for status displays and operational telemetry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CoreMetrics {
    pub rs422_stream: StreamMetrics,
    pub lvds_stream: StreamMetrics,
    pub commands_encoded: u64,
    pub scientific_samples_decoded: u64,
}

impl CoreMetrics {
    pub fn synchronize_streams(&mut self, rs422: StreamMetrics, lvds: StreamMetrics) {
        self.rs422_stream = rs422;
        self.lvds_stream = lvds;
        self.scientific_samples_decoded = lvds
            .frames_decoded
            .saturating_mul(SAMPLE_COUNT as u64)
            .saturating_mul(2);
    }

    pub fn record_command_encoded(&mut self) {
        self.commands_encoded = self.commands_encoded.saturating_add(1);
    }
}

#[cfg(test)]
mod tests {
    use super::{CoreMetrics, DeviceState, ScientificFrameSummary};
    use crate::{
        StreamMetrics, Timestamp, decode_rs422_telemetry, rs422::test_support::valid_frame,
    };

    #[test]
    fn telemetry_updates_latest_value_and_counters_atomically() {
        let telemetry = decode_rs422_telemetry(&valid_frame()).unwrap();
        let mut state = DeviceState::default();
        state.apply_telemetry(telemetry.clone());
        assert_eq!(state.command_counters.accepted, 201);
        assert_eq!(state.command_counters.rejected, 7);
        assert_eq!(state.latest_telemetry, Some(telemetry));

        let summary = ScientificFrameSummary {
            channel_1_time: Timestamp {
                utc_seconds: 1,
                microseconds: 2,
            },
            channel_2_time: Timestamp {
                utc_seconds: 3,
                microseconds: 4,
            },
        };
        state.apply_scientific_summary(summary);
        assert_eq!(state.latest_scientific, Some(summary));
    }

    #[test]
    fn aggregate_metrics_saturate_and_count_both_channels() {
        let mut metrics = CoreMetrics::default();
        let lvds = StreamMetrics {
            frames_decoded: 3,
            ..StreamMetrics::default()
        };
        metrics.synchronize_streams(StreamMetrics::default(), lvds);
        assert_eq!(metrics.scientific_samples_decoded, 6_000);
        metrics.commands_encoded = u64::MAX;
        metrics.record_command_encoded();
        assert_eq!(metrics.commands_encoded, u64::MAX);
    }
}
