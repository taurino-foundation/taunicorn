use bytes::Bytes;
use interprocess::local_socket::tokio::Stream;
use interprocess::local_socket::{
    GenericFilePath, ToFsName as _,
    traits::tokio::{ Stream as _},
};
use pyo3::{IntoPyObjectExt, prelude::*};
use std::{
    path::PathBuf,
    sync::Arc,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::Mutex;
use tokio::sync::mpsc::channel;

use crate::channel::{ReceiverHandle, SenderHandle};

/// # connect - Local Socket Client for Python Integration
/// 
/// Establishes a client connection to a Unix domain socket (or Windows named pipe) 
/// and provides bidirectional communication to a Python handler.
/// 
/// ## Arguments:
/// - `py`: Python interpreter context
/// - `path`: Filesystem path for the socket/pipe (converted to OS-specific format)
/// - `handler`: Python callable that processes the connection
/// 
/// ## Returns:
/// - PyResult containing an async handle that completes immediately after connection setup
/// 
/// ## Key Differences from `serve()`:
/// - **Single connection**: Connects once instead of accepting multiple clients
/// - **Fire-and-forget**: Spawns background tasks and returns immediately
/// - **Non-blocking handler**: Python handler runs detached in background
/// 
/// ## Process Flow:
/// 1. Converts path to OS-specific socket/pipe name
/// 2. Establishes connection to server
/// 3. Creates bidirectional channels for Python↔Socket communication
/// 4. Spawns reader/writer tasks for socket I/O
/// 5. Invokes Python handler with communication handles
/// 6. Returns immediately while tasks run in background
/// 
/// ## Architecture:
/// - Creates separate async tasks for reading and writing
/// - Reader: Socket → Channel → Python
/// - Writer: Python → Channel → Socket
/// - Python handler receives SenderHandle/ReceiverHandle for full-duplex communication
#[pyfunction]
pub fn connect<'py>(
    py: Python<'py>,
    path: String,
    handler: Py<PyAny>,
) -> PyResult<Bound<'py, PyAny>> {
    // Convert path to OS-specific filesystem name for local socket/pipe
    let name = PathBuf::from(path)
        .to_fs_name::<GenericFilePath>()?
        .into_owned();

    // Create bidirectional communication channels with 128-message capacity:
    // - tx_write/rx_write: Python → Rust writer task → Socket
    // - tx_read/rx_read: Socket → Rust reader task → Python
    let (tx_write, mut rx_write) = channel::<Bytes>(128);
    let (tx_read, rx_read) = channel::<Bytes>(128);

    // Wrap channels in thread-safe references for sharing across tasks
    let sender = Arc::new(tx_write);
    let receiver = Arc::new(Mutex::new(rx_read));

    // Convert async Rust future to Python awaitable using tokio runtime
    pyo3_async_runtimes::tokio::future_into_py::<_, Py<PyAny>>(py, async move {
        // Establish connection to the server socket/pipe
        let stream = Stream::connect(name).await?;

        // Split TCP stream into separate read/write halves for concurrent access
        let (mut read_stream, mut write_stream) = stream.split();

        // ──────────────────────────────────────────────────────────────
        // Reader Task: Continuously reads from socket and forwards to Python
        // ──────────────────────────────────────────────────────────────
        tokio::spawn({
            let tx_read = tx_read.clone(); // Clone channel sender for the task
            async move {
                let mut buf = [0u8; 4096]; // 4KB read buffer
                
                // Read loop: Continuously read from socket while connection is open
                while let Ok(n) = read_stream.read(&mut buf).await {
                    if n == 0 { 
                        break; // Connection closed by remote end
                    }
                    // Forward received bytes to Python via channel
                    let _ = tx_read.send(Bytes::copy_from_slice(&buf[..n])).await;
                }
                // Task ends automatically when connection closes
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
            // Task ends when channel is closed (sender dropped)
        });

        // ──────────────────────────────────────────────────────────────
        // Python Handler Invocation (DETACHED)
        // ──────────────────────────────────────────────────────────────
        // Note: Handler runs detached - function returns immediately while handler runs in background
        Python::attach(|py| -> PyResult<()> {
            // Create Python-wrapped handles for bidirectional communication
            let py_sender = Py::new(py, SenderHandle { tx: sender })?;
            let py_receiver = Py::new(py, ReceiverHandle { rx: receiver })?;

            // Call Python handler with sender/receiver objects
            let result = handler.call1(py, (py_sender, py_receiver))?.into_bound_py_any(py)?;
            
            // Check if handler returned an async coroutine
            if let Ok(fut) = pyo3_async_runtimes::tokio::into_future(result) {
                // Spawn handler as detached task - runs independently
                tokio::spawn(async move {
                    let _ = fut.await; // Execute handler but don't block on it
                });
            }
            // Note: If handler is synchronous, it runs immediately and completes here
            Ok(())
        })?;

        // ──────────────────────────────────────────────────────────────
        // IMMEDIATE RETURN - Fire-and-forget pattern
        // ──────────────────────────────────────────────────────────────
        // connect() returns immediately after:
        // 1. Connection established ✓
        // 2. I/O tasks spawned ✓  
        // 3. Python handler invoked ✓
        // 
        // Background tasks continue running independently
        Ok(Python::attach(|py| py.None()))
    })
}

// ──────────────────────────────────────────────────────────────
// CONNECTION LIFECYCLE:
// ──────────────────────────────────────────────────────────────
// 1. Connection Phase: Blocking connect() call
// 2. Setup Phase: Channels created, tasks spawned
// 3. Runtime Phase: I/O tasks run, Python handler executes
// 4. Cleanup Phase: Tasks exit when connection closes
// 
// DATA FLOW:
// Python → SenderHandle → Channel → Writer Task → Socket
// Socket → Reader Task → Channel → ReceiverHandle → Python
// 
// THREADING MODEL:
// - Tokio runtime manages all async tasks
// - Channels provide thread-safe message passing
// - Arc/Mutex enable shared access between tasks
// ──────────────────────────────────────────────────────────────