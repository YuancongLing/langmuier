use std::io;
use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender, TryRecvError, TrySendError, bounded};
use parking_lot::Mutex;

use crate::{
    CommandQueueError, DecodeDelta, ReaderError, ReaderFault, ReaderFaultKind, ReaderMetrics,
    ReaderMetricsSnapshot, Transport, WriteAckError,
};

/// Adapter implemented by the protocol crate's incremental stream decoders.
///
/// The callback avoids allocating a new `Vec` on every OS read. The decoder is
/// responsible for retaining an incomplete suffix between calls and resyncing
/// after malformed input.
pub trait StreamDecoder: Send + 'static {
    type Item: Send + 'static;

    /// # Errors
    ///
    /// Returns a structured decoder fault when parsing cannot continue.
    fn push(
        &mut self,
        bytes: &[u8],
        emit: &mut dyn FnMut(Self::Item),
    ) -> Result<DecodeDelta, ReaderFault>;

    fn finish(&mut self) -> DecodeDelta {
        DecodeDelta::default()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BackpressurePolicy {
    /// Keep acquisition alive for display-only streams and discard the newest item.
    DropNewest,
    /// Stop after the first observable overflow; that overflowing item is lost.
    Stop,
    /// Wait for bounded time; stop acquisition if the consumer stays behind.
    Wait(Duration),
}

#[derive(Clone, Debug)]
pub struct ReaderConfig {
    pub event_capacity: usize,
    pub command_capacity: usize,
    pub max_commands_per_cycle: usize,
    pub read_buffer_bytes: usize,
    pub read_timeout: Duration,
    /// Maximum time shutdown waits before detaching a transport whose read
    /// call did not honor its configured timeout.
    pub shutdown_timeout: Duration,
    pub backpressure: BackpressurePolicy,
}

impl Default for ReaderConfig {
    fn default() -> Self {
        Self {
            event_capacity: 256,
            command_capacity: 32,
            max_commands_per_cycle: 8,
            read_buffer_bytes: 64 * 1024,
            read_timeout: Duration::from_millis(25),
            shutdown_timeout: Duration::from_millis(250),
            backpressure: BackpressurePolicy::Stop,
        }
    }
}

impl ReaderConfig {
    fn validate(&self) -> Result<(), ReaderError> {
        if self.event_capacity == 0 {
            return Err(ReaderError::InvalidConfiguration(
                "event capacity must be non-zero".to_owned(),
            ));
        }
        if self.command_capacity == 0 {
            return Err(ReaderError::InvalidConfiguration(
                "command capacity must be non-zero".to_owned(),
            ));
        }
        if self.max_commands_per_cycle == 0 {
            return Err(ReaderError::InvalidConfiguration(
                "maximum commands per read cycle must be non-zero".to_owned(),
            ));
        }
        if self.read_buffer_bytes == 0 {
            return Err(ReaderError::InvalidConfiguration(
                "read buffer size must be non-zero".to_owned(),
            ));
        }
        if self.read_timeout.is_zero() {
            return Err(ReaderError::InvalidConfiguration(
                "read timeout must be non-zero".to_owned(),
            ));
        }
        if self.shutdown_timeout.is_zero() {
            return Err(ReaderError::InvalidConfiguration(
                "shutdown timeout must be non-zero".to_owned(),
            ));
        }
        if matches!(self.backpressure, BackpressurePolicy::Wait(timeout) if timeout.is_zero()) {
            return Err(ReaderError::InvalidConfiguration(
                "backpressure wait timeout must be non-zero".to_owned(),
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StopReason {
    Requested,
    EndOfStream,
    Backpressure,
    DecoderFault,
    IoFault,
}

#[derive(Clone, Debug)]
pub enum ReaderEvent<T> {
    Started { endpoint: String },
    Frame(T),
    BytesWritten { id: u64, bytes: usize },
    WriteFailed { id: u64, fault: ReaderFault },
    Fault(ReaderFault),
    Stopped { reason: StopReason },
}

#[derive(Clone, Debug)]
pub struct WorkerExit {
    pub reason: StopReason,
    pub metrics: ReaderMetricsSnapshot,
}

/// Reliable lifecycle state independent of the bounded event queue.
#[derive(Clone, Debug, Default)]
pub struct ReaderStatusSnapshot {
    pub started: bool,
    pub terminal: Option<WorkerExit>,
    pub last_fault: Option<ReaderFault>,
    pub panicked: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WriteReceipt {
    pub id: u64,
    pub bytes: usize,
}

/// Per-request acknowledgement independent of the shared event queue.
pub struct WriteTicket {
    id: u64,
    acknowledgement: Receiver<Result<WriteReceipt, ReaderFault>>,
}

impl WriteTicket {
    #[must_use]
    pub const fn id(&self) -> u64 {
        self.id
    }

    /// Waits until the transport reports physical completion.
    ///
    /// # Errors
    ///
    /// Returns the physical write fault or a stopped-reader error.
    pub fn wait(self) -> Result<WriteReceipt, WriteAckError> {
        self.acknowledgement
            .recv()
            .map_err(|_| WriteAckError::ReaderStopped { id: self.id })?
            .map_err(|fault| WriteAckError::Failed { id: self.id, fault })
    }

    /// # Errors
    ///
    /// Returns a timeout, physical write fault, or stopped-reader error.
    pub fn wait_timeout(self, timeout: Duration) -> Result<WriteReceipt, WriteAckError> {
        self.acknowledgement
            .recv_timeout(timeout)
            .map_err(|error| match error {
                crossbeam_channel::RecvTimeoutError::Timeout => WriteAckError::Timeout {
                    id: self.id,
                    timeout,
                },
                crossbeam_channel::RecvTimeoutError::Disconnected => {
                    WriteAckError::ReaderStopped { id: self.id }
                }
            })?
            .map_err(|fault| WriteAckError::Failed { id: self.id, fault })
    }

    /// Polls physical write completion without depending on the shared event
    /// queue. Retain tickets until this returns `Some`.
    #[must_use]
    pub fn try_ack(&self) -> Option<Result<WriteReceipt, WriteAckError>> {
        match self.acknowledgement.try_recv() {
            Ok(result) => {
                Some(result.map_err(|fault| WriteAckError::Failed { id: self.id, fault }))
            }
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => {
                Some(Err(WriteAckError::ReaderStopped { id: self.id }))
            }
        }
    }
}

pub struct ReaderWorker<T> {
    events: Receiver<ReaderEvent<T>>,
    outbound: Sender<OutboundBytes>,
    next_command_id: AtomicU64,
    metrics: Arc<ReaderMetrics>,
    stop: Arc<AtomicBool>,
    status: Arc<Mutex<ReaderStatusSnapshot>>,
    completion: Receiver<WorkerExit>,
    shutdown_timeout: Duration,
    join: Option<JoinHandle<WorkerExit>>,
}

struct OutboundBytes {
    id: u64,
    bytes: Vec<u8>,
    acknowledgement: Sender<Result<WriteReceipt, ReaderFault>>,
}

impl<T: Send + 'static> ReaderWorker<T> {
    /// Starts the sole owner of a transport and its incremental decoder.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid configuration, transport setup failure, or
    /// OS thread creation failure.
    pub fn spawn<D>(
        mut transport: Box<dyn Transport>,
        mut decoder: D,
        config: ReaderConfig,
    ) -> Result<Self, ReaderError>
    where
        D: StreamDecoder<Item = T>,
    {
        config.validate()?;
        transport.set_read_timeout(config.read_timeout)?;

        let (event_tx, events) = bounded(config.event_capacity);
        let (outbound, outbound_rx) = bounded(config.command_capacity);
        let metrics = Arc::new(ReaderMetrics::default());
        let metrics_for_thread = Arc::clone(&metrics);
        let stop = Arc::new(AtomicBool::new(false));
        let stop_for_thread = Arc::clone(&stop);
        let status = Arc::new(Mutex::new(ReaderStatusSnapshot::default()));
        let status_for_thread = Arc::clone(&status);
        let (completion_tx, completion) = bounded(1);
        let endpoint = transport.endpoint().to_owned();
        let shutdown_timeout = config.shutdown_timeout;

        let join = thread::Builder::new()
            .name(format!("reader-{endpoint}"))
            .spawn(move || {
                let result = catch_unwind(AssertUnwindSafe(|| {
                    let exit = run_reader(
                        transport.as_mut(),
                        &mut decoder,
                        &config,
                        &event_tx,
                        &outbound_rx,
                        &metrics_for_thread,
                        &stop_for_thread,
                        &status_for_thread,
                        &endpoint,
                    );
                    // Completion is published only after device/decoder
                    // destructors have run. If either blocks, the owner times
                    // out and detaches without ever entering an unbounded join.
                    drop(decoder);
                    drop(transport);
                    exit
                }));
                match result {
                    Ok(exit) => {
                        status_for_thread.lock().terminal = Some(exit.clone());
                        let _ = completion_tx.send(exit.clone());
                        exit
                    }
                    Err(payload) => {
                        status_for_thread.lock().panicked = true;
                        resume_unwind(payload)
                    }
                }
            })
            .map_err(ReaderError::Spawn)?;

        Ok(Self {
            events,
            outbound,
            next_command_id: AtomicU64::new(1),
            metrics,
            stop,
            status,
            completion,
            shutdown_timeout,
            join: Some(join),
        })
    }

    #[must_use]
    pub fn events(&self) -> &Receiver<ReaderEvent<T>> {
        &self.events
    }

    #[must_use]
    pub fn metrics(&self) -> ReaderMetricsSnapshot {
        self.metrics.snapshot()
    }

    #[must_use]
    pub fn status(&self) -> ReaderStatusSnapshot {
        self.status.lock().clone()
    }

    pub fn request_stop(&self) {
        self.stop.store(true, Ordering::Release);
    }

    /// Queues bytes for the same worker that owns the full-duplex transport.
    ///
    /// Successful physical writes are reported through both the returned
    /// ticket and `BytesWritten`. Saturation returns the original bytes.
    ///
    /// # Errors
    ///
    /// Returns the original bytes when the bounded queue is full or closed,
    /// and rejects empty commands.
    pub fn try_send_bytes(&self, bytes: Vec<u8>) -> Result<WriteTicket, CommandQueueError> {
        if bytes.is_empty() {
            self.metrics.record_command_rejected();
            return Err(CommandQueueError::Empty);
        }
        let id = self.next_command_id.fetch_add(1, Ordering::Relaxed);
        let (acknowledgement, ack_rx) = bounded(1);
        match self.outbound.try_send(OutboundBytes {
            id,
            bytes,
            acknowledgement,
        }) {
            Ok(()) => {
                self.metrics.record_command_queued();
                self.metrics
                    .observe_command_queue_depth(self.outbound.len());
                Ok(WriteTicket {
                    id,
                    acknowledgement: ack_rx,
                })
            }
            Err(TrySendError::Full(command)) => {
                self.metrics.record_command_rejected();
                Err(CommandQueueError::QueueFull(command.bytes))
            }
            Err(TrySendError::Disconnected(command)) => {
                self.metrics.record_command_rejected();
                Err(CommandQueueError::Closed(command.bytes))
            }
        }
    }

    /// Queues an encoded command without blocking.
    ///
    /// # Errors
    ///
    /// Returns an error if the bounded command queue cannot accept it.
    pub fn try_send_command(
        &self,
        command: langmuir_core::CommandFrame,
    ) -> Result<WriteTicket, CommandQueueError> {
        self.try_send_bytes(command.into_bytes().to_vec())
    }

    /// Waits up to `timeout` for space in the bounded outbound queue.
    /// Physical completion is reported by the returned ticket.
    ///
    /// # Errors
    ///
    /// Returns the original bytes on timeout or closure, and rejects empty
    /// commands.
    pub fn send_bytes_timeout(
        &self,
        bytes: Vec<u8>,
        timeout: Duration,
    ) -> Result<WriteTicket, CommandQueueError> {
        if bytes.is_empty() {
            self.metrics.record_command_rejected();
            return Err(CommandQueueError::Empty);
        }
        let id = self.next_command_id.fetch_add(1, Ordering::Relaxed);
        let (acknowledgement, ack_rx) = bounded(1);
        match self.outbound.send_timeout(
            OutboundBytes {
                id,
                bytes,
                acknowledgement,
            },
            timeout,
        ) {
            Ok(()) => {
                self.metrics.record_command_queued();
                self.metrics
                    .observe_command_queue_depth(self.outbound.len());
                Ok(WriteTicket {
                    id,
                    acknowledgement: ack_rx,
                })
            }
            Err(crossbeam_channel::SendTimeoutError::Timeout(command)) => {
                self.metrics.record_command_rejected();
                Err(CommandQueueError::Timeout {
                    bytes: command.bytes,
                    timeout,
                })
            }
            Err(crossbeam_channel::SendTimeoutError::Disconnected(command)) => {
                self.metrics.record_command_rejected();
                Err(CommandQueueError::Closed(command.bytes))
            }
        }
    }

    /// Queues an encoded command, waiting at most `timeout` for queue space.
    ///
    /// # Errors
    ///
    /// Returns an error if queue space is unavailable before the deadline.
    pub fn send_command_timeout(
        &self,
        command: langmuir_core::CommandFrame,
        timeout: Duration,
    ) -> Result<WriteTicket, CommandQueueError> {
        self.send_bytes_timeout(command.into_bytes().to_vec(), timeout)
    }

    /// Waits for worker completion using the configured shutdown timeout.
    ///
    /// # Errors
    ///
    /// Returns an error on timeout, panic, or repeated join.
    pub fn join(&mut self) -> Result<WorkerExit, ReaderError> {
        self.join_timeout(self.shutdown_timeout)
    }

    /// Waits for termination without allowing a non-cooperative device driver
    /// to hang the caller forever. A timed-out worker is detached.
    ///
    /// # Errors
    ///
    /// Returns an error for a zero timeout, timeout, panic, completion-channel
    /// failure, or repeated join.
    pub fn join_timeout(&mut self, timeout: Duration) -> Result<WorkerExit, ReaderError> {
        if timeout.is_zero() {
            return Err(ReaderError::InvalidConfiguration(
                "join timeout must be non-zero".to_owned(),
            ));
        }
        if self.join.is_none() {
            return Err(ReaderError::InvalidConfiguration(
                "reader worker has already been joined".to_owned(),
            ));
        }
        match self.completion.recv_timeout(timeout) {
            Ok(exit) => {
                let Some(join) = self.join.take() else {
                    return Err(ReaderError::InvalidConfiguration(
                        "reader worker has already been joined".to_owned(),
                    ));
                };
                join.join().map_err(|_| ReaderError::WorkerPanicked)?;
                Ok(exit)
            }
            Err(RecvTimeoutError::Timeout) => {
                self.metrics.record_shutdown_timeout();
                self.join.take();
                Err(ReaderError::ShutdownTimeout(timeout))
            }
            Err(RecvTimeoutError::Disconnected) => {
                let Some(join) = self.join.take() else {
                    return Err(ReaderError::InvalidConfiguration(
                        "reader worker has already been joined".to_owned(),
                    ));
                };
                match join.join() {
                    Ok(_) => Err(ReaderError::CompletionChannelClosed),
                    Err(_) => Err(ReaderError::WorkerPanicked),
                }
            }
        }
    }

    /// Requests stop and waits for bounded worker completion.
    ///
    /// # Errors
    ///
    /// Returns an error when shutdown times out or the worker fails.
    pub fn shutdown(mut self) -> Result<WorkerExit, ReaderError> {
        self.request_stop();
        self.join()
    }
}

impl<T> Drop for ReaderWorker<T> {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if self.join.is_some() {
            match self.completion.recv_timeout(self.shutdown_timeout) {
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

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn run_reader<T, D>(
    transport: &mut dyn Transport,
    decoder: &mut D,
    config: &ReaderConfig,
    event_tx: &Sender<ReaderEvent<T>>,
    outbound: &Receiver<OutboundBytes>,
    metrics: &ReaderMetrics,
    stop: &AtomicBool,
    status: &Mutex<ReaderStatusSnapshot>,
    endpoint: &str,
) -> WorkerExit
where
    T: Send + 'static,
    D: StreamDecoder<Item = T>,
{
    status.lock().started = true;
    let _ = event_tx.try_send(ReaderEvent::Started {
        endpoint: endpoint.to_owned(),
    });
    metrics.observe_queue_depth(event_tx.len());
    let mut buffer = vec![0_u8; config.read_buffer_bytes];

    let reason = 'reader: loop {
        if stop.load(Ordering::Acquire) {
            break StopReason::Requested;
        }

        for _ in 0..config.max_commands_per_cycle {
            let command = match outbound.try_recv() {
                Ok(command) => command,
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
            };
            if let Err(error) = transport
                .write_all(&command.bytes)
                .and_then(|()| transport.flush())
            {
                metrics.record_io_error();
                let fault = ReaderFault::new(
                    ReaderFaultKind::Io,
                    format!("command {} write failed: {error}", command.id),
                );
                status.lock().last_fault = Some(fault.clone());
                let _ = command.acknowledgement.send(Err(fault.clone()));
                send_auxiliary_event(
                    event_tx,
                    ReaderEvent::WriteFailed {
                        id: command.id,
                        fault: fault.clone(),
                    },
                    metrics,
                );
                send_auxiliary_event(event_tx, ReaderEvent::Fault(fault), metrics);
                break 'reader StopReason::IoFault;
            }
            metrics.record_write(command.bytes.len());
            let receipt = WriteReceipt {
                id: command.id,
                bytes: command.bytes.len(),
            };
            let _ = command.acknowledgement.send(Ok(receipt));
            send_auxiliary_event(
                event_tx,
                ReaderEvent::BytesWritten {
                    id: receipt.id,
                    bytes: receipt.bytes,
                },
                metrics,
            );
        }

        match transport.read(&mut buffer) {
            Ok(0) => break StopReason::EndOfStream,
            Ok(read) => {
                metrics.record_read(read);
                let mut overflowed = false;
                let mut emit = |item| {
                    metrics.record_frame();
                    if overflowed {
                        metrics.record_event_drop();
                        return;
                    }
                    match send_frame(event_tx, item, config.backpressure, metrics) {
                        Ok(()) => metrics.observe_queue_depth(event_tx.len()),
                        Err(()) => overflowed = true,
                    }
                };

                match decoder.push(&buffer[..read], &mut emit) {
                    Ok(delta) => metrics.record_decode_delta(delta),
                    Err(fault) => {
                        status.lock().last_fault = Some(fault.clone());
                        send_auxiliary_event(event_tx, ReaderEvent::Fault(fault), metrics);
                        break 'reader StopReason::DecoderFault;
                    }
                }
                if overflowed && !matches!(config.backpressure, BackpressurePolicy::DropNewest) {
                    let fault = ReaderFault::new(
                        ReaderFaultKind::Backpressure,
                        "reader event queue remained full",
                    );
                    status.lock().last_fault = Some(fault.clone());
                    send_auxiliary_event(event_tx, ReaderEvent::Fault(fault), metrics);
                    break StopReason::Backpressure;
                }
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::TimedOut
                        | io::ErrorKind::WouldBlock
                        | io::ErrorKind::Interrupted
                ) => {}
            Err(error) => {
                metrics.record_io_error();
                let fault = ReaderFault::new(ReaderFaultKind::Io, error.to_string());
                status.lock().last_fault = Some(fault.clone());
                send_auxiliary_event(event_tx, ReaderEvent::Fault(fault), metrics);
                break StopReason::IoFault;
            }
        }
    };

    metrics.record_decode_delta(decoder.finish());
    send_auxiliary_event(
        event_tx,
        ReaderEvent::Stopped {
            reason: reason.clone(),
        },
        metrics,
    );
    WorkerExit {
        reason,
        metrics: metrics.snapshot(),
    }
}

fn send_frame<T>(
    sender: &Sender<ReaderEvent<T>>,
    item: T,
    policy: BackpressurePolicy,
    metrics: &ReaderMetrics,
) -> Result<(), ()> {
    match policy {
        BackpressurePolicy::DropNewest | BackpressurePolicy::Stop => {
            match sender.try_send(ReaderEvent::Frame(item)) {
                Ok(()) => Ok(()),
                Err(TrySendError::Full(_)) => {
                    metrics.record_overflow();
                    metrics.record_event_drop();
                    if matches!(policy, BackpressurePolicy::DropNewest) {
                        Ok(())
                    } else {
                        Err(())
                    }
                }
                Err(TrySendError::Disconnected(_)) => {
                    metrics.record_event_drop();
                    Err(())
                }
            }
        }
        BackpressurePolicy::Wait(timeout) => {
            match sender.send_timeout(ReaderEvent::Frame(item), timeout) {
                Ok(()) => Ok(()),
                Err(crossbeam_channel::SendTimeoutError::Timeout(_)) => {
                    metrics.record_overflow();
                    metrics.record_event_drop();
                    Err(())
                }
                Err(crossbeam_channel::SendTimeoutError::Disconnected(_)) => {
                    metrics.record_event_drop();
                    Err(())
                }
            }
        }
    }
}

fn send_auxiliary_event<T>(
    sender: &Sender<ReaderEvent<T>>,
    event: ReaderEvent<T>,
    metrics: &ReaderMetrics,
) {
    if sender.try_send(event).is_err() {
        metrics.record_event_drop();
    } else {
        metrics.observe_queue_depth(sender.len());
    }
}

#[cfg(test)]
mod tests {
    use std::io::{self, Read, Write};
    use std::time::Duration;

    use super::*;
    use crate::mock::memory_transport;
    use crate::{Transport, TransportError, WriteAckError};

    #[derive(Default)]
    struct ByteDecoder;

    struct FailingWriteTransport;

    struct SlowReadTransport {
        entered_read: Arc<AtomicBool>,
    }

    impl Read for FailingWriteTransport {
        fn read(&mut self, _output: &mut [u8]) -> io::Result<usize> {
            std::thread::yield_now();
            Err(io::Error::new(io::ErrorKind::TimedOut, "test timeout"))
        }
    }

    impl Write for FailingWriteTransport {
        fn write(&mut self, _bytes: &[u8]) -> io::Result<usize> {
            Err(io::Error::other("injected write failure"))
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl Transport for FailingWriteTransport {
        fn endpoint(&self) -> &'static str {
            "failure://write"
        }

        fn set_read_timeout(&mut self, _timeout: Duration) -> Result<(), TransportError> {
            Ok(())
        }
    }

    impl Read for SlowReadTransport {
        fn read(&mut self, _output: &mut [u8]) -> io::Result<usize> {
            self.entered_read.store(true, Ordering::Release);
            thread::sleep(Duration::from_millis(100));
            Err(io::Error::new(io::ErrorKind::TimedOut, "slow test read"))
        }
    }

    impl Write for SlowReadTransport {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl Transport for SlowReadTransport {
        fn endpoint(&self) -> &'static str {
            "slow://read"
        }

        fn set_read_timeout(&mut self, _timeout: Duration) -> Result<(), TransportError> {
            Ok(())
        }
    }

    impl StreamDecoder for ByteDecoder {
        type Item = u8;

        fn push(
            &mut self,
            bytes: &[u8],
            emit: &mut dyn FnMut(Self::Item),
        ) -> Result<DecodeDelta, ReaderFault> {
            for byte in bytes {
                emit(*byte);
            }
            Ok(DecodeDelta::default())
        }
    }

    #[test]
    fn reads_fragmented_input_without_hardware() {
        let (transport, mut input) = memory_transport(4, Duration::from_millis(5)).unwrap();
        input.feed(vec![1, 2]).unwrap();
        input.feed(vec![3]).unwrap();
        input.close();

        let mut worker = ReaderWorker::spawn(
            Box::new(transport),
            ByteDecoder,
            ReaderConfig {
                event_capacity: 16,
                ..ReaderConfig::default()
            },
        )
        .unwrap();
        let exit = worker.join().unwrap();
        let frames: Vec<_> = worker
            .events()
            .try_iter()
            .filter_map(|event| match event {
                ReaderEvent::Frame(value) => Some(value),
                _ => None,
            })
            .collect();

        assert_eq!(frames, [1, 2, 3]);
        assert_eq!(exit.reason, StopReason::EndOfStream);
        assert_eq!(exit.metrics.bytes_read, 3);
        assert_eq!(exit.metrics.frames_decoded, 3);
    }

    #[test]
    fn stop_policy_reports_the_first_queue_overflow() {
        let (transport, mut input) = memory_transport(1, Duration::from_millis(5)).unwrap();
        input.feed(vec![1, 2, 3]).unwrap();
        input.close();
        let mut worker = ReaderWorker::spawn(
            Box::new(transport),
            ByteDecoder,
            ReaderConfig {
                event_capacity: 1,
                backpressure: BackpressurePolicy::Stop,
                ..ReaderConfig::default()
            },
        )
        .unwrap();

        let exit = worker.join().unwrap();
        assert_eq!(exit.reason, StopReason::Backpressure);
        assert!(exit.metrics.queue_overflows >= 1);
        assert!(exit.metrics.events_dropped >= 1);
    }

    #[test]
    fn wait_policy_times_out_and_stops_with_exact_metrics() {
        let (transport, mut input) = memory_transport(2, Duration::from_millis(5)).unwrap();
        let mut worker = ReaderWorker::spawn(
            Box::new(transport),
            ByteDecoder,
            ReaderConfig {
                event_capacity: 1,
                read_buffer_bytes: 1,
                backpressure: BackpressurePolicy::Wait(Duration::from_millis(5)),
                ..ReaderConfig::default()
            },
        )
        .unwrap();

        assert!(matches!(
            worker.events().recv_timeout(Duration::from_secs(1)),
            Ok(ReaderEvent::Started { .. })
        ));
        input.feed(vec![1]).unwrap();
        input.feed(vec![2]).unwrap();
        input.close();

        let exit = worker.join().unwrap();
        assert_eq!(exit.reason, StopReason::Backpressure);
        assert_eq!(exit.metrics.bytes_read, 2);
        assert_eq!(exit.metrics.read_calls, 2);
        assert_eq!(exit.metrics.frames_decoded, 2);
        assert_eq!(exit.metrics.queue_overflows, 1);
        // The timed-out frame, the best-effort fault, and the best-effort
        // stopped notification all encounter the frame already in the queue.
        assert_eq!(exit.metrics.events_dropped, 3);
        assert!(matches!(
            worker.events().try_recv(),
            Ok(ReaderEvent::Frame(1))
        ));
    }

    #[test]
    fn drop_newest_policy_keeps_reading_and_counts_each_dropped_frame() {
        let (transport, mut input) = memory_transport(3, Duration::from_millis(5)).unwrap();
        let mut worker = ReaderWorker::spawn(
            Box::new(transport),
            ByteDecoder,
            ReaderConfig {
                event_capacity: 1,
                read_buffer_bytes: 1,
                backpressure: BackpressurePolicy::DropNewest,
                ..ReaderConfig::default()
            },
        )
        .unwrap();

        assert!(matches!(
            worker.events().recv_timeout(Duration::from_secs(1)),
            Ok(ReaderEvent::Started { .. })
        ));
        input.feed(vec![1]).unwrap();
        input.feed(vec![2]).unwrap();
        input.feed(vec![3]).unwrap();

        let deadline = std::time::Instant::now() + Duration::from_secs(1);
        while worker.metrics().queue_overflows < 2 && std::time::Instant::now() < deadline {
            thread::yield_now();
        }
        let active_metrics = worker.metrics();
        assert_eq!(active_metrics.frames_decoded, 3);
        assert_eq!(active_metrics.queue_overflows, 2);
        assert_eq!(active_metrics.events_dropped, 2);
        assert!(matches!(
            worker.events().try_recv(),
            Ok(ReaderEvent::Frame(1))
        ));

        input.close();
        let exit = worker.join().unwrap();
        assert_eq!(exit.reason, StopReason::EndOfStream);
        assert_eq!(exit.metrics.bytes_read, 3);
        assert_eq!(exit.metrics.read_calls, 3);
        assert_eq!(exit.metrics.frames_decoded, 3);
        assert_eq!(exit.metrics.queue_overflows, 2);
        assert_eq!(exit.metrics.events_dropped, 2);
        assert!(matches!(
            worker.events().try_recv(),
            Ok(ReaderEvent::Stopped {
                reason: StopReason::EndOfStream
            })
        ));
    }

    #[test]
    fn stop_request_interrupts_timeout_driven_source() {
        let (transport, _input) = memory_transport(1, Duration::from_millis(5)).unwrap();
        let worker =
            ReaderWorker::spawn(Box::new(transport), ByteDecoder, ReaderConfig::default()).unwrap();
        let exit = worker.shutdown().unwrap();
        assert_eq!(exit.reason, StopReason::Requested);
    }

    #[test]
    fn outbound_bytes_are_written_by_the_transport_owner() {
        let (transport, input) = memory_transport(1, Duration::from_millis(5)).unwrap();
        let worker =
            ReaderWorker::spawn(Box::new(transport), ByteDecoder, ReaderConfig::default()).unwrap();
        let ticket = worker.try_send_bytes(vec![0xEB, 0x90, 0x09, 0xD7]).unwrap();
        let command_id = ticket.id();
        assert_eq!(
            ticket.wait_timeout(Duration::from_secs(1)).unwrap(),
            WriteReceipt {
                id: command_id,
                bytes: 4,
            }
        );
        let completion = worker
            .events()
            .iter()
            .find_map(|event| match event {
                ReaderEvent::BytesWritten { id, bytes } => Some((id, bytes)),
                _ => None,
            })
            .unwrap();
        assert_eq!(completion, (command_id, 4));
        assert_eq!(input.written_bytes(), [0xEB, 0x90, 0x09, 0xD7]);
        let exit = worker.shutdown().unwrap();
        assert_eq!(exit.metrics.bytes_written, 4);
        assert_eq!(exit.metrics.write_calls, 1);
        assert_eq!(exit.metrics.commands_queued, 1);
        assert_eq!(exit.metrics.commands_rejected, 0);
    }

    #[test]
    fn physical_write_failure_is_acknowledged_and_stops_reader() {
        let mut worker = ReaderWorker::spawn(
            Box::new(FailingWriteTransport),
            ByteDecoder,
            ReaderConfig::default(),
        )
        .unwrap();
        let ticket = worker.try_send_bytes(vec![1, 2, 3]).unwrap();
        let id = ticket.id();
        assert!(matches!(
            ticket.wait_timeout(Duration::from_secs(1)),
            Err(WriteAckError::Failed { id: failed_id, .. }) if failed_id == id
        ));
        let status = worker.status();
        assert_eq!(status.last_fault.unwrap().kind, ReaderFaultKind::Io);
        let exit = worker.join().unwrap();
        assert_eq!(exit.reason, StopReason::IoFault);
        assert_eq!(exit.metrics.write_calls, 0);
        assert_eq!(exit.metrics.io_errors, 1);
        assert!(worker.events().try_iter().any(
            |event| matches!(event, ReaderEvent::WriteFailed { id: failed_id, .. } if failed_id == id)
        ));
    }

    #[test]
    fn shutdown_timeout_detaches_a_non_cooperative_transport() {
        let entered_read = Arc::new(AtomicBool::new(false));
        let transport = SlowReadTransport {
            entered_read: Arc::clone(&entered_read),
        };
        let worker = ReaderWorker::spawn(
            Box::new(transport),
            ByteDecoder,
            ReaderConfig {
                shutdown_timeout: Duration::from_millis(5),
                ..ReaderConfig::default()
            },
        )
        .unwrap();
        while !entered_read.load(Ordering::Acquire) {
            thread::yield_now();
        }
        let started = std::time::Instant::now();
        assert!(matches!(
            worker.shutdown(),
            Err(ReaderError::ShutdownTimeout(timeout)) if timeout == Duration::from_millis(5)
        ));
        assert!(started.elapsed() < Duration::from_millis(50));
    }
}
