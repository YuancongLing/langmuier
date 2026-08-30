//! Transport and reader-worker infrastructure for the acquisition backend.
//!
//! Production code never falls back to a simulated source. Mock transports and
//! deterministic packet generators are only compiled for tests or with the
//! explicit `mock` feature.

mod core_adapter;
mod error;
mod metrics;
mod transport;
mod udp;
mod worker;

#[cfg(test)]
mod pipeline_test;

#[cfg(any(test, feature = "mock"))]
pub mod mock;
#[cfg(feature = "serial")]
mod serial;

pub use error::{
    CommandQueueError, ReaderError, ReaderFault, ReaderFaultKind, TransportError, WriteAckError,
};
pub use metrics::{DecodeDelta, ReaderMetrics, ReaderMetricsSnapshot};
#[cfg(feature = "serial")]
pub use serial::{SerialConfig, SerialConnector, SerialTransport, available_serial_ports};
pub use transport::{Connector, Transport};
pub use udp::{UdpConfig, UdpConnector, UdpTransport};
pub use worker::{
    BackpressurePolicy, ReaderConfig, ReaderEvent, ReaderStatusSnapshot, ReaderWorker, StopReason,
    StreamDecoder, WorkerExit, WriteReceipt, WriteTicket,
};
