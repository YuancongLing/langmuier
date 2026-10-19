use std::sync::atomic::{AtomicU64, Ordering};

use langmuir_core::{FrameError, UdpLvdsSequenceMetrics};

/// Decoder counters produced for one call to `StreamDecoder::push`.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DecodeDelta {
    pub rejected_frames: u64,
    pub checksum_failures: u64,
    pub length_failures: u64,
    pub header_failures: u64,
    pub field_failures: u64,
    pub timestamp_failures: u64,
    pub sample_failures: u64,
    /// Most recent calculated and received checksum, in that order.
    pub last_checksum_mismatch: Option<(u16, u16)>,
    pub resync_bytes: u64,
}

impl DecodeDelta {
    /// Records one rejected frame candidate and its precise error category.
    pub fn record_frame_error(&mut self, error: &FrameError) {
        self.rejected_frames = self.rejected_frames.saturating_add(1);
        let count = match error {
            FrameError::ChecksumMismatch {
                expected, actual, ..
            } => {
                self.last_checksum_mismatch = Some((*expected, *actual));
                &mut self.checksum_failures
            }
            FrameError::UnexpectedLength { .. } | FrameError::InvalidDeclaredLength { .. } => {
                &mut self.length_failures
            }
            FrameError::InvalidHeader { .. } | FrameError::InvalidTail { .. } => {
                &mut self.header_failures
            }
            FrameError::FieldOutOfRange {
                field: "microseconds" | "timestamp",
                ..
            } => &mut self.timestamp_failures,
            FrameError::UnexpectedField { .. } | FrameError::FieldOutOfRange { .. } => {
                &mut self.field_failures
            }
            FrameError::SampleOutOfRange { .. } => &mut self.sample_failures,
        };
        *count = count.saturating_add(1);
    }
}

/// Lock-free reader counters. They may be sampled while acquisition is active.
#[derive(Debug, Default)]
pub struct ReaderMetrics {
    bytes_read: AtomicU64,
    read_calls: AtomicU64,
    bytes_written: AtomicU64,
    write_calls: AtomicU64,
    commands_queued: AtomicU64,
    commands_rejected: AtomicU64,
    frames_decoded: AtomicU64,
    rejected_frames: AtomicU64,
    checksum_failures: AtomicU64,
    length_failures: AtomicU64,
    header_failures: AtomicU64,
    field_failures: AtomicU64,
    timestamp_failures: AtomicU64,
    sample_failures: AtomicU64,
    // A presence bit and both u16 values share one atomic to prevent mixed pairs.
    last_checksum_mismatch: AtomicU64,
    resync_bytes: AtomicU64,
    events_dropped: AtomicU64,
    queue_overflows: AtomicU64,
    io_errors: AtomicU64,
    shutdown_timeouts: AtomicU64,
    max_event_queue_depth: AtomicU64,
    max_command_queue_depth: AtomicU64,
}

impl ReaderMetrics {
    pub(crate) fn record_read(&self, bytes: usize) {
        self.bytes_read.fetch_add(bytes as u64, Ordering::Relaxed);
        self.read_calls.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn record_frame(&self) {
        self.frames_decoded.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn record_write(&self, bytes: usize) {
        self.bytes_written
            .fetch_add(bytes as u64, Ordering::Relaxed);
        self.write_calls.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn record_command_queued(&self) {
        self.commands_queued.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn record_command_rejected(&self) {
        self.commands_rejected.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn record_decode_delta(&self, delta: DecodeDelta) {
        self.rejected_frames
            .fetch_add(delta.rejected_frames, Ordering::Relaxed);
        self.checksum_failures
            .fetch_add(delta.checksum_failures, Ordering::Relaxed);
        self.length_failures
            .fetch_add(delta.length_failures, Ordering::Relaxed);
        self.header_failures
            .fetch_add(delta.header_failures, Ordering::Relaxed);
        self.field_failures
            .fetch_add(delta.field_failures, Ordering::Relaxed);
        self.timestamp_failures
            .fetch_add(delta.timestamp_failures, Ordering::Relaxed);
        self.sample_failures
            .fetch_add(delta.sample_failures, Ordering::Relaxed);
        if let Some((expected, actual)) = delta.last_checksum_mismatch {
            let packed = (1_u64 << 32) | (u64::from(expected) << 16) | u64::from(actual);
            self.last_checksum_mismatch.store(packed, Ordering::Relaxed);
        }
        self.resync_bytes
            .fetch_add(delta.resync_bytes, Ordering::Relaxed);
    }

    pub(crate) fn record_event_drop(&self) {
        self.events_dropped.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn record_overflow(&self) {
        self.queue_overflows.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn record_io_error(&self) {
        self.io_errors.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn record_shutdown_timeout(&self) {
        self.shutdown_timeouts.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn observe_queue_depth(&self, depth: usize) {
        let depth = depth as u64;
        let mut previous = self.max_event_queue_depth.load(Ordering::Relaxed);
        while depth > previous {
            match self.max_event_queue_depth.compare_exchange_weak(
                previous,
                depth,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(actual) => previous = actual,
            }
        }
    }

    pub(crate) fn observe_command_queue_depth(&self, depth: usize) {
        let depth = depth as u64;
        let mut previous = self.max_command_queue_depth.load(Ordering::Relaxed);
        while depth > previous {
            match self.max_command_queue_depth.compare_exchange_weak(
                previous,
                depth,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(actual) => previous = actual,
            }
        }
    }

    #[must_use]
    pub fn snapshot(&self) -> ReaderMetricsSnapshot {
        let packed_checksum = self.last_checksum_mismatch.load(Ordering::Relaxed);
        ReaderMetricsSnapshot {
            bytes_read: self.bytes_read.load(Ordering::Relaxed),
            read_calls: self.read_calls.load(Ordering::Relaxed),
            bytes_written: self.bytes_written.load(Ordering::Relaxed),
            write_calls: self.write_calls.load(Ordering::Relaxed),
            commands_queued: self.commands_queued.load(Ordering::Relaxed),
            commands_rejected: self.commands_rejected.load(Ordering::Relaxed),
            frames_decoded: self.frames_decoded.load(Ordering::Relaxed),
            rejected_frames: self.rejected_frames.load(Ordering::Relaxed),
            checksum_failures: self.checksum_failures.load(Ordering::Relaxed),
            length_failures: self.length_failures.load(Ordering::Relaxed),
            header_failures: self.header_failures.load(Ordering::Relaxed),
            field_failures: self.field_failures.load(Ordering::Relaxed),
            timestamp_failures: self.timestamp_failures.load(Ordering::Relaxed),
            sample_failures: self.sample_failures.load(Ordering::Relaxed),
            last_checksum_mismatch: (packed_checksum & (1_u64 << 32) != 0).then(|| {
                let bytes = packed_checksum.to_be_bytes();
                (
                    u16::from_be_bytes([bytes[4], bytes[5]]),
                    u16::from_be_bytes([bytes[6], bytes[7]]),
                )
            }),
            // UDP datagram reassembly is owned by the desktop ingress, not
            // the byte-stream reader. Its snapshots fill these fields there.
            udp_fragment_errors: 0,
            reassembly_timeouts: 0,
            reassembly_capacity_rejections: 0,
            reassembled_frames: 0,
            reassembly_pending_frames: 0,
            reassembly_buffered_bytes: 0,
            duplicate_frames_suppressed: 0,
            sequence_conflicts: 0,
            scientific_frames_delivered: 0,
            dedup_cached_frames: 0,
            dedup_cached_bytes: 0,
            udp_sequence: UdpLvdsSequenceMetrics::default(),
            resync_bytes: self.resync_bytes.load(Ordering::Relaxed),
            events_dropped: self.events_dropped.load(Ordering::Relaxed),
            queue_overflows: self.queue_overflows.load(Ordering::Relaxed),
            io_errors: self.io_errors.load(Ordering::Relaxed),
            shutdown_timeouts: self.shutdown_timeouts.load(Ordering::Relaxed),
            max_event_queue_depth: self.max_event_queue_depth.load(Ordering::Relaxed),
            max_command_queue_depth: self.max_command_queue_depth.load(Ordering::Relaxed),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ReaderMetricsSnapshot {
    pub bytes_read: u64,
    pub read_calls: u64,
    pub bytes_written: u64,
    pub write_calls: u64,
    pub commands_queued: u64,
    pub commands_rejected: u64,
    pub frames_decoded: u64,
    pub rejected_frames: u64,
    pub checksum_failures: u64,
    pub length_failures: u64,
    pub header_failures: u64,
    pub field_failures: u64,
    pub timestamp_failures: u64,
    pub sample_failures: u64,
    /// Most recent calculated and received checksum, in that order.
    pub last_checksum_mismatch: Option<(u16, u16)>,
    /// Malformed UDP wrappers or conflicting/invalid fragment assemblies.
    pub udp_fragment_errors: u64,
    /// Incomplete assemblies reclaimed after their first-receipt TTL.
    pub reassembly_timeouts: u64,
    /// New frame candidates rejected because all assembly slots were active.
    pub reassembly_capacity_rejections: u64,
    /// Reassembled byte sequences, including ones later rejected by LVDS CRC.
    pub reassembled_frames: u64,
    /// Current incomplete assemblies; this is a gauge, not a cumulative count.
    pub reassembly_pending_frames: u64,
    /// Payload bytes reserved by incomplete assemblies; excludes metadata.
    pub reassembly_buffered_bytes: u64,
    /// Valid repeated UDP science frames suppressed after exact byte comparison.
    pub duplicate_frames_suppressed: u64,
    /// Valid UDP science candidates reusing a recent sequence with other bytes.
    pub sequence_conflicts: u64,
    /// Accepted UDP science frames passed to the normal controller callback.
    pub scientific_frames_delivered: u64,
    /// Current accepted-frame cache entries; not a cumulative count.
    pub dedup_cached_frames: u64,
    /// Current raw complete-frame bytes retained by the dedup cache.
    pub dedup_cached_bytes: u64,
    /// Accepted UDP science sequence observations; byte-stream readers leave this zero.
    pub udp_sequence: UdpLvdsSequenceMetrics,
    pub resync_bytes: u64,
    pub events_dropped: u64,
    pub queue_overflows: u64,
    pub io_errors: u64,
    pub shutdown_timeouts: u64,
    pub max_event_queue_depth: u64,
    pub max_command_queue_depth: u64,
}

impl ReaderMetricsSnapshot {
    /// Records a rejected frame using the same categories as byte-stream readers.
    pub fn record_frame_error(&mut self, error: &FrameError) {
        let mut delta = DecodeDelta::default();
        delta.record_frame_error(error);
        self.rejected_frames = self.rejected_frames.saturating_add(delta.rejected_frames);
        self.checksum_failures = self
            .checksum_failures
            .saturating_add(delta.checksum_failures);
        self.length_failures = self.length_failures.saturating_add(delta.length_failures);
        self.header_failures = self.header_failures.saturating_add(delta.header_failures);
        self.field_failures = self.field_failures.saturating_add(delta.field_failures);
        self.timestamp_failures = self
            .timestamp_failures
            .saturating_add(delta.timestamp_failures);
        self.sample_failures = self.sample_failures.saturating_add(delta.sample_failures);
        if delta.last_checksum_mismatch.is_some() {
            self.last_checksum_mismatch = delta.last_checksum_mismatch;
        }
    }
}
