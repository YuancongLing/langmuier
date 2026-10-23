//! Bounded, batched storage isolated from acquisition and GUI threads.

mod binary;
mod error;
#[cfg(feature = "hdf5")]
mod hdf5;
#[cfg(any(test, feature = "mock"))]
mod memory;
mod metrics;
mod scientific_csv;
mod service;

pub use binary::{
    BINARY_ENDIAN_MARKER, BINARY_FLAGS, BINARY_FRAME_LEN, BINARY_HEADER_LEN, BINARY_MAGIC,
    BINARY_SAMPLE_COUNT, BINARY_VERSION, BinaryConfig, BinaryError, BinarySink,
};
pub use error::{StorageError, SubmitError};
#[cfg(feature = "hdf5")]
pub use hdf5::{Hdf5Config, Hdf5Error, Hdf5Sink};
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
