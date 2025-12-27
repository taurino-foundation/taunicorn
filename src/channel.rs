use std::sync::Arc;
use bytes::Bytes;
use pyo3::{IntoPyObjectExt, prelude::*};
use tokio::sync::Mutex;
use tokio::sync::mpsc::{Receiver, Sender};

#[pyclass]
#[pyo3(name = "Sender")]
pub struct SenderHandle {
    pub tx: Arc<Sender<Bytes>>,
}

#[pymethods]
impl SenderHandle {
    /// await sender.send(b"...")
    fn send<'py>(&self, py: Python<'py>, data: Py<PyAny>) -> PyResult<Bound<'py, PyAny>> {
        let bytes: Vec<u8> = data.extract(py)?;
        let tx = self.tx.clone();

        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            tx.send(Bytes::from(bytes))
                .await
                .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(e.to_string()))?;
            Ok(())
        })
    }

    fn try_send(&self, py: Python<'_>, data: Py<PyAny>) -> PyResult<bool> {
        let bytes: Vec<u8> = data.extract(py)?;
        Ok(self.tx.try_send(Bytes::from(bytes)).is_ok())
    }
}

#[pyclass]
#[pyo3(name = "Receiver")]
pub struct ReceiverHandle {
    pub rx: Arc<Mutex<Receiver<Bytes>>>,
}

#[pymethods]
impl ReceiverHandle {
    /// await receiver.recv() -> bytes | None
    fn recv<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let rx = self.rx.clone();

        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let mut guard = rx.lock().await;
            match guard.recv().await {
                Some(bytes) => Python::attach(|inner_py| {
                    let b = bytes.to_vec().into_py_any(inner_py).unwrap();
                    Ok(b)
                }),
                None => Python::attach(|inner_py| Ok(inner_py.None())),
            }
        })
    }

    fn try_recv(&self) -> Option<Vec<u8>> {
        self.rx
            .try_lock()
            .ok()
            .and_then(|mut g| g.try_recv().ok())
            .map(|b| b.to_vec())
    }
}
