//! Independent, bounded JSONL provenance writer. Raw bytes remain unchanged.

use crossbeam_channel::{Receiver, Sender, bounded};
use std::{
    fs::OpenOptions,
    io::{BufWriter, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    thread::JoinHandle,
    time::Duration,
};

/// Standalone audits belong to writable user storage, independent of source
/// files and the launch working directory. Raw archive sidecars keep their path.
pub(super) fn standalone_path(base: &Path) -> std::io::Result<PathBuf> {
    static NEXT_AUDIT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    if !base.is_absolute() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "standalone audit base directory must be absolute",
        ));
    }
    let directory = base.join("Langmuier").join("audits");
    std::fs::create_dir_all(&directory)?;
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(std::io::Error::other)?
        .as_nanos();
    let id = NEXT_AUDIT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    Ok(directory.join(format!("lvds-recovery-{timestamp}-{id}.audit.jsonl")))
}

#[derive(Default, Clone)]
pub(super) struct AuditStatus {
    pub dropped: u64,
    pub error: Option<String>,
}

pub(super) struct RecoveryAudit {
    sender: Option<Sender<serde_json::Value>>,
    status: Arc<Mutex<AuditStatus>>,
    finish: Arc<std::sync::atomic::AtomicBool>,
    done: Receiver<()>,
    join: Option<JoinHandle<()>>,
}

impl RecoveryAudit {
    pub(super) fn start(path: &Path) -> std::io::Result<Self> {
        let file = OpenOptions::new().create_new(true).write(true).open(path)?;
        let mut writer = BufWriter::new(file);
        writeln!(
            writer,
            "{}",
            serde_json::json!({"kind":"AUDIT_HEADER","version":1,"bytes":"uppercase hex; normalized prefixes never padded"})
        )?;
        writer.flush()?;
        let (sender, records) = bounded::<serde_json::Value>(128);
        let (completed, done) = bounded(1);
        let status = Arc::new(Mutex::new(AuditStatus::default()));
        let worker_status = status.clone();
        let finish = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let worker_finish = finish.clone();
        let join = std::thread::Builder::new().name("lvds-recovery-audit".to_owned()).spawn(move || {
            let write_result = (|| -> std::io::Result<()> {
                for record in records { writeln!(writer, "{record}")?; }
                let state = worker_status.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
                let complete = worker_finish.load(std::sync::atomic::Ordering::Acquire) && state.dropped == 0 && state.error.is_none();
                writeln!(writer, "{}", serde_json::json!({"kind":"AUDIT_END","complete":complete,"dropped":state.dropped,"error":state.error}))?;
                writer.flush()
            })();
            if let Err(error) = write_result {
                worker_status.lock().unwrap_or_else(std::sync::PoisonError::into_inner).error = Some(error.to_string());
            }
            // Discard any unwritten buffer after failure; Drop must not retry.
            drop(writer.into_parts());
            let _ = completed.try_send(());
        })?;
        Ok(Self {
            sender: Some(sender),
            status,
            finish,
            done,
            join: Some(join),
        })
    }

    pub(super) fn record(&mut self, record: serde_json::Value) {
        if self
            .sender
            .as_ref()
            .is_none_or(|sender| sender.try_send(record).is_err())
        {
            let mut state = self
                .status
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.dropped = state.dropped.saturating_add(1);
            state.error.get_or_insert_with(|| {
                "recovery audit queue overflow or writer unavailable".to_owned()
            });
        }
    }

    /// File readers may wait for this independent bounded queue. Live receive
    /// always uses `record`, which never waits for storage.
    pub(super) fn record_wait(
        &mut self,
        mut record: serde_json::Value,
        cancel: &Receiver<()>,
    ) -> bool {
        let Some(sender) = &self.sender else {
            self.record_lost("audit writer is unavailable");
            return false;
        };
        while cancel.is_empty() {
            match sender.send_timeout(record, Duration::from_millis(20)) {
                Ok(()) => return true,
                Err(crossbeam_channel::SendTimeoutError::Timeout(value)) => record = value,
                Err(crossbeam_channel::SendTimeoutError::Disconnected(_)) => {
                    self.record_lost("audit writer is unavailable");
                    return false;
                }
            }
        }
        self.record_lost("audit cancelled before the pending record was written");
        false
    }

    fn record_lost(&self, error: &str) {
        let mut state = self
            .status
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.dropped = state.dropped.saturating_add(1);
        state.error.get_or_insert_with(|| error.to_owned());
    }

    pub(super) fn mark_failed(&self, error: impl Into<String>) {
        self.status
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .error
            .get_or_insert_with(|| error.into());
    }

    pub(super) fn status(&self) -> AuditStatus {
        self.status
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    pub(super) fn stop(mut self) -> AuditStatus {
        self.finish
            .store(true, std::sync::atomic::Ordering::Release);
        self.sender.take();
        if self.done.recv_timeout(Duration::from_secs(3)).is_ok() {
            if let Some(join) = self.join.take() {
                let _ = join.join();
            }
        } else {
            self.status
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .error = Some("recovery audit close timed out; incomplete".to_owned());
        }
        self.status()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standalone_audit_path_creates_only_the_explicit_base_and_reports_directory_errors() {
        let directory = tempfile::tempdir().unwrap();
        let base = directory.path().join("SYNTHETIC-user-storage");
        let path = standalone_path(&base).unwrap();
        assert_eq!(
            path.parent(),
            Some(base.join("Langmuier").join("audits").as_path())
        );
        let audit = RecoveryAudit::start(&path).unwrap();
        audit.stop();
        let blocked = directory.path().join("SYNTHETIC-file-blocks-directory");
        std::fs::write(&blocked, b"existing file").unwrap();
        assert!(standalone_path(&blocked).is_err());
        assert!(standalone_path(Path::new("relative-data")).is_err());
    }

    #[test]
    fn standalone_audit_enable_and_raw_stop_use_the_user_audit_directory() {
        let (mut controller, _critical, _display) = super::super::e2e_tests::controller();
        let expected_directory = std::env::var_os("LOCALAPPDATA")
            .map_or_else(std::env::temp_dir, std::path::PathBuf::from)
            .join("Langmuier")
            .join("audits");
        controller.set_recovery_enabled(true);
        assert!(controller.recovery_enabled);
        let enabled_path =
            std::path::PathBuf::from(controller.recovery_status.audit_path.as_ref().unwrap());
        assert!(
            enabled_path.is_absolute(),
            "standalone recovery must not depend on the working directory"
        );
        assert_eq!(enabled_path.parent(), Some(expected_directory.as_path()));
        controller.stop_raw_recording();
        let stopped_path =
            std::path::PathBuf::from(controller.recovery_status.audit_path.as_ref().unwrap());
        assert_eq!(stopped_path.parent(), Some(expected_directory.as_path()));
        assert_ne!(
            enabled_path, stopped_path,
            "restarting the audit exclusively creates a new file"
        );
        assert!(controller.recovery_audit.is_some());
        controller.recovery_audit.take().unwrap().stop();
    }

    #[test]
    fn audit_retains_reproducible_bytes_and_a_clean_close_boundary() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("SYNTHETIC-audit.jsonl");
        let mut audit = RecoveryAudit::start(&path).unwrap();
        audit.record(
            serde_json::json!({"raw_record_id":7,"payload_hex":"00FF","quality":"REJECTED"}),
        );
        audit.stop();
        let text = std::fs::read_to_string(&path).unwrap();
        let rows: Vec<serde_json::Value> = text
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(rows[1]["payload_hex"], "00FF");
        assert_eq!(rows.last().unwrap()["kind"], "AUDIT_END");
        assert_eq!(rows.last().unwrap()["complete"], true);
        assert!(RecoveryAudit::start(&path).is_err());
    }

    #[test]
    fn cancelled_pending_audit_record_marks_the_footer_incomplete() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("SYNTHETIC-cancelled-audit.jsonl");
        let mut audit = RecoveryAudit::start(&path).unwrap();
        let (cancel, cancelled) = bounded(1);
        cancel.send(()).unwrap();
        assert!(!audit.record_wait(serde_json::json!({"kind":"PENDING"}), &cancelled));
        let status = audit.stop();
        assert!(
            status.error.is_some(),
            "a skipped pending row must retain an error"
        );
        assert_eq!(status.dropped, 1);
        let text = std::fs::read_to_string(&path).unwrap();
        let footer: serde_json::Value = serde_json::from_str(text.lines().last().unwrap()).unwrap();
        assert_eq!(footer["complete"], false);
    }
}
