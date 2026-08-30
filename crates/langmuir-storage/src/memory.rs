use std::convert::Infallible;
use std::sync::Arc;

use parking_lot::Mutex;

use crate::BatchSink;

#[derive(Debug)]
struct MemorySinkState<T> {
    items: Vec<T>,
    batch_sizes: Vec<usize>,
    flushes: u64,
    closed: bool,
}

impl<T> Default for MemorySinkState<T> {
    fn default() -> Self {
        Self {
            items: Vec::new(),
            batch_sizes: Vec::new(),
            flushes: 0,
            closed: false,
        }
    }
}

/// Pure-Rust sink intended for deterministic integration tests and benchmarks.
pub struct MemorySink<T> {
    state: Arc<Mutex<MemorySinkState<T>>>,
}

/// Read-only handle retained by a test after the sink moves into its worker.
pub struct MemorySinkSnapshot<T> {
    state: Arc<Mutex<MemorySinkState<T>>>,
}

#[must_use]
pub fn memory_sink<T>() -> (MemorySink<T>, MemorySinkSnapshot<T>) {
    let state = Arc::new(Mutex::new(MemorySinkState::default()));
    (
        MemorySink {
            state: Arc::clone(&state),
        },
        MemorySinkSnapshot { state },
    )
}

impl<T: Clone + Send + 'static> BatchSink<T> for MemorySink<T> {
    type Error = Infallible;

    fn write_batch(&mut self, items: &[T]) -> Result<(), Self::Error> {
        let mut state = self.state.lock();
        state.items.extend_from_slice(items);
        state.batch_sizes.push(items.len());
        Ok(())
    }

    fn flush(&mut self) -> Result<(), Self::Error> {
        self.state.lock().flushes += 1;
        Ok(())
    }

    fn close(&mut self) -> Result<(), Self::Error> {
        self.state.lock().closed = true;
        Ok(())
    }
}

impl<T: Clone> MemorySinkSnapshot<T> {
    #[must_use]
    pub fn items(&self) -> Vec<T> {
        self.state.lock().items.clone()
    }

    #[must_use]
    pub fn batch_sizes(&self) -> Vec<usize> {
        self.state.lock().batch_sizes.clone()
    }

    #[must_use]
    pub fn flush_count(&self) -> u64 {
        self.state.lock().flushes
    }

    #[must_use]
    pub fn is_closed(&self) -> bool {
        self.state.lock().closed
    }
}
