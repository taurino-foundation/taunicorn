mod secure;
pub use secure::{
  generate_identity_private_key, identity_public_key, SecureAcceptOptions, SecureConnectOptions,
  SecureConnection,
};
use {
  napi::{bindgen_prelude::*, tokio::runtime::Runtime, Env},
  napi_derive::napi,
  std::{sync::Arc, time::Duration},
  taunicorn::{Endpoint as TuEndpoint, ReceiveResult},
};

const RAW_READ_LIMIT: u32 = 64 * 1024;

// ============================================================
// Gemeinsame Tokio-Runtime für synchrone Factory-Aufrufe
// ============================================================
static RUNTIME: std::sync::OnceLock<Runtime> = std::sync::OnceLock::new();

fn runtime() -> &'static Runtime {
  RUNTIME.get_or_init(|| Runtime::new().expect("failed to create Tokio runtime"))
}

fn map_err(e: anyhow::Error) -> Error { Error::new(Status::GenericFailure, e.to_string()) }

fn parse_endpoint(name: String) -> Result<TuEndpoint> { TuEndpoint::new(name).map_err(map_err) }

// ============================================================
// ServerInfo (Snapshot-Diagnose)
// ============================================================
#[napi(object)]
pub struct ServerInfo {
  pub local_endpoint: String,
  pub is_stopped: bool,
  pub is_paused: bool,
}

// ============================================================
// ConnectionInfo (Snapshot-Diagnose)
// ============================================================
#[napi(object)]
pub struct ConnectionInfo {
  pub id: f64,
  pub local_endpoint: Option<String>,
  pub peer_endpoint: Option<String>,
  pub is_closed: bool,
  pub is_read_shutdown: bool,
  pub is_write_shutdown: bool,
  pub peer_sent_eof: bool,
}

// ============================================================
// Server
// ============================================================
#[napi]
pub struct Server {
  inner: Arc<taunicorn::Server>,
}

#[napi]
impl Server {
  /// Listen on a local endpoint with the platform's default access settings.
  #[napi(factory)]
  pub async fn listen(endpoint: String) -> Result<Self> {
    let server = taunicorn::Server::listen(parse_endpoint(endpoint)?).await.map_err(map_err)?;
    Ok(Self { inner: Arc::new(server) })
  }

  /// Listen for verified same-user peers. Both ends must select private mode.
  /// Unix requires an owner-only XDG_RUNTIME_DIR. No fallback or encryption.
  #[napi(factory)]
  pub async fn listen_private(endpoint: String) -> Result<Self> {
    let server = taunicorn::Server::listen_private(&endpoint).await.map_err(map_err)?;
    Ok(Self { inner: Arc::new(server) })
  }

  /// Server::start — Default-Berechtigungen der Plattform.
  #[napi(factory)]
  pub fn start(endpoint: String) -> Result<Self> {
    let ep = parse_endpoint(endpoint)?;
    let server = runtime().block_on(taunicorn::Server::start(ep)).map_err(map_err)?;
    Ok(Self { inner: Arc::new(server) })
  }

  /// Server::bind — mit plattformspezifischen Rechten.
  /// Unix: mode (z.B. 0o600). Windows: SDDL-String.
  #[napi(factory)]
  pub fn bind(endpoint: String, mode: Option<String>) -> Result<Self> {
    let server = runtime()
      .block_on(async {
        #[cfg(unix)]
        {
          let raw = mode
            .as_deref()
            .map(|s| u32::from_str_radix(s.trim_start_matches("0o"), 8))
            .transpose()
            .map_err(|e| anyhow::anyhow!("invalid mode: {e}"))?;
          taunicorn::Server::bind(&endpoint, raw).await
        }
        #[cfg(windows)]
        {
          taunicorn::Server::bind(&endpoint, mode).await
        }
      })
      .map_err(map_err)?;
    Ok(Self { inner: Arc::new(server) })
  }

  /// Auf den nächsten Client warten. Ein gleichzeitiger stop() bricht ab.
  #[napi]
  pub async fn accept(&self) -> Result<Connection> {
    let conn = self.inner.accept().await.map_err(map_err)?;
    Ok(Connection { inner: Arc::new(conn) })
  }

  /// accept mit Timeout (Millisekunden).
  #[napi]
  pub async fn accept_timeout(&self, timeout_ms: f64) -> Result<Connection> {
    let conn = self
      .inner
      .accept_timeout(Duration::from_millis(timeout_ms as u64))
      .await
      .map_err(map_err)?;
    Ok(Connection { inner: Arc::new(conn) })
  }

  /// Eine frische Verbindung akzeptieren und ihren sicheren Handshake abschließen.
  /// Der private Seed und der gepinnte Client-Schlüssel müssen je 32 Bytes haben.
  #[napi]
  pub fn accept_secure(
    &self,
    env: &Env,
    identity_private_key: Buffer,
    client_identity_public_key: Buffer,
    options: Option<SecureAcceptOptions>,
  ) -> Result<AsyncBlock<SecureConnection>> {
    secure::accept_secure(
      env,
      Arc::clone(&self.inner),
      identity_private_key,
      client_identity_public_key,
      options,
    )
  }

  /// Keine neuen Clients mehr annehmen; bestehende Verbindungen bleiben nutzbar.
  #[napi]
  pub async fn stop(&self) -> Result<()> { self.inner.stop().await.map_err(map_err) }

  /// Synchroner Kompatibilitäts-Alias für stop().
  #[napi]
  pub fn close(&self) { self.inner.close(); }

  #[napi]
  pub fn pause(&self) { self.inner.pause(); }

  #[napi]
  pub fn resume(&self) { self.inner.resume(); }

  #[napi(getter)]
  pub fn is_paused(&self) -> bool { self.inner.is_paused() }

  #[napi(getter)]
  pub fn is_stopped(&self) -> bool { self.inner.is_stopped() }

  #[napi(getter)]
  pub fn is_closed(&self) -> bool { self.inner.is_closed() }

  #[napi(getter)]
  pub fn is_started(&self) -> bool { self.inner.is_started() }

  #[napi(getter)]
  pub fn is_accepting(&self) -> bool { self.inner.is_accepting() }

  #[napi(getter)]
  pub fn endpoint(&self) -> String { self.inner.endpoint().to_string() }

  #[napi(getter)]
  pub fn name(&self) -> String { self.inner.name().to_string() }

  #[napi]
  pub fn local_endpoint(&self) -> String { self.inner.local_endpoint().to_string() }

  #[napi]
  pub fn info(&self) -> ServerInfo {
    let i = self.inner.info();
    ServerInfo {
      local_endpoint: i.local_endpoint.to_string(),
      is_stopped: i.is_stopped,
      is_paused: i.is_paused,
    }
  }
}

/// Client-side entry point for opening a Connection.
#[napi]
pub struct Client;

#[napi]
impl Client {
  /// Verify the server's OS user before returning a raw byte stream; never falls back.
  #[napi]
  pub async fn connect_private(endpoint: String) -> Result<Connection> {
    let conn = taunicorn::Connection::connect_private(&endpoint).await.map_err(map_err)?;
    Ok(Connection { inner: Arc::new(conn) })
  }

  #[napi]
  pub async fn connect(endpoint: String) -> Result<Connection> {
    Connection::connect(endpoint).await
  }
}

// ============================================================
// Connection
// ============================================================
#[napi]
pub struct Connection {
  inner: Arc<taunicorn::Connection>,
}

#[napi]
impl Connection {
  /// Verbindung zu einem Server aufbauen.
  #[napi(factory)]
  pub async fn connect(endpoint: String) -> Result<Self> {
    let ep = parse_endpoint(endpoint)?;
    let conn = taunicorn::Connection::connect(ep).await.map_err(map_err)?;
    Ok(Self { inner: Arc::new(conn) })
  }

  /// connect mit Timeout (Millisekunden).
  #[napi(factory)]
  pub async fn connect_timeout(endpoint: String, timeout_ms: f64) -> Result<Self> {
    let ep = parse_endpoint(endpoint)?;
    let conn =
      taunicorn::Connection::connect_timeout(ep, Duration::from_millis(timeout_ms as u64))
        .await
        .map_err(map_err)?;
    Ok(Self { inner: Arc::new(conn) })
  }

  /// Read at most 64 KiB per raw read; remaining bytes stay in the stream.
  ///
  /// Rückgabe: Buffer mit Daten — oder `null` bei EOF
  /// (ReceiveResult::EndOfStream, sauberes Schließen der Gegenseite).
  #[napi]
  pub async fn receive(&self, max_bytes: u32) -> Result<Option<Buffer>> {
    if max_bytes == 0 {
      return Ok(Some(Vec::new().into())); // Data(0): leeres Ziel, kein EOF
    }

    let conn = Arc::clone(&self.inner);
    let mut buf = vec![0u8; max_bytes.min(RAW_READ_LIMIT) as usize];
    let result = conn.receive(&mut buf).await.map_err(map_err)?;

    Ok(match result {
      ReceiveResult::Data(n) => {
        buf.truncate(n);
        Some(buf.into())
      }
      ReceiveResult::EndOfStream => None,
    })
  }

  /// Den kompletten Buffer senden. Gleichzeitige Sends werden
  /// als ganze Operationen serialisiert.
  #[napi]
  pub async fn send(&self, data: Buffer) -> Result<()> {
    self.inner.send(&data).await.map_err(map_err)
  }

  /// Ein möglicherweise teilweises Write — Anzahl geschriebener Bytes.
  #[napi]
  pub async fn write(&self, data: Buffer) -> Result<f64> {
    let n = self.inner.write(&data).await.map_err(map_err)?;
    Ok(n as f64)
  }

  /// Lokal gepufferte Ausgabe leeren (keine Peer-Bestätigung).
  #[napi]
  pub async fn flush(&self) -> Result<()> { self.inner.flush().await.map_err(map_err) }

  /// Lokale Empfangsrichtung stoppen, Senderichtung bleibt nutzbar.
  #[napi]
  pub async fn shutdown_read(&self) -> Result<()> {
    self.inner.shutdown_read().await.map_err(map_err)
  }

  /// Lokale Senderichtung beenden (Half-Close), Empfang bleibt offen.
  #[napi]
  pub async fn shutdown_write(&self) -> Result<()> {
    self.inner.shutdown_write().await.map_err(map_err)
  }

  /// Beide Richtungen idempotent schließen. Pending I/O wird unterbrochen.
  #[napi]
  pub async fn close(&self) -> Result<()> { self.inner.close().await.map_err(map_err) }

  /// Lese- und Schreibansichten auf dieselbe Verbindung erzeugen.
  ///
  /// Die ursprüngliche Connection bleibt verwendbar. Beide Ansichten teilen
  /// denselben Verbindungszustand: close() beendet beide Richtungen,
  /// shutdown_read() bzw. shutdown_write() nur die jeweilige Richtung.
  /// Wiederholte Aufrufe erzeugen weitere Ansichten auf denselben Byte-Stream.
  #[napi]
  pub fn into_split(&self) -> SplitHalves {
    SplitHalves {
      read: ReadHalf { inner: Arc::clone(&self.inner) },
      write: WriteHalf { inner: Arc::clone(&self.inner) },
    }
  }

  // ---------- Kompatibilitäts-Helper ----------

  /// read_bytes: bis max_bytes lesen, leerer Buffer bei EOF.
  #[napi]
  pub async fn read_bytes(&self, max_bytes: u32) -> Result<Buffer> {
    let data =
      self.inner.read_bytes(max_bytes.min(RAW_READ_LIMIT) as usize).await.map_err(map_err)?;
    Ok(data.into())
  }

  /// read_bytes mit Timeout (Millisekunden).
  #[napi]
  pub async fn read_timeout(&self, max_bytes: u32, timeout_ms: f64) -> Result<Buffer> {
    let data = self
      .inner
      .read_timeout(
        max_bytes.min(RAW_READ_LIMIT) as usize,
        Duration::from_millis(timeout_ms as u64),
      )
      .await
      .map_err(map_err)?;
    Ok(data.into())
  }

  /// write_bytes: ein possibly-partial write.
  #[napi]
  pub async fn write_bytes(&self, data: Buffer) -> Result<f64> {
    let n = self.inner.write_bytes(&data).await.map_err(map_err)?;
    Ok(n as f64)
  }

  /// write_all_bytes = send.
  #[napi]
  pub async fn write_all_bytes(&self, data: Buffer) -> Result<()> {
    self.inner.write_all_bytes(&data).await.map_err(map_err)
  }

  /// send mit Timeout (Millisekunden).
  #[napi]
  pub async fn write_all_timeout(&self, data: Buffer, timeout_ms: f64) -> Result<()> {
    self
      .inner
      .write_all_timeout(&data, Duration::from_millis(timeout_ms as u64))
      .await
      .map_err(map_err)
  }

  // ---------- Diagnose / State ----------

  #[napi(getter)]
  pub fn id(&self) -> f64 { self.inner.id() as f64 }

  #[napi(getter)]
  pub fn endpoint(&self) -> String { self.inner.endpoint().to_string() }

  #[napi(getter)]
  pub fn name(&self) -> String { self.inner.name().to_string() }

  #[napi]
  pub fn local_endpoint(&self) -> Option<String> {
    self.inner.local_endpoint().map(|e| e.to_string())
  }

  #[napi]
  pub fn peer_endpoint(&self) -> Option<String> {
    self.inner.peer_endpoint().map(|e| e.to_string())
  }

  #[napi]
  pub fn info(&self) -> ConnectionInfo {
    let i = self.inner.info();
    ConnectionInfo {
      id: i.id as f64,
      local_endpoint: i.local_endpoint.map(|e| e.to_string()),
      peer_endpoint: i.peer_endpoint.map(|e| e.to_string()),
      is_closed: i.is_closed,
      is_read_shutdown: i.is_read_shutdown,
      is_write_shutdown: i.is_write_shutdown,
      peer_sent_eof: i.peer_sent_eof,
    }
  }

  #[napi(getter)]
  pub fn is_closed(&self) -> bool { self.inner.is_closed() }

  #[napi(getter)]
  pub fn is_read_shutdown(&self) -> bool { self.inner.is_read_shutdown() }

  #[napi(getter)]
  pub fn is_write_shutdown(&self) -> bool { self.inner.is_write_shutdown() }

  #[napi(getter)]
  pub fn peer_sent_eof(&self) -> bool { self.inner.peer_sent_eof() }

  #[napi(getter)]
  pub fn is_started(&self) -> bool { self.inner.is_started() }

  #[napi(getter)]
  pub fn is_active(&self) -> bool { self.inner.is_active() }

  #[napi(getter)]
  pub fn is_available(&self) -> bool { self.inner.is_available() }

  #[napi(getter)]
  pub fn is_paused(&self) -> bool { self.inner.is_paused() }

  #[napi]
  pub fn pause(&self) { self.inner.pause(); }

  #[napi]
  pub fn resume(&self) { self.inner.resume(); }
}

// ============================================================
// into_split-Ergebnis
// ============================================================
// Reiner Rückgabetyp: Nur die Konvertierung Rust -> JavaScript erzeugen.
#[napi(object, object_from_js = false)]
pub struct SplitHalves {
  pub read: ReadHalf,
  pub write: WriteHalf,
}

// ============================================================
// ReadHalf
// ============================================================
/// Leseansicht auf eine gemeinsam genutzte Verbindung.
#[napi]
pub struct ReadHalf {
  inner: Arc<taunicorn::Connection>,
}

#[napi]
impl ReadHalf {
  #[napi]
  pub async fn receive(&self, max_bytes: u32) -> Result<Option<Buffer>> {
    if max_bytes == 0 {
      return Ok(Some(Vec::new().into())); // Data(0): leeres Ziel, kein EOF
    }

    let half = Arc::clone(&self.inner);
    let mut buf = vec![0u8; max_bytes.min(RAW_READ_LIMIT) as usize];
    let result = half.receive(&mut buf).await.map_err(map_err)?;

    Ok(match result {
      ReceiveResult::Data(n) => {
        buf.truncate(n);
        Some(buf.into())
      }
      ReceiveResult::EndOfStream => None,
    })
  }

  #[napi]
  pub async fn shutdown_read(&self) -> Result<()> {
    self.inner.shutdown_read().await.map_err(map_err)
  }

  #[napi(getter)]
  pub fn id(&self) -> f64 { self.inner.id() as f64 }

  #[napi]
  pub fn info(&self) -> ConnectionInfo {
    let i = self.inner.info();
    ConnectionInfo {
      id: i.id as f64,
      local_endpoint: i.local_endpoint.map(|e| e.to_string()),
      peer_endpoint: i.peer_endpoint.map(|e| e.to_string()),
      is_closed: i.is_closed,
      is_read_shutdown: i.is_read_shutdown,
      is_write_shutdown: i.is_write_shutdown,
      peer_sent_eof: i.peer_sent_eof,
    }
  }
}

// ============================================================
// WriteHalf
// ============================================================
/// Schreibansicht auf eine gemeinsam genutzte Verbindung.
#[napi]
pub struct WriteHalf {
  inner: Arc<taunicorn::Connection>,
}

#[napi]
impl WriteHalf {
  #[napi]
  pub async fn send(&self, data: Buffer) -> Result<()> {
    self.inner.send(&data).await.map_err(map_err)
  }

  #[napi]
  pub async fn write(&self, data: Buffer) -> Result<f64> {
    let n = self.inner.write(&data).await.map_err(map_err)?;
    Ok(n as f64)
  }

  #[napi]
  pub async fn flush(&self) -> Result<()> { self.inner.flush().await.map_err(map_err) }

  #[napi]
  pub async fn shutdown_write(&self) -> Result<()> {
    self.inner.shutdown_write().await.map_err(map_err)
  }

  #[napi(getter)]
  pub fn id(&self) -> f64 { self.inner.id() as f64 }

  #[napi]
  pub fn info(&self) -> ConnectionInfo {
    let i = self.inner.info();
    ConnectionInfo {
      id: i.id as f64,
      local_endpoint: i.local_endpoint.map(|e| e.to_string()),
      peer_endpoint: i.peer_endpoint.map(|e| e.to_string()),
      is_closed: i.is_closed,
      is_read_shutdown: i.is_read_shutdown,
      is_write_shutdown: i.is_write_shutdown,
      peer_sent_eof: i.peer_sent_eof,
    }
  }
}

// ============================================================
// LocalTransport-Ersatz (Namespace-Helper)
// ============================================================
/// LocalTransport::start — Server mit Default-Berechtigungen.
#[napi]
pub async fn start_transport(endpoint: String) -> Result<Server> {
  let ep = parse_endpoint(endpoint)?;
  let server = taunicorn::LocalTransport::start(ep).await.map_err(map_err)?;
  Ok(Server { inner: Arc::new(server) })
}

/// LocalTransport::connect — Client-Verbindung.
#[napi]
pub async fn connect_transport(endpoint: String) -> Result<Connection> {
  let ep = parse_endpoint(endpoint)?;
  let conn = taunicorn::LocalTransport::connect(ep).await.map_err(map_err)?;
  Ok(Connection { inner: Arc::new(conn) })
}
