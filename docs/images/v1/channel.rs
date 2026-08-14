use pyo3::types::PyDict;
use pyo3::{IntoPyObjectExt, prelude::*};
use std::sync::Arc;
use tokio::sync::broadcast;
use tokio::sync::mpsc::{Receiver, Sender};
use tokio::sync::{Mutex, mpsc};
#[pyclass]
#[pyo3(name = "MPSCChannel")]
pub struct PyChannel {
    pub receiver: Arc<Mutex<Receiver<Py<PyAny>>>>,
    pub sender: Arc<Sender<Py<PyAny>>>,
}

#[pymethods]
impl PyChannel {
    #[new]
    pub fn new(buffer: usize) -> PyResult<Self> {
        let (sender, receiver) = mpsc::channel(buffer);
        Ok(Self {
            sender: Arc::new(sender),
            receiver: Arc::new(Mutex::new(receiver)),
        })
    }

    fn send<'py>(&self, py: Python<'py>, data: Bound<'py, PyDict>) -> PyResult<Bound<'py, PyAny>> {
        let dict = data.extract()?;
        let sender = self.sender.clone();

        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            sender
                .send(dict)
                .await
                .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(e.to_string()))?;
            Ok(())
        })
    }

    fn try_send(&self, data: Bound<PyDict>) -> PyResult<bool> {
        let dict = data.extract()?;
        Ok(self.sender.try_send(dict).is_ok())
    }
    /// await receiver.recv() -> bytes | None
    fn recv<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let receiver = self.receiver.clone();

        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let mut guard = receiver.lock().await;
            match guard.recv().await {
                Some(bytes) => Python::attach(|inner_py| {
                    let b = bytes.into_py_any(inner_py).unwrap();
                    Ok(b)
                }),
                None => Python::attach(|inner_py| Ok(inner_py.None())),
            }
        })
    }

    fn try_recv<'py>(&self, py: Python<'py>) -> Option<Bound<'py, PyAny>> {
        self.receiver
            .try_lock()
            .ok()
            .and_then(|mut g| g.try_recv().ok())
            .map(|b| {
                b.into_bound(py)
            })
    }
}

#[pyclass]
#[pyo3(name = "BroadcastChannel")]
pub struct PyBroadcastChannel {
    sender: Arc<broadcast::Sender<Arc<Py<PyAny>>>>,
}

#[pymethods]
impl PyBroadcastChannel {
    #[new]
    pub fn new(buffer: usize) -> PyResult<Self> {
        let (sender, _) = broadcast::channel(buffer);
        Ok(Self {
            sender: Arc::new(sender),
        })
    }

    pub fn subscribe(&self) -> PyBroadcastReceiver {
        PyBroadcastReceiver {
            receiver: Arc::new(Mutex::new(self.sender.subscribe())),
        }
    }

    fn send<'py>(&self, py: Python<'py>, data: Bound<'py, PyDict>) -> PyResult<Bound<'py, PyAny>> {
        let obj: Py<PyAny> = data.into_py_any(py)?;
        let value = Arc::new(obj);
        let sender = self.sender.clone();

        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            sender
                .send(value)
                .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(e.to_string()))?;
            Ok(())
        })
    }
}

#[pyclass]
#[pyo3(name = "BroadcastReceiver")]
pub struct PyBroadcastReceiver {
    receiver: Arc<Mutex<broadcast::Receiver<Arc<Py<PyAny>>>>>,
}
#[pymethods]
impl PyBroadcastReceiver {
    fn recv<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let receiver = Arc::clone(&self.receiver);

        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let mut rx = receiver.lock().await;

            match rx.recv().await {
                Ok(value) => Python::attach(|py| {
                    let obj: Py<PyAny> = (*value).clone_ref(py);
                    Ok(obj.into_py_any(py)?)
                }),

                Err(broadcast::error::RecvError::Closed) => Python::attach(|py| Ok(py.None())),

                Err(broadcast::error::RecvError::Lagged(_)) => Err(
                    pyo3::exceptions::PyRuntimeError::new_err("Receiver lagged behind"),
                ),
            }
        })
    }
}
