//! Independent AB business storage. Display event backpressure never feeds this writer.
use std::{
    io,
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use langmuir_core::ExcitationRecord;
use langmuir_storage::{
    BatchSink, BatchWriter, ExcitationCsvConfig, ExcitationCsvSink, StorageServiceConfig,
};

use crate::bridge::ExcitationStorageStatus;

use super::{
    BackendEvent, Controller, ExcitationDecodeFailure, LogLevel, LvdsPacketObservation,
    LvdsPacketType, UdpLvdsDisposition, UdpLvdsIngress,
};

impl UdpLvdsIngress {
    pub(super) fn observe_excitation(&mut self, observation: &LvdsPacketObservation) {
        if observation.frame.packet_type() != LvdsPacketType::Excitation {
            return;
        }
        match observation.disposition {
            UdpLvdsDisposition::Duplicate => {
                self.receive_stats.excitation_duplicates =
                    self.receive_stats.excitation_duplicates.saturating_add(1);
                return;
            }
            UdpLvdsDisposition::SequenceConflict => {
                self.receive_stats.excitation_conflicts =
                    self.receive_stats.excitation_conflicts.saturating_add(1);
                return;
            }
            UdpLvdsDisposition::Accepted => {}
        }
        match langmuir_core::decode_excitation_frame(&observation.frame) {
            Ok(frame) => {
                self.receive_stats.excitation_decoded =
                    self.receive_stats.excitation_decoded.saturating_add(1);
                let record = ExcitationRecord {
                    frame,
                    outer_sequence: observation.outer_sequence,
                    raw_record_id: observation.record_id,
                    source: observation.source.clone(),
                    session: self.audit_context["protocol_session"]
                        .as_str()
                        .unwrap_or("unavailable")
                        .to_owned(),
                    received_unix_nanos: observation.received_unix_nanos,
                };
                self.excitation_latest = Some(record.clone());
                if self.excitation_records.len() < 1024 {
                    self.excitation_records.push_back(record);
                } else {
                    self.receive_stats.excitation_business_dropped = self
                        .receive_stats
                        .excitation_business_dropped
                        .saturating_add(1);
                }
            }
            Err(error) => {
                self.receive_stats.excitation_decode_errors = self
                    .receive_stats
                    .excitation_decode_errors
                    .saturating_add(1);
                let failure = ExcitationDecodeFailure {
                    raw_record_id: observation.record_id,
                    outer_sequence: observation.outer_sequence,
                    reason: error.to_string(),
                };
                if self.audit_rows.len() < 2048 {
                    self.audit_rows.push_back(serde_json::json!({"kind":"AB_DECODE_ERROR",
                        "raw_record_id":failure.raw_record_id,"outer_sequence":failure.outer_sequence,
                        "reason":failure.reason,"context":self.audit_context}));
                }
                if self.excitation_errors.len() >= 128 {
                    self.excitation_errors.pop_front();
                }
                self.excitation_errors.push_back(failure);
            }
        }
    }
}

impl Controller {
    pub(super) fn start_excitation_csv(&mut self, path: &str) {
        match self.excitation_capture.start(Path::new(path)) {
            Ok(()) => {
                self.last_excitation_fault = None;
                self.log(LogLevel::Info, format!("AB CSV 开始保存：{path}"));
            }
            Err(error) => self.fault("AB CSV", error.to_string()),
        }
        self.publish_lvds_status();
    }

    pub(super) fn stop_excitation_csv(&mut self) {
        self.excitation_capture.stop();
        self.poll_excitation_storage();
        self.publish_lvds_status();
    }

    pub(super) fn excitation_storage_status(&self) -> ExcitationStorageStatus {
        self.excitation_capture.status()
    }

    pub(super) fn consume_excitation_records(&mut self, records: Vec<ExcitationRecord>) {
        let received = !records.is_empty();
        for record in records {
            self.excitation_capture.submit(record.clone());
            self.emit_display(
                super::LinkKind::Lvds,
                BackendEvent::ExcitationFrame(Box::new(record)),
            );
        }
        if received {
            // The critical snapshot identifies the authoritative live session
            // before display events can be delivered from the lower-priority queue.
            self.publish_lvds_status();
        }
    }

    pub(super) fn poll_excitation_storage(&mut self) {
        if let Some(error) = self.excitation_capture.status().error
            && self.last_excitation_fault.as_ref() != Some(&error)
        {
            self.last_excitation_fault = Some(error.clone());
            self.fault("AB CSV", error);
        }
    }
}

type Progress = Arc<Mutex<ExcitationStorageStatus>>;

struct TrackedSink {
    sink: ExcitationCsvSink,
    progress: Progress,
    closed: bool,
    finished: Arc<AtomicBool>,
}

impl TrackedSink {
    fn result(&self, result: io::Result<()>) -> io::Result<()> {
        if let Err(error) = &result {
            self.progress
                .lock()
                .unwrap()
                .error
                .get_or_insert_with(|| error.to_string());
        }
        result
    }
}

impl BatchSink<ExcitationRecord> for TrackedSink {
    type Error = io::Error;

    fn write_batch(&mut self, frames: &[ExcitationRecord]) -> io::Result<()> {
        let result = self.sink.write_batch(frames).map_err(io::Error::other);
        self.result(result)?;
        let mut status = self.progress.lock().unwrap();
        status.written = self.sink.frames_written();
        status.rows_written = self.sink.rows_written();
        Ok(())
    }

    fn flush(&mut self) -> io::Result<()> {
        let result = self.sink.flush().map_err(io::Error::other);
        self.result(result)
    }

    fn close(&mut self) -> io::Result<()> {
        if self.closed {
            return Ok(());
        }
        let result = self.sink.close().map_err(io::Error::other);
        self.closed = true;
        let mut progress = self.progress.lock().unwrap();
        progress.closed = result.is_ok();
        progress.active = false;
        drop(progress);
        self.result(result)
    }
}

impl Drop for TrackedSink {
    fn drop(&mut self) {
        let _ = self.close();
        self.finished.store(true, Ordering::Release);
    }
}

#[derive(Default)]
pub(super) struct ExcitationCapture {
    writer: Option<BatchWriter<ExcitationRecord>>,
    progress: Progress,
    finished: Arc<AtomicBool>,
}

impl ExcitationCapture {
    pub(super) fn start(&mut self, path: &Path) -> io::Result<()> {
        if self.writer.is_some() {
            if self.status().active {
                return Err(io::Error::other("AB CSV 已在保存，请先停止"));
            }
            self.stop();
        }
        self.progress = Arc::new(Mutex::new(ExcitationStorageStatus {
            path: Some(path.to_string_lossy().into_owned()),
            ..ExcitationStorageStatus::default()
        }));
        self.finished = Arc::new(AtomicBool::new(false));
        let create = ExcitationCsvSink::create(ExcitationCsvConfig::new(path));
        let sink = match create {
            Ok(sink) => sink,
            Err(error) => {
                self.progress.lock().unwrap().error = Some(error.to_string());
                return Err(io::Error::other(error));
            }
        };
        let writer = BatchWriter::start(
            TrackedSink {
                sink,
                progress: Arc::clone(&self.progress),
                closed: false,
                finished: Arc::clone(&self.finished),
            },
            StorageServiceConfig {
                control_timeout: Duration::from_secs(3),
                ..StorageServiceConfig::default()
            },
        )
        .map_err(|error| {
            self.progress.lock().unwrap().error = Some(error.to_string());
            io::Error::other(error)
        })?;
        self.writer = Some(writer);
        self.progress.lock().unwrap().active = true;
        Ok(())
    }

    pub(super) fn submit(&mut self, frame: ExcitationRecord) {
        let Some(writer) = &self.writer else {
            return;
        };
        let mut status = self.progress.lock().unwrap();
        match writer.submit(frame) {
            Ok(()) => status.enqueued = status.enqueued.saturating_add(1),
            Err(error) => {
                status.dropped = status.dropped.saturating_add(1);
                status.error.get_or_insert_with(|| error.to_string());
            }
        }
    }

    pub(super) fn status(&self) -> ExcitationStorageStatus {
        let mut status = self.progress.lock().unwrap().clone();
        if let Some(writer) = &self.writer {
            let state = writer.status();
            if let Some(error) = state.last_error {
                status.error = Some(error.to_string());
            }
            if state.panicked {
                status.error = Some("AB CSV 存储线程异常终止".to_owned());
            }
            if state.terminal.is_some() || state.panicked {
                status.active = false;
            }
        }
        if self.finished.load(Ordering::Acquire) {
            status.dropped = status
                .dropped
                .saturating_add(status.enqueued.saturating_sub(status.written));
        }
        status
    }

    pub(super) fn stop(&mut self) {
        self.stop_timeout(Duration::from_secs(3));
    }

    fn stop_timeout(&mut self, timeout: Duration) {
        if let Some(writer) = self.writer.take() {
            let result = writer.shutdown_timeout(timeout);
            let mut status = self.progress.lock().unwrap();
            status.active = false;
            if let Err(error) = result {
                status.error = Some(error.to_string());
            }
        }
    }
}

impl Drop for ExcitationCapture {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossbeam_channel::{Receiver, Sender, bounded};

    struct PausedSink {
        entered: Sender<()>,
        resume: Receiver<()>,
        progress: Progress,
        finished: Arc<std::sync::atomic::AtomicBool>,
    }

    impl BatchSink<ExcitationRecord> for PausedSink {
        type Error = io::Error;
        fn write_batch(&mut self, records: &[ExcitationRecord]) -> io::Result<()> {
            self.entered.send(()).unwrap();
            self.resume.recv().unwrap();
            let mut status = self.progress.lock().unwrap();
            status.written += u64::try_from(records.len()).unwrap();
            status.rows_written = status.written * 100;
            Ok(())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
        fn close(&mut self) -> io::Result<()> {
            let mut status = self.progress.lock().unwrap();
            status.closed = true;
            status.active = false;
            Ok(())
        }
    }

    impl Drop for PausedSink {
        fn drop(&mut self) {
            self.finished
                .store(true, std::sync::atomic::Ordering::Release);
        }
    }

    #[test]
    fn bounded_storage_overload_reports_rejected_record_without_waiting_for_sink() {
        let (entered_tx, entered_rx) = bounded(2);
        let (resume_tx, resume_rx) = bounded(2);
        let mut capture = ExcitationCapture::default();
        capture.progress.lock().unwrap().active = true;
        capture.writer = Some(
            BatchWriter::start(
                PausedSink {
                    entered: entered_tx,
                    resume: resume_rx,
                    progress: Arc::clone(&capture.progress),
                    finished: Arc::new(std::sync::atomic::AtomicBool::new(false)),
                },
                StorageServiceConfig {
                    queue_capacity: 1,
                    batch_size: 1,
                    ..StorageServiceConfig::default()
                },
            )
            .unwrap(),
        );
        let common = langmuir_core::parse_common_frame(include_bytes!(
            "../../../crates/langmuir-core/tests/fixtures/excitation/SYNTHETIC-ab-triangle.bin"
        ))
        .unwrap();
        let record = ExcitationRecord {
            frame: langmuir_core::decode_excitation_frame(&common).unwrap(),
            outer_sequence: 1,
            raw_record_id: 1,
            source: "SYNTHETIC".to_owned(),
            session: "bounded-storage".to_owned(),
            received_unix_nanos: 0,
        };
        capture.submit(record.clone());
        entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        capture.submit(record.clone());
        capture.submit(record);
        let status = capture.status();
        assert_eq!(status.enqueued, 2);
        assert_eq!(status.dropped, 1);
        assert!(status.error.is_some());
        resume_tx.send(()).unwrap();
        resume_tx.send(()).unwrap();
        capture.stop();
        assert_eq!(capture.status().written, 2);
        assert_eq!(capture.status().dropped, 1);
    }

    #[test]
    fn shutdown_timeout_keeps_pending_records_uncertain_until_worker_finishes() {
        let (entered_tx, entered_rx) = bounded(1);
        let (resume_tx, resume_rx) = bounded(1);
        let mut capture = ExcitationCapture::default();
        capture.progress.lock().unwrap().active = true;
        capture.writer = Some(
            BatchWriter::start(
                PausedSink {
                    entered: entered_tx,
                    resume: resume_rx,
                    progress: Arc::clone(&capture.progress),
                    finished: Arc::clone(&capture.finished),
                },
                StorageServiceConfig {
                    batch_size: 1,
                    drop_timeout: Duration::from_millis(10),
                    ..StorageServiceConfig::default()
                },
            )
            .unwrap(),
        );
        let common = langmuir_core::parse_common_frame(include_bytes!(
            "../../../crates/langmuir-core/tests/fixtures/excitation/SYNTHETIC-ab-triangle.bin"
        ))
        .unwrap();
        capture.submit(ExcitationRecord {
            frame: langmuir_core::decode_excitation_frame(&common).unwrap(),
            outer_sequence: 1,
            raw_record_id: 1,
            source: "SYNTHETIC".to_owned(),
            session: "timeout".to_owned(),
            received_unix_nanos: 0,
        });
        entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        capture.stop_timeout(Duration::from_millis(20));
        assert_eq!(capture.status().dropped, 0);
        assert!(!capture.status().closed);
        assert!(capture.status().error.is_some());
        resume_tx.send(()).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while !capture.finished.load(std::sync::atomic::Ordering::Acquire) {
            assert!(std::time::Instant::now() < deadline);
            std::thread::yield_now();
        }
        let status = capture.status();
        assert_eq!(status.written, 1);
        assert_eq!(status.dropped, 0);
        assert!(status.closed);
    }
}
