//! PyO3 / asyncio bindings for `local__struct`.
//!
//! Python owns and drives its `asyncio` event loop. Rust async I/O runs on the Tokio runtime
//! managed by `pyo3-async-runtimes`. This module does not duplicate the transport state machine:
//! `Server`, `Connection`, `ReadHalf`, and `WriteHalf` remain the source of
//! truth for connection state, EOF, half-close, ordering, and cancellation semantics.

use taunicorn::{
    Connection as RustConnection, ConnectionInfo as RustConnectionInfo, Endpoint as RustEndpoint,
    LocalTransport as RustLocalTransport, ReadHalf as RustReadHalf, ReceiveResult,
    Server as RustServer, ServerInfo as RustServerInfo, WriteHalf as RustWriteHalf,
};

use pyo3::exceptions::{
    PyBlockingIOError, PyConnectionError, PyOSError, PyRuntimeError, PyTimeoutError, PyTypeError,
    PyValueError,
};
use pyo3::prelude::*;
use pyo3::types::PyBytes;

use std::{
    sync::{Arc, Mutex as StdMutex, OnceLock},
    time::Duration,
};

// -------------------------------------------------------------------------------------------------
// Shared conversion helpers
// -------------------------------------------------------------------------------------------------

/// Transitional mapping while `local__struct` still exposes `anyhow::Error` publicly.
///
/// Once the transport has a typed public error enum this should become a direct enum -> Python
/// exception mapping instead of inspecting error strings.
fn to_py_err(err: anyhow::Error) -> PyErr {
    if err.downcast_ref::<std::io::Error>().is_some() {
        return PyOSError::new_err(err.to_string());
    }

    let message = err.to_string();

    if message.contains("timeout") {
        PyTimeoutError::new_err(message)
    } else if message.contains("paused") {
        PyBlockingIOError::new_err(message)
    } else if message.contains("closed")
        || message.contains("shut down")
        || message.contains("stopped")
    {
        PyConnectionError::new_err(message)
    } else if message.contains("must not be empty") {
        PyValueError::new_err(message)
    } else {
        PyRuntimeError::new_err(message)
    }
}

fn duration_from_seconds(seconds: f64) -> PyResult<Duration> {
    Duration::try_from_secs_f64(seconds).map_err(|_| {
        PyValueError::new_err("timeout must be a finite, non-negative, representable number")
    })
}

fn endpoint_from_py(value: &Bound<'_, PyAny>) -> PyResult<RustEndpoint> {
    if let Ok(endpoint) = value.extract::<PyRef<'_, PyEndpoint>>() {
        return Ok(endpoint.inner.clone());
    }

    if let Ok(name) = value.extract::<String>() {
        return RustEndpoint::new(name).map_err(to_py_err);
    }

    Err(PyTypeError::new_err("endpoint must be a str or Endpoint"))
}

fn bytes_to_python(data: &[u8]) -> PyResult<Py<PyAny>> {
    Python::attach(|py| Ok(PyBytes::new(py, data).into_any().unbind()))
}

async fn receive_bytes(connection: Arc<RustConnection>, max_bytes: usize) -> PyResult<Vec<u8>> {
    if max_bytes == 0 {
        return Ok(Vec::new());
    }

    let mut buffer = vec![0_u8; max_bytes];
    match connection.receive(&mut buffer).await.map_err(to_py_err)? {
        ReceiveResult::Data(n) => {
            buffer.truncate(n);
            Ok(buffer.to_vec())
        }
        ReceiveResult::EndOfStream => Ok(Vec::new()),
    }
}

async fn receive_half_bytes(reader: Arc<RustReadHalf>, max_bytes: usize) -> PyResult<Vec<u8>> {
    if max_bytes == 0 {
        return Ok(Vec::new());
    }

    let mut buffer = vec![0_u8; max_bytes];
    match reader.receive(&mut buffer).await.map_err(to_py_err)? {
        ReceiveResult::Data(n) => {
            buffer.truncate(n);
            Ok(buffer.to_vec())
        }
        ReceiveResult::EndOfStream => Ok(Vec::new()),
    }
}

// `future_into_py` propagates Python Future cancellation to the Rust future. For a write operation
// that is dangerous: Python may not know whether a prefix was already accepted locally. Closing the
// connection on cancellation prevents a blind retry of the complete buffer from duplicating bytes.
struct CloseConnectionUnlessCompleted {
    connection: Option<Arc<RustConnection>>,
}

impl CloseConnectionUnlessCompleted {
    fn new(connection: Arc<RustConnection>) -> Self {
        Self { connection: Some(connection) }
    }

    fn completed(&mut self) {
        self.connection = None;
    }
}

impl Drop for CloseConnectionUnlessCompleted {
    fn drop(&mut self) {
        let Some(connection) = self.connection.take() else {
            return;
        };

        pyo3_async_runtimes::tokio::get_runtime().spawn(async move {
            let _ = connection.close().await;
        });
    }
}

struct ShutdownWriteUnlessCompleted {
    writer: Option<Arc<RustWriteHalf>>,
}

impl ShutdownWriteUnlessCompleted {
    fn new(writer: Arc<RustWriteHalf>) -> Self {
        Self { writer: Some(writer) }
    }

    fn completed(&mut self) {
        self.writer = None;
    }
}

impl Drop for ShutdownWriteUnlessCompleted {
    fn drop(&mut self) {
        let Some(writer) = self.writer.take() else {
            return;
        };

        pyo3_async_runtimes::tokio::get_runtime().spawn(async move {
            let _ = writer.shutdown_write().await;
        });
    }
}

// -------------------------------------------------------------------------------------------------
// Value / diagnostic classes
// -------------------------------------------------------------------------------------------------

#[pyclass(name = "Endpoint", frozen, skip_from_py_object)]
#[derive(Clone)]
pub struct PyEndpoint {
    inner: RustEndpoint,
}

impl PyEndpoint {
    fn from_rust(inner: RustEndpoint) -> Self {
        Self { inner }
    }
}

#[pymethods]
impl PyEndpoint {
    #[new]
    pub fn new(name: String) -> PyResult<Self> {
        Ok(Self { inner: RustEndpoint::new(name).map_err(to_py_err)? })
    }

    #[getter]
    pub fn name(&self) -> String {
        self.inner.as_str().to_owned()
    }

    pub fn __str__(&self) -> String {
        self.inner.as_str().to_owned()
    }

    pub fn __repr__(&self) -> String {
        format!("Endpoint({:?})", self.inner.as_str())
    }
}

#[pyclass(name = "ConnectionInfo", frozen, skip_from_py_object)]
#[derive(Clone)]
pub struct PyConnectionInfo {
    #[pyo3(get)]
    pub id: u64,
    #[pyo3(get)]
    pub local_endpoint: Option<String>,
    #[pyo3(get)]
    pub peer_endpoint: Option<String>,
    #[pyo3(get)]
    pub is_closed: bool,
    #[pyo3(get)]
    pub is_read_shutdown: bool,
    #[pyo3(get)]
    pub is_write_shutdown: bool,
    #[pyo3(get)]
    pub peer_sent_eof: bool,
}

impl From<RustConnectionInfo> for PyConnectionInfo {
    fn from(info: RustConnectionInfo) -> Self {
        Self {
            id: info.id,
            local_endpoint: info.local_endpoint.map(|endpoint| endpoint.to_string()),
            peer_endpoint: info.peer_endpoint.map(|endpoint| endpoint.to_string()),
            is_closed: info.is_closed,
            is_read_shutdown: info.is_read_shutdown,
            is_write_shutdown: info.is_write_shutdown,
            peer_sent_eof: info.peer_sent_eof,
        }
    }
}

#[pymethods]
impl PyConnectionInfo {
    pub fn __repr__(&self) -> String {
        format!(
            "ConnectionInfo(id={}, local_endpoint={:?}, peer_endpoint={:?}, closed={}, read_shutdown={}, write_shutdown={}, peer_sent_eof={})",
            self.id,
            self.local_endpoint,
            self.peer_endpoint,
            self.is_closed,
            self.is_read_shutdown,
            self.is_write_shutdown,
            self.peer_sent_eof,
        )
    }
}

#[pyclass(name = "ServerInfo", frozen, skip_from_py_object)]
#[derive(Clone)]
pub struct PyServerInfo {
    #[pyo3(get)]
    pub local_endpoint: String,
    #[pyo3(get)]
    pub is_stopped: bool,
    #[pyo3(get)]
    pub is_paused: bool,
}

impl From<RustServerInfo> for PyServerInfo {
    fn from(info: RustServerInfo) -> Self {
        Self {
            local_endpoint: info.local_endpoint.to_string(),
            is_stopped: info.is_stopped,
            is_paused: info.is_paused,
        }
    }
}

#[pymethods]
impl PyServerInfo {
    pub fn __repr__(&self) -> String {
        format!(
            "ServerInfo(local_endpoint={:?}, stopped={}, paused={})",
            self.local_endpoint, self.is_stopped, self.is_paused,
        )
    }
}

// -------------------------------------------------------------------------------------------------
// Server
// -------------------------------------------------------------------------------------------------

#[pyclass(name = "Server")]
pub struct PyServer {
    inner: Arc<RustServer>,
}

impl PyServer {
    fn from_rust(inner: RustServer) -> Self {
        Self { inner: Arc::new(inner) }
    }
}

#[pymethods]
impl PyServer {
    /// Start a server with default platform permissions/security settings.
    ///
    /// Python:
    ///     server = await Server.start("my-app")
    #[staticmethod]
    pub fn start<'py>(
        py: Python<'py>,
        endpoint: Bound<'py, PyAny>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let endpoint = endpoint_from_py(&endpoint)?;

        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let server = RustServer::start(endpoint).await.map_err(to_py_err)?;
            Python::attach(|py| Py::new(py, PyServer::from_rust(server)))
        })
    }

    /// Bind with the platform-specific mode/security option from the Rust API.
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
            let server = RustServer::bind(&name, parsed_mode).await.map_err(to_py_err)?;
            Python::attach(|py| Py::new(py, PyServer::from_rust(server)))
        })
    }

    /// Wait for the next client connection.
    pub fn accept<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let server = Arc::clone(&self.inner);

        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let connection = server.accept().await.map_err(to_py_err)?;
            Python::attach(|py| Py::new(py, PyConnection::from_rust(connection)))
        })
    }

    pub fn accept_timeout<'py>(
        &self,
        py: Python<'py>,
        timeout: f64,
    ) -> PyResult<Bound<'py, PyAny>> {
        let server = Arc::clone(&self.inner);
        let timeout = duration_from_seconds(timeout)?;

        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let connection = server.accept_timeout(timeout).await.map_err(to_py_err)?;
            Python::attach(|py| Py::new(py, PyConnection::from_rust(connection)))
        })
    }

    /// Async stop. Pending `accept()` operations are interrupted by the Rust server.
    pub fn stop<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let server = Arc::clone(&self.inner);
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            server.stop().await.map_err(to_py_err)
        })
    }

    /// Synchronous compatibility alias mirroring `Server::close()`.
    pub fn close(&self) {
        self.inner.close();
    }

    pub fn pause(&self) {
        self.inner.pause();
    }

    pub fn resume(&self) {
        self.inner.resume();
    }

    pub fn is_paused(&self) -> bool {
        self.inner.is_paused()
    }

    pub fn is_stopped(&self) -> bool {
        self.inner.is_stopped()
    }

    pub fn is_closed(&self) -> bool {
        self.inner.is_closed()
    }

    pub fn is_started(&self) -> bool {
        self.inner.is_started()
    }

    pub fn is_accepting(&self) -> bool {
        self.inner.is_accepting()
    }

    #[getter]
    pub fn name(&self) -> String {
        self.inner.name().to_owned()
    }

    #[getter]
    pub fn endpoint(&self, py: Python<'_>) -> PyResult<Py<PyEndpoint>> {
        Py::new(py, PyEndpoint::from_rust(self.inner.endpoint().clone()))
    }

    pub fn info(&self, py: Python<'_>) -> PyResult<Py<PyServerInfo>> {
        Py::new(py, PyServerInfo::from(self.inner.info()))
    }

    pub fn __repr__(&self) -> String {
        format!(
            "Server(name={:?}, started={}, paused={}, stopped={}, accepting={})",
            self.inner.name(),
            self.inner.is_started(),
            self.inner.is_paused(),
            self.inner.is_stopped(),
            self.inner.is_accepting(),
        )
    }
}

// -------------------------------------------------------------------------------------------------
// Connection
// -------------------------------------------------------------------------------------------------

#[pyclass(name = "Connection")]
pub struct PyConnection {
    // The Option exists only to model Rust's consuming `into_split(self)` operation. It is not a
    // second transport state machine. I/O futures clone the Arc briefly and never hold this mutex
    // across `.await`, so receive and send remain fully duplex.
    inner: StdMutex<Option<Arc<RustConnection>>>,
}

impl PyConnection {
    fn from_rust(inner: RustConnection) -> Self {
        Self { inner: StdMutex::new(Some(Arc::new(inner))) }
    }

    fn connection(&self) -> PyResult<Arc<RustConnection>> {
        self.inner
            .lock()
            .map_err(|_| PyRuntimeError::new_err(" connection wrapper state is poisoned"))?
            .as_ref()
            .cloned()
            .ok_or_else(|| PyRuntimeError::new_err("Connection was consumed by into_split()"))
    }

    fn take_for_split(&self) -> PyResult<RustConnection> {
        let mut guard = self
            .inner
            .lock()
            .map_err(|_| PyRuntimeError::new_err(" connection wrapper state is poisoned"))?;

        let connection = guard.take().ok_or_else(|| {
            PyRuntimeError::new_err("Connection was already consumed by into_split()")
        })?;

        match Arc::try_unwrap(connection) {
            Ok(connection) => Ok(connection),
            Err(connection) => {
                *guard = Some(connection);
                Err(PyRuntimeError::new_err(
                    "cannot split Connection while an async operation is still pending",
                ))
            }
        }
    }
}

#[pymethods]
impl PyConnection {
    #[staticmethod]
    pub fn connect<'py>(
        py: Python<'py>,
        endpoint: Bound<'py, PyAny>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let endpoint = endpoint_from_py(&endpoint)?;

        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let connection = RustConnection::connect(endpoint).await.map_err(to_py_err)?;
            Python::attach(|py| Py::new(py, PyConnection::from_rust(connection)))
        })
    }

    #[staticmethod]
    pub fn connect_timeout<'py>(
        py: Python<'py>,
        endpoint: Bound<'py, PyAny>,
        timeout: f64,
    ) -> PyResult<Bound<'py, PyAny>> {
        let endpoint = endpoint_from_py(&endpoint)?;
        let timeout = duration_from_seconds(timeout)?;

        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let connection =
                RustConnection::connect_timeout(endpoint, timeout).await.map_err(to_py_err)?;
            Python::attach(|py| Py::new(py, PyConnection::from_rust(connection)))
        })
    }

    /// Receive up to `max_bytes`. `b""` means EOF for non-zero `max_bytes`, matching asyncio's
    /// normal stream convention. `read(0)` also returns `b""` without waiting; use `at_eof()` when
    /// the distinction matters.
    pub fn receive<'py>(&self, py: Python<'py>, max_bytes: usize) -> PyResult<Bound<'py, PyAny>> {
        let connection = self.connection()?;

        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let data = receive_bytes(connection, max_bytes).await?;
            bytes_to_python(&data)
        })
    }

    pub fn read<'py>(&self, py: Python<'py>, max_bytes: usize) -> PyResult<Bound<'py, PyAny>> {
        self.receive(py, max_bytes)
    }

    pub fn read_bytes<'py>(
        &self,
        py: Python<'py>,
        max_bytes: usize,
    ) -> PyResult<Bound<'py, PyAny>> {
        self.receive(py, max_bytes)
    }

    pub fn read_timeout<'py>(
        &self,
        py: Python<'py>,
        max_bytes: usize,
        timeout: f64,
    ) -> PyResult<Bound<'py, PyAny>> {
        let connection = self.connection()?;
        let timeout = duration_from_seconds(timeout)?;

        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let data = connection.read_timeout(max_bytes, timeout).await.map_err(to_py_err)?;
            bytes_to_python(&data)
        })
    }

    /// Send the complete buffer. Same-direction sends are serialized by `Connection`.
    pub fn send<'py>(&self, py: Python<'py>, data: Vec<u8>) -> PyResult<Bound<'py, PyAny>> {
        let connection = self.connection()?;

        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let mut cancellation_guard =
                CloseConnectionUnlessCompleted::new(Arc::clone(&connection));
            let result = connection.send(&data).await.map_err(to_py_err);
            if result.is_ok() {
                cancellation_guard.completed();
            }
            result
        })
    }

    /// One possibly-partial write. On Python-side cancellation the connection is closed because the
    /// caller does not receive the partial progress count.
    pub fn write<'py>(&self, py: Python<'py>, data: Vec<u8>) -> PyResult<Bound<'py, PyAny>> {
        let connection = self.connection()?;

        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let mut cancellation_guard =
                CloseConnectionUnlessCompleted::new(Arc::clone(&connection));
            let result = connection.write(&data).await.map_err(to_py_err);
            if result.is_ok() {
                cancellation_guard.completed();
            }
            result
        })
    }

    pub fn write_bytes<'py>(
        &self,
        py: Python<'py>,
        data: Vec<u8>,
    ) -> PyResult<Bound<'py, PyAny>> {
        self.write(py, data)
    }

    pub fn write_all<'py>(&self, py: Python<'py>, data: Vec<u8>) -> PyResult<Bound<'py, PyAny>> {
        self.send(py, data)
    }

    pub fn write_all_bytes<'py>(
        &self,
        py: Python<'py>,
        data: Vec<u8>,
    ) -> PyResult<Bound<'py, PyAny>> {
        self.send(py, data)
    }

    pub fn write_all_timeout<'py>(
        &self,
        py: Python<'py>,
        data: Vec<u8>,
        timeout: f64,
    ) -> PyResult<Bound<'py, PyAny>> {
        let connection = self.connection()?;
        let timeout = duration_from_seconds(timeout)?;

        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let mut cancellation_guard =
                CloseConnectionUnlessCompleted::new(Arc::clone(&connection));
            let result = connection.write_all_timeout(&data, timeout).await.map_err(to_py_err);
            if result.is_ok() {
                cancellation_guard.completed();
            }
            result
        })
    }

    pub fn flush<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let connection = self.connection()?;
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            connection.flush().await.map_err(to_py_err)
        })
    }

    pub fn shutdown_read<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let connection = self.connection()?;
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            connection.shutdown_read().await.map_err(to_py_err)
        })
    }

    pub fn shutdown_write<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let connection = self.connection()?;
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let mut cancellation_guard =
                CloseConnectionUnlessCompleted::new(Arc::clone(&connection));
            let result = connection.shutdown_write().await.map_err(to_py_err);
            if result.is_ok() {
                cancellation_guard.completed();
            }
            result
        })
    }

    pub fn close<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let connection = self.connection()?;
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let mut cancellation_guard =
                CloseConnectionUnlessCompleted::new(Arc::clone(&connection));
            let result = connection.close().await.map_err(to_py_err);
            if result.is_ok() {
                cancellation_guard.completed();
            }
            result
        })
    }

    /// Consume the Python connection wrapper and expose the concrete Rust read/write half structs.
    ///
    /// This is synchronous because splitting itself performs no I/O. It fails if another async
    /// operation still holds a temporary Arc to the connection.
    pub fn into_split(&self, py: Python<'_>) -> PyResult<(Py<PyReadHalf>, Py<PyWriteHalf>)> {
        let connection = self.take_for_split()?;
        let (reader, writer) = connection.into_split();

        Ok((
            Py::new(py, PyReadHalf { inner: Arc::new(reader) })?,
            Py::new(py, PyWriteHalf { inner: Arc::new(writer) })?,
        ))
    }

    pub fn is_closed(&self) -> PyResult<bool> {
        Ok(self.connection()?.is_closed())
    }

    pub fn is_read_shutdown(&self) -> PyResult<bool> {
        Ok(self.connection()?.is_read_shutdown())
    }

    pub fn is_write_shutdown(&self) -> PyResult<bool> {
        Ok(self.connection()?.is_write_shutdown())
    }

    pub fn peer_sent_eof(&self) -> PyResult<bool> {
        Ok(self.connection()?.peer_sent_eof())
    }

    pub fn at_eof(&self) -> PyResult<bool> {
        self.peer_sent_eof()
    }

    pub fn is_started(&self) -> PyResult<bool> {
        Ok(self.connection()?.is_started())
    }

    pub fn is_active(&self) -> PyResult<bool> {
        Ok(self.connection()?.is_active())
    }

    pub fn is_available(&self) -> PyResult<bool> {
        Ok(self.connection()?.is_available())
    }

    pub fn is_paused(&self) -> PyResult<bool> {
        Ok(self.connection()?.is_paused())
    }

    pub fn pause(&self) -> PyResult<()> {
        self.connection()?.pause();
        Ok(())
    }

    pub fn resume(&self) -> PyResult<()> {
        self.connection()?.resume();
        Ok(())
    }

    #[getter]
    pub fn id(&self) -> PyResult<u64> {
        Ok(self.connection()?.id())
    }

    #[getter]
    pub fn name(&self) -> PyResult<String> {
        Ok(self.connection()?.name().to_owned())
    }

    #[getter]
    pub fn endpoint(&self, py: Python<'_>) -> PyResult<Py<PyEndpoint>> {
        let connection = self.connection()?;
        Py::new(py, PyEndpoint::from_rust(connection.endpoint().clone()))
    }

    #[getter]
    pub fn local_endpoint(&self) -> PyResult<Option<String>> {
        Ok(self.connection()?.local_endpoint().map(|endpoint| endpoint.to_string()))
    }

    #[getter]
    pub fn peer_endpoint(&self) -> PyResult<Option<String>> {
        Ok(self.connection()?.peer_endpoint().map(|endpoint| endpoint.to_string()))
    }

    pub fn info(&self, py: Python<'_>) -> PyResult<Py<PyConnectionInfo>> {
        Py::new(py, PyConnectionInfo::from(self.connection()?.info()))
    }

    pub fn __repr__(&self) -> String {
        match self.connection() {
            Ok(connection) => format!(
                "Connection(id={}, name={:?}, closed={}, paused={}, read_shutdown={}, write_shutdown={}, at_eof={})",
                connection.id(),
                connection.name(),
                connection.is_closed(),
                connection.is_paused(),
                connection.is_read_shutdown(),
                connection.is_write_shutdown(),
                connection.peer_sent_eof(),
            ),
            Err(_) => "Connection(consumed_by_into_split=True)".to_owned(),
        }
    }
}

// -------------------------------------------------------------------------------------------------
// Concrete directional halves
// -------------------------------------------------------------------------------------------------

#[pyclass(name = "ReadHalf")]
pub struct PyReadHalf {
    inner: Arc<RustReadHalf>,
}

#[pymethods]
impl PyReadHalf {
    pub fn receive<'py>(&self, py: Python<'py>, max_bytes: usize) -> PyResult<Bound<'py, PyAny>> {
        let reader = Arc::clone(&self.inner);
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let data = receive_half_bytes(reader, max_bytes).await?;
            bytes_to_python(&data)
        })
    }

    pub fn read<'py>(&self, py: Python<'py>, max_bytes: usize) -> PyResult<Bound<'py, PyAny>> {
        self.receive(py, max_bytes)
    }

    pub fn shutdown_read<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let reader = Arc::clone(&self.inner);
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            reader.shutdown_read().await.map_err(to_py_err)
        })
    }

    #[getter]
    pub fn id(&self) -> u64 {
        self.inner.id()
    }

    pub fn info(&self, py: Python<'_>) -> PyResult<Py<PyConnectionInfo>> {
        Py::new(py, PyConnectionInfo::from(self.inner.info()))
    }

    pub fn __repr__(&self) -> String {
        format!("ReadHalf(id={})", self.inner.id())
    }
}

#[pyclass(name = "WriteHalf")]
pub struct PyWriteHalf {
    inner: Arc<RustWriteHalf>,
}

#[pymethods]
impl PyWriteHalf {
    pub fn send<'py>(&self, py: Python<'py>, data: Vec<u8>) -> PyResult<Bound<'py, PyAny>> {
        let writer = Arc::clone(&self.inner);

        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let mut cancellation_guard = ShutdownWriteUnlessCompleted::new(Arc::clone(&writer));
            let result = writer.send(&data).await.map_err(to_py_err);
            if result.is_ok() {
                cancellation_guard.completed();
            }
            result
        })
    }

    pub fn write<'py>(&self, py: Python<'py>, data: Vec<u8>) -> PyResult<Bound<'py, PyAny>> {
        let writer = Arc::clone(&self.inner);

        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let mut cancellation_guard = ShutdownWriteUnlessCompleted::new(Arc::clone(&writer));
            let result = writer.write(&data).await.map_err(to_py_err);
            if result.is_ok() {
                cancellation_guard.completed();
            }
            result
        })
    }

    pub fn flush<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let writer = Arc::clone(&self.inner);
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            writer.flush().await.map_err(to_py_err)
        })
    }

    pub fn shutdown_write<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let writer = Arc::clone(&self.inner);
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let mut cancellation_guard = ShutdownWriteUnlessCompleted::new(Arc::clone(&writer));
            let result = writer.shutdown_write().await.map_err(to_py_err);
            if result.is_ok() {
                cancellation_guard.completed();
            }
            result
        })
    }

    #[getter]
    pub fn id(&self) -> u64 {
        self.inner.id()
    }

    pub fn info(&self, py: Python<'_>) -> PyResult<Py<PyConnectionInfo>> {
        Py::new(py, PyConnectionInfo::from(self.inner.info()))
    }

    pub fn __repr__(&self) -> String {
        format!("WriteHalf(id={})", self.inner.id())
    }
}

// -------------------------------------------------------------------------------------------------
// Concrete transport namespace
// -------------------------------------------------------------------------------------------------

#[pyclass(name = "LocalTransport")]
pub struct PyLocalTransport;

#[pymethods]
impl PyLocalTransport {
    #[staticmethod]
    pub fn start<'py>(
        py: Python<'py>,
        endpoint: Bound<'py, PyAny>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let endpoint = endpoint_from_py(&endpoint)?;

        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let server = RustLocalTransport::start(endpoint).await.map_err(to_py_err)?;
            Python::attach(|py| Py::new(py, PyServer::from_rust(server)))
        })
    }

    #[staticmethod]
    pub fn connect<'py>(
        py: Python<'py>,
        endpoint: Bound<'py, PyAny>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let endpoint = endpoint_from_py(&endpoint)?;

        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let connection = RustLocalTransport::connect(endpoint).await.map_err(to_py_err)?;
            Python::attach(|py| Py::new(py, PyConnection::from_rust(connection)))
        })
    }
}

// -------------------------------------------------------------------------------------------------
// Module
// -------------------------------------------------------------------------------------------------

pub fn get_taunicorn_version() -> &'static str {
    static VERSION: OnceLock<String> = OnceLock::new();

    VERSION.get_or_init(|| {
        let version = env!("CARGO_PKG_VERSION");
        version.replace("-alpha", "a").replace("-beta", "b")
    })
}

#[pymodule]
pub fn _taunicorn(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add("__version__", get_taunicorn_version())?;

    m.add_class::<PyEndpoint>()?;
    m.add_class::<PyConnectionInfo>()?;
    m.add_class::<PyServerInfo>()?;
    m.add_class::<PyServer>()?;
    m.add_class::<PyConnection>()?;
    m.add_class::<PyReadHalf>()?;
    m.add_class::<PyWriteHalf>()?;
    m.add_class::<PyLocalTransport>()?;

    // Compatibility aliases for the previous Python-facing names. They refer to the new concrete
    // classes; no legacy wrapper implementation remains.
    m.add("Listener", m.getattr("Server")?)?;
    m.add("Stream", m.getattr("Connection")?)?;
    m.add("Client", m.getattr("Connection")?)?;

    Ok(())
}
