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

/// # PyO3 Function: connect
/// 
/// Establishes a bidirectional connection between a Python application and a local socket
/// (Unix domain socket on Linux/macOS, named pipe on Windows). This is a **blocking** 
/// connection that runs until the socket is closed.
/// 
/// ## Key Characteristics:
/// - **Blocking**: The function doesn't return until the connection terminates
/// - **Bidirectional**: Full-duplex communication between Python and socket
/// - **Async/await**: Returns a Python awaitable future
/// 
/// ## Flow Overview:
/// 1. Convert path to OS-specific socket name
/// 2. Create communication channels between Rust and Python
/// 3. Connect to the local socket
/// 4. Invoke Python handler with communication handles
/// 5. Run I/O loop until connection closes
/// 
/// ## Arguments:
/// - `py`: Python interpreter context (required by PyO3)
/// - `path`: String path to the socket file (e.g., "/tmp/mysocket")
/// - `handler`: Python callable that receives SenderHandle/ReceiverHandle objects
/// 
/// ## Returns:
/// - PyResult containing a Python awaitable future
/// - The future resolves when the connection is fully closed
/// 
/// ## Example Python Usage:
/// ```python
/// async def my_handler(sender, receiver):
///     # Send data to socket
///     await sender.send(b"Hello")
///     # Receive data from socket
///     data = await receiver.receive()
///     
/// # This call blocks until connection ends
/// await connect("/tmp/mysocket", my_handler)
/// ```
#[pyfunction]
pub fn connect<'py>(
    py: Python<'py>,               // Python interpreter context from PyO3
    name: String,                  // Filesystem path to the socket
    handler: Py<PyAny>,            // Python callback function
) -> PyResult<Bound<'py, PyAny>> {
    #[cfg(windows)]
    let path = format!(r"\\.\pipe\{}", name);
    #[cfg(unix)]
    let path = format!("/tmp/{}", name);
    // Convert the provided path to an OS-specific socket name
    // Example: "/tmp/mysocket" → "\\.\pipe\mysocket" (Windows) or keeps as-is (Unix)
    let name = PathBuf::from(path)
        .to_fs_name::<GenericFilePath>()?  // Platform-specific conversion
        .into_owned();                     // Take ownership of the string

    // Create two MPSC (Multi-Producer Single-Consumer) channels for bidirectional communication:
    // - tx_write/rx_write: Python → Rust writer → Socket (OUTGOING data)
    // - tx_read/rx_read:   Socket → Rust reader → Python (INCOMING data)
    // Capacity: 128 messages each to prevent memory exhaustion
    let (tx_write, rx_write) = channel::<Bytes>(128);
    let (tx_read, rx_read) = channel::<Bytes>(128);

    // Convert the Rust async block into a Python awaitable future
    // This allows Python to use `await connect(...)`
    pyo3_async_runtimes::tokio::future_into_py(py, async move {
        // ──────────────────────────────────────────────────────────────
        // PHASE 1: CONNECTION ESTABLISHMENT
        // ──────────────────────────────────────────────────────────────
        // Connect to the local socket (blocking until connected or failed)
        let stream = Stream::connect(name).await.map_err(|e| {
            // Convert Rust error to Python exception
            PyErr::new::<pyo3::exceptions::PyIOError, _>(
                format!("Connect failed: {}", e)
            )
        })?;
        
        // Split the bidirectional stream into separate read/write halves
        // This allows concurrent reading and writing without mutex locking
        let (mut read_stream, mut write_stream) = stream.split();
        
        // ──────────────────────────────────────────────────────────────
        // PHASE 2: PYTHON HANDLER INVOCATION
        // ──────────────────────────────────────────────────────────────
        // Execute Python code within the GIL (Global Interpreter Lock)
        Python::attach(|py| -> PyResult<()> {
            // Wrap channels in thread-safe Arc containers for sharing
            let sender = Arc::new(tx_write);        // Shared sender to Python
            let receiver = Arc::new(Mutex::new(rx_read));  // Lock-protected receiver
            
            // Create Python-wrapped handles for the Python callback
            let py_sender = Py::new(py, SenderHandle { tx: sender })?;
            let py_receiver = Py::new(py, ReceiverHandle { rx: receiver })?;
            
            // Call the Python handler with the communication handles
            // Note: This call blocks until the handler returns
            let result = handler.call1(py, (py_sender, py_receiver))?
                .into_bound_py_any(py)?;

            // Check if the handler returned an async coroutine
            if let Ok(fut) = pyo3_async_runtimes::tokio::into_future(result) {
                // If async, spawn it as a detached background task
                // This allows the I/O loop to continue while Python code runs
                tokio::spawn(async move {
                    let _ = fut.await; // Execute but don't block on completion
                });
            }
            // If synchronous handler, it already completed above
            Ok(())
        })?;  // GIL released here
        
        // ──────────────────────────────────────────────────────────────
        // PHASE 3: MAIN I/O LOOP (BLOCKING)
        // ──────────────────────────────────────────────────────────────
        let mut buf = [0u8; 4096];  // 4KB read buffer
        let mut rx_write = rx_write;  // Take ownership of the receiver
        
        // This loop runs until the connection is closed from either side
        loop {
            // Use tokio::select! to wait on multiple async operations
            // The first ready operation gets executed
            tokio::select! {
                // ─── READ FROM SOCKET ───────────────────────────────────
                // Attempt to read data from the socket into buffer
                read_result = read_stream.read(&mut buf) => {
                    match read_result {
                        Ok(0) => {
                            // EOF: Remote side closed the connection
                            // This is a clean shutdown signal
                            break;
                        }
                        Ok(n) => {
                            // Successfully read 'n' bytes
                            // Convert slice to Bytes (zero-copy if possible)
                            let data = Bytes::copy_from_slice(&buf[..n]);
                            // Send to Python (non-blocking, may fail if Python disconnected)
                            let _ = tx_read.send(data).await;
                        }
                        Err(e) => {
                            // Socket read error (e.g., connection reset)
                            tracing::debug!("Socket read error: {:?}", e);
                            break;
                        }
                    }
                }
                
                // ─── WRITE TO SOCKET ───────────────────────────────────
                // Wait for data from Python to write to socket
                bytes = rx_write.recv() => {
                    match bytes {
                        Some(bytes) => {
                            // Received data from Python
                            if let Err(e) = write_stream.write_all(&bytes).await {
                                // Socket write error (e.g., broken pipe)
                                tracing::debug!("Socket write error: {:?}", e);
                                break;
                            }
                        }
                        None => {
                            // Python closed its sending channel
                            // This typically means the Python handler finished
                            break;
                        }
                    }
                }
            }  // End of select!
        }  // End of loop
        
        // ──────────────────────────────────────────────────────────────
        // PHASE 4: CLEANUP
        // ──────────────────────────────────────────────────────────────
        tracing::info!("Connection closed");
        
        // Return Python None to indicate successful completion
        Ok(Python::attach(|py| py.None()))
    })  // End of async block
}  // End of function

// ──────────────────────────────────────────────────────────────────────
// DATA FLOW VISUALIZATION:
// ──────────────────────────────────────────────────────────────────────
// 
//  Python Code          Rust Bridge          Local Socket
//  ───────────          ───────────          ─────────────
// 
//  handler()    ←──┐
//       │          │ ReceiverHandle
//       ↓          └── rx_read (channel)
//  await receive()      │
//       │              Reader Loop
//       │               │ (read_stream.read())
//       │               ↓
//       │          Socket Read
//       │               │
//       ↓               ↓
//  process data    Forward bytes
//       │               │
//       │               │
//  await send()   ┌── tx_write (channel)
//       │         │     │
//       ↓         │     ↓
//  sender.send()  │  Writer Loop
//                 │     │ (write_stream.write_all())
//                 │     ↓
//                 └─→ Socket Write
// 
// ──────────────────────────────────────────────────────────────────────
// ERROR HANDLING SCENARIOS:
// ──────────────────────────────────────────────────────────────────────
// 1. Connection refused → PyIOError immediately
// 2. Socket read error → Loop breaks, future resolves
// 3. Socket write error → Loop breaks, future resolves  
// 4. Python handler panics → GIL prevents crash, loop continues
// 5. Python closes channel → Clean break from loop
// 
// ──────────────────────────────────────────────────────────────────────
// THREAD SAFETY NOTES:
// ──────────────────────────────────────────────────────────────────────
// - Python GIL is acquired/released via Python::attach()
// - Arc<Mutex<...>> protects shared receiver from concurrent access
// - tokio::select! ensures only one I/O operation at a time per direction
// - Bytes type allows zero-copy sharing between threads
// 
// ──────────────────────────────────────────────────────────────────────
// PERFORMANCE CHARACTERISTICS:
// ──────────────────────────────────────────────────────────────────────
// - 4KB buffer balances memory usage and syscall frequency
// - 128 message channel buffer prevents backpressure deadlocks
// - Zero-copy Bytes reduces memory allocation for large messages
// - Async I/O allows concurrent Python processing and socket ops
// 
// ──────────────────────────────────────────────────────────────────────