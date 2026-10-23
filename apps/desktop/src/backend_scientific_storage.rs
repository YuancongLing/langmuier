//! One bounded `BatchWriter` feeds every selected output in the same order.
//! Per-file progress remains available when another output fails.

use std::{
    io,
    path::Path,
    sync::{Arc, Mutex},
};

use langmuir_core::ScientificFrame;
use langmuir_storage::{
    BatchSink, BinaryConfig, BinarySink, ScientificCsvConfig, ScientificCsvSink,
};
#[cfg(any(feature = "hdf5", feature = "hdf5-static"))]
use langmuir_storage::{Hdf5Config, Hdf5Sink};

use crate::bridge::{ScientificSaveFormat, ScientificStorageStatus};

pub(super) type CaptureProgress = Arc<Mutex<ScientificStorageStatus>>;
type OutputSink = Box<dyn BatchSink<ScientificFrame, Error = io::Error>>;

struct TrackedOutput<S> {
    sink: S,
    progress: CaptureProgress,
    count: fn(&S, &mut ScientificStorageStatus),
}

impl<S: BatchSink<ScientificFrame>> BatchSink<ScientificFrame> for TrackedOutput<S> {
    type Error = io::Error;

    fn write_batch(&mut self, frames: &[ScientificFrame]) -> io::Result<()> {
        self.sink.write_batch(frames).map_err(io::Error::other)?;
        self.sink.flush().map_err(io::Error::other)?;
        (self.count)(&self.sink, &mut self.progress.lock().unwrap());
        Ok(())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.sink.flush().map_err(io::Error::other)
    }

    fn close(&mut self) -> io::Result<()> {
        self.sink.close().map_err(io::Error::other)
    }
}

pub(super) struct ScientificCaptureSink {
    outputs: Vec<(&'static str, Option<OutputSink>)>,
    progress: CaptureProgress,
}

impl ScientificCaptureSink {
    pub(super) fn create(
        format: ScientificSaveFormat,
        stem: &Path,
        progress: CaptureProgress,
    ) -> io::Result<Self> {
        let mut capture = Self {
            outputs: Vec::with_capacity(2),
            progress,
        };
        if matches!(
            format,
            ScientificSaveFormat::Lpd | ScientificSaveFormat::LpdAndCsv
        ) {
            let path = stem.with_extension("lpd");
            let sink = BinarySink::create(BinaryConfig::new(&path)).map_err(io::Error::other)?;
            capture.add("LPD", sink, |sink, status| {
                status.lpd_frames_written = u64::try_from(sink.frames_written())
                    .expect("frame count fits u64 on supported targets");
            });
            let status = &mut *capture.progress.lock().unwrap();
            status.lpd_path = Some(path.display().to_string());
            status.path.clone_from(&status.lpd_path);
        }
        if matches!(
            format,
            ScientificSaveFormat::Csv | ScientificSaveFormat::LpdAndCsv
        ) {
            let path = stem.with_extension("csv");
            let sink = ScientificCsvSink::create(ScientificCsvConfig::new(&path))
                .map_err(io::Error::other)?;
            capture.add("CSV", sink, |sink, status| {
                status.csv_frames_written = sink.frames_written();
                status.csv_rows_written = sink.rows_written();
            });
            let status = &mut *capture.progress.lock().unwrap();
            status.csv_path = Some(path.display().to_string());
            if status.path.is_none() {
                status.path.clone_from(&status.csv_path);
            }
        }
        #[cfg(any(feature = "hdf5", feature = "hdf5-static"))]
        if format == ScientificSaveFormat::Hdf5 {
            let path = stem.with_extension("h5");
            let sink = Hdf5Sink::create(Hdf5Config::new(&path)).map_err(io::Error::other)?;
            capture.add("HDF5", sink, |_, _| {});
            capture.progress.lock().unwrap().path = Some(path.display().to_string());
        }
        Ok(capture)
    }

    fn add<S: BatchSink<ScientificFrame>>(
        &mut self,
        label: &'static str,
        sink: S,
        count: fn(&S, &mut ScientificStorageStatus),
    ) {
        self.outputs.push((
            label,
            Some(Box::new(TrackedOutput {
                sink,
                progress: Arc::clone(&self.progress),
                count,
            })),
        ));
    }

    fn record_errors(&self, errors: &[String]) -> io::Result<()> {
        if errors.is_empty() {
            return Ok(());
        }
        let message = errors.join("; ");
        let mut progress = self.progress.lock().unwrap();
        match &mut progress.error {
            Some(previous) => {
                previous.push_str("; ");
                previous.push_str(&message);
            }
            None => progress.error = Some(message.clone()),
        }
        Err(io::Error::other(message))
    }
}

impl BatchSink<ScientificFrame> for ScientificCaptureSink {
    type Error = io::Error;

    fn write_batch(&mut self, frames: &[ScientificFrame]) -> io::Result<()> {
        let mut errors = Vec::new();
        for (label, output) in &mut self.outputs {
            if let Some(sink) = output {
                // Always try the peer output, even after a failure. A flush per
                // batch makes the successful file readable without promising
                // power-loss durability or an atomic transaction between files.
                if let Err(error) = sink.write_batch(frames) {
                    errors.push(format!("{label} write: {error}"));
                }
            }
        }
        self.record_errors(&errors)
    }

    fn flush(&mut self) -> io::Result<()> {
        let mut errors = Vec::new();
        for (label, output) in &mut self.outputs {
            if let Some(sink) = output
                && let Err(error) = sink.flush()
            {
                errors.push(format!("{label} flush: {error}"));
            }
        }
        self.record_errors(&errors)
    }

    fn close(&mut self) -> io::Result<()> {
        let mut errors = Vec::new();
        for (label, output) in &mut self.outputs {
            if let Some(mut sink) = output.take()
                && let Err(error) = sink.close()
            {
                errors.push(format!("{label} close: {error}"));
            }
        }
        self.progress.lock().unwrap().closed = !self.outputs.is_empty();
        self.record_errors(&errors)
    }
}

impl Drop for ScientificCaptureSink {
    fn drop(&mut self) {
        // BatchWriter stops on a failed write/flush. Close every remaining
        // output on that path too and retain close errors in reliable state.
        let _ = self.close();
    }
}

#[cfg(test)]
mod tests {
    use langmuir_core::{SAMPLE_COUNT, Timestamp};
    use langmuir_storage::{BatchWriter, StorageServiceConfig};

    use super::*;

    #[derive(Clone, Copy)]
    enum Failure {
        Write,
        Flush,
        Close,
    }

    struct FaultyLpd {
        sink: BinarySink,
        failure: Failure,
    }

    impl BatchSink<ScientificFrame> for FaultyLpd {
        type Error = io::Error;

        fn write_batch(&mut self, frames: &[ScientificFrame]) -> io::Result<()> {
            if matches!(self.failure, Failure::Write) {
                return Err(io::Error::other("injected LPD write failure"));
            }
            self.sink.write_batch(frames).map_err(io::Error::other)
        }

        fn flush(&mut self) -> io::Result<()> {
            if matches!(self.failure, Failure::Flush) {
                return Err(io::Error::other("injected LPD flush failure"));
            }
            self.sink.flush().map_err(io::Error::other)
        }

        fn close(&mut self) -> io::Result<()> {
            self.sink.close().map_err(io::Error::other)?;
            if matches!(self.failure, Failure::Close) {
                return Err(io::Error::other("injected LPD close failure"));
            }
            Ok(())
        }
    }

    // A deterministic fault below the real BinarySink replaces only the OS
    // failure. Both file encoders and the bounded worker remain real.
    fn failing_dual_capture(
        directory: &Path,
        failure: Failure,
    ) -> (BatchWriter<ScientificFrame>, CaptureProgress) {
        let progress = Arc::new(Mutex::new(ScientificStorageStatus::default()));
        let mut sink = ScientificCaptureSink {
            outputs: Vec::new(),
            progress: Arc::clone(&progress),
        };
        sink.add(
            "LPD",
            FaultyLpd {
                sink: BinarySink::create(BinaryConfig::new(directory.join("capture.lpd"))).unwrap(),
                failure,
            },
            |output, status| {
                status.lpd_frames_written = u64::try_from(output.sink.frames_written()).unwrap();
            },
        );
        sink.add(
            "CSV",
            ScientificCsvSink::create(ScientificCsvConfig::new(directory.join("capture.csv")))
                .unwrap(),
            |output, status| {
                status.csv_frames_written = output.frames_written();
                status.csv_rows_written = output.rows_written();
            },
        );
        let writer = BatchWriter::start(sink, StorageServiceConfig::default()).unwrap();
        writer
            .submit(ScientificFrame {
                timestamp: Timestamp {
                    utc_seconds: 1,
                    microseconds: 0,
                },
                sequence: 9,
                channel_1: Box::new([100; SAMPLE_COUNT]),
                channel_2: Box::new([200; SAMPLE_COUNT]),
            })
            .unwrap();
        (writer, progress)
    }

    #[test]
    fn lpd_flush_failure_keeps_confirmed_count_zero_and_preserves_csv() {
        let directory = tempfile::tempdir().unwrap();
        let (writer, progress) = failing_dual_capture(directory.path(), Failure::Flush);
        assert!(writer.shutdown().is_err());
        let status = progress.lock().unwrap();
        assert_eq!(status.lpd_frames_written, 0);
        assert_eq!(
            (status.csv_frames_written, status.csv_rows_written),
            (1, 125)
        );
        assert!(status.closed);
        assert!(status.error.as_ref().unwrap().contains("LPD flush failure"));
        assert_eq!(
            std::fs::read_to_string(directory.path().join("capture.csv"))
                .unwrap()
                .lines()
                .count(),
            126
        );
    }

    #[test]
    fn lpd_write_failure_still_writes_csv_and_closes_both_outputs() {
        let directory = tempfile::tempdir().unwrap();
        let (writer, progress) = failing_dual_capture(directory.path(), Failure::Write);
        assert!(writer.shutdown().is_err());
        let status = progress.lock().unwrap();
        assert_eq!(status.lpd_frames_written, 0);
        assert_eq!(status.csv_rows_written, 125);
        assert!(status.closed);
        assert!(status.error.as_ref().unwrap().contains("LPD write failure"));
    }

    #[test]
    fn lpd_close_failure_retains_confirmed_counts_and_closes_csv() {
        let directory = tempfile::tempdir().unwrap();
        let (writer, progress) = failing_dual_capture(directory.path(), Failure::Close);
        assert!(writer.shutdown().is_err());
        let status = progress.lock().unwrap();
        assert_eq!(
            (status.lpd_frames_written, status.csv_frames_written),
            (1, 1)
        );
        assert_eq!(status.csv_rows_written, 125);
        assert!(status.closed);
        assert!(status.error.as_ref().unwrap().contains("LPD close failure"));
        assert_eq!(
            std::fs::read_to_string(directory.path().join("capture.csv"))
                .unwrap()
                .lines()
                .count(),
            126
        );
    }
}
