//! Python bindings around the EXISTING taunicorn::security implementation.
//! No handshake, cipher, framing, nonce, or key-derivation algorithm is duplicated.

use std::sync::Arc;

use pyo3::exceptions::{PyConnectionError, PyEOFError, PyOSError, PyOverflowError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyBytesMethods};
use pyo3_async_runtimes::tokio::{future_into_py_with_locals, get_current_locals, get_runtime};
use taunicorn::security::{self as rust_security, SecureConnection as RustSecureConnection};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use crate::{PyConnection, bytes_to_python};

// Protocol-v2 size limit: 64 MiB includes version (1), sequence (8), and AEAD tag (16).
// Prevalidate BEFORE invoking Rust encryption so a rejected size never uses a nonce.
const MAX_PLAINTEXT_SIZE: usize = 64 * 1024 * 1024 - 25;

fn key32(value: &Bound<'_, PyBytes>, name: &str) -> PyResult<[u8; 32]> {
    value
        .as_bytes()
        .try_into()
        .map_err(|_| PyValueError::new_err(format!("{name} must contain exactly 32 bytes")))
}

// The unchanged core exposes anyhow::Error, not a typed protocol error enum.
// These exact-message mappings intentionally refer to the uploaded v2 implementation.
fn secure_error(error: anyhow::Error) -> PyErr {
    let message = error.to_string();
    if message == "connection closed while reading a frame" {
        PyEOFError::new_err(message)
    } else if message == "secure session send sequence exhausted"
        || message == "secure session receive sequence exhausted"
    {
        PyOverflowError::new_err(message)
    } else if error.downcast_ref::<std::io::Error>().is_some() {
        PyOSError::new_err(format!("{error:#}"))
    } else {
        PyConnectionError::new_err(format!("{error:#}"))
    }
}

fn closed_error() -> PyErr {
    PyConnectionError::new_err(
        "secure session is closed or was cancelled; create a new connection",
    )
}

/// Binding-only lifetime and cancellation policy. Cryptographic state stays in `inner`.
pub(crate) struct SecureSession {
    pub(crate) inner: RustSecureConnection,
    pub(crate) cancel: CancellationToken,
    // These outer gates keep cancellation poisoning ordered with future destruction.
    // Separate gates retain full-duplex operation; no single lock covers both directions.
    send_gate: Mutex<()>,
    receive_gate: Mutex<()>,
}

impl SecureSession {
    fn new(inner: RustSecureConnection, cancel: CancellationToken) -> Arc<Self> {
        let session = Arc::new(Self {
            inner,
            cancel: cancel.clone(),
            send_gate: Mutex::new(()),
            receive_gate: Mutex::new(()),
        });
        // The waiter does NOT keep the session alive. Drop cancels the waiter too.
        // The token becomes terminal synchronously; OS cleanup follows on Tokio.
        let weak = Arc::downgrade(&session);
        get_runtime().spawn(async move {
            cancel.cancelled().await;
            if let Some(session) = weak.upgrade() {
                let _ = session.inner.close().await;
            }
        });
        session
    }

    pub(crate) fn is_closed(&self) -> bool {
        self.cancel.is_cancelled() || self.inner.connection().is_closed()
    }

    fn ensure_open(&self) -> PyResult<()> {
        if self.is_closed() { Err(closed_error()) } else { Ok(()) }
    }
}

impl Drop for SecureSession {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

// In addition to Rust Drop guards, observe Python Future cancellation. This covers
// the boundary where Rust already completed but Python discards the result.
#[pyclass(frozen)]
struct CancelOnPythonCancellation {
    token: CancellationToken,
}

#[pymethods]
impl CancelOnPythonCancellation {
    fn __call__(&self, future: &Bound<'_, PyAny>) -> PyResult<()> {
        if future.call_method0("cancelled")?.extract::<bool>()? {
            self.token.cancel();
        }
        Ok(())
    }
}

fn observe_cancellation<'py>(
    py: Python<'py>,
    future: Bound<'py, PyAny>,
    token: CancellationToken,
) -> PyResult<Bound<'py, PyAny>> {
    let guard = token.clone().drop_guard();
    let callback = Py::new(py, CancelOnPythonCancellation { token })?;
    if let Err(error) = future.call_method1("add_done_callback", (callback,)) {
        let _ = future.call_method0("cancel");
        return Err(error);
    }
    let _ = guard.disarm();
    Ok(future)
}

#[pyfunction]
pub(crate) fn generate_identity_private_key<'py>(
    py: Python<'py>,
) -> PyResult<Bound<'py, PyBytes>> {
    let key = py
        .detach(rust_security::generate_identity_private_key)
        .map_err(|error| PyOSError::new_err(format!("{error:#}")))?;
    Ok(PyBytes::new(py, &key))
}

#[pyfunction]
pub(crate) fn identity_public_key<'py>(
    py: Python<'py>,
    identity_private_key: Bound<'py, PyBytes>,
) -> PyResult<Bound<'py, PyBytes>> {
    let key = key32(&identity_private_key, "Ed25519 private identity key")?;
    let public_key = py.detach(move || rust_security::identity_public_key(&key));
    Ok(PyBytes::new(py, &public_key))
}

/// Native bytes-oriented API. The small Python facade adds coroutine/MessagePack compatibility.
#[pyclass(name = "SecureConnection", module = "taunicorn._taunicorn", frozen)]
pub(crate) struct PySecureConnection {
    session: Arc<SecureSession>,
    connection: Py<PyConnection>,
}

// If a successful handshake result is discarded (e.g. a cancelled Python Future),
// retaining the original Connection object must not leave an orphaned live session.
impl Drop for PySecureConnection {
    fn drop(&mut self) {
        self.session.cancel.cancel();
    }
}

impl PySecureConnection {
    fn establish<'py>(
        py: Python<'py>,
        connection: Py<PyConnection>,
        identity_private_key: [u8; 32],
        peer_identity_public_key: [u8; 32],
        client: bool,
    ) -> PyResult<Bound<'py, PyAny>> {
        // Resolve the running event loop BEFORE transferring transport ownership.
        let locals = get_current_locals(py)?;
        let token = CancellationToken::new();
        let raw = connection.borrow(py).begin_secure(token.clone())?;
        let cancel_guard = token.clone().drop_guard();
        let future_token = token.clone();
        let future = future_into_py_with_locals(py, locals, async move {
            // This guard is captured before the first poll, so even an unstarted
            // cancelled future leaves the upgrading wrapper in a terminal state.
            let guard = cancel_guard;
            let handshake = async move {
                if client {
                    RustSecureConnection::client(
                        raw,
                        identity_private_key,
                        peer_identity_public_key,
                    )
                    .await
                } else {
                    // The unchanged Rust server API orders peer key before private key.
                    RustSecureConnection::server(
                        raw,
                        peer_identity_public_key,
                        identity_private_key,
                    )
                    .await
                }
            };
            let secure = tokio::select! {
                biased;
                _ = future_token.cancelled() => return Err(closed_error()),
                result = handshake => result.map_err(secure_error)?,
            };
            let session = SecureSession::new(secure, future_token);
            let result = Python::attach(|py| -> PyResult<Py<Self>> {
                let native = Py::new(
                    py,
                    Self { session: Arc::clone(&session), connection: connection.clone_ref(py) },
                )?;
                // Preserve Python object identity: secure.connection is the original object.
                // Its owner is now the secure session, not a cloned raw Connection.
                connection.borrow(py).finish_secure(session)?;
                Ok(native)
            })?;
            let _ = guard.disarm();
            Ok(result)
        })?;
        observe_cancellation(py, future, token)
    }
}

#[pymethods]
impl PySecureConnection {
    #[staticmethod]
    #[pyo3(signature = (connection, *, identity_private_key, server_identity_public_key))]
    fn client<'py>(
        py: Python<'py>,
        connection: Py<PyConnection>,
        identity_private_key: Bound<'py, PyBytes>,
        server_identity_public_key: Bound<'py, PyBytes>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let peer = key32(&server_identity_public_key, "server identity public key")?;
        let private = key32(&identity_private_key, "Ed25519 private identity key")?;
        Self::establish(py, connection, private, peer, true)
    }

    #[staticmethod]
    #[pyo3(signature = (connection, *, client_identity_public_key, identity_private_key))]
    fn server<'py>(
        py: Python<'py>,
        connection: Py<PyConnection>,
        client_identity_public_key: Bound<'py, PyBytes>,
        identity_private_key: Bound<'py, PyBytes>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let peer = key32(&client_identity_public_key, "client identity public key")?;
        let private = key32(&identity_private_key, "Ed25519 private identity key")?;
        Self::establish(py, connection, private, peer, false)
    }

    #[getter]
    fn connection(&self, py: Python<'_>) -> Py<PyConnection> {
        self.connection.clone_ref(py)
    }

    fn send<'py>(
        &self,
        py: Python<'py>,
        plaintext: Bound<'py, PyBytes>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let data = plaintext.as_bytes();
        if data.len() > MAX_PLAINTEXT_SIZE {
            return Err(PyValueError::new_err("frame exceeds maximum size"));
        }
        let data = data.to_vec();
        let locals = get_current_locals(py)?;
        let session = Arc::clone(&self.session);
        let token = session.cancel.clone();
        let future = future_into_py_with_locals(py, locals, async move {
            let _gate = tokio::select! {
                biased;
                _ = session.cancel.cancelled() => return Err(closed_error()),
                gate = session.send_gate.lock() => gate,
            };
            session.ensure_open()?;
            // Declared AFTER the gate, hence cancelled BEFORE that gate is released.
            let guard = session.cancel.clone().drop_guard();
            tokio::select! {
                biased;
                _ = session.cancel.cancelled() => return Err(closed_error()),
                result = session.inner.send(&data) => result.map_err(secure_error)?,
            }
            let _ = guard.disarm();
            Ok(())
        })?;
        observe_cancellation(py, future, token)
    }

    fn receive<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let locals = get_current_locals(py)?;
        let session = Arc::clone(&self.session);
        let token = session.cancel.clone();
        let future = future_into_py_with_locals(py, locals, async move {
            let _gate = tokio::select! {
                biased;
                _ = session.cancel.cancelled() => return Err(closed_error()),
                gate = session.receive_gate.lock() => gate,
            };
            session.ensure_open()?;
            let guard = session.cancel.clone().drop_guard();
            let plaintext = tokio::select! {
                biased;
                _ = session.cancel.cancelled() => return Err(closed_error()),
                result = session.inner.receive() => result.map_err(secure_error)?,
            };
            // Explicit PyBytes conversion: never depend on Vec<u8> -> Python conversion.
            let result = bytes_to_python(&plaintext)?;
            let _ = guard.disarm();
            Ok(result)
        })?;
        observe_cancellation(py, future, token)
    }

    fn close<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let locals = get_current_locals(py)?;
        self.session.cancel.cancel();
        let session = Arc::clone(&self.session);
        future_into_py_with_locals(py, locals, async move {
            session.inner.close().await.map_err(secure_error)
        })
    }

    /// Immediately prevent further secure I/O; cleanup is scheduled on Tokio.
    fn abort(&self) {
        self.session.cancel.cancel();
    }

    fn is_closed(&self) -> bool {
        self.session.is_closed()
    }
}

pub(crate) fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_class::<PySecureConnection>()?;
    module.add_function(wrap_pyfunction!(generate_identity_private_key, module)?)?;
    module.add_function(wrap_pyfunction!(identity_public_key, module)?)?;
    Ok(())
}
