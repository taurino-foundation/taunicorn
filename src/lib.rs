use anyhow::{Result, anyhow};
use interprocess::local_socket::GenericNamespaced;
use interprocess::local_socket::ToNsName as _;
use interprocess::local_socket::{GenericFilePath, ToFsName as _};
use interprocess::local_socket::{
    ListenerOptions,
    traits::tokio::{Listener as _, Stream as _},
};
#[cfg(windows)]
use interprocess::os::windows::local_socket::ListenerOptionsExt as _;
#[cfg(windows)]
use interprocess::os::windows::security_descriptor::SecurityDescriptor;
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio_util::sync::CancellationToken;
#[cfg(windows)]
use widestring::U16CString;

pub mod python;

static NEXT_SOCKET_ID: AtomicU64 = AtomicU64::new(1);

fn next_socket_id() -> u64 {
    NEXT_SOCKET_ID.fetch_add(1, Ordering::Relaxed)
}

/// Get the appropriate socket name for the current platform.
fn get_async_socket_name(name: &str) -> Result<interprocess::local_socket::Name<'static>> {
    if let Ok(ns_name) = name.to_string().to_ns_name::<GenericNamespaced>() {
        return Ok(ns_name);
    }

    let path = if cfg!(unix) {
        if name.starts_with('/') { name.to_string() } else { format!("/tmp/{}.sock", name) }
    } else if name.starts_with(r"\\.\pipe\") {
        name.to_string()
    } else {
        format!(r"\\.\pipe\{}", name)
    };

    path.to_fs_name::<GenericFilePath>().map_err(|e| anyhow!(std::io::Error::other(e)))
}

#[derive(Debug, Default)]
struct RuntimeState {
    started: AtomicBool,
    closed: AtomicBool,
    paused: AtomicBool,
}

impl RuntimeState {
    fn new_started() -> Self {
        Self {
            started: AtomicBool::new(true),
            closed: AtomicBool::new(false),
            paused: AtomicBool::new(false),
        }
    }

    fn is_started(&self) -> bool {
        self.started.load(Ordering::SeqCst)
    }

    fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }

    fn is_paused(&self) -> bool {
        self.paused.load(Ordering::SeqCst)
    }

    fn pause(&self) {
        if !self.is_closed() {
            self.paused.store(true, Ordering::SeqCst);
        }
    }

    fn resume(&self) {
        if !self.is_closed() {
            self.paused.store(false, Ordering::SeqCst);
        }
    }

    fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
        self.started.store(false, Ordering::SeqCst);
        self.paused.store(false, Ordering::SeqCst);
    }
}

/// Async local socket listener.
pub struct SocketListener {
    inner: interprocess::local_socket::tokio::Listener,
    name: String,
    state: Arc<RuntimeState>,
    close_token: CancellationToken,
}
pub struct SocketStream {
    id: u64,
    inner: interprocess::local_socket::tokio::Stream,
    name: String,
    state: Arc<RuntimeState>,
}
impl SocketListener {
    /// Create a new async local socket listener.
    pub async fn bind(
        name: &str,
        #[cfg(unix)] mode: Option<u32>,
        #[cfg(windows)] mode: Option<String>,
    ) -> Result<Self> {
        #[cfg(unix)]
        let mode = mode.map(|m| m as libc::mode_t);
        let socket_name = get_async_socket_name(name)?;
        let mut opts = ListenerOptions::new().name(socket_name);
        #[cfg(unix)]
        if let Some(mode) = mode {
            opts = opts.mode(mode);
        }
        #[cfg(windows)]
        if let Some(sddl) = &mode {
            let sddl = U16CString::from_str(sddl).map_err(|e| anyhow!(e.to_string()))?;
            let sd = SecurityDescriptor::deserialize(&sddl)?;
            opts = opts.security_descriptor(sd);
        }

        let listener = opts.create_tokio().map_err(|e| anyhow!(std::io::Error::other(e)))?;

        Ok(Self {
            inner: listener,
            name: name.to_string(),
            state: Arc::new(RuntimeState::new_started()),
            close_token: CancellationToken::new(),
        })
    }
    /// Returns true if the listener can currently accept new connections.
    pub fn is_accepting(&self) -> bool {
        self.is_started() && !self.is_closed() && !self.is_paused()
    }

    /// Accept a new incoming connection with a timeout.
    pub async fn accept_timeout(&self, timeout: Duration) -> Result<SocketStream> {
        tokio::time::timeout(timeout, self.accept())
            .await
            .map_err(|_| anyhow!("socket accept timeout"))?
    }
    /// Accept a new incoming connection asynchronously.
    pub async fn accept(&self) -> Result<SocketStream> {
        if self.is_closed() {
            return Err(anyhow!("socket listener is closed"));
        }

        if self.is_paused() {
            return Err(anyhow!("socket listener is paused"));
        }

        let stream = tokio::select! {
            result = self.inner.accept() => {
                result.map_err(|e| anyhow!(std::io::Error::other(e)))?
            }
            _ = self.close_token.cancelled() => {
                return Err(anyhow!("socket listener was closed"));
            }
        };

        Ok(SocketStream {
            id: next_socket_id(),
            inner: stream,
            name: self.name.clone(),
            state: Arc::new(RuntimeState::new_started()),
        })
    }

    /// Get the name of this listener.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns true while the listener is active and not closed.
    pub fn is_started(&self) -> bool {
        self.state.is_started()
    }

    /// Backward-compatible alias for the misspelled method name.
    pub fn is_startetd(&self) -> bool {
        self.is_started()
    }

    /// Returns true after close() was called.
    pub fn is_closed(&self) -> bool {
        self.state.is_closed()
    }

    /// Returns true after pause() was called and before resume().
    pub fn is_paused(&self) -> bool {
        self.state.is_paused()
    }

    /// Pause accepting new connections.
    pub fn pause(&self) {
        self.state.pause();
    }

    /// Resume accepting new connections.
    pub fn resume(&self) {
        self.state.resume();
    }

    /// Close the listener state and cancel pending accept() calls.
    ///
    /// The underlying OS listener is dropped when `SocketListener` itself is dropped.
    pub fn close(&self) {
        self.state.close();
        self.close_token.cancel();
    }
}

impl SocketStream {
    /// Connect to a local socket server asynchronously.
    pub async fn connect(name: &str) -> Result<Self> {
        let socket_name = get_async_socket_name(name)?;

        let stream = interprocess::local_socket::tokio::Stream::connect(socket_name)
            .await
            .map_err(|e| anyhow!(std::io::Error::other(e)))?;

        Ok(Self {
            id: next_socket_id(),
            inner: stream,
            name: name.to_string(),
            state: Arc::new(RuntimeState::new_started()),
        })
    }

    /// Get the name of this stream.
    pub fn name(&self) -> &str {
        &self.name
    }
    /// Unique ID of this socket stream.
    pub fn id(&self) -> u64 {
        self.id
    }

    /// Returns true while the stream is started and not closed.
    pub fn is_active(&self) -> bool {
        self.is_started() && !self.is_closed()
    }

    /// Returns true if the stream can currently be used for reads/writes.
    pub fn is_available(&self) -> bool {
        self.is_active() && !self.is_paused()
    }

    /// Read up to `max_bytes` bytes from the stream.
    pub async fn read_bytes(&mut self, max_bytes: usize) -> Result<Vec<u8>> {
        if !self.is_available() {
            return Err(anyhow!("socket stream is not available"));
        }

        let mut buffer = vec![0_u8; max_bytes];

        let n =
            self.inner.read(&mut buffer).await.map_err(|e| anyhow!(std::io::Error::other(e)))?;

        if n == 0 {
            self.state.close();
        }

        buffer.truncate(n);
        Ok(buffer)
    }

    /// Write bytes to the stream.
    ///
    /// Returns the number of bytes written.
    pub async fn write_bytes(&mut self, data: &[u8]) -> Result<usize> {
        if !self.is_available() {
            return Err(anyhow!("socket stream is not available"));
        }

        let n = self.inner.write(data).await.map_err(|e| anyhow!(std::io::Error::other(e)))?;

        Ok(n)
    }

    /// Write the full buffer to the stream.
    pub async fn write_all_bytes(&mut self, data: &[u8]) -> Result<()> {
        if !self.is_available() {
            return Err(anyhow!("socket stream is not available"));
        }

        self.inner.write_all(data).await.map_err(|e| anyhow!(std::io::Error::other(e)))?;

        Ok(())
    }

    /// Flush the stream.
    pub async fn flush(&mut self) -> Result<()> {
        if self.is_closed() {
            return Err(anyhow!("socket stream is closed"));
        }

        self.inner.flush().await.map_err(|e| anyhow!(std::io::Error::other(e)))?;

        Ok(())
    }

    /// Read with timeout.
    pub async fn read_timeout(&mut self, max_bytes: usize, timeout: Duration) -> Result<Vec<u8>> {
        tokio::time::timeout(timeout, self.read_bytes(max_bytes))
            .await
            .map_err(|_| anyhow!("socket read timeout"))?
    }

    /// Write all bytes with timeout.
    pub async fn write_all_timeout(&mut self, data: &[u8], timeout: Duration) -> Result<()> {
        tokio::time::timeout(timeout, self.write_all_bytes(data))
            .await
            .map_err(|_| anyhow!("socket write timeout"))?
    }
    /// Returns true while the stream is active and not closed.
    pub fn is_started(&self) -> bool {
        self.state.is_started()
    }

    /// Backward-compatible alias for the misspelled method name.
    pub fn is_startetd(&self) -> bool {
        self.is_started()
    }

    /// Returns true after close() was called or the stream observed a shutdown/error.
    pub fn is_closed(&self) -> bool {
        self.state.is_closed()
    }

    /// Returns true after pause() was called and before resume().
    pub fn is_paused(&self) -> bool {
        self.state.is_paused()
    }

    /// Pause user-level reads/writes.
    pub fn pause(&self) {
        self.state.pause();
    }

    /// Resume user-level reads/writes.
    pub fn resume(&self) {
        self.state.resume();
    }

    /// Gracefully close the stream.
    pub async fn close(&mut self) -> Result<()> {
        if self.is_closed() {
            return Ok(());
        }

        self.inner.shutdown().await.map_err(|e| anyhow!(std::io::Error::other(e)))?;
        self.state.close();
        Ok(())
    }

    /// Split into read and write halves.
    #[allow(dead_code)]
    pub fn into_split(
        self,
    ) -> (
        tokio::io::ReadHalf<interprocess::local_socket::tokio::Stream>,
        tokio::io::WriteHalf<interprocess::local_socket::tokio::Stream>,
    ) {
        tokio::io::split(self.inner)
    }
}

impl AsyncRead for SocketStream {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        if self.is_closed() {
            return std::task::Poll::Ready(Err(std::io::Error::new(
                std::io::ErrorKind::NotConnected,
                "socket stream is closed",
            )));
        }

        if self.is_paused() {
            return std::task::Poll::Ready(Err(std::io::Error::new(
                std::io::ErrorKind::WouldBlock,
                "socket stream is paused",
            )));
        }

        let poll = std::pin::Pin::new(&mut self.inner).poll_read(cx, buf);
        if let std::task::Poll::Ready(Err(_)) = &poll {
            self.state.close();
        }
        poll
    }
}

impl AsyncWrite for SocketStream {
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        if self.is_closed() {
            return std::task::Poll::Ready(Err(std::io::Error::new(
                std::io::ErrorKind::NotConnected,
                "socket stream is closed",
            )));
        }

        if self.is_paused() {
            return std::task::Poll::Ready(Err(std::io::Error::new(
                std::io::ErrorKind::WouldBlock,
                "socket stream is paused",
            )));
        }

        let poll = std::pin::Pin::new(&mut self.inner).poll_write(cx, buf);
        if let std::task::Poll::Ready(Err(_)) = &poll {
            self.state.close();
        }
        poll
    }

    fn poll_flush(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        if self.is_closed() {
            return std::task::Poll::Ready(Err(std::io::Error::new(
                std::io::ErrorKind::NotConnected,
                "socket stream is closed",
            )));
        }

        let poll = std::pin::Pin::new(&mut self.inner).poll_flush(cx);
        if let std::task::Poll::Ready(Err(_)) = &poll {
            self.state.close();
        }
        poll
    }

    fn poll_shutdown(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        let poll = std::pin::Pin::new(&mut self.inner).poll_shutdown(cx);
        if let std::task::Poll::Ready(_) = &poll {
            self.state.close();
        }
        poll
    }
}

/* pub fn get_taunicorn_version() -> &'static str {
    static VERSION: OnceLock<String> = OnceLock::new();

    VERSION.get_or_init(|| {
        let version = env!("CARGO_PKG_VERSION");
        version.replace("-alpha", "a").replace("-beta", "b")
    })
} */

/* #[pymodule(gil_used = false)]
fn _taunicorn(_py: Python, module: &Bound<PyModule>) -> PyResult<()> {
    module.add("__version__", get_taunicorn_version())?;
    module.add_class::<Connector>()?;
    module.add_class::<Acceptor>()?;
    module.add_class::<PyChannel>()?;
    module.add_class::<PyBroadcastChannel>()?;
    module.add_class::<PyBroadcastReceiver>()?;
    Ok(())
} */
