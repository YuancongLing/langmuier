use std::error::Error;
use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender, TrySendError, bounded};
use parking_lot::Mutex;

use crate::{StorageError, StorageMetrics, StorageMetricsSnapshot, SubmitError};

pub trait BatchSink<T>: Send + 'static {
    type Error: Error + Send + Sync + 'static;

    /// # Errors
    ///
    /// Returns the sink-specific failure without clearing the caller's batch.
    fn write_batch(&mut self, items: &[T]) -> Result<(), Self::Error>;
    /// # Errors
    ///
    /// Returns the sink-specific flush failure.
    fn flush(&mut self) -> Result<(), Self::Error>;

    /// # Errors
    ///
    /// Returns the sink-specific close or final-flush failure.
    fn close(&mut self) -> Result<(), Self::Error> {
        self.flush()
    }
}

#[derive(Clone, Copy, Debug)]
pub struct StorageServiceConfig {
    pub queue_capacity: usize,
    pub batch_size: usize,
    pub flush_interval: Duration,
    pub control_timeout: Duration,
    /// Maximum time `Drop` waits for graceful close before detaching a blocked
    /// sink. Explicit `shutdown` uses `control_timeout`.
    pub drop_timeout: Duration,
    pub event_capacity: usize,
}

impl Default for StorageServiceConfig {
    fn default() -> Self {
        Self {
            queue_capacity: 512,
            batch_size: 100,
            flush_interval: Duration::from_secs(2),
            control_timeout: Duration::from_secs(10),
            drop_timeout: Duration::from_millis(100),
            event_capacity: 32,
        }
    }
}

impl StorageServiceConfig {
    fn validate(&self) -> Result<(), StorageError> {
        if self.queue_capacity == 0 {
            return Err(StorageError::InvalidConfiguration(
                "queue capacity must be non-zero".to_owned(),
            ));
        }
        if self.batch_size == 0 {
            return Err(StorageError::InvalidConfiguration(
                "batch size must be non-zero".to_owned(),
            ));
        }
        if self.flush_interval.is_zero()
            || self.control_timeout.is_zero()
            || self.drop_timeout.is_zero()
        {
            return Err(StorageError::InvalidConfiguration(
                "flush interval, control timeout, and drop timeout must be non-zero".to_owned(),
            ));
        }
        if self.event_capacity == 0 {
            return Err(StorageError::InvalidConfiguration(
                "event capacity must be non-zero".to_owned(),
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StorageEvent {
    BatchWritten { items: usize, total_items: u64 },
    Flushed,
    Fault(StorageError),
    Stopped { reason: WriterStopReason },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WriterStopReason {
    Requested,
    InputClosed,
    SinkFailed(StorageError),
}

#[derive(Clone, Debug)]
pub struct WriterExit {
    pub reason: WriterStopReason,
    pub metrics: StorageMetricsSnapshot,
}

/// Reliable lifecycle state independent of the bounded progress-event queue.
#[derive(Clone, Debug, Default)]
pub struct StorageStatusSnapshot {
    pub terminal: Option<WriterExit>,
    pub last_error: Option<StorageError>,
    pub panicked: bool,
}

enum Control {
    Flush(Sender<Result<(), StorageError>>),
    Shutdown(Sender<Result<(), StorageError>>),
}

pub struct BatchWriter<T> {
    input: Sender<T>,
    control: Sender<Control>,
    events: Receiver<StorageEvent>,
    metrics: Arc<StorageMetrics>,
    accepting: Arc<AtomicBool>,
    submit_gate: Mutex<()>,
    control_timeout: Duration,
    drop_timeout: Duration,
    status: Arc<Mutex<StorageStatusSnapshot>>,
    completion: Receiver<WriterExit>,
    join: Option<JoinHandle<WriterExit>>,
}

impl<T: Send + 'static> BatchWriter<T> {
    /// Starts a bounded batching worker that owns `sink`.
    ///
    /// # Errors
    ///
    /// Returns invalid configuration or OS thread creation errors.
    pub fn start<S>(sink: S, config: StorageServiceConfig) -> Result<Self, StorageError>
    where
        S: BatchSink<T>,
    {
        config.validate()?;
        let (input, input_rx) = bounded(config.queue_capacity);
        let (control, control_rx) = bounded(4);
        let (event_tx, events) = bounded(config.event_capacity);
        let metrics = Arc::new(StorageMetrics::default());
        let metrics_for_thread = Arc::clone(&metrics);
        let accepting = Arc::new(AtomicBool::new(true));
        let accepting_for_thread = Arc::clone(&accepting);
        let status = Arc::new(Mutex::new(StorageStatusSnapshot::default()));
        let status_for_thread = Arc::clone(&status);
        let (completion_tx, completion) = bounded(1);
        let worker_config = config;
        let join = thread::Builder::new()
            .name("langmuir-storage".to_owned())
            .spawn(move || {
                let result = catch_unwind(AssertUnwindSafe(|| {
                    run_writer(
                        sink,
                        input_rx,
                        control_rx,
                        event_tx,
                        metrics_for_thread,
                        accepting_for_thread,
                        worker_config,
                    )
                }));
                match result {
                    Ok(exit) => {
                        let mut status = status_for_thread.lock();
                        if let WriterStopReason::SinkFailed(error) = &exit.reason {
                            status.last_error = Some(error.clone());
                        }
                        status.terminal = Some(exit.clone());
                        drop(status);
                        let _ = completion_tx.send(exit.clone());
                        exit
                    }
                    Err(payload) => {
                        status_for_thread.lock().panicked = true;
                        resume_unwind(payload)
                    }
                }
            })
            .map_err(|error| StorageError::Sink {
                operation: "spawn",
                message: error.to_string(),
            })?;

        Ok(Self {
            input,
            control,
            events,
            metrics,
            accepting,
            submit_gate: Mutex::new(()),
            control_timeout: config.control_timeout,
            drop_timeout: config.drop_timeout,
            status,
            completion,
            join: Some(join),
        })
    }

    /// Attempts a non-blocking submission.
    ///
    /// # Errors
    ///
    /// Returns the original item when the queue is full or closed.
    pub fn submit(&self, item: T) -> Result<(), SubmitError<T>> {
        let _gate = self.submit_gate.lock();
        if !self.accepting.load(Ordering::Acquire) {
            self.metrics.record_rejected();
            return Err(SubmitError::Closed(item));
        }
        match self.input.try_send(item) {
            Ok(()) => {
                self.metrics.record_submit();
                self.metrics.observe_queue_depth(self.input.len());
                Ok(())
            }
            Err(TrySendError::Full(item)) => {
                self.metrics.record_rejected();
                Err(SubmitError::QueueFull(item))
            }
            Err(TrySendError::Disconnected(item)) => {
                self.metrics.record_rejected();
                Err(SubmitError::Closed(item))
            }
        }
    }

    /// Waits at most `timeout` for bounded storage queue capacity.
    ///
    /// # Errors
    ///
    /// Returns the original item when the wait times out or the worker closes.
    pub fn submit_timeout(&self, item: T, timeout: Duration) -> Result<(), SubmitError<T>> {
        let _gate = self.submit_gate.lock();
        if !self.accepting.load(Ordering::Acquire) {
            self.metrics.record_rejected();
            return Err(SubmitError::Closed(item));
        }
        match self.input.send_timeout(item, timeout) {
            Ok(()) => {
                self.metrics.record_submit();
                self.metrics.observe_queue_depth(self.input.len());
                Ok(())
            }
            Err(crossbeam_channel::SendTimeoutError::Timeout(item)) => {
                self.metrics.record_rejected();
                Err(SubmitError::Timeout { item, timeout })
            }
            Err(crossbeam_channel::SendTimeoutError::Disconnected(item)) => {
                self.metrics.record_rejected();
                Err(SubmitError::Closed(item))
            }
        }
    }

    #[must_use]
    pub fn events(&self) -> &Receiver<StorageEvent> {
        &self.events
    }

    #[must_use]
    pub fn metrics(&self) -> StorageMetricsSnapshot {
        self.metrics.snapshot(self.input.len())
    }

    #[must_use]
    pub fn status(&self) -> StorageStatusSnapshot {
        self.status.lock().clone()
    }

    /// Drains accepted input and flushes it without closing the sink.
    ///
    /// # Errors
    ///
    /// Returns channel timeout/closure or sink errors.
    pub fn flush(&self) -> Result<(), StorageError> {
        let _gate = self.submit_gate.lock();
        let (ack_tx, ack_rx) = bounded(1);
        self.control
            .send_timeout(Control::Flush(ack_tx), self.control_timeout)
            .map_err(|error| match error {
                crossbeam_channel::SendTimeoutError::Timeout(_) => {
                    StorageError::ControlTimeout(self.control_timeout)
                }
                crossbeam_channel::SendTimeoutError::Disconnected(_) => {
                    StorageError::ControlChannelClosed
                }
            })?;
        ack_rx
            .recv_timeout(self.control_timeout)
            .map_err(|error| match error {
                RecvTimeoutError::Timeout => StorageError::ControlTimeout(self.control_timeout),
                RecvTimeoutError::Disconnected => StorageError::ControlChannelClosed,
            })?
    }

    /// Stops acceptance, drains every accepted item, flushes, and closes.
    ///
    /// # Errors
    ///
    /// Returns sink, timeout, channel, or worker panic errors. A timed-out
    /// worker is detached rather than blocking the caller indefinitely.
    pub fn shutdown(mut self) -> Result<WriterExit, StorageError> {
        let deadline = Instant::now() + self.control_timeout;
        let gate = self.submit_gate.lock();
        self.accepting.store(false, Ordering::Release);
        let (ack_tx, ack_rx) = bounded(1);
        let send_wait = deadline.saturating_duration_since(Instant::now());
        if send_wait.is_zero() {
            drop(gate);
            return Err(self.detach_after_timeout());
        }
        match self
            .control
            .send_timeout(Control::Shutdown(ack_tx), send_wait)
        {
            Ok(()) => {}
            Err(crossbeam_channel::SendTimeoutError::Timeout(_)) => {
                drop(gate);
                return Err(self.detach_after_timeout());
            }
            Err(crossbeam_channel::SendTimeoutError::Disconnected(_)) => {
                drop(gate);
                let wait = deadline.saturating_duration_since(Instant::now());
                return interpret_exit(self.wait_for_completion(wait)?);
            }
        }
        drop(gate);
        let ack_wait = deadline.saturating_duration_since(Instant::now());
        if ack_wait.is_zero() {
            return Err(self.detach_after_timeout());
        }
        let shutdown_result = match ack_rx.recv_timeout(ack_wait) {
            Ok(result) => result,
            Err(RecvTimeoutError::Timeout) => {
                return Err(self.detach_after_timeout());
            }
            Err(RecvTimeoutError::Disconnected) => {
                let wait = deadline.saturating_duration_since(Instant::now());
                return interpret_exit(self.wait_for_completion(wait)?);
            }
        };
        let wait = deadline.saturating_duration_since(Instant::now());
        let exit = self.wait_for_completion(wait)?;
        shutdown_result?;
        interpret_exit(exit)
    }

    fn wait_for_completion(&mut self, timeout: Duration) -> Result<WriterExit, StorageError> {
        if self.join.is_none() {
            return Err(StorageError::AlreadyJoined);
        }
        if timeout.is_zero() {
            return Err(self.detach_after_timeout());
        }
        match self.completion.recv_timeout(timeout) {
            Ok(exit) => {
                let Some(join) = self.join.take() else {
                    return Err(StorageError::AlreadyJoined);
                };
                join.join().map_err(|_| StorageError::WorkerPanicked)?;
                Ok(exit)
            }
            Err(RecvTimeoutError::Timeout) => Err(self.detach_after_timeout()),
            Err(RecvTimeoutError::Disconnected) => {
                let Some(join) = self.join.take() else {
                    return Err(StorageError::AlreadyJoined);
                };
                match join.join() {
                    Ok(_) => Err(StorageError::CompletionChannelClosed),
                    Err(_) => Err(StorageError::WorkerPanicked),
                }
            }
        }
    }

    fn detach_after_timeout(&mut self) -> StorageError {
        self.metrics.record_shutdown_timeout();
        self.join.take();
        StorageError::ControlTimeout(self.control_timeout)
    }
}

fn interpret_exit(exit: WriterExit) -> Result<WriterExit, StorageError> {
    match &exit.reason {
        WriterStopReason::SinkFailed(error) => Err(error.clone()),
        WriterStopReason::Requested | WriterStopReason::InputClosed => Ok(exit),
    }
}

impl<T> Drop for BatchWriter<T> {
    fn drop(&mut self) {
        self.accepting.store(false, Ordering::Release);
        if self.join.is_some() {
            let (ack_tx, _ack_rx) = bounded(1);
            let _ = self.control.try_send(Control::Shutdown(ack_tx));
            match self.completion.recv_timeout(self.drop_timeout) {
                Ok(_) | Err(RecvTimeoutError::Disconnected) => {
                    if let Some(join) = self.join.take() {
                        let _ = join.join();
                    }
                }
                Err(RecvTimeoutError::Timeout) => {
                    self.metrics.record_shutdown_timeout();
                    self.join.take();
                }
            }
        }
    }
}

// These owned handles intentionally keep all worker resources alive for the
// full thread lifetime; borrowing them would obscure that lifecycle contract.
#[allow(clippy::needless_pass_by_value)]
fn run_writer<T, S>(
    mut sink: S,
    input: Receiver<T>,
    control: Receiver<Control>,
    events: Sender<StorageEvent>,
    metrics: Arc<StorageMetrics>,
    accepting: Arc<AtomicBool>,
    config: StorageServiceConfig,
) -> WriterExit
where
    T: Send + 'static,
    S: BatchSink<T>,
{
    let mut batch = Vec::with_capacity(config.batch_size);
    let mut flush_deadline = Instant::now() + config.flush_interval;

    let reason = 'worker: loop {
        let wait = flush_deadline.saturating_duration_since(Instant::now());
        crossbeam_channel::select_biased! {
            recv(control) -> message => match message {
                Ok(Control::Flush(ack)) => {
                    let result = drain_input(
                        &input,
                        &mut batch,
                        config.batch_size,
                        &mut sink,
                        &events,
                        &metrics,
                    )
                    .and_then(|()| flush_all(&mut sink, &mut batch, &events, &metrics));
                    let _ = ack.send(result.clone());
                    if let Err(error) = result {
                        break 'worker WriterStopReason::SinkFailed(error);
                    }
                    flush_deadline = Instant::now() + config.flush_interval;
                }
                Ok(Control::Shutdown(ack)) => {
                    accepting.store(false, Ordering::Release);
                    let result = drain_input(
                        &input,
                        &mut batch,
                        config.batch_size,
                        &mut sink,
                        &events,
                        &metrics,
                    )
                    .and_then(|()| flush_all(&mut sink, &mut batch, &events, &metrics))
                    .and_then(|()| close_sink(&mut sink, &metrics));
                    let _ = ack.send(result.clone());
                    break 'worker match result {
                        Ok(()) => WriterStopReason::Requested,
                        Err(error) => WriterStopReason::SinkFailed(error),
                    };
                }
                Err(_) => {
                    accepting.store(false, Ordering::Release);
                    let result = drain_input(
                        &input,
                        &mut batch,
                        config.batch_size,
                        &mut sink,
                        &events,
                        &metrics,
                    )
                    .and_then(|()| flush_all(&mut sink, &mut batch, &events, &metrics))
                    .and_then(|()| close_sink(&mut sink, &metrics));
                    break 'worker match result {
                        Ok(()) => WriterStopReason::InputClosed,
                        Err(error) => WriterStopReason::SinkFailed(error),
                    };
                }
            },
            recv(input) -> message => if let Ok(item) = message {
                batch.push(item);
                if batch.len() >= config.batch_size
                    && let Err(error) = write_batch(&mut sink, &mut batch, &events, &metrics)
                {
                    break 'worker WriterStopReason::SinkFailed(error);
                }
            } else {
                accepting.store(false, Ordering::Release);
                let result = flush_all(&mut sink, &mut batch, &events, &metrics)
                    .and_then(|()| close_sink(&mut sink, &metrics));
                break 'worker match result {
                    Ok(()) => WriterStopReason::InputClosed,
                    Err(error) => WriterStopReason::SinkFailed(error),
                };
            },
            default(wait) => {
                if let Err(error) = flush_all(&mut sink, &mut batch, &events, &metrics) {
                    break 'worker WriterStopReason::SinkFailed(error);
                }
                flush_deadline = Instant::now() + config.flush_interval;
            }
        }
    };

    accepting.store(false, Ordering::Release);
    if let WriterStopReason::SinkFailed(error) = &reason {
        send_event(&events, StorageEvent::Fault(error.clone()), &metrics);
    }
    send_event(
        &events,
        StorageEvent::Stopped {
            reason: reason.clone(),
        },
        &metrics,
    );
    WriterExit {
        reason,
        metrics: metrics.snapshot(input.len()),
    }
}

fn drain_input<T, S>(
    input: &Receiver<T>,
    batch: &mut Vec<T>,
    batch_size: usize,
    sink: &mut S,
    events: &Sender<StorageEvent>,
    metrics: &StorageMetrics,
) -> Result<(), StorageError>
where
    S: BatchSink<T>,
{
    while let Ok(item) = input.try_recv() {
        batch.push(item);
        if batch.len() >= batch_size {
            write_batch(sink, batch, events, metrics)?;
        }
    }
    Ok(())
}

fn write_batch<T, S>(
    sink: &mut S,
    batch: &mut Vec<T>,
    events: &Sender<StorageEvent>,
    metrics: &StorageMetrics,
) -> Result<(), StorageError>
where
    S: BatchSink<T>,
{
    if batch.is_empty() {
        return Ok(());
    }
    let count = batch.len();
    sink.write_batch(batch).map_err(|error| {
        metrics.record_sink_error();
        StorageError::Sink {
            operation: "write batch",
            message: error.to_string(),
        }
    })?;
    batch.clear();
    metrics.record_written(count);
    send_event(
        events,
        StorageEvent::BatchWritten {
            items: count,
            total_items: metrics.snapshot(0).written_items,
        },
        metrics,
    );
    Ok(())
}

fn flush_all<T, S>(
    sink: &mut S,
    batch: &mut Vec<T>,
    events: &Sender<StorageEvent>,
    metrics: &StorageMetrics,
) -> Result<(), StorageError>
where
    S: BatchSink<T>,
{
    write_batch(sink, batch, events, metrics)?;
    sink.flush().map_err(|error| {
        metrics.record_sink_error();
        StorageError::Sink {
            operation: "flush",
            message: error.to_string(),
        }
    })?;
    metrics.record_flush();
    send_event(events, StorageEvent::Flushed, metrics);
    Ok(())
}

fn close_sink<T, S>(sink: &mut S, metrics: &StorageMetrics) -> Result<(), StorageError>
where
    S: BatchSink<T>,
{
    sink.close().map_err(|error| {
        metrics.record_sink_error();
        StorageError::Sink {
            operation: "close",
            message: error.to_string(),
        }
    })
}

fn send_event(events: &Sender<StorageEvent>, event: StorageEvent, metrics: &StorageMetrics) {
    if events.try_send(event).is_err() {
        metrics.record_event_drop();
    }
}

#[cfg(test)]
mod tests {
    use std::convert::Infallible;
    use std::thread;

    use super::*;
    use crate::{SubmitError, memory_sink};

    #[test]
    fn batches_and_drains_every_accepted_item_on_shutdown() {
        let (sink, snapshot) = memory_sink();
        let writer = BatchWriter::start(
            sink,
            StorageServiceConfig {
                queue_capacity: 16,
                batch_size: 3,
                flush_interval: Duration::from_mins(1),
                ..StorageServiceConfig::default()
            },
        )
        .unwrap();
        for value in 0..8 {
            writer.submit(value).unwrap();
        }

        let exit = writer.shutdown().unwrap();
        assert_eq!(exit.reason, WriterStopReason::Requested);
        assert_eq!(snapshot.items(), (0..8).collect::<Vec<_>>());
        assert_eq!(snapshot.batch_sizes(), [3, 3, 2]);
        assert!(snapshot.flush_count() >= 1);
        assert!(snapshot.is_closed());
        assert_eq!(exit.metrics.submitted_items, 8);
        assert_eq!(exit.metrics.written_items, 8);
    }

    #[test]
    fn explicit_flush_writes_a_partial_batch() {
        let (sink, snapshot) = memory_sink();
        let writer = BatchWriter::start(
            sink,
            StorageServiceConfig {
                queue_capacity: 8,
                batch_size: 100,
                flush_interval: Duration::from_mins(1),
                ..StorageServiceConfig::default()
            },
        )
        .unwrap();
        writer.submit(11).unwrap();
        writer.submit(12).unwrap();
        writer.flush().unwrap();
        assert_eq!(snapshot.items(), [11, 12]);
        writer.shutdown().unwrap();
    }

    #[test]
    fn full_queue_returns_the_original_item() {
        struct SlowSink;
        impl BatchSink<u64> for SlowSink {
            type Error = Infallible;
            fn write_batch(&mut self, _items: &[u64]) -> Result<(), Self::Error> {
                thread::sleep(Duration::from_millis(50));
                Ok(())
            }
            fn flush(&mut self) -> Result<(), Self::Error> {
                Ok(())
            }
        }

        let writer = BatchWriter::start(
            SlowSink,
            StorageServiceConfig {
                queue_capacity: 1,
                batch_size: 1,
                flush_interval: Duration::from_mins(1),
                ..StorageServiceConfig::default()
            },
        )
        .unwrap();

        let mut saw_full = false;
        for value in 0..100 {
            if let Err(SubmitError::QueueFull(returned)) = writer.submit(value) {
                assert_eq!(returned, value);
                saw_full = true;
                break;
            }
        }
        assert!(saw_full);
        assert!(writer.metrics().rejected_items >= 1);
        writer.shutdown().unwrap();
    }

    #[test]
    fn timed_submission_returns_the_original_item() {
        struct GatedSink {
            entered: Sender<()>,
            release: Receiver<()>,
            blocked_once: bool,
        }
        impl BatchSink<u64> for GatedSink {
            type Error = Infallible;

            fn write_batch(&mut self, _items: &[u64]) -> Result<(), Self::Error> {
                if !self.blocked_once {
                    self.blocked_once = true;
                    let _ = self.entered.send(());
                    let _ = self.release.recv();
                }
                Ok(())
            }

            fn flush(&mut self) -> Result<(), Self::Error> {
                Ok(())
            }
        }

        let (entered_tx, entered_rx) = bounded(1);
        let (release_tx, release_rx) = bounded(1);
        let writer = BatchWriter::start(
            GatedSink {
                entered: entered_tx,
                release: release_rx,
                blocked_once: false,
            },
            StorageServiceConfig {
                queue_capacity: 1,
                batch_size: 1,
                ..StorageServiceConfig::default()
            },
        )
        .unwrap();
        writer.submit(1).unwrap();
        entered_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        writer.submit(2).unwrap();

        let timeout = Duration::from_millis(5);
        let error = writer.submit_timeout(3, timeout).unwrap_err();
        assert!(matches!(
            error,
            SubmitError::Timeout {
                item: 3,
                timeout: actual
            } if actual == timeout
        ));
        release_tx.send(()).unwrap();
        writer.shutdown().unwrap();
    }

    #[test]
    fn rejects_zero_capacity_configuration() {
        let (sink, _) = memory_sink::<u8>();
        let result = BatchWriter::start(
            sink,
            StorageServiceConfig {
                queue_capacity: 0,
                ..StorageServiceConfig::default()
            },
        );
        assert!(matches!(result, Err(StorageError::InvalidConfiguration(_))));
    }

    #[test]
    fn sink_failure_is_returned_even_if_worker_exits_before_shutdown() {
        struct FailingSink;
        impl BatchSink<u8> for FailingSink {
            type Error = std::io::Error;

            fn write_batch(&mut self, _items: &[u8]) -> Result<(), Self::Error> {
                Err(std::io::Error::other("injected storage failure"))
            }

            fn flush(&mut self) -> Result<(), Self::Error> {
                Ok(())
            }
        }

        let writer = BatchWriter::start(
            FailingSink,
            StorageServiceConfig {
                batch_size: 1,
                event_capacity: 1,
                ..StorageServiceConfig::default()
            },
        )
        .unwrap();
        writer.submit(7).unwrap();
        let deadline = Instant::now() + Duration::from_secs(1);
        while writer.status().terminal.is_none() && Instant::now() < deadline {
            thread::yield_now();
        }
        let status = writer.status();
        assert!(status.terminal.is_some());
        assert!(status.last_error.is_some());
        let error = writer.shutdown().unwrap_err();
        assert!(matches!(
            error,
            StorageError::Sink {
                operation: "write batch",
                ..
            }
        ));
    }

    #[test]
    fn shutdown_timeout_detaches_a_blocked_sink() {
        struct BlockedSink;
        impl BatchSink<u8> for BlockedSink {
            type Error = Infallible;

            fn write_batch(&mut self, _items: &[u8]) -> Result<(), Self::Error> {
                thread::sleep(Duration::from_millis(100));
                Ok(())
            }

            fn flush(&mut self) -> Result<(), Self::Error> {
                Ok(())
            }
        }

        let writer = BatchWriter::start(
            BlockedSink,
            StorageServiceConfig {
                batch_size: 1,
                control_timeout: Duration::from_millis(5),
                drop_timeout: Duration::from_millis(5),
                ..StorageServiceConfig::default()
            },
        )
        .unwrap();
        writer.submit(1).unwrap();
        let started = Instant::now();
        assert!(matches!(
            writer.shutdown(),
            Err(StorageError::ControlTimeout(timeout)) if timeout == Duration::from_millis(5)
        ));
        assert!(started.elapsed() < Duration::from_millis(50));
    }
}
