use crate::{SocketListener as RustSocketListener, SocketStream as RustSocketStream};
use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;
use pyo3::types::PyBytes;
use std::{sync::Arc, time::Duration};
use tokio::sync::Mutex;

fn to_py_err<E: std::fmt::Display>(err: E) -> PyErr {
    PyRuntimeError::new_err(err.to_string())
}

fn duration_from_seconds(seconds: f64) -> PyResult<Duration> {
    if !seconds.is_finite() || seconds < 0.0 {
        return Err(PyRuntimeError::new_err("timeout must be a finite positive number"));
    }

    Ok(Duration::from_secs_f64(seconds))
}

#[pyclass(name = "SocketListener")]
pub struct PySocketListener {
    inner: Arc<RustSocketListener>,
}

#[pyclass(name = "SocketStream")]
pub struct PySocketStream {
    inner: Arc<Mutex<RustSocketStream>>,
}

#[pymethods]
impl PySocketListener {
    #[staticmethod]
    #[pyo3(signature = (name, *, mode=None))]
    pub fn bind<'py>(
        py: Python<'py>,
        name: String,
        mode: Option<Bound<'py, PyAny>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        #[cfg(unix)]
        let parsed_mode: Option<u32> = match mode {
            Some(value) => Some(value.extract::<u32>()?),
            None => None,
        };

        #[cfg(windows)]
        let parsed_mode: Option<String> = match mode {
            Some(value) => Some(value.extract::<String>()?),
            None => None,
        };

        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let listener =
                RustSocketListener::bind(&name, parsed_mode).await.map_err(to_py_err)?;

            Python::attach(|py| Py::new(py, PySocketListener { inner: Arc::new(listener) }))
        })
    }

    pub fn accept<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let listener = self.inner.clone();

        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let stream = listener.accept().await.map_err(to_py_err)?;

            Python::attach(|py| {
                Py::new(py, PySocketStream { inner: Arc::new(Mutex::new(stream)) })
            })
        })
    }

    pub fn accept_timeout<'py>(
        &self,
        py: Python<'py>,
        timeout: f64,
    ) -> PyResult<Bound<'py, PyAny>> {
        let listener = self.inner.clone();
        let timeout = duration_from_seconds(timeout)?;

        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let stream = listener.accept_timeout(timeout).await.map_err(to_py_err)?;

            Python::attach(|py| {
                Py::new(py, PySocketStream { inner: Arc::new(Mutex::new(stream)) })
            })
        })
    }

    pub fn name(&self) -> String {
        self.inner.name().to_string()
    }

    pub fn is_started(&self) -> bool {
        self.inner.is_started()
    }

    pub fn is_startetd(&self) -> bool {
        self.inner.is_startetd()
    }

    pub fn is_closed(&self) -> bool {
        self.inner.is_closed()
    }

    pub fn is_paused(&self) -> bool {
        self.inner.is_paused()
    }

    pub fn is_accepting(&self) -> bool {
        self.inner.is_accepting()
    }

    pub fn pause(&self) {
        self.inner.pause();
    }

    pub fn resume(&self) {
        self.inner.resume();
    }

    pub fn close(&self) {
        self.inner.close();
    }

    pub fn __repr__(&self) -> String {
        format!(
            "SocketListener(name={:?}, started={}, paused={}, closed={}, accepting={})",
            self.name(),
            self.is_started(),
            self.is_paused(),
            self.is_closed(),
            self.is_accepting(),
        )
    }
}

#[pymethods]
impl PySocketStream {
    #[staticmethod]
    pub fn connect<'py>(py: Python<'py>, name: String) -> PyResult<Bound<'py, PyAny>> {
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let stream = RustSocketStream::connect(&name).await.map_err(to_py_err)?;

            Python::attach(|py| {
                Py::new(py, PySocketStream { inner: Arc::new(Mutex::new(stream)) })
            })
        })
    }

    pub fn id<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let stream = self.inner.clone();

        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let guard = stream.lock().await;
            Ok(guard.id())
        })
    }

    pub fn name<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let stream = self.inner.clone();

        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let guard = stream.lock().await;
            Ok(guard.name().to_string())
        })
    }

    pub fn read<'py>(&self, py: Python<'py>, max_bytes: usize) -> PyResult<Bound<'py, PyAny>> {
        self.read_bytes(py, max_bytes)
    }

    pub fn read_bytes<'py>(
        &self,
        py: Python<'py>,
        max_bytes: usize,
    ) -> PyResult<Bound<'py, PyAny>> {
        let stream = self.inner.clone();

        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let mut guard = stream.lock().await;
            let buffer = guard.read_bytes(max_bytes).await.map_err(to_py_err)?;

            Python::attach(|py| Ok(PyBytes::new(py, &buffer).into_any().unbind()))
        })
    }

    pub fn read_timeout<'py>(
        &self,
        py: Python<'py>,
        max_bytes: usize,
        timeout: f64,
    ) -> PyResult<Bound<'py, PyAny>> {
        let stream = self.inner.clone();
        let timeout = duration_from_seconds(timeout)?;

        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let mut guard = stream.lock().await;
            let buffer = guard.read_timeout(max_bytes, timeout).await.map_err(to_py_err)?;

            Python::attach(|py| Ok(PyBytes::new(py, &buffer).into_any().unbind()))
        })
    }

    pub fn write<'py>(&self, py: Python<'py>, data: Vec<u8>) -> PyResult<Bound<'py, PyAny>> {
        self.write_bytes(py, data)
    }

    pub fn write_bytes<'py>(
        &self,
        py: Python<'py>,
        data: Vec<u8>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let stream = self.inner.clone();

        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let mut guard = stream.lock().await;
            let n = guard.write_bytes(&data).await.map_err(to_py_err)?;
            Ok(n)
        })
    }

    pub fn write_all<'py>(&self, py: Python<'py>, data: Vec<u8>) -> PyResult<Bound<'py, PyAny>> {
        self.write_all_bytes(py, data)
    }

    pub fn write_all_bytes<'py>(
        &self,
        py: Python<'py>,
        data: Vec<u8>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let stream = self.inner.clone();

        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let mut guard = stream.lock().await;
            guard.write_all_bytes(&data).await.map_err(to_py_err)?;
            Ok(())
        })
    }

    pub fn write_all_timeout<'py>(
        &self,
        py: Python<'py>,
        data: Vec<u8>,
        timeout: f64,
    ) -> PyResult<Bound<'py, PyAny>> {
        let stream = self.inner.clone();
        let timeout = duration_from_seconds(timeout)?;

        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let mut guard = stream.lock().await;
            guard.write_all_timeout(&data, timeout).await.map_err(to_py_err)?;

            Ok(())
        })
    }

    pub fn flush<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let stream = self.inner.clone();

        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let mut guard = stream.lock().await;
            guard.flush().await.map_err(to_py_err)?;
            Ok(())
        })
    }

    pub fn close<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let stream = self.inner.clone();

        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let mut guard = stream.lock().await;
            guard.close().await.map_err(to_py_err)?;
            Ok(())
        })
    }

    pub fn is_started<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let stream = self.inner.clone();

        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let guard = stream.lock().await;
            Ok(guard.is_started())
        })
    }

    pub fn is_startetd<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let stream = self.inner.clone();

        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let guard = stream.lock().await;
            Ok(guard.is_startetd())
        })
    }

    pub fn is_closed<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let stream = self.inner.clone();

        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let guard = stream.lock().await;
            Ok(guard.is_closed())
        })
    }

    pub fn is_paused<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let stream = self.inner.clone();

        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let guard = stream.lock().await;
            Ok(guard.is_paused())
        })
    }

    pub fn is_active<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let stream = self.inner.clone();

        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let guard = stream.lock().await;
            Ok(guard.is_active())
        })
    }

    pub fn is_available<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let stream = self.inner.clone();

        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let guard = stream.lock().await;
            Ok(guard.is_available())
        })
    }

    pub fn pause<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let stream = self.inner.clone();

        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let guard = stream.lock().await;
            guard.pause();
            Ok(())
        })
    }

    pub fn resume<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let stream = self.inner.clone();

        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let guard = stream.lock().await;
            guard.resume();
            Ok(())
        })
    }

    pub fn __repr__(&self) -> String {
        "SocketStream(...)".to_string()
    }
}

pub fn python_module(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PySocketListener>()?;
    m.add_class::<PySocketStream>()?;
    Ok(())
}
