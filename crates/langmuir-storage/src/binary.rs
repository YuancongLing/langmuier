use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use langmuir_core::{SAMPLE_COUNT, SAMPLE_INTERVAL_MICROS, ScientificFrame};
use thiserror::Error;

use crate::BatchSink;

pub const BINARY_MAGIC: [u8; 8] = *b"LMPROBE\0";
pub const BINARY_VERSION: u16 = 1;
pub const BINARY_HEADER_LEN: u16 = 32;
pub const BINARY_ENDIAN_MARKER: u32 = 0x0102_0304;
pub const BINARY_SAMPLE_COUNT: u32 = 1_000;
pub const BINARY_FRAME_LEN: u32 = 4 * 4 + 2 * BINARY_SAMPLE_COUNT * 4;
pub const BINARY_FLAGS: u32 = 1;

const _: () = assert!(SAMPLE_COUNT == 1_000);

#[derive(Clone, Debug)]
pub struct BinaryConfig {
    pub path: PathBuf,
    pub buffer_capacity: usize,
}

impl BinaryConfig {
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            buffer_capacity: 1024 * 1024,
        }
    }
}

#[derive(Debug, Error)]
pub enum BinaryError {
    #[error("invalid binary storage configuration: {0}")]
    InvalidConfiguration(String),
    #[error("could not create parent directory {path}: {source}")]
    CreateDirectory {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("could not exclusively create binary capture {path}: {source}")]
    CreateFile {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("could not initialize binary capture: {source}; cleanup: {cleanup}")]
    Initialize {
        #[source]
        source: std::io::Error,
        cleanup: String,
    },
    #[error("binary capture I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("binary sink is already closed")]
    Closed,
}

/// Portable, dependency-free scientific-frame sink.
///
/// All integer fields are little-endian. Files are opened with `create_new`,
/// so an existing capture is never truncated or overwritten.
pub struct BinarySink {
    path: PathBuf,
    writer: Option<BufWriter<File>>,
    frames_written: usize,
}

impl BinarySink {
    /// Exclusively creates a versioned `.lpd` capture and writes its header.
    ///
    /// # Errors
    ///
    /// Returns invalid configuration, directory/file creation, initialization,
    /// or I/O errors. Existing files are never overwritten.
    pub fn create(config: BinaryConfig) -> Result<Self, BinaryError> {
        validate_config(&config)?;
        if let Some(parent) = config
            .path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent).map_err(|source| BinaryError::CreateDirectory {
                path: parent.to_path_buf(),
                source,
            })?;
        }

        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&config.path)
            .map_err(|source| BinaryError::CreateFile {
                path: config.path.clone(),
                source,
            })?;
        let mut writer = BufWriter::with_capacity(config.buffer_capacity, file);
        if let Err(source) = write_header(&mut writer) {
            drop(writer);
            let cleanup = match std::fs::remove_file(&config.path) {
                Ok(()) => "removed incomplete output file".to_owned(),
                Err(error) => format!("could not remove incomplete output file: {error}"),
            };
            return Err(BinaryError::Initialize { source, cleanup });
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

    #[must_use]
    pub const fn frames_written(&self) -> usize {
        self.frames_written
    }
}

impl BatchSink<ScientificFrame> for BinarySink {
    type Error = BinaryError;

    fn write_batch(&mut self, frames: &[ScientificFrame]) -> Result<(), Self::Error> {
        if frames.is_empty() {
            return Ok(());
        }
        let writer = self.writer.as_mut().ok_or(BinaryError::Closed)?;
        let mut encoded = Vec::with_capacity(frames.len() * BINARY_FRAME_LEN as usize);
        for frame in frames {
            encoded.extend_from_slice(&frame.channel_1_time.utc_seconds.to_le_bytes());
            encoded.extend_from_slice(&frame.channel_1_time.microseconds.to_le_bytes());
            encoded.extend_from_slice(&frame.channel_2_time.utc_seconds.to_le_bytes());
            encoded.extend_from_slice(&frame.channel_2_time.microseconds.to_le_bytes());
            for sample in frame.channel_1.iter() {
                encoded.extend_from_slice(&sample.to_le_bytes());
            }
            for sample in frame.channel_2.iter() {
                encoded.extend_from_slice(&sample.to_le_bytes());
            }
        }
        writer.write_all(&encoded)?;
        self.frames_written += frames.len();
        Ok(())
    }

    fn flush(&mut self) -> Result<(), Self::Error> {
        self.writer.as_mut().ok_or(BinaryError::Closed)?.flush()?;
        Ok(())
    }

    fn close(&mut self) -> Result<(), Self::Error> {
        let mut writer = self.writer.take().ok_or(BinaryError::Closed)?;
        writer.flush()?;
        drop(writer);
        Ok(())
    }
}

fn write_header(writer: &mut BufWriter<File>) -> Result<(), std::io::Error> {
    writer.write_all(&BINARY_MAGIC)?;
    writer.write_all(&BINARY_VERSION.to_le_bytes())?;
    writer.write_all(&BINARY_HEADER_LEN.to_le_bytes())?;
    writer.write_all(&BINARY_ENDIAN_MARKER.to_le_bytes())?;
    writer.write_all(&BINARY_SAMPLE_COUNT.to_le_bytes())?;
    writer.write_all(&SAMPLE_INTERVAL_MICROS.to_le_bytes())?;
    writer.write_all(&BINARY_FRAME_LEN.to_le_bytes())?;
    writer.write_all(&BINARY_FLAGS.to_le_bytes())?;
    Ok(())
}

fn validate_config(config: &BinaryConfig) -> Result<(), BinaryError> {
    if config.path.as_os_str().is_empty() {
        return Err(BinaryError::InvalidConfiguration(
            "output path must not be empty".to_owned(),
        ));
    }
    if config.buffer_capacity == 0 {
        return Err(BinaryError::InvalidConfiguration(
            "buffer capacity must be non-zero".to_owned(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::io::Read;

    use langmuir_core::Timestamp;
    use tempfile::tempdir;

    use super::*;

    fn frame(index: u32) -> ScientificFrame {
        let mut channel_1 = Box::new([0_u32; SAMPLE_COUNT]);
        let mut channel_2 = Box::new([0_u32; SAMPLE_COUNT]);
        for sample_index in 0..SAMPLE_COUNT {
            let sample = u32::try_from(sample_index).expect("test sample index fits u32");
            channel_1[sample_index] = index * 10_000 + sample;
            channel_2[sample_index] = 1_000_000 + index * 10_000 + sample;
        }
        ScientificFrame {
            channel_1_time: Timestamp {
                utc_seconds: 1_700_000_000 + index,
                microseconds: 123_456,
            },
            channel_2_time: Timestamp {
                utc_seconds: 1_700_000_001 + index,
                microseconds: 654_321,
            },
            channel_1,
            channel_2,
        }
    }

    fn le_u32(bytes: &[u8], offset: usize) -> u32 {
        u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
    }

    #[test]
    fn header_and_frames_round_trip_from_documented_layout() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("capture.lpd");
        let mut sink = BinarySink::create(BinaryConfig::new(&path)).unwrap();
        sink.write_batch(&[frame(0), frame(1)]).unwrap();
        sink.close().unwrap();

        let mut bytes = Vec::new();
        File::open(path).unwrap().read_to_end(&mut bytes).unwrap();
        assert_eq!(&bytes[..8], &BINARY_MAGIC);
        assert_eq!(u16::from_le_bytes(bytes[8..10].try_into().unwrap()), 1);
        assert_eq!(u16::from_le_bytes(bytes[10..12].try_into().unwrap()), 32);
        assert_eq!(le_u32(&bytes, 12), BINARY_ENDIAN_MARKER);
        assert_eq!(le_u32(&bytes, 16), BINARY_SAMPLE_COUNT);
        assert_eq!(le_u32(&bytes, 20), 10);
        assert_eq!(le_u32(&bytes, 24), BINARY_FRAME_LEN);
        assert_eq!(le_u32(&bytes, 28), BINARY_FLAGS);
        assert_eq!(bytes.len(), 32 + 2 * BINARY_FRAME_LEN as usize);

        let first = BINARY_HEADER_LEN as usize;
        assert_eq!(le_u32(&bytes, first), 1_700_000_000);
        assert_eq!(le_u32(&bytes, first + 4), 123_456);
        assert_eq!(le_u32(&bytes, first + 8), 1_700_000_001);
        assert_eq!(le_u32(&bytes, first + 12), 654_321);
        assert_eq!(le_u32(&bytes, first + 16), 0);
        assert_eq!(le_u32(&bytes, first + 16 + 999 * 4), 999);
        assert_eq!(le_u32(&bytes, first + 16 + SAMPLE_COUNT * 4), 1_000_000);

        let second = first + BINARY_FRAME_LEN as usize;
        assert_eq!(le_u32(&bytes, second), 1_700_000_001);
        assert_eq!(le_u32(&bytes, second + 16), 10_000);
    }

    #[test]
    fn never_overwrites_an_existing_file() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("existing.lpd");
        std::fs::write(&path, b"keep me").unwrap();
        assert!(BinarySink::create(BinaryConfig::new(&path)).is_err());
        assert_eq!(std::fs::read(path).unwrap(), b"keep me");
    }
}
