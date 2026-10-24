use std::fs::{File, OpenOptions};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};

use langmuir_core::{SAMPLE_COUNT, SAMPLE_INTERVAL_MICROS, ScientificFrame, reconstruct_adc16};
use thiserror::Error;

use crate::BatchSink;

/// UTF-8 header for paired LVDS code10 and signed reconstructed ADC samples.
pub const SCIENTIFIC_CSV_HEADER: &str = "frame_index,frame_seconds,frame_microseconds,frame_seq_u8,sample_index,sample_time_us,ch1_code10,ch2_code10,ch1_adc16_reconstructed,ch2_adc16_reconstructed,integrity\n";

const _: () = assert!(SAMPLE_COUNT == 100);

#[derive(Clone, Debug)]
pub struct ScientificCsvConfig {
    pub path: PathBuf,
    pub buffer_capacity: usize,
}

impl ScientificCsvConfig {
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            buffer_capacity: 1024 * 1024,
        }
    }
}

#[derive(Debug, Error)]
pub enum ScientificCsvError {
    #[error("invalid scientific CSV storage configuration: {0}")]
    InvalidConfiguration(String),
    #[error("could not create parent directory {path}: {source}")]
    CreateDirectory {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("could not exclusively create scientific CSV capture {path}: {source}")]
    CreateFile {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("could not initialize scientific CSV capture: {source}; cleanup: {cleanup}")]
    Initialize {
        #[source]
        source: io::Error,
        cleanup: String,
    },
    #[error("scientific CSV capture I/O failed: {0}")]
    Io(#[from] io::Error),
    #[error("scientific CSV sink is already closed")]
    Closed,
    #[error("scientific CSV capture counter exceeds u64")]
    CounterOverflow,
}

/// Buffered scientific CSV output fed directly by accepted scientific frames.
///
/// Each frame produces 100 paired code10 rows, in supplied frame order. Signed
/// reconstructed ADC values are derived from the preserved 10bit codes; they
/// are not a lossless recovery of the original 16bit ADC samples. The
/// device seconds field is preserved without assuming a calendar epoch. CRC is
/// explicitly marked as unchecked. Sorting and deduplication belong upstream.
/// A batch is counted only after its buffered bytes have successfully reached
/// the output file. This ordinary flush does not promise power-loss durability.
pub struct ScientificCsvSink {
    path: PathBuf,
    output: CsvWriter<File>,
}

impl ScientificCsvSink {
    /// Exclusively creates a capture and writes its UTF-8 CSV header.
    ///
    /// # Errors
    ///
    /// Returns configuration, directory/file creation, or header initialization
    /// errors. Existing files are never truncated or overwritten.
    pub fn create(config: ScientificCsvConfig) -> Result<Self, ScientificCsvError> {
        if config.path.as_os_str().is_empty() {
            return Err(ScientificCsvError::InvalidConfiguration(
                "output path must not be empty".to_owned(),
            ));
        }
        if config.buffer_capacity == 0 {
            return Err(ScientificCsvError::InvalidConfiguration(
                "buffer capacity must be non-zero".to_owned(),
            ));
        }
        if let Some(parent) = config
            .path
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent).map_err(|source| {
                ScientificCsvError::CreateDirectory {
                    path: parent.to_path_buf(),
                    source,
                }
            })?;
        }
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&config.path)
            .map_err(|source| ScientificCsvError::CreateFile {
                path: config.path.clone(),
                source,
            })?;
        let mut writer = BufWriter::with_capacity(config.buffer_capacity, file);
        if let Err(source) = writer
            .write_all(SCIENTIFIC_CSV_HEADER.as_bytes())
            .and_then(|()| writer.flush())
        {
            // Do not retry a failed buffer during Drop; initialization failure
            // must never leave a capture that looks successfully initialized.
            drop(writer.into_parts());
            let cleanup = match std::fs::remove_file(&config.path) {
                Ok(()) => "removed incomplete output file".to_owned(),
                Err(error) => format!("could not remove incomplete output file: {error}"),
            };
            return Err(ScientificCsvError::Initialize { source, cleanup });
        }
        Ok(Self {
            path: config.path,
            output: CsvWriter {
                writer: Some(writer),
                frames_written: 0,
                rows_written: 0,
            },
        })
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Number of complete frames in successfully flushed batches.
    #[must_use]
    pub const fn frames_written(&self) -> u64 {
        self.output.frames_written
    }

    /// Number of sample rows in successfully flushed batches, excluding header.
    #[must_use]
    pub const fn rows_written(&self) -> u64 {
        self.output.rows_written
    }
}

impl BatchSink<ScientificFrame> for ScientificCsvSink {
    type Error = ScientificCsvError;

    fn write_batch(&mut self, frames: &[ScientificFrame]) -> Result<(), Self::Error> {
        self.output.write_batch(frames)
    }

    fn flush(&mut self) -> Result<(), Self::Error> {
        self.output.flush()
    }

    fn close(&mut self) -> Result<(), Self::Error> {
        self.output.close()
    }
}

struct CsvWriter<W: Write> {
    writer: Option<BufWriter<W>>,
    frames_written: u64,
    rows_written: u64,
}

impl<W: Write> CsvWriter<W> {
    fn write_batch(&mut self, frames: &[ScientificFrame]) -> Result<(), ScientificCsvError> {
        let writer = self.writer.as_mut().ok_or(ScientificCsvError::Closed)?;
        if frames.is_empty() {
            return Ok(());
        }
        let count = u64::try_from(frames.len()).map_err(|_| ScientificCsvError::CounterOverflow)?;
        let frames_written = self
            .frames_written
            .checked_add(count)
            .ok_or(ScientificCsvError::CounterOverflow)?;
        let samples_per_frame =
            u64::try_from(SAMPLE_COUNT).map_err(|_| ScientificCsvError::CounterOverflow)?;
        let rows_written = frames_written
            .checked_mul(samples_per_frame)
            .ok_or(ScientificCsvError::CounterOverflow)?;
        let result = write_frames(writer, frames, self.frames_written);
        if let Err(error) = result {
            self.discard_writer();
            return Err(error.into());
        }
        self.frames_written = frames_written;
        self.rows_written = rows_written;
        Ok(())
    }

    fn flush(&mut self) -> Result<(), ScientificCsvError> {
        let result = self
            .writer
            .as_mut()
            .ok_or(ScientificCsvError::Closed)?
            .flush();
        if let Err(error) = result {
            self.discard_writer();
            return Err(error.into());
        }
        Ok(())
    }

    fn close(&mut self) -> Result<(), ScientificCsvError> {
        let mut writer = self.writer.take().ok_or(ScientificCsvError::Closed)?;
        let result = writer.flush();
        drop(writer.into_parts());
        result.map_err(ScientificCsvError::Io)
    }

    fn discard_writer(&mut self) {
        if let Some(writer) = self.writer.take() {
            // The failed batch can contain a partial final row. Discard its
            // unwritten buffer and prohibit retries that could duplicate rows.
            drop(writer.into_parts());
        }
    }
}

fn write_frames<W: Write>(
    writer: &mut W,
    frames: &[ScientificFrame],
    first_index: u64,
) -> io::Result<()> {
    // Reuse one frame's formatting buffer even for a long capture or large
    // batch. No rows accumulate beyond this frame and the configured BufWriter.
    let mut encoded = Vec::with_capacity(SAMPLE_COUNT * 128);
    for (frame_index, frame) in (first_index..).zip(frames) {
        encoded.clear();
        let base = u64::from(frame.timestamp.utc_seconds) * 1_000_000
            + u64::from(frame.timestamp.microseconds);
        for (sample_index, (ch1, ch2)) in
            (0_u64..).zip(frame.channel_1.iter().zip(frame.channel_2.iter()))
        {
            // Both timestamp fields are u32; all 100 samples fit comfortably
            // within u64, including the maximum u32 seconds and a second carry.
            let sample_time = base + sample_index * u64::from(SAMPLE_INTERVAL_MICROS);
            let adc1 = reconstruct_adc16(*ch1);
            let adc2 = reconstruct_adc16(*ch2);
            writeln!(
                encoded,
                "{frame_index},{},{},{},{sample_index},{sample_time},{ch1},{ch2},{adc1},{adc2},CRC_NOT_CHECKED",
                frame.timestamp.utc_seconds, frame.timestamp.microseconds, frame.frame_seq_u8
            )?;
        }
        writer.write_all(&encoded)?;
    }
    // One flush per batch, never one per ADC sample. Counters therefore report
    // confirmed writes through the OS file boundary rather than queue admission.
    writer.flush()
}

#[cfg(test)]
mod tests {
    use std::io::{self, BufRead};

    use langmuir_core::{SAMPLE_COUNT, ScientificFrame, Timestamp};
    use tempfile::tempdir;

    use super::*;

    fn frame(frame_seq_u8: u8, seconds: u32, microseconds: u32) -> ScientificFrame {
        let mut channel_1 = Box::new([0; SAMPLE_COUNT]);
        let mut channel_2 = Box::new([0; SAMPLE_COUNT]);
        for (first, second) in channel_1
            .as_chunks_mut::<4>()
            .0
            .iter_mut()
            .zip(channel_2.as_chunks_mut::<4>().0.iter_mut())
        {
            first.copy_from_slice(&[1023, 0, 512, 1]);
            second.copy_from_slice(&[0, 1023, 0, 1023]);
        }
        ScientificFrame {
            timestamp: Timestamp {
                utc_seconds: seconds,
                microseconds,
            },
            frame_seq_u8,
            channel_1,
            channel_2,
        }
    }

    #[test]
    fn single_frame_saves_100_code10_pairs_signed_reconstruction_and_integer_device_times() {
        // Catches omitted samples, swapped/normalized channels, float times,
        // a missing integrity marker, or using arrival time instead of device time.
        let directory = tempdir().unwrap();
        let path = directory.path().join("nested").join("science.csv");
        let mut sink = ScientificCsvSink::create(ScientificCsvConfig::new(&path)).unwrap();
        sink.write_batch(&[frame(42, 1_694_498_816, 999_800)])
            .unwrap();
        assert_eq!(sink.path(), path);
        assert_eq!(sink.frames_written(), 1);
        assert_eq!(sink.rows_written(), 100);
        // Successful batches have reached the file, independently of close.
        let before_close = std::fs::read_to_string(&path).unwrap();
        sink.close().unwrap();
        let csv = std::fs::read_to_string(path).unwrap();
        assert_eq!(before_close, csv);
        let lines: Vec<_> = csv.lines().collect();
        assert_eq!(lines.len(), 101);
        assert_eq!(
            lines[0],
            "frame_index,frame_seconds,frame_microseconds,frame_seq_u8,sample_index,sample_time_us,ch1_code10,ch2_code10,ch1_adc16_reconstructed,ch2_adc16_reconstructed,integrity"
        );
        assert_eq!(
            lines[1],
            "0,1694498816,999800,42,0,1694498816999800,1023,0,32704,-32768,CRC_NOT_CHECKED"
        );
        assert_eq!(
            lines[2],
            "0,1694498816,999800,42,1,1694498817000300,0,1023,-32768,32704,CRC_NOT_CHECKED"
        );
        assert_eq!(
            lines[3],
            "0,1694498816,999800,42,2,1694498817000800,512,0,0,-32768,CRC_NOT_CHECKED"
        );
        assert_eq!(
            lines[100],
            "0,1694498816,999800,42,99,1694498817049300,1,1023,-32704,32704,CRC_NOT_CHECKED"
        );
    }

    #[test]
    fn hundred_frames_stream_10000_rows_with_continuous_indices_across_batches() {
        // Catches counters or indices reset at each batch and accumulation
        // that is only written at capture close.
        let directory = tempdir().unwrap();
        let path = directory.path().join("science.csv");
        let mut config = ScientificCsvConfig::new(&path);
        config.buffer_capacity = 64;
        let mut sink = ScientificCsvSink::create(config).unwrap();
        for index in 0..100_u8 {
            sink.write_batch(&[frame(index, 10, 0)]).unwrap();
        }
        assert_eq!(sink.frames_written(), 100);
        assert_eq!(sink.rows_written(), 10_000);
        sink.close().unwrap();
        let csv = std::fs::read_to_string(path).unwrap();
        let lines: Vec<_> = csv.lines().collect();
        assert_eq!(lines.len(), 10_001);
        assert_eq!(
            lines[101],
            "1,10,0,1,0,10000000,1023,0,32704,-32768,CRC_NOT_CHECKED"
        );
        assert_eq!(
            lines[10_000],
            "99,10,0,99,99,10049500,1,1023,-32704,32704,CRC_NOT_CHECKED"
        );
    }

    #[test]
    fn long_capture_writes_bounded_batches_incrementally_before_close() {
        // Catches retaining capture rows until close. Both input generation
        // and read-back hold at most one batch or one CSV row, respectively.
        let directory = tempdir().unwrap();
        let path = directory.path().join("long-science.csv");
        let mut config = ScientificCsvConfig::new(&path);
        config.buffer_capacity = 4096;
        let mut sink = ScientificCsvSink::create(config).unwrap();
        let mut previous_bytes = std::fs::metadata(&path).unwrap().len();
        for first in (0..10_000_u16).step_by(100) {
            let batch: Vec<_> = (first..first + 100)
                .map(|index| frame(u8::try_from(index & 0xFF).unwrap(), 10, 0))
                .collect();
            sink.write_batch(&batch).unwrap();
            let bytes = std::fs::metadata(&path).unwrap().len();
            assert!(
                bytes > previous_bytes,
                "each completed batch must reach the file"
            );
            previous_bytes = bytes;
        }
        assert_eq!(sink.frames_written(), 10_000);
        assert_eq!(sink.rows_written(), 1_000_000);
        let reader = io::BufReader::new(File::open(&path).unwrap());
        let mut count = 0_u64;
        let mut last = String::new();
        for line in reader.lines() {
            last = line.unwrap();
            count += 1;
        }
        assert_eq!(count, 1_000_001);
        assert_eq!(
            last,
            "9999,10,0,15,99,10049500,1,1023,-32704,32704,CRC_NOT_CHECKED"
        );
        sink.close().unwrap();
        assert_eq!(std::fs::metadata(path).unwrap().len(), previous_bytes);
    }

    #[test]
    fn preserves_sequence_wrap_backward_timestamps_and_supplied_frame_order() {
        // The sink must not sort, drop, deduplicate, or reconstruct u32 sequences.
        let directory = tempdir().unwrap();
        let path = directory.path().join("science.csv");
        let mut sink = ScientificCsvSink::create(ScientificCsvConfig::new(&path)).unwrap();
        let last = frame(0, 1, 7);
        sink.write_batch(&[frame(u8::MAX, u32::MAX, 999_999), last.clone(), last])
            .unwrap();
        sink.close().unwrap();
        let csv = std::fs::read_to_string(path).unwrap();
        let lines: Vec<_> = csv.lines().collect();
        assert_eq!(lines.len(), 301);
        assert_eq!(
            lines[100],
            "0,4294967295,999999,255,99,4294967296049499,1,1023,-32704,32704,CRC_NOT_CHECKED"
        );
        assert_eq!(
            lines[101],
            "1,1,7,0,0,1000007,1023,0,32704,-32768,CRC_NOT_CHECKED"
        );
        assert_eq!(
            lines[201],
            "2,1,7,0,0,1000007,1023,0,32704,-32768,CRC_NOT_CHECKED"
        );
    }

    #[test]
    fn existing_file_and_invalid_destinations_report_errors_without_overwrite() {
        // Catches truncating an existing capture or claiming a bad path succeeded.
        let directory = tempdir().unwrap();
        let existing = directory.path().join("existing.csv");
        std::fs::write(&existing, b"keep capture").unwrap();
        assert!(matches!(
            ScientificCsvSink::create(ScientificCsvConfig::new(&existing)),
            Err(ScientificCsvError::CreateFile { .. })
        ));
        assert_eq!(std::fs::read(&existing).unwrap(), b"keep capture");
        assert!(matches!(
            ScientificCsvSink::create(ScientificCsvConfig::new(existing.join("child.csv"))),
            Err(ScientificCsvError::CreateDirectory { .. })
        ));
        assert!(matches!(
            ScientificCsvSink::create(ScientificCsvConfig::new(directory.path())),
            Err(ScientificCsvError::CreateFile { .. })
        ));
        assert!(matches!(
            ScientificCsvSink::create(ScientificCsvConfig::new("")),
            Err(ScientificCsvError::InvalidConfiguration(_))
        ));
        let mut config = ScientificCsvConfig::new(directory.path().join("zero.csv"));
        config.buffer_capacity = 0;
        assert!(matches!(
            ScientificCsvSink::create(config),
            Err(ScientificCsvError::InvalidConfiguration(_))
        ));
        assert!(!directory.path().join("zero.csv").exists());
    }

    #[test]
    fn empty_batches_do_not_add_rows_and_closed_sinks_reject_operations() {
        // Catches empty batches consuming a frame index and writes after closure.
        let directory = tempdir().unwrap();
        let path = directory.path().join("science.csv");
        let mut sink = ScientificCsvSink::create(ScientificCsvConfig::new(&path)).unwrap();
        sink.write_batch(&[]).unwrap();
        sink.flush().unwrap();
        assert_eq!(sink.frames_written(), 0);
        assert_eq!(sink.rows_written(), 0);
        sink.close().unwrap();
        assert_eq!(std::fs::read_to_string(path).unwrap().lines().count(), 1);
        assert!(matches!(
            sink.write_batch(&[frame(1, 1, 0)]),
            Err(ScientificCsvError::Closed)
        ));
        assert!(matches!(
            sink.write_batch(&[]),
            Err(ScientificCsvError::Closed)
        ));
        assert!(matches!(sink.flush(), Err(ScientificCsvError::Closed)));
        assert!(matches!(sink.close(), Err(ScientificCsvError::Closed)));
    }

    struct FailingOutput {
        fail_write: bool,
        fail_flush: bool,
    }

    impl io::Write for FailingOutput {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if self.fail_write {
                Err(io::Error::other("injected disk write failure"))
            } else {
                Ok(bytes.len())
            }
        }

        fn flush(&mut self) -> io::Result<()> {
            if self.fail_flush {
                Err(io::Error::other("injected flush failure"))
            } else {
                Ok(())
            }
        }
    }

    #[test]
    fn write_and_batch_flush_failures_do_not_claim_confirmed_frames_or_rows() {
        // Injection replaces only the external Write boundary: encoding,
        // buffering, state and accounting are the real production code.
        for output in [
            FailingOutput {
                fail_write: true,
                fail_flush: false,
            },
            FailingOutput {
                fail_write: false,
                fail_flush: true,
            },
        ] {
            let mut writer = CsvWriter {
                writer: Some(io::BufWriter::with_capacity(64, output)),
                frames_written: 0,
                rows_written: 0,
            };
            assert!(matches!(
                writer.write_batch(&[frame(1, 1, 0)]),
                Err(ScientificCsvError::Io(_))
            ));
            assert_eq!(writer.frames_written, 0);
            assert_eq!(writer.rows_written, 0);
            assert!(matches!(
                writer.write_batch(&[frame(2, 2, 0)]),
                Err(ScientificCsvError::Closed)
            ));
        }
    }

    #[test]
    fn permission_disk_full_and_other_write_errors_retain_kind_and_stop_capture() {
        // Catches converting an OS error into an untyped message, counting a
        // failed write as confirmed, or allowing a retry that duplicates rows.
        struct FailedWrite(io::ErrorKind);

        impl io::Write for FailedWrite {
            fn write(&mut self, _bytes: &[u8]) -> io::Result<usize> {
                Err(io::Error::from(self.0))
            }

            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }

        for kind in [
            io::ErrorKind::PermissionDenied,
            io::ErrorKind::StorageFull,
            io::ErrorKind::Other,
        ] {
            let mut writer = CsvWriter {
                writer: Some(io::BufWriter::with_capacity(64, FailedWrite(kind))),
                frames_written: 0,
                rows_written: 0,
            };
            let error = writer.write_batch(&[frame(1, 1, 0)]).unwrap_err();
            let ScientificCsvError::Io(error) = error else {
                panic!("external write failure must retain its I/O error");
            };
            assert_eq!(error.kind(), kind);
            assert_eq!(writer.frames_written, 0);
            assert_eq!(writer.rows_written, 0);
            assert!(matches!(
                writer.write_batch(&[frame(2, 2, 0)]),
                Err(ScientificCsvError::Closed)
            ));
        }
    }

    #[test]
    fn explicit_flush_and_close_failures_are_reported() {
        for close in [false, true] {
            let output = FailingOutput {
                fail_write: false,
                fail_flush: true,
            };
            let mut writer = CsvWriter {
                writer: Some(io::BufWriter::new(output)),
                frames_written: 0,
                rows_written: 0,
            };
            let result = if close {
                writer.close()
            } else {
                writer.flush()
            };
            assert!(matches!(result, Err(ScientificCsvError::Io(_))));
            assert!(matches!(writer.flush(), Err(ScientificCsvError::Closed)));
        }
    }
}
