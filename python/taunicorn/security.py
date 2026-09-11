from __future__ import annotations

import asyncio
import hashlib
import hmac
import os
import struct
from dataclasses import dataclass
from typing import Any

import msgpack
from cryptography.exceptions import InvalidSignature, InvalidTag
from cryptography.hazmat.primitives import hashes, serialization
from cryptography.hazmat.primitives.asymmetric.ed25519 import (
    Ed25519PrivateKey,
    Ed25519PublicKey,
)
from cryptography.hazmat.primitives.asymmetric.x25519 import (
    X25519PrivateKey,
    X25519PublicKey,
)
from cryptography.hazmat.primitives.ciphers.aead import ChaCha20Poly1305
from cryptography.hazmat.primitives.kdf.hkdf import HKDF

from ._taunicorn import Connection

# =============================================================================
# Constants
# =============================================================================

CLIENT_FINISHED = b"taunicorn-e2ee-v2/client-finished"
CLIENT_HELLO = 0x01

DIRECTION_CLIENT_TO_SERVER = 0x01
DIRECTION_SERVER_TO_CLIENT = 0x02

ED25519_PUBLIC_KEY_SIZE = 32
ED25519_SIGNATURE_SIZE = 64

HANDSHAKE_NONCE_SIZE = 32

MAGIC = b"TNE2"

MAX_FRAME_SIZE = 64 * 1024 * 1024

SERVER_FINISHED = b"taunicorn-e2ee-v2/server-finished"
SERVER_HELLO = 0x02

VERSION = 2

X25519_PUBLIC_KEY_SIZE = 32


# =============================================================================
# Handshake Structures
# =============================================================================


@dataclass(frozen=True, slots=True)
class _ClientHello:
    identity_public_key: bytes
    ephemeral_public_key: bytes
    nonce: bytes
    signature: bytes


@dataclass(frozen=True, slots=True)
class _KeyMaterial:
    client_to_server_key: bytes
    client_to_server_nonce_prefix: bytes
    server_to_client_key: bytes
    server_to_client_nonce_prefix: bytes


@dataclass(frozen=True, slots=True)
class _ServerHello:
    identity_public_key: bytes
    ephemeral_public_key: bytes
    nonce: bytes
    signature: bytes


@dataclass(slots=True)
class _DirectionState:
    cipher: ChaCha20Poly1305
    direction: int
    nonce_prefix: bytes
    sequence: int = 0


# =============================================================================
# Identity
# =============================================================================


def generate_identity_private_key() -> bytes:
    """
    Generate a persistent Ed25519 identity private key.

    This key identifies one endpoint across multiple sessions. It must be
    generated once, stored securely, and never transmitted to the peer.

    Returns
    -------
    bytes
        A 32-byte Ed25519 private key.
    """
    private_key = Ed25519PrivateKey.generate()

    return private_key.private_bytes(
        encoding=serialization.Encoding.Raw,
        format=serialization.PrivateFormat.Raw,
        encryption_algorithm=serialization.NoEncryption(),
    )


def identity_public_key(
    identity_private_key: bytes,
) -> bytes:
    """
    Derive the public Ed25519 identity from a persistent private identity key.

    The public key is safe to distribute. The opposite endpoint pins this key
    and uses it to reject connections signed by an unknown identity.
    """
    private_key = _load_identity_private_key(identity_private_key)

    return private_key.public_key().public_bytes(
        encoding=serialization.Encoding.Raw,
        format=serialization.PublicFormat.Raw,
    )


# =============================================================================
# Secure Connection
# =============================================================================


class SecureConnection:
    """
    Authenticated and encrypted message layer over a Taunicorn byte stream.

    Security properties
    -------------------
    - Persistent Ed25519 endpoint identity.
    - Ephemeral X25519 key agreement for every connection.
    - Forward secrecy for session traffic.
    - HKDF-SHA256 session key derivation.
    - ChaCha20-Poly1305 confidentiality and integrity.
    - Independent keys for both traffic directions.
    - Strict per-direction sequence numbers for replay protection.
    - Handshake transcript binding.
    - Explicit encrypted key confirmation.
    """

    def __init__(
        self,
        connection: Connection,
        *,
        receive_state: _DirectionState,
        send_state: _DirectionState,
        transcript_hash: bytes,
    ) -> None:
        """
        Construct an established secure connection.

        This constructor is internal. Call `client()` or `server()` because the
        cryptographic handshake must complete before application traffic is
        permitted.
        """
        self._connection = connection
        self._receive_lock = asyncio.Lock()
        self._receive_state = receive_state
        self._send_lock = asyncio.Lock()
        self._send_state = send_state
        self._transcript_hash = transcript_hash

    # =========================================================================
    # Client
    # =========================================================================

    @classmethod
    async def client(
        cls,
        connection: Connection,
        *,
        identity_private_key: bytes,
        server_identity_public_key: bytes,
    ) -> SecureConnection:
        """
        Establish the client side of an authenticated encrypted session.

        The client proves ownership of its persistent Ed25519 identity by
        signing a fresh ephemeral X25519 public key and handshake nonce.

        The expected server identity is included in the client signature. This
        prevents the signed hello from being valid for another server identity.

        The server then signs the complete handshake state. The client accepts
        the connection only when that signature belongs to the pinned server
        public key.
        """
        _validate_public_key(
            server_identity_public_key,
            "server identity public key",
        )

        identity_private = _load_identity_private_key(identity_private_key)

        client_identity_public = identity_private.public_key().public_bytes(
            encoding=serialization.Encoding.Raw,
            format=serialization.PublicFormat.Raw,
        )

        ephemeral_private = X25519PrivateKey.generate()

        client_ephemeral_public = ephemeral_private.public_key().public_bytes(
            encoding=serialization.Encoding.Raw,
            format=serialization.PublicFormat.Raw,
        )

        client_nonce = os.urandom(HANDSHAKE_NONCE_SIZE)

        signature_message = _client_signature_message(
            client_ephemeral_public=client_ephemeral_public,
            client_identity_public=client_identity_public,
            client_nonce=client_nonce,
            server_identity_public=server_identity_public_key,
        )

        client_signature = identity_private.sign(signature_message)

        await _write_frame(
            connection,
            _encode_client_hello(
                _ClientHello(
                    identity_public_key=client_identity_public,
                    ephemeral_public_key=client_ephemeral_public,
                    nonce=client_nonce,
                    signature=client_signature,
                )
            ),
        )

        server_hello = _decode_server_hello(await _read_frame(connection))

        if not hmac.compare_digest(
            server_hello.identity_public_key,
            server_identity_public_key,
        ):
            raise ConnectionError(
                "server identity does not match the pinned public key"
            )

        server_signature_message = _server_signature_message(
            client_ephemeral_public=client_ephemeral_public,
            client_identity_public=client_identity_public,
            client_nonce=client_nonce,
            server_ephemeral_public=server_hello.ephemeral_public_key,
            server_identity_public=server_hello.identity_public_key,
            server_nonce=server_hello.nonce,
        )

        _verify_signature(
            identity_public_key=server_hello.identity_public_key,
            message=server_signature_message,
            signature=server_hello.signature,
        )

        shared_secret = ephemeral_private.exchange(
            X25519PublicKey.from_public_bytes(server_hello.ephemeral_public_key)
        )

        _validate_shared_secret(shared_secret)

        transcript_hash = _transcript_hash(
            client_ephemeral_public=client_ephemeral_public,
            client_identity_public=client_identity_public,
            client_nonce=client_nonce,
            server_ephemeral_public=server_hello.ephemeral_public_key,
            server_identity_public=server_hello.identity_public_key,
            server_nonce=server_hello.nonce,
        )

        key_material = _derive_key_material(
            shared_secret=shared_secret,
            transcript_hash=transcript_hash,
        )

        secure = cls(
            connection,
            receive_state=_DirectionState(
                cipher=ChaCha20Poly1305(key_material.server_to_client_key),
                direction=DIRECTION_SERVER_TO_CLIENT,
                nonce_prefix=(key_material.server_to_client_nonce_prefix),
            ),
            send_state=_DirectionState(
                cipher=ChaCha20Poly1305(key_material.client_to_server_key),
                direction=DIRECTION_CLIENT_TO_SERVER,
                nonce_prefix=(key_material.client_to_server_nonce_prefix),
            ),
            transcript_hash=transcript_hash,
        )

        server_finished = await secure.receive()

        if not hmac.compare_digest(
            server_finished,
            SERVER_FINISHED,
        ):
            raise ConnectionError("invalid server key confirmation")

        await secure.send(CLIENT_FINISHED)

        return secure

    # =========================================================================
    # Close
    # =========================================================================

    async def close(self) -> None:
        """
        Close the underlying Taunicorn connection.
        """
        await self._connection.close()

    # =========================================================================
    # Connection
    # =========================================================================

    @property
    def connection(self) -> Connection:
        """
        Return the underlying Taunicorn connection.

        Application code normally does not need this because plaintext should
        not bypass this secure layer after the handshake.
        """
        return self._connection

    # =========================================================================
    # Receive
    # =========================================================================

    async def receive(self) -> bytes:
        """
        Receive, authenticate, replay-check, and decrypt one complete message.

        The encrypted frame contains a protocol version, a monotonically
        increasing sequence number, and a ChaCha20-Poly1305 ciphertext.

        The direction, sequence number, protocol version, and complete
        handshake transcript hash are authenticated as AEAD associated data.
        """
        async with self._receive_lock:
            state = self._receive_state

            frame = await _read_frame(self._connection)

            if len(frame) < 25:
                raise ConnectionError("encrypted frame is too short")

            version = frame[0]

            if version != VERSION:
                raise ConnectionError(f"unsupported secure protocol version: {version}")

            sequence_bytes = frame[1:9]

            sequence = int.from_bytes(
                sequence_bytes,
                "big",
            )

            if sequence != state.sequence:
                raise ConnectionError(
                    "invalid message sequence: "
                    f"expected {state.sequence}, "
                    f"received {sequence}"
                )

            nonce = _make_nonce(
                nonce_prefix=state.nonce_prefix,
                sequence=sequence_bytes,
            )

            aad = _make_aad(
                direction=state.direction,
                sequence=sequence_bytes,
                transcript_hash=self._transcript_hash,
            )

            try:
                plaintext = state.cipher.decrypt(
                    nonce,
                    frame[9:],
                    aad,
                )
            except InvalidTag as exc:
                raise ConnectionError("message authentication failed") from exc

            state.sequence += 1

            return plaintext

    async def receive_msgpack(self) -> Any:
        """
        Receive one encrypted message and decode its plaintext as MessagePack.
        """
        plaintext = await self.receive()

        return msgpack.unpackb(
            plaintext,
            raw=False,
        )

    # =========================================================================
    # Send
    # =========================================================================

    async def send(
        self,
        plaintext: bytes,
    ) -> None:
        """
        Encrypt and authenticate one complete application message.

        A unique nonce is derived from the direction-specific random prefix and
        the monotonically increasing 64-bit sequence number. The sequence is
        incremented only after the complete encrypted frame was sent.
        """
        async with self._send_lock:
            state = self._send_state

            if state.sequence == 2**64 - 1:
                raise OverflowError("secure session sequence exhausted")

            sequence_bytes = state.sequence.to_bytes(
                8,
                "big",
            )

            nonce = _make_nonce(
                nonce_prefix=state.nonce_prefix,
                sequence=sequence_bytes,
            )

            aad = _make_aad(
                direction=state.direction,
                sequence=sequence_bytes,
                transcript_hash=self._transcript_hash,
            )

            ciphertext = state.cipher.encrypt(
                nonce,
                plaintext,
                aad,
            )

            frame = bytes([VERSION]) + sequence_bytes + ciphertext

            await _write_frame(
                self._connection,
                frame,
            )

            state.sequence += 1

    async def send_msgpack(
        self,
        value: Any,
    ) -> None:
        """
        Encode an object as MessagePack and send it as one encrypted message.
        """
        plaintext = msgpack.packb(
            value,
            use_bin_type=True,
        )

        await self.send(plaintext)

    # =========================================================================
    # Server
    # =========================================================================

    @classmethod
    async def server(
        cls,
        connection: Connection,
        *,
        client_identity_public_key: bytes,
        identity_private_key: bytes,
    ) -> SecureConnection:
        """
        Establish the server side of an authenticated encrypted session.

        The server accepts only the configured client Ed25519 identity. It
        verifies that the client signed its ephemeral X25519 key, fresh nonce,
        and this server's identity.

        The server then creates its own ephemeral X25519 key and signs the
        complete handshake state so the client can authenticate the server.
        """
        _validate_public_key(
            client_identity_public_key,
            "client identity public key",
        )

        identity_private = _load_identity_private_key(identity_private_key)

        server_identity_public = identity_private.public_key().public_bytes(
            encoding=serialization.Encoding.Raw,
            format=serialization.PublicFormat.Raw,
        )

        client_hello = _decode_client_hello(await _read_frame(connection))

        if not hmac.compare_digest(
            client_hello.identity_public_key,
            client_identity_public_key,
        ):
            raise ConnectionError(
                "client identity does not match the pinned public key"
            )

        client_signature_message = _client_signature_message(
            client_ephemeral_public=client_hello.ephemeral_public_key,
            client_identity_public=client_hello.identity_public_key,
            client_nonce=client_hello.nonce,
            server_identity_public=server_identity_public,
        )

        _verify_signature(
            identity_public_key=client_hello.identity_public_key,
            message=client_signature_message,
            signature=client_hello.signature,
        )

        ephemeral_private = X25519PrivateKey.generate()

        server_ephemeral_public = ephemeral_private.public_key().public_bytes(
            encoding=serialization.Encoding.Raw,
            format=serialization.PublicFormat.Raw,
        )

        server_nonce = os.urandom(HANDSHAKE_NONCE_SIZE)

        server_signature_message = _server_signature_message(
            client_ephemeral_public=client_hello.ephemeral_public_key,
            client_identity_public=client_hello.identity_public_key,
            client_nonce=client_hello.nonce,
            server_ephemeral_public=server_ephemeral_public,
            server_identity_public=server_identity_public,
            server_nonce=server_nonce,
        )

        server_signature = identity_private.sign(server_signature_message)

        await _write_frame(
            connection,
            _encode_server_hello(
                _ServerHello(
                    identity_public_key=server_identity_public,
                    ephemeral_public_key=server_ephemeral_public,
                    nonce=server_nonce,
                    signature=server_signature,
                )
            ),
        )

        shared_secret = ephemeral_private.exchange(
            X25519PublicKey.from_public_bytes(client_hello.ephemeral_public_key)
        )

        _validate_shared_secret(shared_secret)

        transcript_hash = _transcript_hash(
            client_ephemeral_public=client_hello.ephemeral_public_key,
            client_identity_public=client_hello.identity_public_key,
            client_nonce=client_hello.nonce,
            server_ephemeral_public=server_ephemeral_public,
            server_identity_public=server_identity_public,
            server_nonce=server_nonce,
        )

        key_material = _derive_key_material(
            shared_secret=shared_secret,
            transcript_hash=transcript_hash,
        )

        secure = cls(
            connection,
            receive_state=_DirectionState(
                cipher=ChaCha20Poly1305(key_material.client_to_server_key),
                direction=DIRECTION_CLIENT_TO_SERVER,
                nonce_prefix=(key_material.client_to_server_nonce_prefix),
            ),
            send_state=_DirectionState(
                cipher=ChaCha20Poly1305(key_material.server_to_client_key),
                direction=DIRECTION_SERVER_TO_CLIENT,
                nonce_prefix=(key_material.server_to_client_nonce_prefix),
            ),
            transcript_hash=transcript_hash,
        )

        await secure.send(SERVER_FINISHED)

        client_finished = await secure.receive()

        if not hmac.compare_digest(
            client_finished,
            CLIENT_FINISHED,
        ):
            raise ConnectionError("invalid client key confirmation")

        return secure


# =============================================================================
# Authentication Helpers
# =============================================================================


def _client_signature_message(
    *,
    client_ephemeral_public: bytes,
    client_identity_public: bytes,
    client_nonce: bytes,
    server_identity_public: bytes,
) -> bytes:
    """
    Build the exact byte string signed by the client.

    Including the expected server identity prevents a client authentication
    signature from being redirected to another server identity.
    """
    return (
        MAGIC
        + b"/client-auth/"
        + bytes([VERSION])
        + client_identity_public
        + server_identity_public
        + client_ephemeral_public
        + client_nonce
    )


def _server_signature_message(
    *,
    client_ephemeral_public: bytes,
    client_identity_public: bytes,
    client_nonce: bytes,
    server_ephemeral_public: bytes,
    server_identity_public: bytes,
    server_nonce: bytes,
) -> bytes:
    """
    Build the exact byte string signed by the server.

    This binds both identities, both ephemeral keys, and both fresh nonces to
    one authenticated handshake.
    """
    return (
        MAGIC
        + b"/server-auth/"
        + bytes([VERSION])
        + client_identity_public
        + server_identity_public
        + client_ephemeral_public
        + server_ephemeral_public
        + client_nonce
        + server_nonce
    )


def _verify_signature(
    *,
    identity_public_key: bytes,
    message: bytes,
    signature: bytes,
) -> None:
    """
    Verify peer possession of the expected persistent Ed25519 identity key.
    """
    try:
        Ed25519PublicKey.from_public_bytes(identity_public_key).verify(
            signature,
            message,
        )
    except InvalidSignature as exc:
        raise ConnectionError("peer identity signature verification failed") from exc


# =============================================================================
# Encoding Helpers
# =============================================================================


def _decode_client_hello(
    data: bytes,
) -> _ClientHello:
    """
    Parse and validate the fixed-size client handshake frame.
    """
    expected_size = (
        2
        + ED25519_PUBLIC_KEY_SIZE
        + X25519_PUBLIC_KEY_SIZE
        + HANDSHAKE_NONCE_SIZE
        + ED25519_SIGNATURE_SIZE
    )

    if len(data) != expected_size:
        raise ConnectionError("invalid client hello size")

    if data[0] != CLIENT_HELLO:
        raise ConnectionError("invalid client hello type")

    if data[1] != VERSION:
        raise ConnectionError("invalid client hello version")

    return _ClientHello(
        identity_public_key=data[2:34],
        ephemeral_public_key=data[34:66],
        nonce=data[66:98],
        signature=data[98:162],
    )


def _decode_server_hello(
    data: bytes,
) -> _ServerHello:
    """
    Parse and validate the fixed-size server handshake frame.
    """
    expected_size = (
        2
        + ED25519_PUBLIC_KEY_SIZE
        + X25519_PUBLIC_KEY_SIZE
        + HANDSHAKE_NONCE_SIZE
        + ED25519_SIGNATURE_SIZE
    )

    if len(data) != expected_size:
        raise ConnectionError("invalid server hello size")

    if data[0] != SERVER_HELLO:
        raise ConnectionError("invalid server hello type")

    if data[1] != VERSION:
        raise ConnectionError("invalid server hello version")

    return _ServerHello(
        identity_public_key=data[2:34],
        ephemeral_public_key=data[34:66],
        nonce=data[66:98],
        signature=data[98:162],
    )


def _encode_client_hello(
    hello: _ClientHello,
) -> bytes:
    """
    Encode the client handshake into a deterministic binary representation.

    A fixed binary representation is used so Python and Rust sign exactly the
    same bytes without depending on serializer implementation details.
    """
    return (
        bytes(
            [
                CLIENT_HELLO,
                VERSION,
            ]
        )
        + hello.identity_public_key
        + hello.ephemeral_public_key
        + hello.nonce
        + hello.signature
    )


def _encode_server_hello(
    hello: _ServerHello,
) -> bytes:
    """
    Encode the server handshake into the deterministic binary wire format.
    """
    return (
        bytes(
            [
                SERVER_HELLO,
                VERSION,
            ]
        )
        + hello.identity_public_key
        + hello.ephemeral_public_key
        + hello.nonce
        + hello.signature
    )


# =============================================================================
# Framing Helpers
# =============================================================================


async def _read_exact(
    connection: Connection,
    size: int,
) -> bytes:
    """
    Reassemble exactly `size` bytes from the byte-oriented transport.

    Taunicorn receive operations may return arbitrary stream chunks, so one
    encrypted application message cannot rely on one receive call.
    """
    result = bytearray()

    while len(result) < size:
        chunk = await connection.receive(size - len(result))

        if not chunk:
            raise EOFError("connection closed while reading a frame")

        result.extend(chunk)

    return bytes(result)


async def _read_frame(
    connection: Connection,
) -> bytes:
    """
    Read one length-prefixed protocol frame.
    """
    header = await _read_exact(
        connection,
        4,
    )

    size = struct.unpack(
        ">I",
        header,
    )[0]

    if size == 0:
        raise ConnectionError("zero-length frame is invalid")

    if size > MAX_FRAME_SIZE:
        raise ConnectionError(f"frame exceeds maximum size: {size}")

    return await _read_exact(
        connection,
        size,
    )


async def _write_frame(
    connection: Connection,
    payload: bytes,
) -> None:
    """
    Prefix one protocol frame with an unsigned 32-bit big-endian length.

    The length is intentionally outside encryption because the receiver needs
    it to reassemble the stream. Message integrity is provided by the encrypted
    frame itself.
    """
    if not payload:
        raise ValueError("zero-length frame is invalid")

    if len(payload) > MAX_FRAME_SIZE:
        raise ValueError("frame exceeds maximum size")

    await connection.send(
        struct.pack(
            ">I",
            len(payload),
        )
        + payload
    )


# =============================================================================
# Key Derivation Helpers
# =============================================================================


def _derive_key_material(
    *,
    shared_secret: bytes,
    transcript_hash: bytes,
) -> _KeyMaterial:
    """
    Derive independent encryption keys and nonce prefixes for both directions.

    Separate directional keys prevent accidental key/nonce reuse between
    client-to-server and server-to-client traffic.
    """
    output = HKDF(
        algorithm=hashes.SHA256(),
        length=72,
        salt=transcript_hash,
        info=b"taunicorn-e2ee-v2/session-keys",
    ).derive(shared_secret)

    return _KeyMaterial(
        client_to_server_key=output[0:32],
        server_to_client_key=output[32:64],
        client_to_server_nonce_prefix=output[64:68],
        server_to_client_nonce_prefix=output[68:72],
    )


def _transcript_hash(
    *,
    client_ephemeral_public: bytes,
    client_identity_public: bytes,
    client_nonce: bytes,
    server_ephemeral_public: bytes,
    server_identity_public: bytes,
    server_nonce: bytes,
) -> bytes:
    """
    Hash all handshake identities, ephemeral keys, and freshness values.

    The hash is used both by HKDF and every encrypted message's AEAD associated
    data, binding application traffic to exactly one authenticated handshake.
    """
    return hashlib.sha256(
        MAGIC
        + b"/transcript/"
        + bytes([VERSION])
        + client_identity_public
        + server_identity_public
        + client_ephemeral_public
        + server_ephemeral_public
        + client_nonce
        + server_nonce
    ).digest()


# =============================================================================
# Message Encryption Helpers
# =============================================================================


def _make_aad(
    *,
    direction: int,
    sequence: bytes,
    transcript_hash: bytes,
) -> bytes:
    """
    Build authenticated but unencrypted message metadata.

    Modifying the version, direction, sequence number, or handshake association
    therefore causes ChaCha20-Poly1305 authentication to fail.
    """
    return (
        MAGIC
        + bytes(
            [
                VERSION,
                direction,
            ]
        )
        + sequence
        + transcript_hash
    )


def _make_nonce(
    *,
    nonce_prefix: bytes,
    sequence: bytes,
) -> bytes:
    """
    Build the 96-bit ChaCha20-Poly1305 nonce.

    Four session-specific bytes are combined with the 64-bit sequence number.
    A fresh session derives a fresh prefix, and every message increments the
    sequence, making nonces unique for one directional key.
    """
    if len(nonce_prefix) != 4:
        raise ValueError("nonce prefix must contain four bytes")

    if len(sequence) != 8:
        raise ValueError("sequence must contain eight bytes")

    return nonce_prefix + sequence


# =============================================================================
# Validation Helpers
# =============================================================================


def _load_identity_private_key(
    value: bytes,
) -> Ed25519PrivateKey:
    """
    Load and validate a persistent 32-byte Ed25519 private identity key.
    """
    if len(value) != 32:
        raise ValueError("Ed25519 private identity key must contain exactly 32 bytes")

    return Ed25519PrivateKey.from_private_bytes(value)


def _validate_public_key(
    value: bytes,
    name: str,
) -> None:
    """
    Reject malformed persistent Ed25519 public keys before the handshake.
    """
    if len(value) != 32:
        raise ValueError(f"{name} must contain exactly 32 bytes")


def _validate_shared_secret(
    shared_secret: bytes,
) -> None:
    """
    Reject a non-contributory X25519 exchange.

    An all-zero shared secret must never be accepted as session key material.
    """
    if hmac.compare_digest(
        shared_secret,
        b"\x00" * 32,
    ):
        raise ConnectionError("invalid X25519 shared secret")
