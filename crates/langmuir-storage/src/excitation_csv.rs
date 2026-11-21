//! Independent scientific CSV for decoded AB windows, in accepted arrival order.
//! CSV v2 preserves the original 22 columns and appends timing/status metadata.
use std::fs::{File, OpenOptions};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};

use langmuir_core::{EXCITATION_SAMPLE_COUNT, ExcitationRecord};
use thiserror::Error;

use crate::BatchSink;

pub const EXCITATION_CSV_HEADER: &str = "ab_frame_index,raw_record_id,udp_sequence_u32,pa0_seq,sci_frame_seq,frame_seconds,frame_microseconds,sample_index,sample_time_us,wave_mV,wave_V,f_act_mHz,f_act_Hz,work_mode,scan_freq,scan_voltage_mV,ad9833_status,dac8563_status,integrity,source,session,received_unix_nanos,interval_us,points,waveform_status\n";

#[derive(Clone, Debug)]
pub struct ExcitationCsvConfig {
    pub path: PathBuf,
    pub buffer_capacity: usize,
}

impl ExcitationCsvConfig {
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            buffer_capacity: 1024 * 1024,
        }
    }
}

#[derive(Debug, Error)]
pub enum ExcitationCsvError {
    #[error("invalid excitation CSV configuration: {0}")]
    InvalidConfiguration(String),
    #[error("could not create AB CSV directory {path}: {source}")]
    CreateDirectory {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("could not exclusively create AB CSV {path}: {source}")]
    CreateFile {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("could not initialize AB CSV: {source}; cleanup: {cleanup}")]
    Initialize {
        #[source]
        source: io::Error,
        cleanup: String,
    },
    #[error("AB CSV I/O failed: {0}")]
    Io(#[from] io::Error),
    #[error("AB CSV sink is already closed")]
    Closed,
    #[error("AB CSV capture counter exceeds u64")]
    CounterOverflow,
    #[error("invalid AB sample time at raw record {raw_record_id}, sample {sample_index}")]
    InvalidSampleTime {
        raw_record_id: u64,
        sample_index: usize,
    },
}

pub struct ExcitationCsvSink {
    path: PathBuf,
    output: CsvWriter<File>,
}

impl ExcitationCsvSink {
    /// Exclusively creates a UTF-8 CSV; existing captures are never overwritten.
    ///
    /// # Errors
    /// Returns configuration, file creation and header initialization errors.
    pub fn create(config: ExcitationCsvConfig) -> Result<Self, ExcitationCsvError> {
        if config.path.as_os_str().is_empty() || config.buffer_capacity == 0 {
            return Err(ExcitationCsvError::InvalidConfiguration(
                "output path and buffer capacity must be nonempty".to_owned(),
            ));
        }
        if let Some(parent) = config
            .path
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent).map_err(|source| {
                ExcitationCsvError::CreateDirectory {
                    path: parent.to_path_buf(),
                    source,
                }
            })?;
        }
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&config.path)
            .map_err(|source| ExcitationCsvError::CreateFile {
                path: config.path.clone(),
                source,
            })?;
        let mut writer = BufWriter::with_capacity(config.buffer_capacity, file);
        if let Err(source) = writer
            .write_all(EXCITATION_CSV_HEADER.as_bytes())
            .and_then(|()| writer.flush())
        {
            // Never retry an incomplete buffer during Drop or retain a file
            // that looks like a successfully initialized scientific capture.
            drop(writer.into_parts());
            let cleanup = match std::fs::remove_file(&config.path) {
                Ok(()) => "removed incomplete output file".to_owned(),
                Err(error) => format!("could not remove incomplete output file: {error}"),
            };
            return Err(ExcitationCsvError::Initialize { source, cleanup });
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

    #[must_use]
    pub const fn frames_written(&self) -> u64 {
        self.output.frames_written
    }

    #[must_use]
    pub const fn rows_written(&self) -> u64 {
        self.output.rows_written
    }
}

impl BatchSink<ExcitationRecord> for ExcitationCsvSink {
    type Error = ExcitationCsvError;
    fn write_batch(&mut self, records: &[ExcitationRecord]) -> Result<(), Self::Error> {
        self.output.write_batch(records)
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
    fn write_batch(&mut self, records: &[ExcitationRecord]) -> Result<(), ExcitationCsvError> {
        let writer = self.writer.as_mut().ok_or(ExcitationCsvError::Closed)?;
        if records.is_empty() {
            return Ok(());
        }
        let count =
            u64::try_from(records.len()).map_err(|_| ExcitationCsvError::CounterOverflow)?;
        let frames_written = self
            .frames_written
            .checked_add(count)
            .ok_or(ExcitationCsvError::CounterOverflow)?;
        let rows_written = frames_written
            .checked_mul(100)
            .ok_or(ExcitationCsvError::CounterOverflow)?;
        if let Err(error) = write_records(writer, records, self.frames_written) {
            self.discard_writer();
            return Err(error);
        }
        self.frames_written = frames_written;
        self.rows_written = rows_written;
        Ok(())
    }

    fn flush(&mut self) -> Result<(), ExcitationCsvError> {
        if let Err(error) = self
            .writer
            .as_mut()
            .ok_or(ExcitationCsvError::Closed)?
            .flush()
        {
            self.discard_writer();
            return Err(error.into());
        }
        Ok(())
    }

    fn close(&mut self) -> Result<(), ExcitationCsvError> {
        let mut writer = self.writer.take().ok_or(ExcitationCsvError::Closed)?;
        let result = writer.flush();
        drop(writer.into_parts());
        result.map_err(ExcitationCsvError::Io)
    }

    fn discard_writer(&mut self) {
        if let Some(writer) = self.writer.take() {
            // A failed batch may have a partial final row. Prohibit retries and
            // discard unwritten bytes, retaining only confirmed batch counts.
            drop(writer.into_parts());
        }
    }
}

fn write_records<W: Write>(
    writer: &mut W,
    records: &[ExcitationRecord],
    first_index: u64,
) -> Result<(), ExcitationCsvError> {
    // Bound formatting memory to one 100-point window. Preserve every received
    // window and its arrival order even if device time overlaps or moves back.
    let mut encoded = Vec::with_capacity(EXCITATION_SAMPLE_COUNT * 192);
    for (frame_index, record) in (first_index..).zip(records) {
        encoded.clear();
        let frame = &record.frame;
        let waveform_status = if frame.has_sample_timing() {
            "TIMED_SAMPLES"
        } else {
            "NO_SAMPLE_TIME"
        };
        for (sample_index, millivolts) in frame.wave_mv.iter().copied().enumerate() {
            let time = frame.sample_time_us(sample_index);
            if time.is_none() && frame.has_sample_timing() {
                return Err(ExcitationCsvError::InvalidSampleTime {
                    raw_record_id: record.raw_record_id,
                    sample_index,
                });
            }
            write!(
                encoded,
                "{frame_index},{},{},{},{},{},{},{sample_index},",
                record.raw_record_id,
                record.outer_sequence,
                frame.pa0_seq,
                frame.sci_frame_seq,
                frame.timestamp.utc_seconds,
                frame.timestamp.microseconds,
            )?;
            if let Some(time) = time {
                write!(encoded, "{time}")?;
            }
            write!(
                encoded,
                ",{millivolts},{}.{:03},{},{}.{:03},0x{:02X},{},{},{},{},CRC_NOT_CHECKED,",
                millivolts / 1000,
                millivolts % 1000,
                frame.f_act_mhz,
                frame.f_act_mhz / 1000,
                frame.f_act_mhz % 1000,
                frame.work_mode,
                frame.scan_freq,
                frame.scan_voltage_mv,
                frame.ad9833_status,
                frame.dac8563_status,
            )?;
            write_csv_field(&mut encoded, &record.source)?;
            encoded.push(b',');
            write_csv_field(&mut encoded, &record.session)?;
            writeln!(
                encoded,
                ",{},{},{},{waveform_status}",
                record.received_unix_nanos, frame.interval_us, frame.points
            )?;
        }
        writer.write_all(&encoded)?;
    }
    // Count only batches that reach the file boundary. This ordinary flush
    // promises neither power-loss durability nor a successful future fsync.
    writer.flush()?;
    Ok(())
}

fn write_csv_field<W: Write>(writer: &mut W, value: &str) -> io::Result<()> {
    if !value
        .bytes()
        .any(|byte| matches!(byte, b',' | b'"' | b'\r' | b'\n'))
    {
        return writer.write_all(value.as_bytes());
    }
    writer.write_all(b"\"")?;
    for (index, part) in value.split('"').enumerate() {
        if index > 0 {
            writer.write_all(b"\"\"")?;
        }
        writer.write_all(part.as_bytes())?;
    }
    writer.write_all(b"\"")
}

#[cfg(test)]
mod tests {
    use super::*;
    use langmuir_core::{decode_excitation_frame, parse_common_frame};
    use tempfile::tempdir;

    const WIRES: [&[u8; 269]; 4] = [
        include_bytes!("../../langmuir-core/tests/fixtures/excitation/SYNTHETIC-ab-triangle.bin"),
        include_bytes!("../../langmuir-core/tests/fixtures/excitation/SYNTHETIC-ab-sine.bin"),
        include_bytes!("../../langmuir-core/tests/fixtures/excitation/SYNTHETIC-ab-dc.bin"),
        include_bytes!("../../langmuir-core/tests/fixtures/excitation/SYNTHETIC-ab-all_zero.bin"),
    ];

    fn record(index: usize) -> ExcitationRecord {
        let offset = u32::try_from(index).unwrap();
        ExcitationRecord {
            frame: decode_excitation_frame(&parse_common_frame(WIRES[index]).unwrap()).unwrap(),
            outer_sequence: 0x1235_000C + offset,
            raw_record_id: 21 + u64::from(offset),
            source: "SYNTHETIC".to_owned(),
            session: "session-AB".to_owned(),
            received_unix_nanos: 1_234_567_890,
        }
    }

    #[test]
    fn all_400_rows_match_independent_python_csv_including_latest_time_and_mv() {
        // Catches any wrong sample, timestamp, metadata, voltage conversion or
        // accidentally passing only decimated display data to formal storage.
        let directory = tempdir().unwrap();
        let path = directory.path().join("nested/AB.csv");
        let mut sink = ExcitationCsvSink::create(ExcitationCsvConfig::new(&path)).unwrap();
        sink.write_batch(&[record(0), record(1)]).unwrap();
        assert_eq!((sink.frames_written(), sink.rows_written()), (2, 200));
        assert_eq!(std::fs::read_to_string(&path).unwrap().lines().count(), 201);
        sink.write_batch(&[record(2), record(3)]).unwrap();
        assert_eq!((sink.frames_written(), sink.rows_written()), (4, 400));
        assert_eq!(sink.path(), path);
        sink.close().unwrap();
        assert_eq!(
            std::fs::read_to_string(path).unwrap(),
            include_str!(
                "../../langmuir-core/tests/fixtures/excitation/SYNTHETIC-ab-expected-v2.csv"
            )
        );
    }

    #[test]
    fn actual_zero_interval_status_saves_100_raw_slots_with_blank_times_and_v2_status() {
        // Catches writing 100 fabricated equal timestamps, dropping status
        // frames, or replacing raw slots with a DC setpoint/derived waveform.
        let directory = tempdir().unwrap();
        let path = directory.path().join("actual-status.csv");
        let mut sink = ExcitationCsvSink::create(ExcitationCsvConfig::new(&path)).unwrap();
        let wire = include_bytes!(
            "../../langmuir-core/tests/fixtures/excitation/REAL-202611212141-ab-no-sample-time.bin"
        );
        let value = ExcitationRecord {
            frame: decode_excitation_frame(&parse_common_frame(wire).unwrap()).unwrap(),
            outer_sequence: 9407,
            raw_record_id: 5,
            source: "192.168.1.128:1534".to_owned(),
            session: "real-status-regression".to_owned(),
            received_unix_nanos: 1_795_268_270_230_819_300,
        };
        sink.write_batch(&[value]).unwrap();
        assert_eq!((sink.frames_written(), sink.rows_written()), (1, 100));
        sink.close().unwrap();
        let csv = std::fs::read_to_string(path).unwrap();
        let mut lines = csv.lines();
        let header = lines.next().unwrap();
        let rows: Vec<Vec<&str>> = lines.map(|line| line.split(',').collect()).collect();
        assert_eq!(rows.len(), 100);
        eprintln!(
            "actual raw AB 5 CSV: first sample_time_us={:?}, last={:?}, all_100_equal={}",
            rows[0][8],
            rows[99][8],
            rows.iter().all(|row| row[8] == rows[0][8])
        );
        assert!(
            rows.iter().all(|row| row[8].is_empty()),
            "untimed slots need blank times"
        );
        assert_eq!(
            header,
            "ab_frame_index,raw_record_id,udp_sequence_u32,pa0_seq,sci_frame_seq,frame_seconds,frame_microseconds,sample_index,sample_time_us,wave_mV,wave_V,f_act_mHz,f_act_Hz,work_mode,scan_freq,scan_voltage_mV,ad9833_status,dac8563_status,integrity,source,session,received_unix_nanos,interval_us,points,waveform_status"
        );
        for (index, row) in rows.iter().enumerate() {
            assert_eq!(row.len(), 25);
            assert_eq!(&row[..7], ["0", "5", "9407", "183", "94", "367", "141944"]);
            assert_eq!(row[7], index.to_string());
            assert_eq!(
                &row[9..19],
                [
                    "0",
                    "0.000",
                    "0",
                    "0.000",
                    "0x55",
                    "0",
                    "0",
                    "0",
                    "0",
                    "CRC_NOT_CHECKED"
                ]
            );
            assert_eq!(
                &row[19..],
                [
                    "192.168.1.128:1534",
                    "real-status-regression",
                    "1795268270230819300",
                    "0",
                    "100",
                    "NO_SAMPLE_TIME"
                ]
            );
        }
    }

    #[test]
    fn invalid_positive_interval_time_writes_no_partial_window() {
        // Catches confusing invalid timed data with an accepted untimed status.
        let directory = tempdir().unwrap();
        let path = directory.path().join("invalid-time.csv");
        let mut sink = ExcitationCsvSink::create(ExcitationCsvConfig::new(&path)).unwrap();
        let mut value = record(0);
        value.frame.timestamp.utc_seconds = 0;
        value.frame.timestamp.microseconds = 50;
        value.frame.interval_us = 1;
        assert!(matches!(
            sink.write_batch(&[value]),
            Err(ExcitationCsvError::InvalidSampleTime {
                raw_record_id: 21,
                sample_index: 0
            })
        ));
        assert_eq!((sink.frames_written(), sink.rows_written()), (0, 0));
        assert!(matches!(sink.flush(), Err(ExcitationCsvError::Closed)));
        assert_eq!(
            std::fs::read_to_string(path).unwrap(),
            EXCITATION_CSV_HEADER
        );
    }

    #[test]
    fn preserves_arrival_order_repeated_overlapping_windows_and_independent_seq_wrap() {
        // Catches sorting/deduplication by device time or coupling AA/AB seq8.
        let directory = tempdir().unwrap();
        let path = directory.path().join("AB.csv");
        let mut sink = ExcitationCsvSink::create(ExcitationCsvConfig::new(&path)).unwrap();
        sink.write_batch(&[record(1), record(0), record(0)])
            .unwrap();
        sink.close().unwrap();
        let csv = std::fs::read_to_string(path).unwrap();
        let rows: Vec<_> = csv.lines().collect();
        assert_eq!(rows.len(), 301);
        assert!(rows[1].starts_with("0,22,305463309,0,200,10,50000,0,8980800,"));
        assert!(rows[101].starts_with("1,21,305463308,255,17,9,250000,0,8180800,"));
        assert!(rows[201].starts_with("2,21,305463308,255,17,9,250000,0,8180800,"));
    }

    #[test]
    fn exclusive_creation_and_invalid_paths_never_overwrite_existing_capture() {
        // Catches truncation, silent fallback or pretending bad paths succeeded.
        let directory = tempdir().unwrap();
        let existing = directory.path().join("AB.csv");
        std::fs::write(&existing, b"keep original").unwrap();
        assert!(matches!(
            ExcitationCsvSink::create(ExcitationCsvConfig::new(&existing)),
            Err(ExcitationCsvError::CreateFile { .. })
        ));
        assert_eq!(std::fs::read(&existing).unwrap(), b"keep original");
        assert!(matches!(
            ExcitationCsvSink::create(ExcitationCsvConfig::new(existing.join("child.csv"))),
            Err(ExcitationCsvError::CreateDirectory { .. })
        ));
        assert!(matches!(
            ExcitationCsvSink::create(ExcitationCsvConfig::new("")),
            Err(ExcitationCsvError::InvalidConfiguration(_))
        ));
        let mut config = ExcitationCsvConfig::new(directory.path().join("zero.csv"));
        config.buffer_capacity = 0;
        assert!(matches!(
            ExcitationCsvSink::create(config),
            Err(ExcitationCsvError::InvalidConfiguration(_))
        ));
        assert!(!directory.path().join("zero.csv").exists());
    }

    #[test]
    fn provenance_csv_escaping_preserves_quotes_newlines_and_unicode() {
        // Catches corrupting a source/session that contains CSV delimiters.
        let directory = tempdir().unwrap();
        let path = directory.path().join("AB.csv");
        let mut sink = ExcitationCsvSink::create(ExcitationCsvConfig::new(&path)).unwrap();
        let mut value = record(0);
        value.source = "FPGA,\"device\"\nsource".to_owned();
        value.session = "会话,AB".to_owned();
        sink.write_batch(&[value]).unwrap();
        sink.close().unwrap();
        let csv = std::fs::read_to_string(path).unwrap();
        assert!(
            csv.contains(
                ",CRC_NOT_CHECKED,\"FPGA,\"\"device\"\"\nsource\",\"会话,AB\",1234567890,10800,100,TIMED_SAMPLES\n"
            )
        );
    }

    #[test]
    fn empty_batches_and_close_do_not_create_rows_or_allow_retry() {
        // Catches empty batches consuming indices or writes after the file closes.
        let directory = tempdir().unwrap();
        let path = directory.path().join("AB.csv");
        let mut sink = ExcitationCsvSink::create(ExcitationCsvConfig::new(&path)).unwrap();
        sink.write_batch(&[]).unwrap();
        sink.flush().unwrap();
        assert_eq!((sink.frames_written(), sink.rows_written()), (0, 0));
        sink.close().unwrap();
        assert_eq!(
            std::fs::read_to_string(path).unwrap(),
            EXCITATION_CSV_HEADER
        );
        assert!(matches!(
            sink.write_batch(&[]),
            Err(ExcitationCsvError::Closed)
        ));
        assert!(matches!(sink.flush(), Err(ExcitationCsvError::Closed)));
        assert!(matches!(sink.close(), Err(ExcitationCsvError::Closed)));
    }

    struct FailedOutput {
        kind: io::ErrorKind,
        on_flush: bool,
    }
    impl Write for FailedOutput {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if self.on_flush {
                Ok(bytes.len())
            } else {
                Err(self.kind.into())
            }
        }
        fn flush(&mut self) -> io::Result<()> {
            if self.on_flush {
                Err(self.kind.into())
            } else {
                Ok(())
            }
        }
    }

    #[test]
    fn disk_write_and_flush_errors_retain_kind_count_nothing_and_forbid_retry() {
        // Inject only the external I/O boundary; real formatting/buffering/
        // lifecycle must not count or retry a partially written batch.
        for kind in [
            io::ErrorKind::PermissionDenied,
            io::ErrorKind::StorageFull,
            io::ErrorKind::Other,
        ] {
            for on_flush in [false, true] {
                let mut output = CsvWriter {
                    writer: Some(BufWriter::with_capacity(
                        64,
                        FailedOutput { kind, on_flush },
                    )),
                    frames_written: 0,
                    rows_written: 0,
                };
                let ExcitationCsvError::Io(error) = output.write_batch(&[record(0)]).unwrap_err()
                else {
                    panic!("I/O kind must be retained");
                };
                assert_eq!(error.kind(), kind);
                assert_eq!((output.frames_written, output.rows_written), (0, 0));
                assert!(matches!(
                    output.write_batch(&[record(1)]),
                    Err(ExcitationCsvError::Closed)
                ));
            }
        }
    }

    #[test]
    fn explicit_flush_and_close_report_error_and_discard_failed_buffer() {
        for close in [false, true] {
            let mut output = CsvWriter {
                writer: Some(BufWriter::new(FailedOutput {
                    kind: io::ErrorKind::StorageFull,
                    on_flush: true,
                })),
                frames_written: 0,
                rows_written: 0,
            };
            let result = if close {
                output.close()
            } else {
                output.flush()
            };
            assert!(matches!(result, Err(ExcitationCsvError::Io(_))));
            assert!(matches!(output.flush(), Err(ExcitationCsvError::Closed)));
        }
    }
}
