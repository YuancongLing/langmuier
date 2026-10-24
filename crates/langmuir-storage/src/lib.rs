//! Bounded, batched storage isolated from acquisition and GUI threads.

mod binary;
mod binary_v3;
mod error;
#[cfg(feature = "hdf5")]
mod hdf5;
mod legacy;
#[cfg(any(test, feature = "mock"))]
mod memory;
mod metrics;
mod scientific_csv;
mod service;

pub use binary::{
    BINARY_ENDIAN_MARKER, BINARY_FLAGS, BINARY_FRAME_LEN, BINARY_HEADER_LEN, BINARY_MAGIC,
    BINARY_SAMPLE_COUNT, BINARY_VERSION, BinaryConfig, BinaryError, BinarySink,
};
pub use binary_v3::{
    BinaryV3Error, BinaryV3Reader, BinaryV3Sink, V3_CHANNEL_COUNT, V3_ENDIAN_MARKER,
    V3_FLAG_CRC_NOT_CHECKED, V3_FLAG_RAW_DEVICE_TIME, V3_FLAGS, V3_FRAME_LEN, V3_HEADER_LEN,
    V3_MAGIC, V3_REPRESENTATION_CODE10_U16, V3_SAMPLE_COUNT, V3_SAMPLE_INTERVAL_MICROS,
    V3_SEQUENCE_BITS, V3_VERSION,
};
pub use error::{StorageError, SubmitError};
#[cfg(feature = "hdf5")]
pub use hdf5::{Hdf5Config, Hdf5Error, Hdf5Sink};
pub use legacy::{
    LEGACY_SAMPLE_COUNT_V2, LEGACY_SAMPLE_INTERVAL_MICROS_V2, LegacyScientificFrameV2,
};
#[cfg(any(test, feature = "mock"))]
pub use memory::{MemorySink, MemorySinkSnapshot, memory_sink};
pub use metrics::{StorageMetrics, StorageMetricsSnapshot};
pub use scientific_csv::{
    SCIENTIFIC_CSV_HEADER, ScientificCsvConfig, ScientificCsvError, ScientificCsvSink,
};
pub use service::{
    BatchSink, BatchWriter, StorageEvent, StorageServiceConfig, StorageStatusSnapshot, WriterExit,
    WriterStopReason,
};
