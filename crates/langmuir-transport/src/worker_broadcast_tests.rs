use std::io::{self, Read, Write};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use crossbeam_channel::{Receiver, Sender, bounded};
use parking_lot::Mutex;

use super::tests::ByteDecoder;
use super::*;
use crate::{Transport, TransportError};

struct GateTransport {
    first_read: Option<(Sender<()>, Receiver<()>)>,
    first_write: Option<(Sender<()>, Receiver<()>)>,
    bytes: Arc<Mutex<Vec<u8>>>,
}

impl Read for GateTransport {
    fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
        if let Some((entered, release)) = self.first_read.take() {
            entered.send(()).unwrap();
            release.recv_timeout(Duration::from_secs(2)).unwrap();
        } else {
            std::thread::sleep(Duration::from_millis(1));
        }
        Err(io::ErrorKind::TimedOut.into())
    }
}

impl Write for GateTransport {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if let Some((entered, release)) = self.first_write.take() {
            entered.send(()).unwrap();
            release.recv_timeout(Duration::from_secs(2)).unwrap();
        }
        let count = bytes.len().min(2);
        self.bytes.lock().extend_from_slice(&bytes[..count]);
        Ok(count)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Transport for GateTransport {
    fn endpoint(&self) -> &'static str {
        "controlled://star-broadcast-writer"
    }

    fn set_read_timeout(&mut self, _: Duration) -> Result<(), TransportError> {
        Ok(())
    }
}

fn gated_worker(
    write_gate: Option<(Sender<()>, Receiver<()>)>,
) -> (ReaderWorker<u8>, Sender<()>, Arc<Mutex<Vec<u8>>>) {
    let (entered, read_entered) = bounded(1);
    let (release, read_release) = bounded(1);
    let bytes = Arc::new(Mutex::new(Vec::new()));
    let worker = ReaderWorker::spawn(
        Box::new(GateTransport {
            first_read: Some((entered, read_release)),
            first_write: write_gate,
            bytes: Arc::clone(&bytes),
        }),
        ByteDecoder,
        ReaderConfig::default(),
    )
    .unwrap();
    read_entered.recv_timeout(Duration::from_secs(1)).unwrap();
    (worker, release, bytes)
}

#[test]
fn cancelled_broadcast_is_never_written_and_does_not_disconnect_writer() {
    let (worker, release, bytes) = gated_worker(None);
    let cancel = BroadcastCancellation::default();
    let ticket = worker
        .try_send_broadcast(vec![0xEB, 0x90, 1, 2], cancel.clone())
        .unwrap();
    cancel.cancel();
    let control = worker.try_send_bytes(vec![9, 8, 7]).unwrap();
    release.send(()).unwrap();
    assert!(matches!(
        ticket.wait_timeout(Duration::from_secs(1)),
        Err(WriteAckError::Cancelled { .. })
    ));
    control.wait_timeout(Duration::from_secs(1)).unwrap();
    assert_eq!(&*bytes.lock(), &[9, 8, 7]);
    let exit = worker.shutdown().unwrap();
    assert_eq!(exit.reason, StopReason::Requested);
    assert_eq!(exit.metrics.io_errors, 0);
}

#[test]
fn bounded_broadcasts_leave_control_capacity_and_partial_writes_never_interleave() {
    let (worker, release, bytes) = gated_worker(None);
    let mut broadcasts = Vec::new();
    for marker in 1..=3 {
        broadcasts.push(
            worker
                .try_send_broadcast(
                    vec![0xEB, 0x90, marker, marker, marker],
                    BroadcastCancellation::default(),
                )
                .unwrap(),
        );
    }
    assert!(matches!(
        worker.try_send_broadcast(vec![4], BroadcastCancellation::default()),
        Err(CommandQueueError::QueueFull(_))
    ));
    assert_eq!(worker.broadcast_queue_depth(), 3);
    let control = worker.try_send_bytes(vec![0xEB, 0x90, 0x21]).unwrap();
    release.send(()).unwrap();
    control.wait_timeout(Duration::from_secs(1)).unwrap();
    for ticket in broadcasts {
        ticket.wait_timeout(Duration::from_secs(1)).unwrap();
    }
    assert_eq!(
        &*bytes.lock(),
        &[
            0xEB, 0x90, 0x21, 0xEB, 0x90, 1, 1, 1, 0xEB, 0x90, 2, 2, 2, 0xEB, 0x90, 3, 3, 3
        ]
    );
    worker.shutdown().unwrap();
}

#[test]
fn cancellation_waits_for_started_whole_frame_and_prevents_next_start() {
    let (write_entered, entered) = bounded(1);
    let (write_release, release_write) = bounded(1);
    let (worker, release_read, bytes) = gated_worker(Some((write_entered, release_write)));
    let cancel = BroadcastCancellation::default();
    let first = worker
        .try_send_broadcast(vec![1, 2, 3, 4, 5], cancel.clone())
        .unwrap();
    let second = worker
        .try_send_broadcast(vec![6, 7, 8], cancel.clone())
        .unwrap();
    release_read.send(()).unwrap();
    entered.recv_timeout(Duration::from_secs(1)).unwrap();
    let (cancelled, cancelled_rx) = bounded(1);
    let join = std::thread::spawn(move || {
        cancel.cancel();
        cancelled.send(()).unwrap();
    });
    assert!(
        cancelled_rx
            .recv_timeout(Duration::from_millis(10))
            .is_err()
    );
    write_release.send(()).unwrap();
    cancelled_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    join.join().unwrap();
    first.wait_timeout(Duration::from_secs(1)).unwrap();
    assert!(matches!(
        second.wait_timeout(Duration::from_secs(1)),
        Err(WriteAckError::Cancelled { .. })
    ));
    assert_eq!(&*bytes.lock(), &[1, 2, 3, 4, 5]);
    worker.shutdown().unwrap();
}

#[test]
fn deferred_encoding_uses_writer_start_time_and_receipt_contains_actual_bytes() {
    let (worker, release, bytes) = gated_worker(None);
    let enqueued = Instant::now();
    let (encoded_tx, encoded_rx) = bounded(1);
    let ticket = worker
        .try_send_timed_broadcast(BroadcastCancellation::default(), move |time| {
            encoded_tx.send(time).unwrap();
            Ok(vec![0xEB, 0x90, 0xFF, 0x11])
        })
        .unwrap();
    assert!(encoded_rx.is_empty());
    let before_release = SystemTime::now();
    release.send(()).unwrap();
    let receipt = ticket.wait_timeout(Duration::from_secs(1)).unwrap();
    let encoded_at = encoded_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    assert!(encoded_at >= before_release);
    assert_eq!(encoded_at, receipt.started_at);
    assert!(receipt.started >= enqueued);
    assert!(receipt.completed >= receipt.started);
    assert_eq!(receipt.frame, *bytes.lock());
    assert_eq!(receipt.frame, [0xEB, 0x90, 0xFF, 0x11]);
    worker.shutdown().unwrap();
}
