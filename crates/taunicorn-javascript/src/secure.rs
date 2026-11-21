use {
  crate::{map_err, parse_endpoint, ConnectionInfo},
  napi::{
    bindgen_prelude::*,
    tokio::{sync::Mutex as AsyncMutex, time::timeout},
    Env,
  },
  napi_derive::napi,
  std::{
    future::Future,
    sync::{
      atomic::{AtomicBool, Ordering},
      Arc,
    },
    time::Duration,
  },
  taunicorn::security::{self as tu_security, SecureConnection as TuSecureConnection},
};

// Diese Größen entsprechen der angehängten taunicorn/security.rs, Protokoll v2.
const IDENTITY_KEY_SIZE: usize = 32;
const MAX_SECURE_FRAME_BYTES: usize = 64 * 1024 * 1024;
const SECURE_FRAME_OVERHEAD_BYTES: usize = 1 + 8 + 16;
const MAX_SECURE_MESSAGE_BYTES: usize = MAX_SECURE_FRAME_BYTES - SECURE_FRAME_OVERHEAD_BYTES;

const DEFAULT_CONNECT_TIMEOUT_MS: u64 = 30_000;
const DEFAULT_HANDSHAKE_TIMEOUT_MS: u64 = 10_000;

// ============================================================
// Schlüssel und Eingabeprüfung
// ============================================================

fn copy_identity_key(bytes: &[u8], name: &str) -> Result<[u8; IDENTITY_KEY_SIZE]> {
  bytes.try_into().map_err(|_| {
    Error::new(
      Status::InvalidArg,
      format!("{name} must contain exactly {IDENTITY_KEY_SIZE} bytes"),
    )
  })
}

fn optional_timeout(value: Option<f64>, name: &str) -> Result<Option<Duration>> {
  let Some(value) = value else {
    return Ok(None);
  };

  if !value.is_finite() || value.fract() != 0.0 || value < 1.0 || value > u32::MAX as f64 {
    return Err(Error::new(
      Status::InvalidArg,
      format!("{name} must be an integer between 1 and {} milliseconds", u32::MAX),
    ));
  }

  Ok(Some(Duration::from_millis(value as u64)))
}

fn check_message_size(size: usize) -> Result<()> {
  if size > MAX_SECURE_MESSAGE_BYTES {
    return Err(Error::new(
      Status::InvalidArg,
      format!(
        "message exceeds the secure protocol limit: {size} > {MAX_SECURE_MESSAGE_BYTES} bytes"
      ),
    ));
  }
  Ok(())
}

/// Erzeugt einen privaten Ed25519-Identitätsschlüssel als 32-Byte-Seed.
/// Den privaten Schlüssel geschützt aufbewahren und nicht an den Peer senden.
#[napi]
pub fn generate_identity_private_key() -> Result<Buffer> {
  let key = tu_security::generate_identity_private_key().map_err(map_err)?;
  Ok(key.to_vec().into())
}

/// Leitet den öffentlichen Ed25519-Schlüssel aus einem 32-Byte-Seed ab.
/// Der öffentliche Schlüssel dient der Gegenstelle als vertrauenswürdiger Pin.
#[napi]
pub fn identity_public_key(identity_private_key: Buffer) -> Result<Buffer> {
  let key = copy_identity_key(&identity_private_key, "identityPrivateKey")?;
  Ok(tu_security::identity_public_key(&key).to_vec().into())
}

// ============================================================
// Optionen
// ============================================================

#[napi(object)]
#[derive(Default)]
pub struct SecureConnectOptions {
  /// Zeit für den Transport-Verbindungsaufbau; Standard: 30 Sekunden.
  pub connect_timeout_ms: Option<f64>,
  /// Zeit für den anschließenden Handshake; Standard: 10 Sekunden.
  pub handshake_timeout_ms: Option<f64>,
}

#[napi(object)]
#[derive(Default)]
pub struct SecureAcceptOptions {
  /// Ohne Angabe unbegrenzt auf eine Verbindung warten; stop() bricht ab.
  pub accept_timeout_ms: Option<f64>,
  /// Beginnt nach erfolgreichem Accept; Standard: 10 Sekunden.
  pub handshake_timeout_ms: Option<f64>,
}

// ============================================================
// Gemeinsam genutzter Zustand und Fehlerbehandlung
// ============================================================

struct SecureSession {
  secure: TuSecureConnection,
  unusable: AtomicBool,
  send_gate: AsyncMutex<()>,
  receive_gate: AsyncMutex<()>,
}

/// Sperrt eine Sitzung auch dann, wenn eine bereits gestartete Operation
/// abgebrochen wird. Drop führt kein asynchrones Schließen aus.
struct OperationGuard<'a> {
  unusable: &'a AtomicBool,
  armed: bool,
}

impl<'a> OperationGuard<'a> {
  fn new(unusable: &'a AtomicBool) -> Self { Self { unusable, armed: true } }

  fn disarm(&mut self) { self.armed = false; }
}

impl Drop for OperationGuard<'_> {
  fn drop(&mut self) {
    if self.armed {
      self.unusable.store(true, Ordering::SeqCst);
    }
  }
}

impl SecureSession {
  fn new(secure: TuSecureConnection) -> Self {
    Self {
      secure,
      unusable: AtomicBool::new(false),
      send_gate: AsyncMutex::new(()),
      receive_gate: AsyncMutex::new(()),
    }
  }

  fn is_closed(&self) -> bool {
    self.unusable.load(Ordering::SeqCst) || self.secure.connection().is_closed()
  }

  fn ensure_open(&self) -> Result<()> {
    if self.is_closed() {
      return Err(Error::new(
        Status::GenericFailure,
        "secure connection is closed or unusable; establish a new session".to_string(),
      ));
    }
    Ok(())
  }

  async fn run<T, F>(
    &self,
    gate: &AsyncMutex<()>,
    operation_name: &str,
    operation_timeout: Option<Duration>,
    future: F,
  ) -> Result<T>
  where
    T: Send,
    F: Future<Output = anyhow::Result<T>> + Send,
  {
    // Unter demselben Gate wird vor jedem I/O der Zustand erneut geprüft.
    // So kann nach einem Fehler kein wartender Aufruf dieselbe Sequenz nutzen.
    let _gate = gate.lock().await;
    self.ensure_open()?;

    // Nach dem Gate deklariert: Bei Abbruch wird zuerst die Sitzung gesperrt,
    // anschließend wird der Gate-Lock freigegeben.
    let mut operation = OperationGuard::new(&self.unusable);

    // Die optionale Frist gilt für das native I/O ab Erwerb des Richtungs-Gates.
    let outcome = match operation_timeout {
      Some(duration) => match timeout(duration, future).await {
        Ok(result) => result.map_err(map_err),
        Err(_) => Err(Error::new(
          Status::GenericFailure,
          format!("secure {operation_name} timed out; the session is no longer usable"),
        )),
      },
      None => future.await.map_err(map_err),
    };

    match outcome {
      Ok(value) => {
        operation.disarm();
        // Die andere Richtung oder close() könnte inzwischen geschlossen haben.
        self.ensure_open()?;
        Ok(value)
      }
      Err(error) => {
        // Vor jeder weiteren await-Stelle und vor Freigabe des Gates sperren.
        self.unusable.store(true, Ordering::SeqCst);
        operation.disarm();
        // Den ursprünglichen Fehler erhalten; die Sitzung bleibt auch dann
        // gesperrt, wenn das Schließen der Transportverbindung fehlschlägt.
        let _ = self.secure.close().await;
        Err(error)
      }
    }
  }

  async fn send_owned(&self, data: Vec<u8>, operation_timeout: Option<Duration>) -> Result<()> {
    self.run(&self.send_gate, "send", operation_timeout, self.secure.send(&data)).await
  }

  async fn receive_owned(&self, operation_timeout: Option<Duration>) -> Result<Vec<u8>> {
    self.run(&self.receive_gate, "receive", operation_timeout, self.secure.receive()).await
  }
}

// ============================================================
// SecureConnection
// ============================================================

/// N-API-Zugriff auf die authentifizierte Nachrichtenschicht von taunicorn.
/// Die rohe Transportverbindung wird nicht nach JavaScript zurückgegeben.
#[napi]
pub struct SecureConnection {
  inner: Arc<SecureSession>,
}

impl SecureConnection {
  fn from_native(secure: TuSecureConnection) -> Self {
    Self { inner: Arc::new(SecureSession::new(secure)) }
  }
}

#[napi]
impl SecureConnection {
  /// Transport öffnen, Server-Pin prüfen und den Client-Handshake abschließen.
  /// Beide Schlüssel werden synchron vor dem Start des Future kopiert.
  /// Normaler statischer Export: AsyncBlock<Self> ist keine napi(factory).
  #[napi]
  pub fn connect(
    env: &Env,
    endpoint: String,
    identity_private_key: Buffer,
    server_identity_public_key: Buffer,
    options: Option<SecureConnectOptions>,
  ) -> Result<AsyncBlock<SecureConnection>> {
    let private_key = copy_identity_key(&identity_private_key, "identityPrivateKey")?;
    let server_key = copy_identity_key(&server_identity_public_key, "serverIdentityPublicKey")?;
    let endpoint = parse_endpoint(endpoint)?;
    let options = options.unwrap_or_default();
    let connect_timeout = optional_timeout(options.connect_timeout_ms, "connectTimeoutMs")?
      .unwrap_or(Duration::from_millis(DEFAULT_CONNECT_TIMEOUT_MS));
    let handshake_timeout = optional_timeout(options.handshake_timeout_ms, "handshakeTimeoutMs")?
      .unwrap_or(Duration::from_millis(DEFAULT_HANDSHAKE_TIMEOUT_MS));

    AsyncBlockBuilder::new(async move {
      let connection = taunicorn::Connection::connect_timeout(endpoint, connect_timeout)
        .await
        .map_err(map_err)?;

      // Der Handshake besitzt die frische Verbindung vollständig. Bei Fehler
      // oder Timeout wird sie zusammen mit dem Handshake-Future verworfen.
      let secure = timeout(
        handshake_timeout,
        TuSecureConnection::client(connection, private_key, server_key),
      )
      .await
      .map_err(|_| {
        Error::new(Status::GenericFailure, "secure client handshake timed out".to_string())
      })?
      .map_err(map_err)?;

      Ok(SecureConnection::from_native(secure))
    })
    .build(env)
  }

  /// Eine vollständige Nachricht senden; leere Nachrichten sind zulässig.
  /// timeoutMs gilt nach Erwerb des Sendegates. Ein I/O-Timeout sperrt die Sitzung.
  #[napi]
  pub fn send(&self, env: &Env, data: Buffer, timeout_ms: Option<f64>) -> Result<AsyncBlock<()>> {
    self.inner.ensure_open()?;
    let operation_timeout = optional_timeout(timeout_ms, "timeoutMs")?;
    check_message_size(data.len())?;

    // Vor Dispatch auf dem JS-Thread kopieren; Buffer synchronisiert keine Bytes.
    let data = data.to_vec();
    let inner = Arc::clone(&self.inner);
    AsyncBlockBuilder::new(async move { inner.send_owned(data, operation_timeout).await })
      .build(env)
  }

  /// Genau eine vollständige authentifizierte Nachricht empfangen.
  /// EOF ist gemäß nativer API ein Fehler; ein leerer Buffer ist eine Nachricht.
  #[napi]
  pub fn receive(&self, env: &Env, timeout_ms: Option<f64>) -> Result<AsyncBlock<Buffer>> {
    self.inner.ensure_open()?;
    let operation_timeout = optional_timeout(timeout_ms, "timeoutMs")?;
    let inner = Arc::clone(&self.inner);
    AsyncBlockBuilder::new(
      async move { Ok(inner.receive_owned(operation_timeout).await?.into()) },
    )
    .build(env)
  }

  /// JSON-kompatible Werte als eine verschlüsselte MessagePack-Nachricht senden.
  /// Die N-API-Konvertierung erstellt einen eigenen Rust-Wert vor dem Dispatch.
  #[napi]
  pub fn send_msgpack(
    &self,
    env: &Env,
    value: serde_json::Value,
    timeout_ms: Option<f64>,
  ) -> Result<AsyncBlock<()>> {
    self.inner.ensure_open()?;
    let operation_timeout = optional_timeout(timeout_ms, "timeoutMs")?;
    let inner = Arc::clone(&self.inner);
    AsyncBlockBuilder::new(async move {
      // Entspricht der Serialisierung in taunicorn::security::send_msgpack.
      let data = rmp_serde::to_vec_named(&value).map_err(|error| {
        Error::new(Status::InvalidArg, format!("MessagePack encoding failed: {error}"))
      })?;
      check_message_size(data.len())?;
      inner.send_owned(data, operation_timeout).await
    })
    .build(env)
  }

  /// Eine Nachricht entschlüsseln und als JSON-kompatiblen MessagePack-Wert lesen.
  /// Ein Formatfehler nach erfolgreicher Authentifizierung sperrt die Sitzung nicht.
  #[napi]
  pub fn receive_msgpack(
    &self,
    env: &Env,
    timeout_ms: Option<f64>,
  ) -> Result<AsyncBlock<serde_json::Value>> {
    self.inner.ensure_open()?;
    let operation_timeout = optional_timeout(timeout_ms, "timeoutMs")?;
    let inner = Arc::clone(&self.inner);
    AsyncBlockBuilder::new(async move {
      let data = inner.receive_owned(operation_timeout).await?;
      rmp_serde::from_slice::<serde_json::Value>(&data).map_err(|error| {
        Error::new(Status::GenericFailure, format!("MessagePack decoding failed: {error}"))
      })
    })
    .build(env)
  }

  /// Die Sitzung sofort für neue Operationen sperren und den Transport schließen.
  #[napi]
  pub fn close(&self, env: &Env) -> Result<AsyncBlock<()>> {
    self.inner.unusable.store(true, Ordering::SeqCst);
    let inner = Arc::clone(&self.inner);
    // Keine Richtungs-Gates erwerben: ein wartendes receive() darf close()
    // nicht daran hindern, das native I/O zu unterbrechen.
    AsyncBlockBuilder::new(async move { inner.secure.close().await.map_err(map_err) }).build(env)
  }

  #[napi(getter)]
  pub fn id(&self) -> f64 { self.inner.secure.connection().id() as f64 }

  #[napi(getter)]
  pub fn is_closed(&self) -> bool { self.inner.is_closed() }

  #[napi]
  pub fn info(&self) -> ConnectionInfo {
    let i = self.inner.secure.connection().info();
    ConnectionInfo {
      id: i.id as f64,
      local_endpoint: i.local_endpoint.map(|endpoint| endpoint.to_string()),
      peer_endpoint: i.peer_endpoint.map(|endpoint| endpoint.to_string()),
      is_closed: i.is_closed || self.inner.unusable.load(Ordering::SeqCst),
      is_read_shutdown: i.is_read_shutdown,
      is_write_shutdown: i.is_write_shutdown,
      peer_sent_eof: i.peer_sent_eof,
    }
  }
}

pub(crate) fn accept_secure(
  env: &Env,
  server: Arc<taunicorn::Server>,
  identity_private_key: Buffer,
  client_identity_public_key: Buffer,
  options: Option<SecureAcceptOptions>,
) -> Result<AsyncBlock<SecureConnection>> {
  let private_key = copy_identity_key(&identity_private_key, "identityPrivateKey")?;
  let client_key = copy_identity_key(&client_identity_public_key, "clientIdentityPublicKey")?;
  let options = options.unwrap_or_default();
  let accept_timeout = optional_timeout(options.accept_timeout_ms, "acceptTimeoutMs")?;
  let handshake_timeout = optional_timeout(options.handshake_timeout_ms, "handshakeTimeoutMs")?
    .unwrap_or(Duration::from_millis(DEFAULT_HANDSHAKE_TIMEOUT_MS));

  AsyncBlockBuilder::new(async move {
    let connection = match accept_timeout {
      Some(duration) => server.accept_timeout(duration).await,
      None => server.accept().await,
    }
    .map_err(map_err)?;
    let secure =
      timeout(handshake_timeout, TuSecureConnection::server(connection, client_key, private_key))
        .await
        .map_err(|_| {
          Error::new(Status::GenericFailure, "secure server handshake timed out".to_string())
        })?
        .map_err(map_err)?;

    Ok(SecureConnection::from_native(secure))
  })
  .build(env)
}
