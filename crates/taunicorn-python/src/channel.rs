use std::sync::Arc;

use pyo3::exceptions::{PyBlockingIOError, PyRuntimeError, PyValueError};
use pyo3::prelude::*;
use pyo3_async_runtimes::tokio::future_into_py;
use tokio::sync::{
    mpsc::{
        channel, unbounded_channel,
        error::{TryRecvError, TrySendError},
        Receiver, Sender, UnboundedReceiver, UnboundedSender,
    },
    Mutex, Semaphore,
};
use tokio_util::sync::CancellationToken;

mod exceptions {
    use super::*;
    use pyo3::create_exception;

    create_exception!(
        rust_queues, QueueClosed, PyRuntimeError,
        "Sending is closed, or the closed queue has been drained."
    );
    create_exception!(
        rust_queues, QueueFull, PyBlockingIOError,
        "The bounded queue currently has no free capacity."
    );
    create_exception!(
        rust_queues, QueueEmpty, PyBlockingIOError,
        "No item is currently available; the queue is not fully drained and closed."
    );
    create_exception!(
        rust_queues, QueueBusy, PyBlockingIOError,
        "The receiver is currently locked by another operation."
    );
}

pub use exceptions::{QueueBusy, QueueClosed, QueueEmpty, QueueFull};
// Export the actual exception raised by the async bridge, not a lookalike type.
pub use pyo3_async_runtimes::err::RustPanic;

fn closed_error() -> PyErr {
    QueueClosed::new_err("queue is closed")
}

fn recv_error(err: TryRecvError) -> PyErr {
    match err {
        TryRecvError::Empty => QueueEmpty::new_err("queue is empty"),
        TryRecvError::Disconnected => closed_error(),
    }
}

/// Bounded FIFO queue of Python objects. Async methods require a running asyncio loop.
#[pyclass(module = "rust_queues", frozen)]
pub struct BoundedQueue {
    tx: Sender<Py<PyAny>>,
    rx: Arc<Mutex<Receiver<Py<PyAny>>>>,
    shutdown: CancellationToken,
}

#[pymethods]
impl BoundedQueue {
    #[new]
    fn new(capacity: usize) -> PyResult<Self> {
        // Tokio panics for zero or an unsupported capacity; report a Python error instead.
        if capacity == 0 || capacity > Semaphore::MAX_PERMITS {
            return Err(PyValueError::new_err(format!(
                "capacity must be between 1 and {}", Semaphore::MAX_PERMITS
            )));
        }
        let (tx, rx) = channel(capacity);
        Ok(Self {
            tx,
            rx: Arc::new(Mutex::new(rx)),
            shutdown: CancellationToken::new(),
        })
    }

    /// Return an asyncio.Future; await it to send, waiting for space if necessary.
    fn send<'py>(&self, py: Python<'py>, item: Py<PyAny>) -> PyResult<Bound<'py, PyAny>> {
        let tx = self.tx.clone();
        let shutdown = self.shutdown.clone();
        future_into_py(py, async move {
            tokio::select! {
                biased;
                _ = shutdown.cancelled() => Err(closed_error()),
                result = tx.send(item) => result.map_err(|_| closed_error()),
            }
        })
    }

    /// Return an asyncio.Future for the next item. Closed and drained raises QueueClosed.
    fn recv<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let rx = Arc::clone(&self.rx);
        let shutdown = self.shutdown.clone();
        future_into_py(py, async move {
            let mut rx = rx.lock().await;
            let item = tokio::select! {
                biased;
                _ = shutdown.cancelled() => {
                    // Close the channel but preserve items already in its buffer.
                    rx.close();
                    rx.recv().await
                }
                item = rx.recv() => item,
            };
            item.ok_or_else(closed_error)
        })
    }

    /// Send immediately; raise QueueFull or QueueClosed instead of waiting.
    fn try_send(&self, item: Py<PyAny>) -> PyResult<()> {
        if self.is_closed() {
            return Err(closed_error());
        }
        self.tx.try_send(item).map_err(|err| match err {
            TrySendError::Full(_) => QueueFull::new_err("queue is full"),
            TrySendError::Closed(_) => closed_error(),
        })
    }

    /// Receive immediately; raise QueueEmpty, QueueBusy or QueueClosed.
    fn try_recv(&self) -> PyResult<Py<PyAny>> {
        let mut rx = self.rx.try_lock()
            .map_err(|_| QueueBusy::new_err("receiver is busy"))?;
        if self.shutdown.is_cancelled() {
            rx.close();
        }
        rx.try_recv().map_err(recv_error)
    }

    /// Stop new sends and wake waiters. Buffered items remain readable. Idempotent.
    fn close(&self) {
        // Never wait for this mutex: recv() may hold it while waiting for an item.
        self.shutdown.cancel();
        if let Ok(mut rx) = self.rx.try_lock() {
            rx.close();
        }
    }

    /// True once sending is closed; buffered items may still be readable.
    fn is_closed(&self) -> bool {
        self.shutdown.is_cancelled() || self.tx.is_closed()
    }

    /// Snapshot of currently available channel capacity, not a reservation.
    fn capacity(&self) -> usize {
        self.tx.capacity()
    }

    /// The capacity selected at construction.
    fn max_capacity(&self) -> usize {
        self.tx.max_capacity()
    }
}

/// Unbounded FIFO queue of Python objects; memory use is not bounded.
#[pyclass(module = "rust_queues", frozen)]
pub struct UnboundedQueue {
    tx: UnboundedSender<Py<PyAny>>,
    rx: Arc<Mutex<UnboundedReceiver<Py<PyAny>>>>,
    shutdown: CancellationToken,
}

#[pymethods]
impl UnboundedQueue {
    #[new]
    fn new() -> Self {
        let (tx, rx) = unbounded_channel();
        Self {
            tx,
            rx: Arc::new(Mutex::new(rx)),
            shutdown: CancellationToken::new(),
        }
    }

    /// Return an asyncio.Future for a send. There is no wait for channel capacity.
    fn send<'py>(&self, py: Python<'py>, item: Py<PyAny>) -> PyResult<Bound<'py, PyAny>> {
        let tx = self.tx.clone();
        let shutdown = self.shutdown.clone();
        future_into_py(py, async move {
            if shutdown.is_cancelled() {
                return Err(closed_error());
            }
            tx.send(item).map_err(|_| closed_error())
        })
    }

    /// Return an asyncio.Future for the next item. Closed and drained raises QueueClosed.
    fn recv<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let rx = Arc::clone(&self.rx);
        let shutdown = self.shutdown.clone();
        future_into_py(py, async move {
            let mut rx = rx.lock().await;
            let item = tokio::select! {
                biased;
                _ = shutdown.cancelled() => {
                    rx.close();
                    rx.recv().await
                }
                item = rx.recv() => item,
            };
            item.ok_or_else(closed_error)
        })
    }

    /// Send immediately without needing an asyncio loop. Raise QueueClosed on shutdown.
    fn try_send(&self, item: Py<PyAny>) -> PyResult<()> {
        if self.is_closed() {
            return Err(closed_error());
        }
        self.tx.send(item).map_err(|_| closed_error())
    }

    /// Receive immediately; raise QueueEmpty, QueueBusy or QueueClosed.
    fn try_recv(&self) -> PyResult<Py<PyAny>> {
        let mut rx = self.rx.try_lock()
            .map_err(|_| QueueBusy::new_err("receiver is busy"))?;
        if self.shutdown.is_cancelled() {
            rx.close();
        }
        rx.try_recv().map_err(recv_error)
    }

    /// Stop new sends and wake waiters. Buffered items remain readable. Idempotent.
    fn close(&self) {
        self.shutdown.cancel();
        if let Ok(mut rx) = self.rx.try_lock() {
            rx.close();
        }
    }

    /// True once sending is closed; buffered items may still be readable.
    fn is_closed(&self) -> bool {
        self.shutdown.is_cancelled() || self.tx.is_closed()
    }
}

