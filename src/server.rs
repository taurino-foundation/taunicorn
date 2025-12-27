use bytes::Bytes;
use interprocess::local_socket::{
    GenericFilePath, ListenerOptions, ToFsName as _,
    traits::tokio::{Listener as _, Stream as _},
};
#[cfg(unix)]
use interprocess::os::unix::local_socket::ListenerOptionsExt as _;
#[cfg(windows)]
use interprocess::os::windows::local_socket::ListenerOptionsExt as _;
#[cfg(windows)]
use interprocess::os::windows::security_descriptor::SecurityDescriptor;
use pyo3::{IntoPyObjectExt, prelude::*};
use std::{
    path::PathBuf,
    sync::Arc,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::Mutex;
use tokio::sync::mpsc::channel;
use widestring::U16CString;

use crate::channel::{ReceiverHandle, SenderHandle};

/// # serve - Local Socket Server for Python Integration
/// 
/// Creates an async Unix domain socket (or Windows named pipe) server that integrates with Python.
/// The server accepts connections and delegates client handling to a Python callback function.
/// 
/// ## Arguments:
/// - `py`: Python interpreter context
/// - `path`: Filesystem path for the socket/pipe (converted to OS-specific format)
/// - `handler`: Python callable that processes client connections
/// - `sddl`: Optional Windows Security Descriptor Definition Language string (Windows only)
/// 
/// ## Returns:
/// - PyResult containing an async handle that runs the server loop
/// 
/// ## Process Flow:
/// 1. Converts the path to OS-specific socket/pipe name
/// 2. Configures listener options with platform-specific settings
/// 3. Creates async listener and enters infinite accept loop
/// 4. For each client: spawns reader/writer tasks and invokes Python handler
/// 
/// ## Architecture:
/// - Uses bidirectional channels (mpsc) for async communication between:
///   - Socket I/O tasks (Rust)
///   - Python handler (Python runtime)
/// - Reader task: Reads from socket → forwards to Python via channel
/// - Writer task: Receives from Python via channel → writes to socket
/// - Python handler: Receives SenderHandle/ReceiverHandle for bidirectional communication
#[pyfunction]
pub fn server<'py>(
    py: Python<'py>,
    path: String,
    handler: Py<PyAny>,
    sddl: Option<String>,
) -> PyResult<Bound<'py, PyAny>> {
    // Convert path to OS-specific filesystem name for local socket/pipe
    let name = PathBuf::from(path)
        .to_fs_name::<GenericFilePath>()
        .map_err(|e| pyo3::exceptions::PyValueError::new_err(e.to_string()))?
        .into_owned();

    // Initialize listener configuration
    let mut opts = ListenerOptions::new();

    // Windows-specific security configuration using SDDL (Security Descriptor)
    #[cfg(windows)]
    if let Some(sddl) = sddl {
        // Convert SDDL string to Windows UCS-2 format
        let sddl = U16CString::from_str(&sddl)
            .map_err(|e| pyo3::exceptions::PyValueError::new_err(e.to_string()))?;
        // Parse security descriptor from SDDL string
        let sd = SecurityDescriptor::deserialize(&sddl)
            .map_err(|e| pyo3::exceptions::PyValueError::new_err(e.to_string()))?;
        // Apply security descriptor to listener options
        opts = opts.security_descriptor(sd);
    }

    // Convert async Rust future to Python awaitable using tokio runtime
    pyo3_async_runtimes::tokio::future_into_py::<_, Py<PyAny>>(py, async move {
        // Create async listener with configured options
        let listener = opts
            .name(name)
            .create_tokio()
            .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(e.to_string()))?;
        
        // Main server loop - continuously accepts new client connections
        loop {
            // Accept incoming client connection
            let stream = listener.accept().await
                .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(e.to_string()))?;

            // Clone Python handler reference for this connection
            let handler = Python::attach(|py| handler.clone_ref(py));

            // Create bidirectional communication channels:
            // - tx_write/rx_write: Python → Rust writer task → Socket
            // - tx_read/rx_read: Socket → Rust reader task → Python
            let (tx_write, mut rx_write) = channel::<Bytes>(128);
            let (tx_read, rx_read) = channel::<Bytes>(128);

            // Wrap channels in thread-safe references for sharing across tasks
            let sender = Arc::new(tx_write);
            let receiver = Arc::new(Mutex::new(rx_read));

            // Split TCP stream into separate read/write halves
            let (mut read_stream, mut write_stream) = stream.split();

            // ──────────────────────────────────────────────────────────────
            // Reader Task: Reads from socket and forwards to Python
            // ──────────────────────────────────────────────────────────────
            tokio::spawn({
                let tx_read = tx_read.clone();
                async move {
                    let mut buf = [0u8; 4096]; // 4KB read buffer
                    while let Ok(n) = read_stream.read(&mut buf).await {
                        if n == 0 {
                            break; // Connection closed
                        }
                        // Forward received bytes to Python via channel
                        let _ = tx_read
                            .send(Bytes::copy_from_slice(&buf[..n]))
                            .await;
                    }
                }
            });

            // ──────────────────────────────────────────────────────────────
            // Writer Task: Receives from Python and writes to socket
            // ──────────────────────────────────────────────────────────────
            tokio::spawn(async move {
                // Continuously read from Python→Rust channel
                while let Some(bytes) = rx_write.recv().await {
                    let _ = write_stream.write_all(&bytes).await;
                }
            });

            // ──────────────────────────────────────────────────────────────
            // Python Handler Invocation
            // ──────────────────────────────────────────────────────────────
            let fut = Python::attach(|py| -> PyResult<Option<_>> {
                // Create Python-wrapped handles for bidirectional communication
                let py_sender = Py::new(py, SenderHandle { tx: sender })?;
                let py_receiver = Py::new(py, ReceiverHandle { rx: receiver })?;

                // Call Python handler with sender/receiver objects
                let result = handler.call1(py, (py_sender, py_receiver))?.into_bound_py_any(py)?;
                
                // Convert Python awaitable to Rust future if handler is async
                Ok(pyo3_async_runtimes::tokio::into_future(result).ok())
            })?;

            // If handler returned an async future, await it
            if let Some(fut) = fut {
                fut.await?;
            }
        }

        // Loop is infinite, this point is never reached
        // Required for type system but marked as unreachable
        #[allow(unreachable_code)]
        Ok(Python::attach(|py| py.None()))
    })
}