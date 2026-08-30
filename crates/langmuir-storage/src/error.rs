use std::fmt;
use std::time::Duration;

use thiserror::Error;

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum StorageError {
    #[error("invalid storage configuration: {0}")]
    InvalidConfiguration(String),
    #[error("storage sink {operation} failed: {message}")]
    Sink {
        operation: &'static str,
        message: String,
    },
    #[error("storage control channel is closed")]
    ControlChannelClosed,
    #[error("storage worker completion channel closed unexpectedly")]
    CompletionChannelClosed,
    #[error("storage operation did not finish within {0:?}")]
    ControlTimeout(Duration),
    #[error("storage worker panicked")]
    WorkerPanicked,
    #[error("storage worker has already been joined")]
    AlreadyJoined,
}

/// A failed submission retains ownership of the item so callers can retry or
/// stop acquisition without silently losing data.
#[derive(Debug)]
pub enum SubmitError<T> {
    QueueFull(T),
    Timeout { item: T, timeout: Duration },
    Closed(T),
}

impl<T> SubmitError<T> {
    #[must_use]
    pub fn into_inner(self) -> T {
        match self {
            Self::QueueFull(item) | Self::Closed(item) | Self::Timeout { item, .. } => item,
        }
    }
}

impl<T> fmt::Display for SubmitError<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::QueueFull(_) => formatter.write_str("storage input queue is full"),
            Self::Timeout { timeout, .. } => {
                write!(formatter, "storage input queue stayed full for {timeout:?}")
            }
            Self::Closed(_) => formatter.write_str("storage input queue is closed"),
        }
    }
}

impl<T: fmt::Debug> std::error::Error for SubmitError<T> {}
