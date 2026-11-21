//! AB history keeps one complete selected frame and a bounded error page.
//! On-demand readers rescan the prefix to preserve fragment and dedup context.
use super::{HistoryReader, UdpLvdsIngress};
use crate::bridge::ExcitationHistoryStatus;
use crossbeam_channel::{Receiver, Sender, bounded};
use langmuir_core::ExcitationRecord;
use langmuir_storage::{BatchSink, ExcitationCsvConfig, ExcitationCsvSink};
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    thread::JoinHandle,
};

const MAX_ERRORS: usize = 128;

pub(super) fn consume(ingress: &mut UdpLvdsIngress, status: &mut ExcitationHistoryStatus) {
    for record in ingress.excitation_records.drain(..) {
        status.selected_index = Some(status.frames);
        status.frames = status.frames.saturating_add(1);
        status.selected = Some(record);
    }
    for error in ingress.excitation_errors.drain(..) {
        if status.errors.len() >= MAX_ERRORS {
            status.errors.remove(0);
        }
        status.errors.push(error);
    }
}

pub(super) fn discard(ingress: &mut UdpLvdsIngress) {
    ingress.excitation_records.clear();
    ingress.excitation_errors.clear();
}

pub(super) struct FrameSelection {
    pub(super) index: u64,
    pub(super) revision: u64,
    pub(super) result: Receiver<Result<Option<ExcitationRecord>, String>>,
    cancel: Sender<()>,
    join: Option<JoinHandle<()>>,
}

impl FrameSelection {
    pub(super) fn start(
        input: &Path,
        recovery: bool,
        index: u64,
        revision: u64,
        epoch: Arc<AtomicU64>,
    ) -> std::io::Result<Self> {
        let input = input.to_owned();
        let (completed, result) = bounded(1);
        let (cancel, cancelled) = bounded(1);
        let join = std::thread::Builder::new()
            .name("lvds-history-ab-frame".to_owned())
            .spawn(move || {
                let result = read_frame(&input, recovery, index, revision, &epoch, &cancelled);
                let _ = completed.try_send(result);
            })?;
        Ok(Self {
            index,
            revision,
            result,
            cancel,
            join: Some(join),
        })
    }
}

impl Drop for FrameSelection {
    fn drop(&mut self) {
        let _ = self.cancel.try_send(());
        if self.join.as_ref().is_some_and(JoinHandle::is_finished)
            && let Some(join) = self.join.take()
        {
            let _ = join.join();
        }
    }
}

fn read_frame(
    input: &Path,
    recovery: bool,
    index: u64,
    revision: u64,
    epoch: &AtomicU64,
    cancel: &Receiver<()>,
) -> Result<Option<ExcitationRecord>, String> {
    if !cancel.is_empty() || epoch.load(Ordering::Acquire) != revision {
        return Ok(None);
    }
    let mut reader = HistoryReader::open(input, recovery)?;
    let mut ordinal = 0_u64;
    while reader.next()?.is_some() {
        if !cancel.is_empty() || epoch.load(Ordering::Acquire) != revision {
            return Ok(None);
        }
        reader.ingress.audit_rows.clear();
        reader.ingress.excitation_errors.clear();
        for record in reader.ingress.excitation_records.drain(..) {
            if ordinal == index {
                return Ok(Some(record));
            }
            ordinal = ordinal.saturating_add(1);
        }
    }
    match reader.warning {
        Some(warning) => Err(warning),
        None => Ok(None),
    }
}

pub(super) struct CsvExport {
    pub(super) result: Receiver<Result<u64, String>>,
    cancel: Sender<()>,
    join: Option<JoinHandle<()>>,
}

impl CsvExport {
    pub(super) fn start(input: &Path, path: String, recovery: bool) -> std::io::Result<Self> {
        let input = input.to_owned();
        let output = PathBuf::from(path);
        let (completed, result) = bounded(1);
        let (cancel, cancelled) = bounded(1);
        let join = std::thread::Builder::new()
            .name("lvds-history-ab-csv-export".to_owned())
            .spawn(move || {
                let outcome = export(&input, &output, recovery, &cancelled);
                let _ = completed.try_send(outcome);
            })?;
        Ok(Self {
            result,
            cancel,
            join: Some(join),
        })
    }
}

impl Drop for CsvExport {
    fn drop(&mut self) {
        let _ = self.cancel.try_send(());
        if self.join.as_ref().is_some_and(JoinHandle::is_finished)
            && let Some(join) = self.join.take()
        {
            let _ = join.join();
        }
    }
}

fn export(
    input: &Path,
    output: &Path,
    recovery: bool,
    cancel: &Receiver<()>,
) -> Result<u64, String> {
    if !cancel.is_empty() {
        return Err("AB export cancelled".to_owned());
    }
    let mut reader = HistoryReader::open(input, recovery)?;
    let mut sink = ExcitationCsvSink::create(ExcitationCsvConfig::new(output))
        .map_err(|error| error.to_string())?;
    let result = (|| {
        while reader.next()?.is_some() {
            if !cancel.is_empty() {
                return Err("AB export cancelled; the output is incomplete".to_owned());
            }
            reader.ingress.audit_rows.clear();
            reader.ingress.excitation_errors.clear();
            for record in reader.ingress.excitation_records.drain(..) {
                sink.write_batch(std::slice::from_ref(&record))
                    .map_err(|error| error.to_string())?;
            }
        }
        if let Some(warning) = reader.warning {
            return Err(warning);
        }
        Ok(sink.rows_written())
    })();
    let closed = sink.close().map_err(|error| error.to_string());
    match (result, closed) {
        (Err(error), _) | (_, Err(error)) => Err(error),
        (Ok(rows), Ok(())) => Ok(rows),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn excitation_history_cancelled_and_superseded_selection_skips_file_io() {
        let directory = tempfile::tempdir().unwrap();
        let input = directory.path().join("absent.lvdsraw");
        let (tx, cancel) = bounded(1);
        let epoch = AtomicU64::new(2);
        assert_eq!(read_frame(&input, false, 0, 1, &epoch, &cancel), Ok(None));
        tx.send(()).unwrap();
        assert_eq!(read_frame(&input, false, 0, 2, &epoch, &cancel), Ok(None));
    }

    #[test]
    fn excitation_history_cancelled_export_does_not_create_an_output() {
        let directory = tempfile::tempdir().unwrap();
        let input = directory.path().join("absent.lvdsraw");
        let output = directory.path().join("cancelled.csv");
        let (tx, cancel) = bounded(1);
        tx.send(()).unwrap();
        let error = export(&input, &output, false, &cancel).unwrap_err();
        assert!(error.contains("cancelled"));
        assert!(!output.exists());
    }
}
