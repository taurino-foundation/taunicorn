use {
  crate::{Connection, ConnectionCore, ReceiveResult},
  anyhow::{Result, anyhow, bail},
  chacha20poly1305::{
    ChaCha20Poly1305, Nonce,
    aead::{Aead, KeyInit, Payload},
  },
  ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey},
  hkdf::Hkdf,
  sha2::{Digest, Sha256},
  std::{sync::Arc, time::Duration},
  tokio::{
    sync::{Mutex, MutexGuard, Semaphore},
    time::{Instant, timeout_at},
  },
  x25519_dalek::{EphemeralSecret, PublicKey as X25519PublicKey},
};

// =============================================================================
// Constants
// =============================================================================

const AEAD_KEY_SIZE: usize = 32;
const AEAD_NONCE_SIZE: usize = 12;
const AEAD_TAG_SIZE: usize = 16;

const CLIENT_FINISHED: &[u8] = b"taunicorn-e2ee-v2/client-finished";
const CLIENT_HELLO: u8 = 0x01;

const DIRECTION_CLIENT_TO_SERVER: u8 = 0x01;
const DIRECTION_SERVER_TO_CLIENT: u8 = 0x02;

const ED25519_PRIVATE_KEY_SIZE: usize = 32;
const ED25519_PUBLIC_KEY_SIZE: usize = 32;
const ED25519_SIGNATURE_SIZE: usize = 64;

const ENCRYPTED_FRAME_HEADER_SIZE: usize = VERSION_SIZE + SEQUENCE_SIZE;
const FRAME_LENGTH_SIZE: usize = size_of::<u32>();

const HANDSHAKE_NONCE_SIZE: usize = 32;
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
static HANDSHAKES: Semaphore = Semaphore::const_new(64);

const HELLO_HEADER_SIZE: usize = MESSAGE_TYPE_SIZE + VERSION_SIZE;
const HELLO_SIZE: usize = HELLO_HEADER_SIZE
  + ED25519_PUBLIC_KEY_SIZE
  + X25519_PUBLIC_KEY_SIZE
  + HANDSHAKE_NONCE_SIZE
  + ED25519_SIGNATURE_SIZE;

const HKDF_OUTPUT_SIZE: usize = (AEAD_KEY_SIZE * 2) + (NONCE_PREFIX_SIZE * 2);

const MAGIC: &[u8; 4] = b"TNE2";
const MAX_FRAME_SIZE: usize = 64 * 1024 * 1024;
const MESSAGE_TYPE_SIZE: usize = size_of::<u8>();

const MIN_ENCRYPTED_FRAME_SIZE: usize = ENCRYPTED_FRAME_HEADER_SIZE + AEAD_TAG_SIZE;

/// Largest plaintext carried by a valid Secure v2 application frame.
pub const MAX_PLAINTEXT_SIZE: usize = MAX_FRAME_SIZE - MIN_ENCRYPTED_FRAME_SIZE;

const NONCE_PREFIX_SIZE: usize = AEAD_NONCE_SIZE - SEQUENCE_SIZE;

const SEQUENCE_SIZE: usize = size_of::<u64>();

const SERVER_FINISHED: &[u8] = b"taunicorn-e2ee-v2/server-finished";
const SERVER_HELLO: u8 = 0x02;

const TRANSCRIPT_HASH_SIZE: usize = 32;

const VERSION: u8 = 2;
const VERSION_SIZE: usize = size_of::<u8>();

const X25519_PUBLIC_KEY_SIZE: usize = 32;
const X25519_SHARED_SECRET_SIZE: usize = 32;

// =============================================================================
// Handshake Structures
// =============================================================================

struct ClientHello {
  ephemeral_public_key: [u8; X25519_PUBLIC_KEY_SIZE],
  identity_public_key: [u8; ED25519_PUBLIC_KEY_SIZE],
  nonce: [u8; HANDSHAKE_NONCE_SIZE],
  signature: [u8; ED25519_SIGNATURE_SIZE],
}

struct DirectionState {
  cipher: ChaCha20Poly1305,
  direction: u8,
  nonce_prefix: [u8; NONCE_PREFIX_SIZE],
  sequence: u64,
}

// Drop before unlocking a direction, so waiters cannot inherit uncertain state.
struct CloseOnDrop<'a> {
  core: &'a ConnectionCore,
  armed: bool,
}

impl<'a> CloseOnDrop<'a> {
  fn new(core: &'a ConnectionCore) -> Self { Self { core, armed: true } }

  fn complete(mut self) -> Result<()> {
    if self.core.state.is_closed() {
      bail!("secure connection is closed");
    }
    self.armed = false;
    Ok(())
  }
}

impl Drop for CloseOnDrop<'_> {
  fn drop(&mut self) {
    if self.armed {
      self.core.fail_connection();
    }
  }
}

async fn bounded_handshake(
  core: Arc<ConnectionCore>,
  exchange: impl Future<Output = Result<SecureConnection>>,
) -> Result<SecureConnection> {
  let guard = CloseOnDrop::new(&core);
  let _slot = HANDSHAKES.try_acquire().map_err(|_| anyhow!("secure handshake limit reached"))?;
  let deadline = Instant::now() + HANDSHAKE_TIMEOUT;
  let secure =
    timeout_at(deadline, exchange).await.map_err(|_| anyhow!("secure handshake timed out"))??;
  // Synchronous crypto can finish without yielding to the timer.
  if Instant::now() >= deadline {
    bail!("secure handshake timed out");
  }
  guard.complete()?;
  Ok(secure)
}

struct KeyMaterial {
  client_to_server_key: [u8; AEAD_KEY_SIZE],
  client_to_server_nonce_prefix: [u8; NONCE_PREFIX_SIZE],
  server_to_client_key: [u8; AEAD_KEY_SIZE],
  server_to_client_nonce_prefix: [u8; NONCE_PREFIX_SIZE],
}

struct ServerHello {
  ephemeral_public_key: [u8; X25519_PUBLIC_KEY_SIZE],
  identity_public_key: [u8; ED25519_PUBLIC_KEY_SIZE],
  nonce: [u8; HANDSHAKE_NONCE_SIZE],
  signature: [u8; ED25519_SIGNATURE_SIZE],
}

// =============================================================================
// Identity
// =============================================================================

/// Generates a persistent Ed25519 private identity key.
///
/// Generate this key once per endpoint and store it in protected persistent
/// storage. The private identity key authenticates this endpoint across
/// sessions and must never be transmitted to the peer.
pub fn generate_identity_private_key() -> Result<[u8; ED25519_PRIVATE_KEY_SIZE]> {
  let mut key = [0u8; ED25519_PRIVATE_KEY_SIZE];

  getrandom::fill(&mut key)
    .map_err(|error| anyhow!("failed to generate Ed25519 identity key: {error}"))?;

  Ok(key)
}

/// Derives the public Ed25519 identity from a persistent private identity key.
///
/// The peer pins this public key and uses it to authenticate handshake
/// signatures. Unlike the private key, this value may be distributed.
pub fn identity_public_key(
  identity_private_key: &[u8; ED25519_PRIVATE_KEY_SIZE],
) -> [u8; ED25519_PUBLIC_KEY_SIZE] {
  SigningKey::from_bytes(identity_private_key).verifying_key().to_bytes()
}

// =============================================================================
// Secure Connection
// =============================================================================

/// Authenticated encrypted message layer over a Taunicorn byte stream.
///
/// Persistent Ed25519 identities authenticate the endpoints. Each connection
/// creates fresh ephemeral X25519 secrets, so compromise of a long-term
/// identity key does not reveal previously established traffic keys.
///
/// HKDF-SHA256 derives independent keys and nonce prefixes for both traffic
/// directions. ChaCha20-Poly1305 provides confidentiality and integrity for
/// every message. Strict sequence numbers reject replayed, reordered, and
/// skipped messages within one session.
pub struct SecureConnection {
  connection: Connection,
  receive_state: Mutex<DirectionState>,
  send_state: Mutex<DirectionState>,
  transcript_hash: [u8; TRANSCRIPT_HASH_SIZE],
}

impl SecureConnection {
  // =========================================================================
  // Client
  // =========================================================================

  /// Establishes the client side of a mutually authenticated secure session.
  ///
  /// The client:
  /// 1. Creates a fresh ephemeral X25519 key and handshake nonce.
  /// 2. Signs that ephemeral state together with both endpoint identities.
  /// 3. Verifies the server's pinned Ed25519 identity and signature.
  /// 4. Computes the ephemeral X25519 shared secret.
  /// 5. Derives directional AEAD keys from the shared secret and transcript.
  /// 6. Verifies the server's encrypted key-confirmation message.
  pub async fn client(
    connection: Connection,
    identity_private_key: [u8; ED25519_PRIVATE_KEY_SIZE],
    server_identity_public_key: [u8; ED25519_PUBLIC_KEY_SIZE],
  ) -> Result<Self> {
    bounded_handshake(
      Arc::clone(&connection.core),
      Self::client_exchange(connection, identity_private_key, server_identity_public_key),
    )
    .await
  }

  async fn client_exchange(
    connection: Connection,
    identity_private_key: [u8; ED25519_PRIVATE_KEY_SIZE],
    server_identity_public_key: [u8; ED25519_PUBLIC_KEY_SIZE],
  ) -> Result<Self> {
    let identity_private = SigningKey::from_bytes(&identity_private_key);
    let client_identity_public = identity_private.verifying_key().to_bytes();

    let ephemeral_private = EphemeralSecret::random();
    let client_ephemeral_public = X25519PublicKey::from(&ephemeral_private).to_bytes();

    let client_nonce = random_handshake_nonce()?;

    let signature_message = client_signature_message(
      &client_ephemeral_public,
      &client_identity_public,
      &client_nonce,
      &server_identity_public_key,
    );

    let client_signature = identity_private.sign(&signature_message).to_bytes();

    let client_hello = ClientHello {
      ephemeral_public_key: client_ephemeral_public,
      identity_public_key: client_identity_public,
      nonce: client_nonce,
      signature: client_signature,
    };

    write_frame(&connection, &encode_client_hello(&client_hello)).await?;

    let server_hello = decode_server_hello(&read_frame(&connection, FrameLimit::Hello).await?)?;

    if server_hello.identity_public_key != server_identity_public_key {
      bail!("server identity does not match the pinned public key");
    }

    let signature_message = server_signature_message(
      &client_ephemeral_public,
      &client_identity_public,
      &client_nonce,
      &server_hello.ephemeral_public_key,
      &server_hello.identity_public_key,
      &server_hello.nonce,
    );

    verify_signature(
      &server_hello.identity_public_key,
      &signature_message,
      &server_hello.signature,
    )?;

    let server_ephemeral_public = X25519PublicKey::from(server_hello.ephemeral_public_key);

    let shared_secret = ephemeral_private.diffie_hellman(&server_ephemeral_public);

    if !shared_secret.was_contributory() {
      bail!("invalid non-contributory X25519 shared secret");
    }

    let transcript_hash = handshake_transcript_hash(
      &client_ephemeral_public,
      &client_identity_public,
      &client_nonce,
      &server_hello.ephemeral_public_key,
      &server_hello.identity_public_key,
      &server_hello.nonce,
    );

    let key_material = derive_key_material(shared_secret.as_bytes(), &transcript_hash)?;

    let secure = Self {
      connection,
      receive_state: Mutex::new(DirectionState {
        cipher: create_cipher(&key_material.server_to_client_key)?,
        direction: DIRECTION_SERVER_TO_CLIENT,
        nonce_prefix: key_material.server_to_client_nonce_prefix,
        sequence: 0,
      }),
      send_state: Mutex::new(DirectionState {
        cipher: create_cipher(&key_material.client_to_server_key)?,
        direction: DIRECTION_CLIENT_TO_SERVER,
        nonce_prefix: key_material.client_to_server_nonce_prefix,
        sequence: 0,
      }),
      transcript_hash,
    };

    let server_finished = secure.receive_frame(FrameLimit::Finished(SERVER_FINISHED)).await?;

    if server_finished.as_slice() != SERVER_FINISHED {
      bail!("invalid server key confirmation");
    }

    secure.send(CLIENT_FINISHED).await?;

    Ok(secure)
  }

  // =========================================================================
  // Close
  // =========================================================================

  /// Closes the underlying Taunicorn connection.
  pub async fn close(&self) -> Result<()> { self.connection.close().await }

  // =========================================================================
  // Connection
  // =========================================================================

  /// Returns the underlying transport connection.
  ///
  /// Application code should normally not use the raw connection after the
  /// secure handshake, because direct writes bypass encryption and framing.
  pub fn connection(&self) -> &Connection { &self.connection }

  /// Reports terminal state without granting access to the raw transport.
  pub fn is_closed(&self) -> bool { self.connection.is_closed() }

  /// Synchronously disables both directions and wakes pending I/O.
  pub fn abort(&self) { self.connection.core.fail_connection(); }

  async fn lock_direction<'a>(
    &self,
    state: &'a Mutex<DirectionState>,
  ) -> Result<MutexGuard<'a, DirectionState>> {
    tokio::select! {
        biased;
        _ = self.connection.core.close_token.cancelled() => Err(anyhow!("secure connection is closed")),
        guard = state.lock() => Ok(guard),
    }
  }

  // =========================================================================
  // Receive
  // =========================================================================

  /// Receives, authenticates, replay-checks, and decrypts one message.
  ///
  /// The receiver requires the exact next sequence number. The version,
  /// traffic direction, sequence number, and handshake transcript hash are
  /// authenticated as AEAD associated data.
  pub async fn receive(&self) -> Result<Vec<u8>> {
    self.receive_limited(MAX_PLAINTEXT_SIZE).await
  }

  /// Bounds the plaintext size before allocating the ciphertext frame.
  pub async fn receive_limited(&self, max_plaintext: usize) -> Result<Vec<u8>> {
    self.receive_frame(FrameLimit::Application(max_plaintext)).await
  }

  async fn receive_frame(&self, limit: FrameLimit) -> Result<Vec<u8>> {
    let mut state = self.lock_direction(&self.receive_state).await?;
    let guard = CloseOnDrop::new(&self.connection.core);
    let frame = read_frame(&self.connection, limit).await?;

    if frame[0] != VERSION {
      bail!("unsupported secure protocol version: expected {}, received {}", VERSION, frame[0],);
    }

    let sequence_start = VERSION_SIZE;
    let sequence_end = sequence_start + SEQUENCE_SIZE;

    let sequence_bytes: [u8; SEQUENCE_SIZE] = frame[sequence_start..sequence_end]
      .try_into()
      .map_err(|_| anyhow!("invalid message sequence encoding"))?;

    let received_sequence = u64::from_be_bytes(sequence_bytes);

    if received_sequence != state.sequence {
      bail!(
        "invalid message sequence: expected {}, received {}",
        state.sequence,
        received_sequence,
      );
    }

    let nonce = make_nonce(state.nonce_prefix, sequence_bytes);

    let aad = make_aad(state.direction, sequence_bytes, &self.transcript_hash);

    let plaintext = state
      .cipher
      .decrypt(Nonce::from_slice(&nonce), Payload {
        msg: &frame[ENCRYPTED_FRAME_HEADER_SIZE..],
        aad: &aad,
      })
      .map_err(|_| anyhow!("message authentication failed"))?;

    state.sequence = state
      .sequence
      .checked_add(1)
      .ok_or_else(|| anyhow!("secure session receive sequence exhausted"))?;

    guard.complete()?;
    Ok(plaintext)
  }

  /// Receives one encrypted message and deserializes its plaintext as
  /// MessagePack.
  pub async fn receive_msgpack<T>(&self) -> Result<T>
  where
    T: serde::de::DeserializeOwned,
  {
    let plaintext = self.receive().await?;
    Ok(rmp_serde::from_slice(&plaintext)?)
  }

  // =========================================================================
  // Send
  // =========================================================================

  /// Encrypts and authenticates one complete application message.
  ///
  /// Each traffic direction has its own key and nonce prefix. The current
  /// 64-bit sequence number completes the 96-bit ChaCha20-Poly1305 nonce.
  /// The sequence is reserved before encryption and never retried.
  pub async fn send(&self, plaintext: &[u8]) -> Result<()> {
    self.send_limited(plaintext, MAX_PLAINTEXT_SIZE).await
  }

  /// Rejects an oversized local message before consuming a nonce or allocating ciphertext.
  pub async fn send_limited(&self, plaintext: &[u8], max_plaintext: usize) -> Result<()> {
    if plaintext.len() > max_plaintext.min(MAX_PLAINTEXT_SIZE) {
      bail!("secure message exceeds plaintext limit");
    }
    let mut state = self.lock_direction(&self.send_state).await?;
    let guard = CloseOnDrop::new(&self.connection.core);

    let sequence = state.sequence;

    state.sequence =
      sequence.checked_add(1).ok_or_else(|| anyhow!("secure session send sequence exhausted"))?;

    let sequence_bytes = sequence.to_be_bytes();

    let nonce = make_nonce(state.nonce_prefix, sequence_bytes);

    let aad = make_aad(state.direction, sequence_bytes, &self.transcript_hash);

    let ciphertext = state
      .cipher
      .encrypt(Nonce::from_slice(&nonce), Payload { msg: plaintext, aad: &aad })
      .map_err(|_| anyhow!("message encryption failed"))?;

    let mut frame = Vec::with_capacity(ENCRYPTED_FRAME_HEADER_SIZE + ciphertext.len());

    frame.push(VERSION);
    frame.extend_from_slice(&sequence_bytes);
    frame.extend_from_slice(&ciphertext);

    write_frame(&self.connection, &frame).await?;

    guard.complete()
  }

  /// Serializes a value as MessagePack and sends it as one encrypted message.
  pub async fn send_msgpack<T>(&self, value: &T) -> Result<()>
  where
    T: serde::Serialize,
  {
    let plaintext = rmp_serde::to_vec_named(value)?;
    self.send(&plaintext).await
  }

  // =========================================================================
  // Server
  // =========================================================================

  /// Establishes the server side of a mutually authenticated secure session.
  ///
  /// The server:
  /// 1. Verifies that the client identity matches the pinned Ed25519 key.
  /// 2. Verifies the client's signed ephemeral handshake state.
  /// 3. Creates and signs its own fresh ephemeral X25519 state.
  /// 4. Computes the same ephemeral X25519 shared secret.
  /// 5. Derives the same directional session keys.
  /// 6. Sends and verifies encrypted key-confirmation messages.
  pub async fn server(
    connection: Connection,
    client_identity_public_key: [u8; ED25519_PUBLIC_KEY_SIZE],
    identity_private_key: [u8; ED25519_PRIVATE_KEY_SIZE],
  ) -> Result<Self> {
    bounded_handshake(
      Arc::clone(&connection.core),
      Self::server_exchange(connection, client_identity_public_key, identity_private_key),
    )
    .await
  }

  async fn server_exchange(
    connection: Connection,
    client_identity_public_key: [u8; ED25519_PUBLIC_KEY_SIZE],
    identity_private_key: [u8; ED25519_PRIVATE_KEY_SIZE],
  ) -> Result<Self> {
    let identity_private = SigningKey::from_bytes(&identity_private_key);
    let server_identity_public = identity_private.verifying_key().to_bytes();

    let client_hello = decode_client_hello(&read_frame(&connection, FrameLimit::Hello).await?)?;

    if client_hello.identity_public_key != client_identity_public_key {
      bail!("client identity does not match the pinned public key");
    }

    let signature_message = client_signature_message(
      &client_hello.ephemeral_public_key,
      &client_hello.identity_public_key,
      &client_hello.nonce,
      &server_identity_public,
    );

    verify_signature(
      &client_hello.identity_public_key,
      &signature_message,
      &client_hello.signature,
    )?;

    let ephemeral_private = EphemeralSecret::random();
    let server_ephemeral_public = X25519PublicKey::from(&ephemeral_private).to_bytes();

    let server_nonce = random_handshake_nonce()?;

    let signature_message = server_signature_message(
      &client_hello.ephemeral_public_key,
      &client_hello.identity_public_key,
      &client_hello.nonce,
      &server_ephemeral_public,
      &server_identity_public,
      &server_nonce,
    );

    let server_signature = identity_private.sign(&signature_message).to_bytes();

    let server_hello = ServerHello {
      ephemeral_public_key: server_ephemeral_public,
      identity_public_key: server_identity_public,
      nonce: server_nonce,
      signature: server_signature,
    };

    write_frame(&connection, &encode_server_hello(&server_hello)).await?;

    let client_ephemeral_public = X25519PublicKey::from(client_hello.ephemeral_public_key);

    let shared_secret = ephemeral_private.diffie_hellman(&client_ephemeral_public);

    if !shared_secret.was_contributory() {
      bail!("invalid non-contributory X25519 shared secret");
    }

    let transcript_hash = handshake_transcript_hash(
      &client_hello.ephemeral_public_key,
      &client_hello.identity_public_key,
      &client_hello.nonce,
      &server_ephemeral_public,
      &server_identity_public,
      &server_nonce,
    );

    let key_material = derive_key_material(shared_secret.as_bytes(), &transcript_hash)?;

    let secure = Self {
      connection,
      receive_state: Mutex::new(DirectionState {
        cipher: create_cipher(&key_material.client_to_server_key)?,
        direction: DIRECTION_CLIENT_TO_SERVER,
        nonce_prefix: key_material.client_to_server_nonce_prefix,
        sequence: 0,
      }),
      send_state: Mutex::new(DirectionState {
        cipher: create_cipher(&key_material.server_to_client_key)?,
        direction: DIRECTION_SERVER_TO_CLIENT,
        nonce_prefix: key_material.server_to_client_nonce_prefix,
        sequence: 0,
      }),
      transcript_hash,
    };

    secure.send(SERVER_FINISHED).await?;

    let client_finished = secure.receive_frame(FrameLimit::Finished(CLIENT_FINISHED)).await?;

    if client_finished.as_slice() != CLIENT_FINISHED {
      bail!("invalid client key confirmation");
    }

    Ok(secure)
  }
}

// =============================================================================
// Authentication Helpers
// =============================================================================

/// Builds the deterministic byte sequence signed by the client.
///
/// The message binds the client's persistent identity, the pinned server
/// identity, the client's ephemeral X25519 key, and a fresh handshake nonce.
/// This prevents a valid client signature from being redirected to a different
/// server identity.
fn client_signature_message(
  client_ephemeral_public: &[u8; X25519_PUBLIC_KEY_SIZE],
  client_identity_public: &[u8; ED25519_PUBLIC_KEY_SIZE],
  client_nonce: &[u8; HANDSHAKE_NONCE_SIZE],
  server_identity_public: &[u8; ED25519_PUBLIC_KEY_SIZE],
) -> Vec<u8> {
  let mut message = Vec::new();

  message.extend_from_slice(MAGIC);
  message.extend_from_slice(b"/client-auth/");
  message.push(VERSION);
  message.extend_from_slice(client_identity_public);
  message.extend_from_slice(server_identity_public);
  message.extend_from_slice(client_ephemeral_public);
  message.extend_from_slice(client_nonce);

  message
}

/// Builds the deterministic byte sequence signed by the server.
///
/// The server signature covers both persistent identities, both ephemeral
/// X25519 public keys, and both fresh nonces. This binds all relevant handshake
/// values to one authenticated session.
fn server_signature_message(
  client_ephemeral_public: &[u8; X25519_PUBLIC_KEY_SIZE],
  client_identity_public: &[u8; ED25519_PUBLIC_KEY_SIZE],
  client_nonce: &[u8; HANDSHAKE_NONCE_SIZE],
  server_ephemeral_public: &[u8; X25519_PUBLIC_KEY_SIZE],
  server_identity_public: &[u8; ED25519_PUBLIC_KEY_SIZE],
  server_nonce: &[u8; HANDSHAKE_NONCE_SIZE],
) -> Vec<u8> {
  let mut message = Vec::new();

  message.extend_from_slice(MAGIC);
  message.extend_from_slice(b"/server-auth/");
  message.push(VERSION);
  message.extend_from_slice(client_identity_public);
  message.extend_from_slice(server_identity_public);
  message.extend_from_slice(client_ephemeral_public);
  message.extend_from_slice(server_ephemeral_public);
  message.extend_from_slice(client_nonce);
  message.extend_from_slice(server_nonce);

  message
}

/// Verifies that a handshake signature was produced by the expected persistent
/// Ed25519 identity.
fn verify_signature(
  identity_public_key: &[u8; ED25519_PUBLIC_KEY_SIZE],
  message: &[u8],
  signature: &[u8; ED25519_SIGNATURE_SIZE],
) -> Result<()> {
  let verifying_key = VerifyingKey::from_bytes(identity_public_key)?;

  let signature = Signature::try_from(signature.as_slice())
    .map_err(|_| anyhow!("invalid Ed25519 signature encoding"))?;

  verifying_key
    .verify(message, &signature)
    .map_err(|_| anyhow!("peer identity signature verification failed"))
}

// =============================================================================
// Cipher Helpers
// =============================================================================

/// Constructs one ChaCha20-Poly1305 instance from a 256-bit session key.
fn create_cipher(key: &[u8; AEAD_KEY_SIZE]) -> Result<ChaCha20Poly1305> {
  ChaCha20Poly1305::new_from_slice(key).map_err(|_| anyhow!("invalid ChaCha20-Poly1305 key"))
}

// =============================================================================
// Encoding Helpers
// =============================================================================

/// Parses and validates a fixed-size client handshake frame.
///
/// Offsets are derived from protocol constants instead of hard-coded byte
/// positions so changing one field size updates the complete parser layout.
fn decode_client_hello(data: &[u8]) -> Result<ClientHello> {
  if data.len() != HELLO_SIZE {
    bail!("invalid client hello size: expected {}, received {}", HELLO_SIZE, data.len(),);
  }

  if data[0] != CLIENT_HELLO {
    bail!("invalid client hello type");
  }

  if data[1] != VERSION {
    bail!("invalid client hello version: expected {}, received {}", VERSION, data[1],);
  }

  let mut offset = HELLO_HEADER_SIZE;

  let identity_public_key = take_array::<ED25519_PUBLIC_KEY_SIZE>(data, &mut offset)?;

  let ephemeral_public_key = take_array::<X25519_PUBLIC_KEY_SIZE>(data, &mut offset)?;

  let nonce = take_array::<HANDSHAKE_NONCE_SIZE>(data, &mut offset)?;

  let signature = take_array::<ED25519_SIGNATURE_SIZE>(data, &mut offset)?;

  debug_assert_eq!(offset, HELLO_SIZE);

  Ok(ClientHello { ephemeral_public_key, identity_public_key, nonce, signature })
}

/// Parses and validates a fixed-size server handshake frame.
///
/// This uses the exact same field layout as the client hello but requires the
/// server message type.
fn decode_server_hello(data: &[u8]) -> Result<ServerHello> {
  if data.len() != HELLO_SIZE {
    bail!("invalid server hello size: expected {}, received {}", HELLO_SIZE, data.len(),);
  }

  if data[0] != SERVER_HELLO {
    bail!("invalid server hello type");
  }

  if data[1] != VERSION {
    bail!("invalid server hello version: expected {}, received {}", VERSION, data[1],);
  }

  let mut offset = HELLO_HEADER_SIZE;

  let identity_public_key = take_array::<ED25519_PUBLIC_KEY_SIZE>(data, &mut offset)?;

  let ephemeral_public_key = take_array::<X25519_PUBLIC_KEY_SIZE>(data, &mut offset)?;

  let nonce = take_array::<HANDSHAKE_NONCE_SIZE>(data, &mut offset)?;

  let signature = take_array::<ED25519_SIGNATURE_SIZE>(data, &mut offset)?;

  debug_assert_eq!(offset, HELLO_SIZE);

  Ok(ServerHello { ephemeral_public_key, identity_public_key, nonce, signature })
}

/// Encodes a client hello into the deterministic cross-language wire format.
///
/// The handshake intentionally avoids MessagePack because signatures must cover
/// an exact byte representation that is identical in Rust and Python.
fn encode_client_hello(hello: &ClientHello) -> Vec<u8> {
  let mut output = Vec::with_capacity(HELLO_SIZE);

  output.push(CLIENT_HELLO);
  output.push(VERSION);
  output.extend_from_slice(&hello.identity_public_key);
  output.extend_from_slice(&hello.ephemeral_public_key);
  output.extend_from_slice(&hello.nonce);
  output.extend_from_slice(&hello.signature);

  debug_assert_eq!(output.len(), HELLO_SIZE);

  output
}

/// Encodes a server hello into the deterministic cross-language wire format.
fn encode_server_hello(hello: &ServerHello) -> Vec<u8> {
  let mut output = Vec::with_capacity(HELLO_SIZE);

  output.push(SERVER_HELLO);
  output.push(VERSION);
  output.extend_from_slice(&hello.identity_public_key);
  output.extend_from_slice(&hello.ephemeral_public_key);
  output.extend_from_slice(&hello.nonce);
  output.extend_from_slice(&hello.signature);

  debug_assert_eq!(output.len(), HELLO_SIZE);

  output
}

/// Extracts a fixed-size byte array from a protocol message and advances the
/// caller-owned offset.
///
/// Centralizing bounds checking removes repeated slicing arithmetic from the
/// handshake decoders.
fn take_array<const N: usize>(data: &[u8], offset: &mut usize) -> Result<[u8; N]> {
  let end = offset.checked_add(N).ok_or_else(|| anyhow!("protocol offset overflow"))?;

  let bytes =
    data.get(*offset..end).ok_or_else(|| anyhow!("protocol field exceeds frame boundary"))?;

  let value = bytes.try_into().map_err(|_| anyhow!("invalid fixed-size protocol field"))?;

  *offset = end;

  Ok(value)
}

// =============================================================================
// Framing Helpers
// =============================================================================

/// Fills the supplied buffer from Taunicorn's byte-oriented stream.
///
/// One transport receive operation is not one protocol message. This helper
/// therefore loops until the requested byte count has been collected or EOF is
/// observed.
async fn read_exact(connection: &Connection, result: &mut [u8]) -> Result<()> {
  let mut offset = 0;

  while offset < result.len() {
    match connection.receive(&mut result[offset..]).await? {
      ReceiveResult::Data(0) => {
        bail!("zero-length transport read while awaiting frame data");
      }
      ReceiveResult::Data(received) => {
        offset = offset
          .checked_add(received)
          .ok_or_else(|| anyhow!("transport receive offset overflow"))?;
      }
      ReceiveResult::EndOfStream => {
        bail!("connection closed while reading a frame");
      }
    }
  }

  Ok(())
}

#[derive(Clone, Copy)]
enum FrameLimit {
  Hello,
  Finished(&'static [u8]),
  Application(usize),
}

impl FrameLimit {
  fn checked_size(self, header: [u8; FRAME_LENGTH_SIZE]) -> Result<usize> {
    let sizes = match self {
      Self::Hello => HELLO_SIZE..=HELLO_SIZE,
      Self::Finished(label) => {
        let size = MIN_ENCRYPTED_FRAME_SIZE + label.len();
        size..=size
      }
      Self::Application(max) => {
        MIN_ENCRYPTED_FRAME_SIZE..=(MIN_ENCRYPTED_FRAME_SIZE + max.min(MAX_PLAINTEXT_SIZE))
      }
    };
    let size = u32::from_be_bytes(header) as usize;
    if !sizes.contains(&size) {
      bail!("invalid secure frame length: expected {sizes:?}, received {size}");
    }
    Ok(size)
  }
}

/// Reads one unsigned 32-bit big-endian length-prefixed protocol frame.
///
/// The outer length is intentionally not encrypted because the receiver needs
/// it before it can reconstruct the complete ciphertext. The encrypted frame's
/// contents are authenticated by ChaCha20-Poly1305.
async fn read_frame(connection: &Connection, limit: FrameLimit) -> Result<Vec<u8>> {
  let mut header = [0; FRAME_LENGTH_SIZE];
  read_exact(connection, &mut header).await?;
  let size = limit.checked_size(header)?;
  let mut frame = vec![0; size];
  read_exact(connection, &mut frame).await?;
  Ok(frame)
}

/// Writes one complete length-prefixed protocol frame.
///
/// `Connection::send` is used rather than a partial write API so the entire
/// prefix and payload are submitted as one ordered transport operation.
async fn write_frame(connection: &Connection, payload: &[u8]) -> Result<()> {
  if payload.is_empty() {
    bail!("zero-length frame is invalid");
  }

  if payload.len() > MAX_FRAME_SIZE {
    bail!("frame exceeds maximum size: {} > {}", payload.len(), MAX_FRAME_SIZE,);
  }

  let size =
    u32::try_from(payload.len()).map_err(|_| anyhow!("frame length does not fit into u32"))?;

  let mut frame = Vec::with_capacity(FRAME_LENGTH_SIZE + payload.len());

  frame.extend_from_slice(&size.to_be_bytes());
  frame.extend_from_slice(payload);

  connection.send(&frame).await
}

// =============================================================================
// Key Derivation Helpers
// =============================================================================

/// Derives independent AEAD keys and nonce prefixes for both directions.
///
/// The X25519 secret provides fresh per-connection key material. The transcript
/// hash is used as HKDF salt so the derived traffic keys are cryptographically
/// bound to both identities, both ephemeral keys, both handshake nonces, and
/// the protocol version.
fn derive_key_material(
  shared_secret: &[u8; X25519_SHARED_SECRET_SIZE],
  transcript_hash: &[u8; TRANSCRIPT_HASH_SIZE],
) -> Result<KeyMaterial> {
  let hkdf = Hkdf::<Sha256>::new(Some(transcript_hash), shared_secret);

  let mut output = [0u8; HKDF_OUTPUT_SIZE];

  hkdf
    .expand(b"taunicorn-e2ee-v2/session-keys", &mut output)
    .map_err(|_| anyhow!("HKDF expansion failed"))?;

  let mut offset = 0;

  let client_to_server_key = take_array::<AEAD_KEY_SIZE>(&output, &mut offset)?;

  let server_to_client_key = take_array::<AEAD_KEY_SIZE>(&output, &mut offset)?;

  let client_to_server_nonce_prefix = take_array::<NONCE_PREFIX_SIZE>(&output, &mut offset)?;

  let server_to_client_nonce_prefix = take_array::<NONCE_PREFIX_SIZE>(&output, &mut offset)?;

  debug_assert_eq!(offset, HKDF_OUTPUT_SIZE);

  Ok(KeyMaterial {
    client_to_server_key,
    client_to_server_nonce_prefix,
    server_to_client_key,
    server_to_client_nonce_prefix,
  })
}

/// Hashes the complete authenticated handshake transcript.
///
/// The same transcript hash is used by HKDF and as AEAD associated data. This
/// ensures subsequent application traffic belongs to exactly this handshake.
fn handshake_transcript_hash(
  client_ephemeral_public: &[u8; X25519_PUBLIC_KEY_SIZE],
  client_identity_public: &[u8; ED25519_PUBLIC_KEY_SIZE],
  client_nonce: &[u8; HANDSHAKE_NONCE_SIZE],
  server_ephemeral_public: &[u8; X25519_PUBLIC_KEY_SIZE],
  server_identity_public: &[u8; ED25519_PUBLIC_KEY_SIZE],
  server_nonce: &[u8; HANDSHAKE_NONCE_SIZE],
) -> [u8; TRANSCRIPT_HASH_SIZE] {
  let mut hash = Sha256::new();

  hash.update(MAGIC);
  hash.update(b"/transcript/");
  hash.update([VERSION]);
  hash.update(client_identity_public);
  hash.update(server_identity_public);
  hash.update(client_ephemeral_public);
  hash.update(server_ephemeral_public);
  hash.update(client_nonce);
  hash.update(server_nonce);

  hash.finalize().into()
}

// =============================================================================
// Message Encryption Helpers
// =============================================================================

/// Builds authenticated but unencrypted metadata for one encrypted message.
///
/// Direction and sequence are authenticated to prevent cross-direction replay
/// and message reordering. The transcript hash additionally binds the message
/// to the exact authenticated handshake that established its traffic keys.
fn make_aad(
  direction: u8,
  sequence: [u8; SEQUENCE_SIZE],
  transcript_hash: &[u8; TRANSCRIPT_HASH_SIZE],
) -> Vec<u8> {
  let mut aad = Vec::with_capacity(
    MAGIC.len() + VERSION_SIZE + MESSAGE_TYPE_SIZE + SEQUENCE_SIZE + TRANSCRIPT_HASH_SIZE,
  );

  aad.extend_from_slice(MAGIC);
  aad.push(VERSION);
  aad.push(direction);
  aad.extend_from_slice(&sequence);
  aad.extend_from_slice(transcript_hash);

  aad
}

/// Constructs the 96-bit ChaCha20-Poly1305 nonce.
///
/// Each direction receives its own HKDF-derived nonce prefix and key. Appending
/// the monotonically increasing 64-bit sequence number guarantees unique
/// nonces under one directional session key until the sequence space is
/// exhausted.
fn make_nonce(
  nonce_prefix: [u8; NONCE_PREFIX_SIZE],
  sequence: [u8; SEQUENCE_SIZE],
) -> [u8; AEAD_NONCE_SIZE] {
  let mut nonce = [0u8; AEAD_NONCE_SIZE];

  nonce[..NONCE_PREFIX_SIZE].copy_from_slice(&nonce_prefix);

  nonce[NONCE_PREFIX_SIZE..].copy_from_slice(&sequence);

  nonce
}

// =============================================================================
// Randomness Helpers
// =============================================================================

/// Generates a fresh handshake nonce from the operating system CSPRNG.
///
/// The nonce gives every handshake explicit freshness in addition to the fresh
/// ephemeral X25519 key pair and is included in signatures and the transcript.
fn random_handshake_nonce() -> Result<[u8; HANDSHAKE_NONCE_SIZE]> {
  let mut nonce = [0u8; HANDSHAKE_NONCE_SIZE];

  getrandom::fill(&mut nonce)
    .map_err(|error| anyhow!("failed to obtain handshake randomness: {error}"))?;

  Ok(nonce)
}

#[cfg(test)]
mod tests {
  use {
    super::*,
    crate::{Client, Server},
    futures_util::poll,
  };

  #[tokio::test]
  async fn cancelling_reserved_send_interrupts_pending_reads() -> Result<()> {
    tokio::time::timeout(Duration::from_secs(5), async {
      let name = format!("taunicorn-send-drop-{}", std::process::id());
      let listener = Server::listen(name.as_str()).await?;
      let (client, peer) = tokio::try_join!(Client::connect(name.as_str()), listener.accept())?;
      let c = generate_identity_private_key()?;
      let s = generate_identity_private_key()?;
      let (client, _peer) = tokio::try_join!(
        SecureConnection::client(client, c, identity_public_key(&s)),
        SecureConnection::server(peer, identity_public_key(&c), s)
      )?;
      // Pause at the transport gate, without filling a socket or writing a partial frame.
      let _gate = client.connection.core.write_gate.lock().await;
      let mut read = Box::pin(client.receive());
      let mut queued = Box::pin(client.receive());
      let mut send = Box::pin(client.send(b"x"));
      assert!(poll!(&mut read).is_pending());
      assert!(poll!(&mut queued).is_pending());
      assert!(poll!(&mut send).is_pending());
      drop(send);
      assert!(client.is_closed());
      // Leave the active read parked: the queued call must wake independently.
      assert!(queued.await.is_err());
      assert!(read.await.is_err());
      Ok(())
    })
    .await?
  }

  #[test]
  fn validate_only_the_length_prefix_for_each_context() {
    for (limit, min, max) in [
      (FrameLimit::Hello, 162_u32, 162_u32),
      (FrameLimit::Finished(CLIENT_FINISHED), 58, 58),
      (FrameLimit::Finished(SERVER_FINISHED), 58, 58),
      (FrameLimit::Application(3), 25, 28),
      (FrameLimit::Application(usize::MAX), 25, 64 * 1024 * 1024),
    ] {
      assert!(limit.checked_size((min - 1).to_be_bytes()).is_err());
      assert_eq!(limit.checked_size(min.to_be_bytes()).unwrap(), min as usize);
      assert_eq!(limit.checked_size(max.to_be_bytes()).unwrap(), max as usize);
      assert!(limit.checked_size((max + 1).to_be_bytes()).is_err());
    }
  }

  #[tokio::test]
  async fn dropping_handshake_disables_retained_transport() -> Result<()> {
    tokio::time::timeout(Duration::from_secs(5), async {
      let name = format!("taunicorn-handshake-drop-{}", std::process::id());
      let server = Server::listen(name.as_str()).await?;
      let (_peer, connection) =
        tokio::try_join!(Client::connect(name.as_str()), server.accept())?;
      let core = Arc::clone(&connection.core);
      let mut handshake =
        Box::pin(SecureConnection::server(connection, [0; 32], generate_identity_private_key()?));
      assert!(poll!(&mut handshake).is_pending());
      drop(handshake);
      assert!(core.state.is_closed());
      assert!(core.close_token.is_cancelled());
      assert!(core.stream.lock().unwrap().is_none());
      Ok(())
    })
    .await?
  }
}
