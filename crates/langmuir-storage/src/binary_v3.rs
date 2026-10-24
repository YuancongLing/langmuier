use std::fs::{File, OpenOptions};
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};

use langmuir_core::{SAMPLE_COUNT, SAMPLE_INTERVAL_MICROS, ScientificFrame, Timestamp};
use thiserror::Error;

use crate::{BatchSink, BinaryConfig};

pub const V3_MAGIC: [u8; 8] = *b"LMPROBE\0";
pub const V3_VERSION: u16 = 3;
pub const V3_HEADER_LEN: u16 = 40;
pub const V3_ENDIAN_MARKER: u32 = 0x0102_0304;
pub const V3_SAMPLE_COUNT: u32 = 100;
pub const V3_SAMPLE_INTERVAL_MICROS: u32 = 500;
pub const V3_FRAME_LEN: u32 = 409;
pub const V3_REPRESENTATION_CODE10_U16: u32 = 1;
pub const V3_FLAG_CRC_NOT_CHECKED: u32 = 1;
pub const V3_FLAG_RAW_DEVICE_TIME: u32 = 2;
pub const V3_FLAGS: u32 = V3_FLAG_CRC_NOT_CHECKED | V3_FLAG_RAW_DEVICE_TIME;
pub const V3_CHANNEL_COUNT: u16 = 2;
pub const V3_SEQUENCE_BITS: u16 = 8;

const HEADER_BYTES: usize = 40;
const RECORD_BYTES: usize = 409;
const _: () = assert!(SAMPLE_COUNT == 100 && SAMPLE_INTERVAL_MICROS == 500);

#[derive(Debug, Error)]
pub enum BinaryV3Error {
    #[error("invalid v3 storage configuration: {0}")]
    InvalidConfiguration(String),
    #[error("could not create parent directory {path}: {source}")]
    CreateDirectory {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("could not exclusively create v3 capture {path}: {source}")]
    CreateFile {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("could not initialize v3 capture: {source}; cleanup: {cleanup}")]
    Initialize {
        #[source]
        source: io::Error,
        cleanup: String,
    },
    #[error("v3 capture I/O failed: {0}")]
    Io(#[from] io::Error),
    #[error("v3 sink is already closed")]
    Closed,
    #[error("not a Langmuir scientific capture")]
    InvalidMagic,
    #[error("unsupported scientific capture version {0}; v3 required")]
    UnsupportedVersion(u16),
    #[error("invalid v3 metadata {field}: expected {expected}, received {actual}")]
    InvalidMetadata {
        field: &'static str,
        expected: u64,
        actual: u64,
    },
    #[error("invalid device microseconds {0}; expected less than 1000000")]
    InvalidMicroseconds(u32),
    #[error("invalid channel {channel} code10 at sample {index}: {value}")]
    InvalidCode10 {
        channel: u8,
        index: usize,
        value: u16,
    },
    #[error("v3 frame counter exceeds u64")]
    CounterOverflow,
}

/// Explicitly versioned 100-point code10 capture, with little-endian integers.
///
/// The 40-byte header records version, endian marker, sample count, 500 us
/// interval, 409-byte record size, code10-u16 representation, unchecked CRC and
/// raw device-time flags, two channels, and the eight-bit inner sequence width.
/// Each record is seconds u32, microseconds u32, seq8, then 100 u16 codes for
/// channel 1 and 100 u16 codes for channel 2. No points or old sequences are
/// synthesized. A successful batch is flushed to File; this is not a guarantee
/// against power loss.
pub struct BinaryV3Sink {
    path: PathBuf,
    writer: Option<BufWriter<File>>,
    frames_written: u64,
}

impl BinaryV3Sink {
    /// Exclusively creates a v3 capture and flushes its header.
    ///
    /// # Errors
    ///
    /// Returns configuration, exclusive creation, or header I/O errors. An
    /// existing file is never truncated, overwritten, or reinterpreted as v3.
    pub fn create(config: BinaryConfig) -> Result<Self, BinaryV3Error> {
        if config.path.as_os_str().is_empty() || config.buffer_capacity == 0 {
            return Err(BinaryV3Error::InvalidConfiguration(
                "path must not be empty and buffer capacity must be non-zero".to_owned(),
            ));
        }
        if let Some(parent) = config
            .path
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent).map_err(|source| BinaryV3Error::CreateDirectory {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&config.path)
            .map_err(|source| BinaryV3Error::CreateFile {
                path: config.path.clone(),
                source,
            })?;
        let mut writer = BufWriter::with_capacity(config.buffer_capacity, file);
        if let Err(source) = writer
            .write_all(&encode_header())
            .and_then(|()| writer.flush())
        {
            drop(writer.into_parts());
            let cleanup = match std::fs::remove_file(&config.path) {
                Ok(()) => "removed incomplete output file".to_owned(),
                Err(error) => format!("could not remove incomplete output file: {error}"),
            };
            return Err(BinaryV3Error::Initialize { source, cleanup });
        }
        Ok(Self {
            path: config.path,
            writer: Some(writer),
            frames_written: 0,
        })
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Complete frames in batches successfully flushed to the output file.
    #[must_use]
    pub const fn frames_written(&self) -> u64 {
        self.frames_written
    }

    fn discard_writer(&mut self) {
        if let Some(writer) = self.writer.take() {
            drop(writer.into_parts());
        }
    }
}

impl BatchSink<ScientificFrame> for BinaryV3Sink {
    type Error = BinaryV3Error;

    fn write_batch(&mut self, frames: &[ScientificFrame]) -> Result<(), Self::Error> {
        let writer = self.writer.as_mut().ok_or(BinaryV3Error::Closed)?;
        if frames.is_empty() {
            return Ok(());
        }
        let count = u64::try_from(frames.len()).map_err(|_| BinaryV3Error::CounterOverflow)?;
        let total = self
            .frames_written
            .checked_add(count)
            .ok_or(BinaryV3Error::CounterOverflow)?;
        // Validate the full batch before writing any of it. Manually constructed
        // frames cannot smuggle invalid codes into an otherwise valid v3 file.
        for frame in frames {
            validate_frame(frame)?;
        }
        let result = (|| -> io::Result<()> {
            for frame in frames {
                writer.write_all(&encode_frame(frame))?;
            }
            writer.flush()
        })();
        if let Err(error) = result {
            self.discard_writer();
            return Err(error.into());
        }
        self.frames_written = total;
        Ok(())
    }

    fn flush(&mut self) -> Result<(), Self::Error> {
        let result = self.writer.as_mut().ok_or(BinaryV3Error::Closed)?.flush();
        if let Err(error) = result {
            self.discard_writer();
            return Err(error.into());
        }
        Ok(())
    }

    fn close(&mut self) -> Result<(), Self::Error> {
        let mut writer = self.writer.take().ok_or(BinaryV3Error::Closed)?;
        let result = writer.flush();
        drop(writer.into_parts());
        result.map_err(BinaryV3Error::Io)
    }
}

/// Streaming v3-only reader. Memory use is bounded to one 409-byte record and
/// its two decoded 100-point arrays. Legacy files require their legacy schema.
pub struct BinaryV3Reader<R: Read> {
    reader: R,
}

impl BinaryV3Reader<BufReader<File>> {
    /// Opens a v3 capture and validates every header metadata field.
    ///
    /// # Errors
    ///
    /// Returns open/header I/O errors, an unsupported version, or invalid metadata.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, BinaryV3Error> {
        Self::new(BufReader::new(File::open(path)?))
    }
}

impl<R: Read> BinaryV3Reader<R> {
    /// Reads and strictly validates a v3 header; no version guessing is used.
    ///
    /// # Errors
    ///
    /// Returns header I/O errors, an unsupported version, or invalid metadata.
    pub fn new(mut reader: R) -> Result<Self, BinaryV3Error> {
        let mut header = [0_u8; HEADER_BYTES];
        reader.read_exact(&mut header)?;
        validate_header(&header)?;
        Ok(Self { reader })
    }

    /// Reads one frame, returning `None` only at an exact record boundary.
    ///
    /// # Errors
    ///
    /// Returns truncated-record/I/O errors, invalid device time, or code10 values
    /// outside 0..=1023. Record order and device timestamps are never changed.
    pub fn next_frame(&mut self) -> Result<Option<ScientificFrame>, BinaryV3Error> {
        let mut record = [0_u8; RECORD_BYTES];
        loop {
            match self.reader.read(&mut record[..1]) {
                Ok(0) => return Ok(None),
                Ok(_) => break,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => return Err(error.into()),
            }
        }
        self.reader.read_exact(&mut record[1..])?;
        let timestamp = Timestamp {
            utc_seconds: u32::from_le_bytes([record[0], record[1], record[2], record[3]]),
            microseconds: u32::from_le_bytes([record[4], record[5], record[6], record[7]]),
        };
        let mut channel_1 = Box::new([0; SAMPLE_COUNT]);
        let mut channel_2 = Box::new([0; SAMPLE_COUNT]);
        for (sample, bytes) in channel_1
            .iter_mut()
            .chain(channel_2.iter_mut())
            .zip(record[9..].as_chunks::<2>().0.iter())
        {
            *sample = u16::from_le_bytes([bytes[0], bytes[1]]);
        }
        let frame = ScientificFrame {
            timestamp,
            frame_seq_u8: record[8],
            channel_1,
            channel_2,
        };
        validate_frame(&frame)?;
        Ok(Some(frame))
    }
}

fn validate_frame(frame: &ScientificFrame) -> Result<(), BinaryV3Error> {
    if frame.timestamp.microseconds >= 1_000_000 {
        return Err(BinaryV3Error::InvalidMicroseconds(
            frame.timestamp.microseconds,
        ));
    }
    for (channel, samples) in [(1, &frame.channel_1), (2, &frame.channel_2)] {
        for (index, &value) in samples.iter().enumerate() {
            if value > 1023 {
                return Err(BinaryV3Error::InvalidCode10 {
                    channel,
                    index,
                    value,
                });
            }
        }
    }
    Ok(())
}

fn encode_header() -> [u8; HEADER_BYTES] {
    let mut bytes = [0; HEADER_BYTES];
    bytes[..8].copy_from_slice(&V3_MAGIC);
    bytes[8..10].copy_from_slice(&V3_VERSION.to_le_bytes());
    bytes[10..12].copy_from_slice(&V3_HEADER_LEN.to_le_bytes());
    for (offset, value) in [
        (12, V3_ENDIAN_MARKER),
        (16, V3_SAMPLE_COUNT),
        (20, V3_SAMPLE_INTERVAL_MICROS),
        (24, V3_FRAME_LEN),
        (28, V3_REPRESENTATION_CODE10_U16),
        (32, V3_FLAGS),
    ] {
        bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }
    bytes[36..38].copy_from_slice(&V3_CHANNEL_COUNT.to_le_bytes());
    bytes[38..40].copy_from_slice(&V3_SEQUENCE_BITS.to_le_bytes());
    bytes
}

fn validate_header(header: &[u8; HEADER_BYTES]) -> Result<(), BinaryV3Error> {
    if header[..8] != V3_MAGIC {
        return Err(BinaryV3Error::InvalidMagic);
    }
    let version = u16::from_le_bytes([header[8], header[9]]);
    if version != V3_VERSION {
        return Err(BinaryV3Error::UnsupportedVersion(version));
    }
    let expected = encode_header();
    for (field, offset, width) in [
        ("header length", 10, 2),
        ("endian marker", 12, 4),
        ("samples per channel", 16, 4),
        ("sample interval us", 20, 4),
        ("record length", 24, 4),
        ("sample representation", 28, 4),
        ("integrity/time flags", 32, 4),
        ("channel count", 36, 2),
        ("sequence bits", 38, 2),
    ] {
        let read = |bytes: &[u8]| -> u64 {
            bytes[offset..offset + width]
                .iter()
                .enumerate()
                .fold(0, |value, (shift, byte)| {
                    value | (u64::from(*byte) << (shift * 8))
                })
        };
        let actual = read(header);
        let expected = read(&expected);
        if actual != expected {
            return Err(BinaryV3Error::InvalidMetadata {
                field,
                expected,
                actual,
            });
        }
    }
    Ok(())
}

fn encode_frame(frame: &ScientificFrame) -> [u8; RECORD_BYTES] {
    let mut bytes = [0; RECORD_BYTES];
    bytes[..4].copy_from_slice(&frame.timestamp.utc_seconds.to_le_bytes());
    bytes[4..8].copy_from_slice(&frame.timestamp.microseconds.to_le_bytes());
    bytes[8] = frame.frame_seq_u8;
    for (output, code) in bytes[9..]
        .as_chunks_mut::<2>()
        .0
        .iter_mut()
        .zip(frame.channel_1.iter().chain(frame.channel_2.iter()))
    {
        output.copy_from_slice(&code.to_le_bytes());
    }
    bytes
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use langmuir_core::{ScientificFrame, Timestamp};
    use tempfile::tempdir;

    use super::*;
    use crate::{BatchSink, BinaryConfig};

    fn golden_header() -> Vec<u8> {
        vec![
            b'L', b'M', b'P', b'R', b'O', b'B', b'E', 0, 3, 0, 40, 0, 4, 3, 2, 1, 100, 0, 0, 0,
            244, 1, 0, 0, 153, 1, 0, 0, 1, 0, 0, 0, 3, 0, 0, 0, 2, 0, 8, 0,
        ]
    }

    fn golden_file() -> Vec<u8> {
        // Independently specified LE file bytes, not encoded by either API.
        let mut bytes = golden_header();
        bytes.extend_from_slice(&[0, 0, 0, 0x65, 0x78, 0x41, 0x0F, 0, 255]);
        for _ in 0..25 {
            bytes.extend_from_slice(&[0xFF, 3, 0, 0, 0, 2, 1, 0]);
        }
        for _ in 0..25 {
            bytes.extend_from_slice(&[0, 0, 0xFF, 3, 0, 0, 0xFF, 3]);
        }
        bytes
    }

    fn frame() -> ScientificFrame {
        let mut channel_1 = Box::new([0_u16; 100]);
        let mut channel_2 = Box::new([0_u16; 100]);
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
                utc_seconds: 1_694_498_816,
                microseconds: 999_800,
            },
            frame_seq_u8: 255,
            channel_1,
            channel_2,
        }
    }

    #[test]
    fn reader_decodes_independent_v3_bytes_without_padding_or_legacy_sequence() {
        // Catches wrong LE offsets, channel swaps, record padding, or seq16.
        let mut reader = BinaryV3Reader::new(Cursor::new(golden_file())).unwrap();
        let decoded = reader.next_frame().unwrap().unwrap();
        assert_eq!(decoded, frame());
        assert_eq!(decoded.channel_1.len(), 100);
        assert_eq!(decoded.channel_2[99], 1023);
        assert!(reader.next_frame().unwrap().is_none());
    }

    #[test]
    fn sink_writes_exact_independent_v3_layout_and_closes_readably() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("capture.lpd");
        let mut sink = BinaryV3Sink::create(BinaryConfig::new(&path)).unwrap();
        sink.write_batch(&[frame()]).unwrap();
        assert_eq!(sink.frames_written(), 1);
        assert_eq!(sink.path(), path);
        assert_eq!(std::fs::read(&path).unwrap(), golden_file());
        sink.close().unwrap();
        let mut reader = BinaryV3Reader::open(&path).unwrap();
        assert_eq!(reader.next_frame().unwrap().unwrap(), frame());
        assert!(reader.next_frame().unwrap().is_none());
        assert!(sink.write_batch(&[frame()]).is_err());
        assert!(sink.flush().is_err());
        assert!(sink.close().is_err());
    }

    #[test]
    fn reader_rejects_v2_unknown_versions_and_every_mismatched_header_field() {
        // Every metadata field controls decoding or scientific interpretation.
        for offset in [0, 8, 10, 12, 16, 20, 24, 28, 32, 36, 38] {
            let mut bytes = golden_file();
            bytes[offset] ^= 1;
            assert!(
                BinaryV3Reader::new(Cursor::new(bytes)).is_err(),
                "offset {offset}"
            );
        }
        let mut v2 = golden_file();
        v2[8..10].copy_from_slice(&[2, 0]);
        assert!(BinaryV3Reader::new(Cursor::new(v2)).is_err());
    }

    #[test]
    fn reader_rejects_partial_header_partial_record_invalid_time_and_code10() {
        let bytes = golden_file();
        for end in 0..40 {
            assert!(BinaryV3Reader::new(Cursor::new(&bytes[..end])).is_err());
        }
        let mut header_only = BinaryV3Reader::new(Cursor::new(golden_header())).unwrap();
        assert!(header_only.next_frame().unwrap().is_none());
        for end in 41..449 {
            let mut reader = BinaryV3Reader::new(Cursor::new(&bytes[..end])).unwrap();
            assert!(reader.next_frame().is_err(), "truncated after {end} bytes");
        }
        for offset in [49, 247, 249, 447] {
            let mut invalid = golden_file();
            invalid[offset..offset + 2].copy_from_slice(&1024_u16.to_le_bytes());
            let mut reader = BinaryV3Reader::new(Cursor::new(invalid)).unwrap();
            assert!(reader.next_frame().is_err(), "invalid code at {offset}");
        }
        let mut invalid_time = golden_file();
        invalid_time[44..48].copy_from_slice(&1_000_000_u32.to_le_bytes());
        let mut reader = BinaryV3Reader::new(Cursor::new(invalid_time)).unwrap();
        assert!(reader.next_frame().is_err());
    }

    #[test]
    fn sink_preserves_seq8_wrap_and_device_time_rollback_in_supplied_order() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("order.lpd");
        let first = frame();
        let mut second = first.clone();
        second.frame_seq_u8 = 0;
        second.timestamp = Timestamp {
            utc_seconds: 1,
            microseconds: 7,
        };
        let mut sink = BinaryV3Sink::create(BinaryConfig::new(&path)).unwrap();
        sink.write_batch(&[first.clone(), second.clone()]).unwrap();
        sink.close().unwrap();
        let mut reader = BinaryV3Reader::open(path).unwrap();
        assert_eq!(reader.next_frame().unwrap(), Some(first));
        assert_eq!(reader.next_frame().unwrap(), Some(second));
        assert_eq!(reader.next_frame().unwrap(), None);
    }

    #[test]
    fn sink_rejects_invalid_frames_and_never_overwrites_existing_capture() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("capture.lpd");
        std::fs::write(&path, b"retain legacy capture").unwrap();
        assert!(BinaryV3Sink::create(BinaryConfig::new(&path)).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"retain legacy capture");
        let path = directory.path().join("invalid.lpd");
        let mut sink = BinaryV3Sink::create(BinaryConfig::new(&path)).unwrap();
        let mut bad_code = frame();
        bad_code.channel_2[99] = 1024;
        assert!(sink.write_batch(&[frame(), bad_code]).is_err());
        let mut bad_time = frame();
        bad_time.timestamp.microseconds = 1_000_000;
        assert!(sink.write_batch(&[bad_time]).is_err());
        assert_eq!(sink.frames_written(), 0);
        sink.close().unwrap();
        assert_eq!(std::fs::read(path).unwrap(), golden_header());
    }
}
