use langmuir_core::{
    FrameError, LvdsStreamDecoder, Rs422StreamDecoder, Rs422Telemetry, ScientificFrame,
    StreamMetrics,
};

use crate::{DecodeDelta, ReaderFault, StreamDecoder};

fn delta(
    before: StreamMetrics,
    after: StreamMetrics,
    checksum_failures: u64,
    length_failures: u64,
) -> DecodeDelta {
    DecodeDelta {
        rejected_frames: after.frames_rejected.saturating_sub(before.frames_rejected),
        resync_bytes: after.bytes_discarded.saturating_sub(before.bytes_discarded),
        checksum_failures,
        length_failures,
    }
}

fn classify_error(error: &FrameError, checksum_failures: &mut u64, length_failures: &mut u64) {
    match error {
        FrameError::ChecksumMismatch { .. } => {
            *checksum_failures = checksum_failures.saturating_add(1);
        }
        FrameError::UnexpectedLength { .. } | FrameError::InvalidDeclaredLength { .. } => {
            *length_failures = length_failures.saturating_add(1);
        }
        FrameError::InvalidHeader { .. }
        | FrameError::InvalidTail { .. }
        | FrameError::UnexpectedField { .. }
        | FrameError::FieldOutOfRange { .. } => {}
    }
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
                let mut checksum_failures = 0_u64;
                let mut length_failures = 0_u64;
                self.push_with(bytes, emit, |error| {
                    classify_error(&error, &mut checksum_failures, &mut length_failures);
                });
                Ok(delta(
                    before,
                    self.metrics(),
                    checksum_failures,
                    length_failures,
                ))
            }

            fn finish(&mut self) -> DecodeDelta {
                let before = self.metrics();
                self.clear_buffer();
                delta(before, self.metrics(), 0, 0)
            }
        }
    };
}

impl_decoder_adapter!(Rs422StreamDecoder, Rs422Telemetry);
impl_decoder_adapter!(LvdsStreamDecoder, ScientificFrame);

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use langmuir_core::{LvdsStreamDecoder, Rs422StreamDecoder};

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
        assert_eq!(scientific.channel_1_time.microseconds, 9);
        assert_eq!(scientific.channel_1[0], 2048);
        assert_eq!(exit.metrics.frames_decoded, 1);
    }
}
