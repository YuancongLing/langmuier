use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Debug, Default)]
pub struct StorageMetrics {
    submitted_items: AtomicU64,
    written_items: AtomicU64,
    rejected_items: AtomicU64,
    written_batches: AtomicU64,
    flushes: AtomicU64,
    sink_errors: AtomicU64,
    shutdown_timeouts: AtomicU64,
    events_dropped: AtomicU64,
    max_queue_depth: AtomicU64,
}

impl StorageMetrics {
    pub(crate) fn record_submit(&self) {
        self.submitted_items.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn record_written(&self, items: usize) {
        self.written_items
            .fetch_add(items as u64, Ordering::Relaxed);
        self.written_batches.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn record_rejected(&self) {
        self.rejected_items.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn record_flush(&self) {
        self.flushes.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn record_sink_error(&self) {
        self.sink_errors.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn record_event_drop(&self) {
        self.events_dropped.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn record_shutdown_timeout(&self) {
        self.shutdown_timeouts.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn observe_queue_depth(&self, depth: usize) {
        let depth = depth as u64;
        let mut previous = self.max_queue_depth.load(Ordering::Relaxed);
        while depth > previous {
            match self.max_queue_depth.compare_exchange_weak(
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
    pub fn snapshot(&self, queue_depth: usize) -> StorageMetricsSnapshot {
        StorageMetricsSnapshot {
            submitted_items: self.submitted_items.load(Ordering::Relaxed),
            written_items: self.written_items.load(Ordering::Relaxed),
            rejected_items: self.rejected_items.load(Ordering::Relaxed),
            written_batches: self.written_batches.load(Ordering::Relaxed),
            flushes: self.flushes.load(Ordering::Relaxed),
            sink_errors: self.sink_errors.load(Ordering::Relaxed),
            shutdown_timeouts: self.shutdown_timeouts.load(Ordering::Relaxed),
            events_dropped: self.events_dropped.load(Ordering::Relaxed),
            queue_depth: queue_depth as u64,
            max_queue_depth: self.max_queue_depth.load(Ordering::Relaxed),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct StorageMetricsSnapshot {
    pub submitted_items: u64,
    pub written_items: u64,
    pub rejected_items: u64,
    pub written_batches: u64,
    pub flushes: u64,
    pub sink_errors: u64,
    pub shutdown_timeouts: u64,
    pub events_dropped: u64,
    pub queue_depth: u64,
    pub max_queue_depth: u64,
}
