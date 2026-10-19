use langmuir_core::{
    LvdsStreamDecoder, Rs422StreamDecoder, Rs422Telemetry, ScientificFrame, StreamMetrics,
};

use crate::{DecodeDelta, ReaderFault, StreamDecoder};

fn delta(before: StreamMetrics, after: StreamMetrics, mut errors: DecodeDelta) -> DecodeDelta {
    errors.rejected_frames = after.frames_rejected.saturating_sub(before.frames_rejected);
    errors.resync_bytes = after.bytes_discarded.saturating_sub(before.bytes_discarded);
    errors
}

macro_rules! impl_decoder_adapter {
    ($decoder:ty, $item:ty) => {
        impl StreamDecoder for $decoder {
            type Item = $item;

            fn push(
                &mut self,
                bytes: &[u8],
                emit: &mut dyn FnMut(Self::Item),
            ) -> Result<DecodeDelta, ReaderFault> {
                let before = self.metrics();
                let mut errors = DecodeDelta::default();
                self.push_with(bytes, emit, |error| {
                    errors.record_frame_error(&error);
                });
                Ok(delta(before, self.metrics(), errors))
            }

            fn finish(&mut self) -> DecodeDelta {
                let before = self.metrics();
                self.clear_buffer();
                delta(before, self.metrics(), DecodeDelta::default())
            }
        }
    };
}

impl_decoder_adapter!(Rs422StreamDecoder, Rs422Telemetry);
impl_decoder_adapter!(LvdsStreamDecoder, ScientificFrame);

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use langmuir_core::{
        LVDS_CRC_CALC_END, LVDS_CRC_CALC_START, LVDS_CRC_FIELD_OFFSET, LvdsStreamDecoder,
        Rs422StreamDecoder, crc16_ccitt_false,
    };

    use crate::mock::{lvds_packet, memory_transport, rs422_packet};
    use crate::{ReaderConfig, ReaderEvent, ReaderWorker, StopReason};

    #[test]
    fn core_rs422_decoder_runs_in_reader_worker() {
        let (transport, mut input) = memory_transport(4, Duration::from_millis(5)).unwrap();
        let expected = rs422_packet(7, 1_700_000_000);
        input.feed(expected[..11].to_vec()).unwrap();
        input.feed(expected[11..].to_vec()).unwrap();
        input.close();

        let mut worker = ReaderWorker::spawn(
            Box::new(transport),
            Rs422StreamDecoder::new(),
            ReaderConfig {
                event_capacity: 8,
                read_buffer_bytes: 13,
                ..ReaderConfig::default()
            },
        )
        .unwrap();
        let exit = worker.join().unwrap();
        let telemetry = worker.events().try_iter().find_map(|event| match event {
            ReaderEvent::Frame(frame) => Some(frame),
            _ => None,
        });
        assert_eq!(exit.reason, StopReason::EndOfStream);
        assert_eq!(telemetry.unwrap().accepted_command_count, 7);
    }

    #[test]
    fn core_lvds_decoder_runs_in_reader_worker_with_fragmentation() {
        let (transport, mut input) = memory_transport(2, Duration::from_millis(5)).unwrap();
        input.feed(lvds_packet(9, 1_700_000_000)).unwrap();
        input.close();

        let mut worker = ReaderWorker::spawn(
            Box::new(transport),
            LvdsStreamDecoder::new(),
            ReaderConfig {
                event_capacity: 8,
                read_buffer_bytes: 257,
                ..ReaderConfig::default()
            },
        )
        .unwrap();
        let exit = worker.join().unwrap();
        let scientific = worker.events().try_iter().find_map(|event| match event {
            ReaderEvent::Frame(frame) => Some(frame),
            _ => None,
        });
        assert_eq!(exit.reason, StopReason::EndOfStream);
        let scientific = scientific.unwrap();
        assert_eq!(scientific.timestamp.microseconds, 9);
        assert_eq!(scientific.sequence, 0);
        assert_eq!(scientific.channel_1[0], 2057);
        assert_eq!(exit.metrics.frames_decoded, 1);
    }

    #[test]
    fn reader_rejects_bad_crc_without_emitting_a_scientific_frame() {
        let (transport, mut input) = memory_transport(1, Duration::from_millis(5)).unwrap();
        let mut corrupt = lvds_packet(9, 1_700_000_000);
        let actual = u16::from_be_bytes([
            corrupt[LVDS_CRC_FIELD_OFFSET],
            corrupt[LVDS_CRC_FIELD_OFFSET + 1],
        ]);
        corrupt[19] ^= 0x02;
        let expected = crc16_ccitt_false(&corrupt[LVDS_CRC_CALC_START..LVDS_CRC_CALC_END]);
        input.feed(corrupt).unwrap();
        input.close();
        let mut worker = ReaderWorker::spawn(
            Box::new(transport),
            LvdsStreamDecoder::new(),
            ReaderConfig {
                event_capacity: 8,
                read_buffer_bytes: 31,
                ..ReaderConfig::default()
            },
        )
        .unwrap();
        let exit = worker.join().unwrap();
        assert_eq!(exit.reason, StopReason::EndOfStream);
        assert!(
            worker
                .events()
                .try_iter()
                .all(|event| !matches!(event, ReaderEvent::Frame(_)))
        );
        assert_eq!(exit.metrics.frames_decoded, 0);
        assert_eq!(exit.metrics.rejected_frames, 1);
        assert_eq!(exit.metrics.checksum_failures, 1);
        assert_eq!(
            exit.metrics.last_checksum_mismatch,
            Some((expected, actual))
        );
    }

    #[test]
    fn reader_resynchronizes_after_crc_error_without_losing_valid_frames() {
        let (transport, mut input) = memory_transport(1, Duration::from_millis(5)).unwrap();
        let mut corrupt = lvds_packet(11, 1_700_000_000);
        corrupt[LVDS_CRC_FIELD_OFFSET + 1] ^= 0x01;
        let wire = [
            lvds_packet(9, 1_700_000_000),
            corrupt,
            lvds_packet(10, 1_700_000_000),
        ]
        .concat();
        input.feed(wire).unwrap();
        input.close();
        let mut worker = ReaderWorker::spawn(
            Box::new(transport),
            LvdsStreamDecoder::new(),
            ReaderConfig {
                event_capacity: 8,
                read_buffer_bytes: 17,
                ..ReaderConfig::default()
            },
        )
        .unwrap();
        let exit = worker.join().unwrap();
        let frames: Vec<_> = worker
            .events()
            .try_iter()
            .filter_map(|event| match event {
                ReaderEvent::Frame(frame) => Some(frame),
                _ => None,
            })
            .collect();
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].channel_1[0], 2_057);
        assert_eq!(frames[1].channel_1[0], 2_058);
        assert_eq!(exit.metrics.frames_decoded, 2);
        assert_eq!(exit.metrics.rejected_frames, 1);
        assert_eq!(exit.metrics.checksum_failures, 1);
        assert!(exit.metrics.resync_bytes >= 269);
    }
}
