//! Concrete, struct-based transport API for ordered, full-duplex local byte streams.
//!
//! This module intentionally contains no custom transport traits. The semantic contract is
//! expressed directly by `SocketServer`, `SocketConnection`, `SocketReadHalf`, and
//! `SocketWriteHalf`.
//!
//! There is deliberately no framing, message protocol, serialization, authentication,
//! session management, routing, retry/replay logic, acknowledgement, or business semantics.
//! A successful `send` is not guaranteed to correspond to one `receive` at the peer.

use anyhow::{Result, anyhow};
use interprocess::local_socket::traits::tokio::{Listener as _, Stream as _};
use interprocess::local_socket::{GenericNamespaced, ListenerOptions, ToNsName as _};
#[cfg(unix)]
use interprocess::os::unix::local_socket::ListenerOptionsExt as _;
#[cfg(windows)]
use interprocess::os::windows::local_socket::ListenerOptionsExt as _;
#[cfg(windows)]
use interprocess::os::windows::security_descriptor::SecurityDescriptor;
use std::{
    fmt,
    sync::{
        Arc, Mutex as StdMutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::Mutex,
};
use tokio_util::sync::CancellationToken;
#[cfg(windows)]
use widestring::U16CString;

type RawListener = interprocess::local_socket::tokio::Listener;
type RawStream = interprocess::local_socket::tokio::Stream;

static NEXT_SOCKET_ID: AtomicU64 = AtomicU64::new(1);

fn next_socket_id() -> u64 {
    NEXT_SOCKET_ID.fetch_add(1, Ordering::Relaxed)
}

/// Logical, cross-platform local transport endpoint.
///
/// The same opaque value is mapped by `interprocess::GenericNamespaced` to the platform's local
/// socket mechanism.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct SocketEndpoint(String);

impl SocketEndpoint {
    pub fn new(name: impl Into<String>) -> Result<Self> {
        let name = name.into();
        if name.is_empty() {
            return Err(anyhow!("socket endpoint must not be empty"));
        }
        Ok(Self(name))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SocketEndpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<String> for SocketEndpoint {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl From<&str> for SocketEndpoint {
    fn from(value: &str) -> Self {
        Self(value.to_owned())
    }
}

/// Result of one byte-stream receive operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[must_use]
pub enum ReceiveResult {
    /// Number of bytes written into the supplied destination buffer.
    ///
    /// `Data(0)` is reserved for an empty destination buffer and is not EOF.
    Data(usize),

    /// The peer cleanly closed its sending direction after all preceding bytes.
    EndOfStream,
}

/// Snapshot diagnostics for a connection.
#[derive(Clone, Debug)]
pub struct SocketConnectionInfo {
    pub id: u64,
    pub local_endpoint: Option<SocketEndpoint>,
    pub peer_endpoint: Option<SocketEndpoint>,
    pub is_closed: bool,
    pub is_read_shutdown: bool,
    pub is_write_shutdown: bool,
    pub peer_sent_eof: bool,
}

/// Snapshot diagnostics for a server.
#[derive(Clone, Debug)]
pub struct SocketServerInfo {
    pub local_endpoint: SocketEndpoint,
    pub is_stopped: bool,
    pub is_paused: bool,
}

fn get_async_socket_name(name: &str) -> Result<interprocess::local_socket::Name<'static>> {
    if name.is_empty() {
        return Err(anyhow!("socket name must not be empty"));
    }

    name.to_string()
        .to_ns_name::<GenericNamespaced>()
        .map_err(|e| anyhow!(std::io::Error::other(e)))
}

#[derive(Debug, Default)]
struct ServerState {
    stopped: AtomicBool,
    paused: AtomicBool,
}

impl ServerState {
    fn is_stopped(&self) -> bool {
        self.stopped.load(Ordering::SeqCst)
    }

    fn is_paused(&self) -> bool {
        self.paused.load(Ordering::SeqCst)
    }

    fn pause(&self) {
        if !self.is_stopped() {
            self.paused.store(true, Ordering::SeqCst);
        }
    }

    fn resume(&self) {
        if !self.is_stopped() {
            self.paused.store(false, Ordering::SeqCst);
        }
    }

    /// Returns true only for the first transition into the stopped state.
    fn stop(&self) -> bool {
        let was_stopped = self.stopped.swap(true, Ordering::SeqCst);
        self.paused.store(false, Ordering::SeqCst);
        !was_stopped
    }
}

/// Concrete listener side of the local byte transport.
pub struct SocketServer {
    // `Arc` lets an in-flight accept keep the listener alive without holding a blocking mutex
    // across `.await`. `stop()` removes the server-owned Arc and cancels pending accepts.
    inner: StdMutex<Option<Arc<RawListener>>>,
    endpoint: SocketEndpoint,
    state: Arc<ServerState>,
    stop_token: CancellationToken,
}

impl SocketServer {
    /// Start a server with default platform permissions/security settings.
    pub async fn start(endpoint: impl Into<SocketEndpoint>) -> Result<Self> {
        let endpoint = endpoint.into();
        Self::bind(endpoint.as_str(), None).await
    }

    /// Bind a server with the platform-specific permission/security option used by the old API.
    pub async fn bind(
        name: &str,
        #[cfg(unix)] mode: Option<u32>,
        #[cfg(windows)] mode: Option<String>,
    ) -> Result<Self> {
        let endpoint = SocketEndpoint::new(name)?;
        let socket_name = get_async_socket_name(endpoint.as_str())?;
        let mut opts = ListenerOptions::new().name(socket_name);

        #[cfg(unix)]
        if let Some(mode) = mode {
            opts = opts.mode(mode as libc::mode_t);
        }

        #[cfg(windows)]
        if let Some(sddl) = &mode {
            let sddl = U16CString::from_str(sddl).map_err(|e| anyhow!(e.to_string()))?;
            let sd = SecurityDescriptor::deserialize(&sddl)?;
            opts = opts.security_descriptor(sd);
        }

        let listener = opts.create_tokio().map_err(|e| anyhow!(std::io::Error::other(e)))?;

        Ok(Self {
            inner: StdMutex::new(Some(Arc::new(listener))),
            endpoint,
            state: Arc::new(ServerState::default()),
            stop_token: CancellationToken::new(),
        })
    }

    fn listener(&self) -> Result<Arc<RawListener>> {
        if self.is_stopped() {
            return Err(anyhow!("socket server is stopped"));
        }

        self.inner
            .lock()
            .map_err(|_| anyhow!("socket server state is poisoned"))?
            .as_ref()
            .cloned()
            .ok_or_else(|| anyhow!("socket server is stopped"))
    }

    /// Wait for the next client. A concurrent `stop()` interrupts this operation.
    pub async fn accept(&self) -> Result<SocketConnection> {
        if self.is_paused() {
            return Err(anyhow!("socket server is paused"));
        }

        let listener = self.listener()?;
        let stream = tokio::select! {
            result = listener.accept() => {
                result.map_err(|e| anyhow!(std::io::Error::other(e)))?
            }
            _ = self.stop_token.cancelled() => {
                return Err(anyhow!("socket server was stopped"));
            }
        };

        Ok(SocketConnection::from_accepted(stream, self.endpoint.clone()))
    }

    pub async fn accept_timeout(&self, timeout: Duration) -> Result<SocketConnection> {
        tokio::time::timeout(timeout, self.accept())
            .await
            .map_err(|_| anyhow!("socket accept timeout"))?
    }

    /// Stop accepting new clients and release the listener once in-flight accepts observe the
    /// cancellation. Already accepted connections remain independent and usable.
    pub async fn stop(&self) -> Result<()> {
        self.stop_now()
    }

    /// Synchronous compatibility alias for the former listener `close()` operation.
    pub fn close(&self) {
        let _ = self.stop_now();
    }

    fn stop_now(&self) -> Result<()> {
        if self.state.stop() {
            self.stop_token.cancel();
            let _ =
                self.inner.lock().map_err(|_| anyhow!("socket server state is poisoned"))?.take();
        }
        Ok(())
    }

    pub fn pause(&self) {
        self.state.pause();
    }

    pub fn resume(&self) {
        self.state.resume();
    }

    pub fn is_paused(&self) -> bool {
        self.state.is_paused()
    }

    pub fn is_stopped(&self) -> bool {
        self.state.is_stopped()
    }

    pub fn is_closed(&self) -> bool {
        self.is_stopped()
    }

    pub fn is_started(&self) -> bool {
        !self.is_stopped()
    }

    pub fn is_startetd(&self) -> bool {
        self.is_started()
    }

    pub fn is_accepting(&self) -> bool {
        !self.is_stopped() && !self.is_paused()
    }

    pub fn endpoint(&self) -> &SocketEndpoint {
        &self.endpoint
    }

    pub fn name(&self) -> &str {
        self.endpoint.as_str()
    }

    pub fn local_endpoint(&self) -> SocketEndpoint {
        self.endpoint.clone()
    }

    pub fn info(&self) -> SocketServerInfo {
        SocketServerInfo {
            local_endpoint: self.endpoint.clone(),
            is_stopped: self.is_stopped(),
            is_paused: self.is_paused(),
        }
    }
}

#[derive(Debug, Default)]
struct ConnectionState {
    closed: AtomicBool,
    paused: AtomicBool,
    read_shutdown: AtomicBool,
    write_shutdown: AtomicBool,
    peer_sent_eof: AtomicBool,
}

impl ConnectionState {
    fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }

    fn is_paused(&self) -> bool {
        self.paused.load(Ordering::SeqCst)
    }

    fn is_read_shutdown(&self) -> bool {
        self.read_shutdown.load(Ordering::SeqCst)
    }

    fn is_write_shutdown(&self) -> bool {
        self.write_shutdown.load(Ordering::SeqCst)
    }

    fn peer_sent_eof(&self) -> bool {
        self.peer_sent_eof.load(Ordering::SeqCst)
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

    /// Returns true only for the first full-close transition.
    fn close(&self) -> bool {
        let was_closed = self.closed.swap(true, Ordering::SeqCst);
        self.paused.store(false, Ordering::SeqCst);
        self.read_shutdown.store(true, Ordering::SeqCst);
        self.write_shutdown.store(true, Ordering::SeqCst);
        !was_closed
    }
}

struct ConnectionCore {
    id: u64,
    endpoint: SocketEndpoint,
    local_endpoint: Option<SocketEndpoint>,
    peer_endpoint: Option<SocketEndpoint>,
    stream: StdMutex<Option<Arc<RawStream>>>,
    state: ConnectionState,

    // Direction-specific gates preserve byte ordering for same-direction concurrent calls while
    // still allowing one receive and one send to progress at the same time.
    read_gate: Mutex<()>,
    write_gate: Mutex<()>,

    // Full close interrupts both directions. Read shutdown additionally interrupts pending reads
    // without forcing the write direction closed.
    close_token: CancellationToken,
    read_shutdown_token: CancellationToken,
}

impl ConnectionCore {
    fn new(
        stream: RawStream,
        endpoint: SocketEndpoint,
        local_endpoint: Option<SocketEndpoint>,
        peer_endpoint: Option<SocketEndpoint>,
    ) -> Self {
        Self {
            id: next_socket_id(),
            endpoint,
            local_endpoint,
            peer_endpoint,
            stream: StdMutex::new(Some(Arc::new(stream))),
            state: ConnectionState::default(),
            read_gate: Mutex::new(()),
            write_gate: Mutex::new(()),
            close_token: CancellationToken::new(),
            read_shutdown_token: CancellationToken::new(),
        }
    }

    fn stream(&self) -> Result<Arc<RawStream>> {
        if self.state.is_closed() {
            return Err(anyhow!("socket connection is closed"));
        }

        self.stream
            .lock()
            .map_err(|_| anyhow!("socket connection state is poisoned"))?
            .as_ref()
            .cloned()
            .ok_or_else(|| anyhow!("socket connection is closed"))
    }

    fn take_stream(&self) -> Result<Option<Arc<RawStream>>> {
        Ok(self.stream.lock().map_err(|_| anyhow!("socket connection state is poisoned"))?.take())
    }

    fn ensure_common_io_available(&self) -> Result<()> {
        if self.state.is_closed() {
            return Err(anyhow!("socket connection is closed"));
        }
        if self.state.is_paused() {
            return Err(anyhow!("socket connection is paused"));
        }
        Ok(())
    }

    fn ensure_read_available(&self) -> Result<()> {
        self.ensure_common_io_available()?;
        if self.state.is_read_shutdown() {
            return Err(anyhow!("socket receive direction is shut down"));
        }
        Ok(())
    }

    fn ensure_write_available(&self) -> Result<()> {
        self.ensure_common_io_available()?;
        if self.state.is_write_shutdown() {
            return Err(anyhow!("socket send direction is shut down"));
        }
        Ok(())
    }

    /// Force the entire connection into a terminal state after an I/O failure for which the
    /// amount of transport progress is no longer safe to replay blindly.
    fn fail_connection(&self) {
        if self.state.close() {
            self.close_token.cancel();
            self.read_shutdown_token.cancel();
            if let Ok(mut stream) = self.stream.lock() {
                let _ = stream.take();
            }
        }
    }

    async fn receive(&self, buffer: &mut [u8]) -> Result<ReceiveResult> {
        if buffer.is_empty() {
            return Ok(ReceiveResult::Data(0));
        }

        if self.state.peer_sent_eof() {
            return Ok(ReceiveResult::EndOfStream);
        }
        self.ensure_read_available()?;

        let _read_guard = tokio::select! {
            _ = self.close_token.cancelled() => {
                return Err(anyhow!("socket connection was closed"));
            }
            _ = self.read_shutdown_token.cancelled() => {
                return Err(anyhow!("socket receive direction was shut down"));
            }
            guard = self.read_gate.lock() => guard,
        };

        // State may have changed while this call waited behind another receive.
        if self.state.peer_sent_eof() {
            return Ok(ReceiveResult::EndOfStream);
        }
        self.ensure_read_available()?;

        let stream = self.stream()?;
        let mut io = stream.as_ref();
        let result = tokio::select! {
            _ = self.close_token.cancelled() => {
                return Err(anyhow!("socket connection was closed"));
            }
            _ = self.read_shutdown_token.cancelled() => {
                return Err(anyhow!("socket receive direction was shut down"));
            }
            result = io.read(buffer) => result,
        };

        match result {
            Ok(0) => {
                // EOF closes only the peer->local direction. The local send direction may still
                // be usable, which is the key half-close distinction missing from the old state.
                self.state.peer_sent_eof.store(true, Ordering::SeqCst);
                Ok(ReceiveResult::EndOfStream)
            }
            Ok(n) => Ok(ReceiveResult::Data(n)),
            Err(e) => {
                self.fail_connection();
                Err(anyhow!(std::io::Error::other(e)))
            }
        }
    }

    async fn write_once(&self, data: &[u8]) -> Result<usize> {
        if data.is_empty() {
            return Ok(0);
        }
        self.ensure_write_available()?;

        let _write_guard = tokio::select! {
            _ = self.close_token.cancelled() => {
                return Err(anyhow!("socket connection was closed"));
            }
            guard = self.write_gate.lock() => guard,
        };

        self.ensure_write_available()?;
        let stream = self.stream()?;
        let mut io = stream.as_ref();
        let result = tokio::select! {
            _ = self.close_token.cancelled() => {
                return Err(anyhow!("socket connection was closed"));
            }
            result = io.write(data) => result,
        };

        match result {
            Ok(0) => {
                self.fail_connection();
                Err(anyhow!(
                    "socket write returned zero for a non-empty buffer; connection closed"
                ))
            }
            Ok(n) => Ok(n),
            Err(e) => {
                self.fail_connection();
                Err(anyhow!(std::io::Error::other(e)))
            }
        }
    }

    async fn send(&self, data: &[u8]) -> Result<()> {
        if data.is_empty() {
            return Ok(());
        }
        self.ensure_write_available()?;

        let _write_guard = tokio::select! {
            _ = self.close_token.cancelled() => {
                return Err(anyhow!("socket connection was closed"));
            }
            guard = self.write_gate.lock() => guard,
        };

        self.ensure_write_available()?;
        let stream = self.stream()?;
        let mut io = stream.as_ref();
        let mut written = 0usize;

        while written < data.len() {
            let result = tokio::select! {
                _ = self.close_token.cancelled() => {
                    return Err(anyhow!(
                        "socket connection closed after {written} of {} bytes were written; do not blindly retry the full buffer",
                        data.len()
                    ));
                }
                result = io.write(&data[written..]) => result,
            };

            match result {
                Ok(0) => {
                    self.fail_connection();
                    return Err(anyhow!(
                        "socket write returned zero after {written} of {} bytes; connection closed",
                        data.len()
                    ));
                }
                Ok(n) => written += n,
                Err(e) => {
                    self.fail_connection();
                    return Err(anyhow!(
                        "socket write failed after {written} of {} bytes; connection closed because a partial prefix may have been written: {e}",
                        data.len()
                    ));
                }
            }
        }

        Ok(())
    }

    async fn send_timeout(&self, data: &[u8], timeout: Duration) -> Result<()> {
        if data.is_empty() {
            return Ok(());
        }
        self.ensure_write_available()?;

        let deadline = tokio::time::Instant::now() + timeout;
        let _write_guard = tokio::select! {
            _ = self.close_token.cancelled() => {
                return Err(anyhow!("socket connection was closed"));
            }
            result = tokio::time::timeout_at(deadline, self.write_gate.lock()) => {
                result.map_err(|_| anyhow!("socket write timeout before any write progress"))?
            }
        };

        self.ensure_write_available()?;
        let stream = self.stream()?;
        let mut io = stream.as_ref();
        let mut written = 0usize;

        while written < data.len() {
            let result = tokio::select! {
                _ = self.close_token.cancelled() => {
                    return Err(anyhow!(
                        "socket connection closed after {written} of {} bytes were written; do not blindly retry the full buffer",
                        data.len()
                    ));
                }
                result = tokio::time::timeout_at(deadline, io.write(&data[written..])) => result,
            };

            match result {
                Ok(Ok(0)) => {
                    self.fail_connection();
                    return Err(anyhow!(
                        "socket write returned zero after {written} of {} bytes; connection closed",
                        data.len()
                    ));
                }
                Ok(Ok(n)) => written += n,
                Ok(Err(e)) => {
                    self.fail_connection();
                    return Err(anyhow!(
                        "socket write failed after {written} of {} bytes; connection closed because a partial prefix may have been written: {e}",
                        data.len()
                    ));
                }
                Err(_) => {
                    self.fail_connection();
                    return Err(anyhow!(
                        "socket write timeout after {written} of {} bytes; connection closed because a partial prefix may have been written",
                        data.len()
                    ));
                }
            }
        }

        Ok(())
    }

    async fn flush(&self) -> Result<()> {
        self.ensure_write_available()?;

        let _write_guard = tokio::select! {
            _ = self.close_token.cancelled() => {
                return Err(anyhow!("socket connection was closed"));
            }
            guard = self.write_gate.lock() => guard,
        };

        self.ensure_write_available()?;
        let stream = self.stream()?;
        let mut io = stream.as_ref();
        let result = tokio::select! {
            _ = self.close_token.cancelled() => {
                return Err(anyhow!("socket connection was closed"));
            }
            result = io.flush() => result,
        };

        match result {
            Ok(()) => Ok(()),
            Err(e) => {
                self.fail_connection();
                Err(anyhow!(std::io::Error::other(e)))
            }
        }
    }

    async fn shutdown_read(&self) -> Result<()> {
        if self.state.is_closed() || self.state.is_read_shutdown() {
            return Ok(());
        }

        self.state.read_shutdown.store(true, Ordering::SeqCst);
        self.read_shutdown_token.cancel();
        Ok(())
    }

    async fn shutdown_write(&self) -> Result<()> {
        if self.state.is_closed() || self.state.is_write_shutdown() {
            return Ok(());
        }

        // The gate is acquired before the state transition. Existing sends that were already
        // queued ahead of this shutdown are allowed to finish; later sends observe shutdown.
        let _write_guard = self.write_gate.lock().await;
        if self.state.is_closed() || self.state.is_write_shutdown() {
            return Ok(());
        }

        self.state.write_shutdown.store(true, Ordering::SeqCst);
        let stream = self.stream()?;
        let mut io = stream.as_ref();
        match io.shutdown().await {
            Ok(()) => Ok(()),
            Err(e) => {
                self.fail_connection();
                Err(anyhow!(std::io::Error::other(e)))
            }
        }
    }

    async fn close(&self) -> Result<()> {
        if !self.state.close() {
            return Ok(());
        }

        // Cancellation first wakes pending I/O. Removing the core-owned Arc means the OS resource
        // is released as soon as the cancelled in-flight operations drop their temporary Arcs.
        self.close_token.cancel();
        self.read_shutdown_token.cancel();
        let stream = self.take_stream()?;

        if let Some(stream) = stream {
            // Wait until any send that is currently unwinding from cancellation releases the
            // directional gate, then perform a best-effort graceful write shutdown.
            let _write_guard = self.write_gate.lock().await;
            let mut io = stream.as_ref();
            io.shutdown().await.map_err(|e| anyhow!(std::io::Error::other(e)))?;
        }

        Ok(())
    }

    fn info(&self) -> SocketConnectionInfo {
        SocketConnectionInfo {
            id: self.id,
            local_endpoint: self.local_endpoint.clone(),
            peer_endpoint: self.peer_endpoint.clone(),
            is_closed: self.state.is_closed(),
            is_read_shutdown: self.state.is_read_shutdown(),
            is_write_shutdown: self.state.is_write_shutdown(),
            peer_sent_eof: self.state.peer_sent_eof(),
        }
    }
}

/// One concrete ordered, asynchronous, full-duplex local byte-stream connection.
///
/// `receive(&self, ..)` and `send(&self, ..)` can progress concurrently. Same-direction calls are
/// serialized so concurrent full-buffer sends cannot interleave their bytes.
pub struct SocketConnection {
    core: Arc<ConnectionCore>,
}

impl SocketConnection {
    fn from_accepted(stream: RawStream, server_endpoint: SocketEndpoint) -> Self {
        Self {
            core: Arc::new(ConnectionCore::new(
                stream,
                server_endpoint.clone(),
                Some(server_endpoint),
                None,
            )),
        }
    }

    /// Connect to a server. The resulting value is already the ordinary connection object;
    /// there is no separate client behavior layer.
    pub async fn connect(endpoint: impl Into<SocketEndpoint>) -> Result<Self> {
        let endpoint = endpoint.into();
        if endpoint.as_str().is_empty() {
            return Err(anyhow!("socket endpoint must not be empty"));
        }

        let socket_name = get_async_socket_name(endpoint.as_str())?;
        let stream = RawStream::connect(socket_name)
            .await
            .map_err(|e| anyhow!(std::io::Error::other(e)))?;

        Ok(Self {
            core: Arc::new(ConnectionCore::new(stream, endpoint.clone(), None, Some(endpoint))),
        })
    }

    pub async fn connect_timeout(
        endpoint: impl Into<SocketEndpoint>,
        timeout: Duration,
    ) -> Result<Self> {
        let endpoint = endpoint.into();
        tokio::time::timeout(timeout, Self::connect(endpoint))
            .await
            .map_err(|_| anyhow!("socket connect timeout"))?
    }

    /// Receive one arbitrary byte-stream chunk into `buffer`.
    pub async fn receive(&self, buffer: &mut [u8]) -> Result<ReceiveResult> {
        self.core.receive(buffer).await
    }

    /// Send the complete buffer. Concurrent sends are serialized as whole operations.
    pub async fn send(&self, data: &[u8]) -> Result<()> {
        self.core.send(data).await
    }

    /// Perform one possibly-partial write.
    pub async fn write(&self, data: &[u8]) -> Result<usize> {
        self.core.write_once(data).await
    }

    /// Flush locally buffered output. This is not a peer acknowledgement.
    pub async fn flush(&self) -> Result<()> {
        self.core.flush().await
    }

    /// Stop local receives while leaving the sending direction available.
    pub async fn shutdown_read(&self) -> Result<()> {
        self.core.shutdown_read().await
    }

    /// Finish the local sending direction while leaving the receiving direction available.
    pub async fn shutdown_write(&self) -> Result<()> {
        self.core.shutdown_write().await
    }

    /// Idempotently close both directions. Pending local I/O is interrupted.
    pub async fn close(&self) -> Result<()> {
        self.core.close().await
    }

    /// Consume this connection and expose independently usable concrete directional structs.
    pub fn into_split(self) -> (SocketReadHalf, SocketWriteHalf) {
        let core = self.core;
        (SocketReadHalf { core: Arc::clone(&core) }, SocketWriteHalf { core })
    }

    pub fn id(&self) -> u64 {
        self.core.id
    }

    pub fn endpoint(&self) -> &SocketEndpoint {
        &self.core.endpoint
    }

    pub fn name(&self) -> &str {
        self.core.endpoint.as_str()
    }

    pub fn local_endpoint(&self) -> Option<SocketEndpoint> {
        self.core.local_endpoint.clone()
    }

    pub fn peer_endpoint(&self) -> Option<SocketEndpoint> {
        self.core.peer_endpoint.clone()
    }

    pub fn info(&self) -> SocketConnectionInfo {
        self.core.info()
    }

    pub fn is_closed(&self) -> bool {
        self.core.state.is_closed()
    }

    pub fn is_read_shutdown(&self) -> bool {
        self.core.state.is_read_shutdown()
    }

    pub fn is_write_shutdown(&self) -> bool {
        self.core.state.is_write_shutdown()
    }

    pub fn peer_sent_eof(&self) -> bool {
        self.core.state.peer_sent_eof()
    }

    pub fn is_started(&self) -> bool {
        !self.is_closed()
    }

    pub fn is_startetd(&self) -> bool {
        self.is_started()
    }

    pub fn is_active(&self) -> bool {
        !self.is_closed()
    }

    pub fn is_available(&self) -> bool {
        self.is_active() && !self.is_paused()
    }

    pub fn is_paused(&self) -> bool {
        self.core.state.is_paused()
    }

    pub fn pause(&self) {
        self.core.state.pause();
    }

    pub fn resume(&self) {
        self.core.state.resume();
    }

    // ---- Compatibility helpers for the previous struct API -------------------------------

    pub async fn read_bytes(&self, max_bytes: usize) -> Result<Vec<u8>> {
        if max_bytes == 0 {
            return Ok(Vec::new());
        }

        let mut buffer = vec![0_u8; max_bytes];
        match self.receive(&mut buffer).await? {
            ReceiveResult::Data(n) => {
                buffer.truncate(n);
                Ok(buffer.to_vec())
            }
            ReceiveResult::EndOfStream => Ok(Vec::new()),
        }
    }

    pub async fn read_timeout(&self, max_bytes: usize, timeout: Duration) -> Result<Vec<u8>> {
        tokio::time::timeout(timeout, self.read_bytes(max_bytes))
            .await
            .map_err(|_| anyhow!("socket read timeout"))?
    }

    pub async fn write_bytes(&self, data: &[u8]) -> Result<usize> {
        self.write(data).await
    }

    pub async fn write_all_bytes(&self, data: &[u8]) -> Result<()> {
        self.send(data).await
    }

    pub async fn write_all_timeout(&self, data: &[u8], timeout: Duration) -> Result<()> {
        self.core.send_timeout(data, timeout).await
    }
}

/// Concrete receive half returned by `SocketConnection::into_split`.
pub struct SocketReadHalf {
    core: Arc<ConnectionCore>,
}

impl SocketReadHalf {
    pub async fn receive(&self, buffer: &mut [u8]) -> Result<ReceiveResult> {
        self.core.receive(buffer).await
    }

    pub async fn shutdown_read(&self) -> Result<()> {
        self.core.shutdown_read().await
    }

    pub fn id(&self) -> u64 {
        self.core.id
    }

    pub fn info(&self) -> SocketConnectionInfo {
        self.core.info()
    }
}

/// Concrete send half returned by `SocketConnection::into_split`.
pub struct SocketWriteHalf {
    core: Arc<ConnectionCore>,
}

impl SocketWriteHalf {
    pub async fn send(&self, data: &[u8]) -> Result<()> {
        self.core.send(data).await
    }

    pub async fn write(&self, data: &[u8]) -> Result<usize> {
        self.core.write_once(data).await
    }

    pub async fn flush(&self) -> Result<()> {
        self.core.flush().await
    }

    pub async fn shutdown_write(&self) -> Result<()> {
        self.core.shutdown_write().await
    }

    pub fn id(&self) -> u64 {
        self.core.id
    }

    pub fn info(&self) -> SocketConnectionInfo {
        self.core.info()
    }
}

/// Zero-sized namespace corresponding to the old transport-association concept, but concrete.
#[derive(Clone, Copy, Debug, Default)]
pub struct LocalSocketTransport;

impl LocalSocketTransport {
    pub async fn start(endpoint: impl Into<SocketEndpoint>) -> Result<SocketServer> {
        SocketServer::start(endpoint).await
    }

    pub async fn connect(endpoint: impl Into<SocketEndpoint>) -> Result<SocketConnection> {
        SocketConnection::connect(endpoint).await
    }
}

/// Backward-compatible names for existing call sites. These are aliases, not traits.
pub type SocketListener = SocketServer;
pub type SocketStream = SocketConnection;
pub type SocketClient = SocketConnection;
