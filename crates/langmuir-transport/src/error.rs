use std::fmt;
use std::io;

use thiserror::Error;

/// Errors raised while opening or configuring a byte transport.
#[derive(Debug, Error)]
pub enum TransportError {
    #[error("invalid transport configuration: {0}")]
    InvalidConfiguration(String),
    #[error("failed to open transport {endpoint}: {message}")]
    Open { endpoint: String, message: String },
    #[error("failed to configure transport {endpoint}: {message}")]
    Configure { endpoint: String, message: String },
    #[error("transport I/O failed: {0}")]
    Io(#[from] io::Error),
}

/// Stable categories suitable for metrics and UI presentation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReaderFaultKind {
    Io,
    Decoder,
    Backpressure,
}

/// A structured, cloneable fault sent over the bounded event channel.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReaderFault {
    pub kind: ReaderFaultKind,
    pub message: String,
}

impl ReaderFault {
    #[must_use]
    pub fn new(kind: ReaderFaultKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}

/// Errors controlling or joining a reader worker.
#[derive(Debug, Error)]
pub enum ReaderError {
    #[error("invalid reader configuration: {0}")]
    InvalidConfiguration(String),
    #[error("failed to spawn reader worker: {0}")]
    Spawn(#[source] io::Error),
    #[error("reader transport setup failed: {0}")]
    Transport(#[from] TransportError),
    #[error("reader worker panicked")]
    WorkerPanicked,
    #[error("reader worker did not stop within {0:?}; worker was detached")]
    ShutdownTimeout(std::time::Duration),
    #[error("reader worker completion channel closed unexpectedly")]
    CompletionChannelClosed,
}

/// A failed outbound submission retains its bytes for retry or logging.
#[derive(Debug)]
pub enum CommandQueueError {
    Empty,
    QueueFull(Vec<u8>),
    Timeout {
        bytes: Vec<u8>,
        timeout: std::time::Duration,
    },
    Closed(Vec<u8>),
}

impl CommandQueueError {
    #[must_use]
    pub fn into_bytes(self) -> Vec<u8> {
        match self {
            Self::Empty => Vec::new(),
            Self::QueueFull(bytes) | Self::Closed(bytes) | Self::Timeout { bytes, .. } => bytes,
        }
    }
}

impl fmt::Display for CommandQueueError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => formatter.write_str("outbound command must not be empty"),
            Self::QueueFull(_) => formatter.write_str("outbound command queue is full"),
            Self::Timeout { timeout, .. } => {
                write!(
                    formatter,
                    "outbound command queue stayed full for {timeout:?}"
                )
            }
            Self::Closed(_) => formatter.write_str("outbound command queue is closed"),
        }
    }
}

impl std::error::Error for CommandQueueError {}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum WriteAckError {
    #[error("command {id} write failed: {fault:?}")]
    Failed { id: u64, fault: ReaderFault },
    #[error("command {id} acknowledgement timed out after {timeout:?}")]
    Timeout {
        id: u64,
        timeout: std::time::Duration,
    },
    #[error("reader stopped before acknowledging command {id}")]
    ReaderStopped { id: u64 },
}
