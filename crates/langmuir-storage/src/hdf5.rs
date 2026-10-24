use std::path::{Path, PathBuf};

use crate::{LEGACY_SAMPLE_COUNT_V2 as SAMPLE_COUNT, LegacyScientificFrameV2};
use ::hdf5::{Attribute, Dataset, File};
use ndarray::Array2;
use thiserror::Error;

use crate::BatchSink;

#[derive(Clone, Debug)]
pub struct Hdf5Config {
    pub path: PathBuf,
    pub chunk_frames: usize,
    pub compression_level: Option<u8>,
}

impl Hdf5Config {
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            chunk_frames: 1_000,
            compression_level: Some(4),
        }
    }
}

#[derive(Debug, Error)]
pub enum Hdf5Error {
    #[error("invalid HDF5 configuration: {0}")]
    InvalidConfiguration(String),
    #[error("could not create parent directory {path}: {source}")]
    CreateDirectory {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("HDF5 operation failed: {0}")]
    Hdf5(#[from] ::hdf5::Error),
    #[error("HDF5 initialization failed: {source}; cleanup: {cleanup}")]
    Initialize {
        #[source]
        source: ::hdf5::Error,
        cleanup: String,
    },
    #[error("HDF5 batch commit failed: {source}; rollback: {rollback}")]
    BatchCommit {
        #[source]
        source: ::hdf5::Error,
        rollback: String,
    },
    #[error("sample value {value} cannot be represented by Python-compatible int32")]
    SampleOutOfRange { value: u32 },
    #[error("HDF5 sink is already closed")]
    Closed,
}

/// HDF5 sink with the same dataset names, dimensions, and scalar types as the
/// optimized Python logger. This legacy `[N,125]` writer accepts only
/// [`LegacyScientificFrameV2`]. New 100-point code10 scientific acquisition is
/// intentionally unsupported; no padding or schema reinterpretation occurs.
pub struct Hdf5Sink {
    path: PathBuf,
    file: Option<File>,
    time_dataset: Option<Dataset>,
    sequence_dataset: Option<Dataset>,
    channel_1_dataset: Option<Dataset>,
    channel_2_dataset: Option<Dataset>,
    committed_frames: Option<Attribute>,
    frames_written: usize,
}

impl Hdf5Sink {
    /// Exclusively creates the Python-compatible datasets and commit marker.
    ///
    /// # Errors
    ///
    /// Returns configuration, directory, HDF5 initialization, or cleanup
    /// errors. Existing files are never overwritten.
    pub fn create(config: Hdf5Config) -> Result<Self, Hdf5Error> {
        validate_config(&config)?;
        if let Some(parent) = config
            .path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent).map_err(|source| Hdf5Error::CreateDirectory {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        let file = File::create_excl(&config.path)?;
        let datasets =
            (|| -> Result<(Dataset, Dataset, Dataset, Dataset, Attribute), ::hdf5::Error> {
                let time_dataset = file
                    .new_dataset::<f64>()
                    .shape((0..,))
                    .chunk((config.chunk_frames,))
                    .create("t1_base")?;
                let sequence_dataset = file
                    .new_dataset::<u16>()
                    .shape((0..,))
                    .chunk((config.chunk_frames,))
                    .create("sequence")?;

                let channel_1_builder = file
                    .new_dataset::<i32>()
                    .shape((0.., SAMPLE_COUNT))
                    .chunk((config.chunk_frames, SAMPLE_COUNT));
                let channel_1_builder = match config.compression_level {
                    Some(level) => channel_1_builder.deflate(level),
                    None => channel_1_builder,
                };
                let channel_1_dataset = channel_1_builder.create("y1_channel")?;

                let channel_2_builder = file
                    .new_dataset::<i32>()
                    .shape((0.., SAMPLE_COUNT))
                    .chunk((config.chunk_frames, SAMPLE_COUNT));
                let channel_2_builder = match config.compression_level {
                    Some(level) => channel_2_builder.deflate(level),
                    None => channel_2_builder,
                };
                let channel_2_dataset = channel_2_builder.create("y2_channel")?;
                let committed_frames = file.new_attr::<u64>().create("committed_frames")?;
                committed_frames.write_scalar(&0_u64)?;
                Ok((
                    time_dataset,
                    sequence_dataset,
                    channel_1_dataset,
                    channel_2_dataset,
                    committed_frames,
                ))
            })();
        let (
            time_dataset,
            sequence_dataset,
            channel_1_dataset,
            channel_2_dataset,
            committed_frames,
        ) = match datasets {
            Ok(datasets) => datasets,
            Err(source) => {
                drop(file);
                let cleanup = match std::fs::remove_file(&config.path) {
                    Ok(()) => "removed incomplete output file".to_owned(),
                    Err(error) => format!("could not remove incomplete output file: {error}"),
                };
                return Err(Hdf5Error::Initialize { source, cleanup });
            }
        };

        Ok(Self {
            path: config.path,
            file: Some(file),
            time_dataset: Some(time_dataset),
            sequence_dataset: Some(sequence_dataset),
            channel_1_dataset: Some(channel_1_dataset),
            channel_2_dataset: Some(channel_2_dataset),
            committed_frames: Some(committed_frames),
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

impl BatchSink<LegacyScientificFrameV2> for Hdf5Sink {
    type Error = Hdf5Error;

    fn write_batch(&mut self, frames: &[LegacyScientificFrameV2]) -> Result<(), Self::Error> {
        if frames.is_empty() {
            return Ok(());
        }
        let times: Vec<f64> = frames
            .iter()
            .map(LegacyScientificFrameV2::channel_1_base_seconds)
            .collect();
        let sequences: Vec<u16> = frames.iter().map(|frame| frame.sequence).collect();
        let mut channel_1 = Vec::with_capacity(frames.len() * SAMPLE_COUNT);
        let mut channel_2 = Vec::with_capacity(frames.len() * SAMPLE_COUNT);
        for frame in frames {
            for value in frame.channel_1.iter() {
                channel_1.push(
                    i32::try_from(*value)
                        .map_err(|_| Hdf5Error::SampleOutOfRange { value: *value })?,
                );
            }
            for value in frame.channel_2.iter() {
                channel_2.push(
                    i32::try_from(*value)
                        .map_err(|_| Hdf5Error::SampleOutOfRange { value: *value })?,
                );
            }
        }
        let channel_1 = Array2::from_shape_vec((frames.len(), SAMPLE_COUNT), channel_1)
            .expect("validated scientific frame shape");
        let channel_2 = Array2::from_shape_vec((frames.len(), SAMPLE_COUNT), channel_2)
            .expect("validated scientific frame shape");

        let start = self.frames_written;
        let end = start + frames.len();
        let time_dataset = self.time_dataset.as_ref().ok_or(Hdf5Error::Closed)?;
        let sequence_dataset = self.sequence_dataset.as_ref().ok_or(Hdf5Error::Closed)?;
        let channel_1_dataset = self.channel_1_dataset.as_ref().ok_or(Hdf5Error::Closed)?;
        let channel_2_dataset = self.channel_2_dataset.as_ref().ok_or(Hdf5Error::Closed)?;
        let committed_frames = self.committed_frames.as_ref().ok_or(Hdf5Error::Closed)?;
        let commit = (|| -> Result<(), ::hdf5::Error> {
            time_dataset.resize((end,))?;
            sequence_dataset.resize((end,))?;
            channel_1_dataset.resize((end, SAMPLE_COUNT))?;
            channel_2_dataset.resize((end, SAMPLE_COUNT))?;
            time_dataset.write_slice(&times, start..end)?;
            sequence_dataset.write_slice(&sequences, start..end)?;
            channel_1_dataset.write_slice(channel_1.view(), (start..end, ..))?;
            channel_2_dataset.write_slice(channel_2.view(), (start..end, ..))?;
            committed_frames.write_scalar(
                &u64::try_from(end).expect("usize never exceeds u64 on supported targets"),
            )?;
            Ok(())
        })();
        if let Err(source) = commit {
            let rollback = rollback_shapes(
                time_dataset,
                sequence_dataset,
                channel_1_dataset,
                channel_2_dataset,
                committed_frames,
                start,
            );
            return Err(Hdf5Error::BatchCommit { source, rollback });
        }
        self.frames_written = end;
        Ok(())
    }

    fn flush(&mut self) -> Result<(), Self::Error> {
        self.file.as_ref().ok_or(Hdf5Error::Closed)?.flush()?;
        Ok(())
    }

    fn close(&mut self) -> Result<(), Self::Error> {
        let file = self.file.take().ok_or(Hdf5Error::Closed)?;
        file.flush()?;
        self.time_dataset.take();
        self.sequence_dataset.take();
        self.channel_1_dataset.take();
        self.channel_2_dataset.take();
        self.committed_frames.take();
        file.close()?;
        Ok(())
    }
}

fn rollback_shapes(
    time_dataset: &Dataset,
    sequence_dataset: &Dataset,
    channel_1_dataset: &Dataset,
    channel_2_dataset: &Dataset,
    committed_frames: &Attribute,
    frames: usize,
) -> String {
    let mut failures = Vec::new();
    if let Err(error) = time_dataset.resize((frames,)) {
        failures.push(format!("t1_base: {error}"));
    }
    if let Err(error) = sequence_dataset.resize((frames,)) {
        failures.push(format!("sequence: {error}"));
    }
    if let Err(error) = channel_1_dataset.resize((frames, SAMPLE_COUNT)) {
        failures.push(format!("y1_channel: {error}"));
    }
    if let Err(error) = channel_2_dataset.resize((frames, SAMPLE_COUNT)) {
        failures.push(format!("y2_channel: {error}"));
    }
    let committed = u64::try_from(frames).expect("usize never exceeds u64 on supported targets");
    if let Err(error) = committed_frames.write_scalar(&committed) {
        failures.push(format!("committed_frames: {error}"));
    }
    if failures.is_empty() {
        "restored all dataset shapes".to_owned()
    } else {
        format!("failed to restore {}", failures.join(", "))
    }
}

fn validate_config(config: &Hdf5Config) -> Result<(), Hdf5Error> {
    if config.path.as_os_str().is_empty() {
        return Err(Hdf5Error::InvalidConfiguration(
            "output path must not be empty".to_owned(),
        ));
    }
    if config.chunk_frames == 0 {
        return Err(Hdf5Error::InvalidConfiguration(
            "chunk frame count must be non-zero".to_owned(),
        ));
    }
    if matches!(config.compression_level, Some(level) if level > 9) {
        return Err(Hdf5Error::InvalidConfiguration(
            "deflate level must be in 0..=9".to_owned(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use langmuir_core::Timestamp;
    use tempfile::tempdir;

    use super::*;

    fn frame(index: u32) -> LegacyScientificFrameV2 {
        let mut channel_1 = Box::new([0_u32; SAMPLE_COUNT]);
        let mut channel_2 = Box::new([0_u32; SAMPLE_COUNT]);
        for sample in 0..SAMPLE_COUNT {
            let sample_u32 = u32::try_from(sample).expect("sample index fits u32");
            channel_1[sample] = index * 1000 + sample_u32;
            channel_2[sample] = 1_000_000 + index * 1000 + sample_u32;
        }
        LegacyScientificFrameV2 {
            timestamp: Timestamp {
                utc_seconds: 1_700_000_000 + index,
                microseconds: 123_456,
            },
            sequence: u16::try_from(index).unwrap(),
            channel_1,
            channel_2,
        }
    }

    #[test]
    fn writes_python_compatible_dataset_names_shapes_and_values() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("scientific.h5");
        let mut config = Hdf5Config::new(&path);
        config.chunk_frames = 2;
        config.compression_level = None;
        let sink = Hdf5Sink::create(config).unwrap();
        let writer = crate::BatchWriter::start(
            sink,
            crate::StorageServiceConfig {
                batch_size: 2,
                ..crate::StorageServiceConfig::default()
            },
        )
        .unwrap();
        for index in 0..3 {
            writer.submit(frame(index)).unwrap();
        }
        writer.shutdown().unwrap();

        let file = File::open(path).unwrap();
        let times = file.dataset("t1_base").unwrap();
        let sequences = file.dataset("sequence").unwrap();
        let channel_1 = file.dataset("y1_channel").unwrap();
        let channel_2 = file.dataset("y2_channel").unwrap();
        let committed_frames = file.attr("committed_frames").unwrap();
        assert_eq!(times.shape(), [3]);
        assert_eq!(sequences.shape(), [3]);
        assert_eq!(channel_1.shape(), [3, SAMPLE_COUNT]);
        assert_eq!(channel_2.shape(), [3, SAMPLE_COUNT]);
        assert_eq!(committed_frames.read_scalar::<u64>().unwrap(), 3);
        let values = channel_1.read_2d::<i32>().unwrap();
        assert_eq!(values[[0, 0]], 0);
        assert_eq!(values[[2, 124]], 2_124);
        let values = channel_2.read_2d::<i32>().unwrap();
        assert_eq!(values[[0, 0]], 1_000_000);
        assert_eq!(values[[2, 124]], 1_002_124);
    }

    #[test]
    fn never_overwrites_an_existing_file() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("existing.h5");
        std::fs::write(&path, b"keep me").unwrap();
        let result = Hdf5Sink::create(Hdf5Config::new(&path));
        assert!(result.is_err());
        assert_eq!(std::fs::read(path).unwrap(), b"keep me");
    }

    #[cfg(feature = "hdf5-static")]
    #[test]
    fn static_build_supports_the_default_deflate_configuration() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("default-compression.h5");
        let mut sink = Hdf5Sink::create(Hdf5Config::new(&path)).unwrap();
        sink.write_batch(&[frame(0)]).unwrap();
        sink.close().unwrap();

        let file = File::open(path).unwrap();
        assert_eq!(
            file.attr("committed_frames")
                .unwrap()
                .read_scalar::<u64>()
                .unwrap(),
            1
        );
        assert_eq!(
            file.dataset("y1_channel").unwrap().shape(),
            [1, SAMPLE_COUNT]
        );
    }
}
