use std::sync::atomic::{AtomicU64, Ordering};

/// Decoder counters produced for one call to `StreamDecoder::push`.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DecodeDelta {
    pub rejected_frames: u64,
    pub checksum_failures: u64,
    pub length_failures: u64,
    pub resync_bytes: u64,
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
    pub resync_bytes: u64,
    pub events_dropped: u64,
    pub queue_overflows: u64,
    pub io_errors: u64,
    pub shutdown_timeouts: u64,
    pub max_event_queue_depth: u64,
    pub max_command_queue_depth: u64,
}
