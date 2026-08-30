use std::io::{Read, Write};
use std::time::Duration;

use crate::TransportError;

/// A full-duplex byte stream owned by exactly one reader worker.
///
/// Implementations must honor `set_read_timeout`; bounded read latency is what
/// lets the worker observe a stop request without forcibly terminating a thread.
pub trait Transport: Read + Write + Send + 'static {
    fn endpoint(&self) -> &str;

    /// # Errors
    ///
    /// Returns an error when the underlying transport cannot apply the
    /// requested timeout.
    fn set_read_timeout(&mut self, timeout: Duration) -> Result<(), TransportError>;
}

/// Opens a configured transport. Connection failure is always explicit.
pub trait Connector: Send + Sync {
    /// # Errors
    ///
    /// Returns configuration, open, or device errors without falling back to
    /// a simulated transport.
    fn connect(&self) -> Result<Box<dyn Transport>, TransportError>;
}
