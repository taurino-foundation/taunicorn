"""Coroutine-compatible facade over Taunicorn's native secure connection.

All cryptography and protocol processing run in the existing Rust implementation.
MessagePack is optional and is only imported when its convenience methods are used.
"""

from __future__ import annotations

from typing import Any

from ._taunicorn import Connection
from ._taunicorn import SecureConnection as _NativeSecureConnection
from ._taunicorn import generate_identity_private_key, identity_public_key

__all__ = [
    "SecureConnection",
    "generate_identity_private_key",
    "identity_public_key",
]


class SecureConnection:
    """Authenticated encrypted messages, with Python's existing async API.

    Create instances with ``await client(...)`` or ``await server(...)``.
    The raw connection is exclusively transferred to the existing Rust secure
    session. After success, ``connection`` still returns the original Python
    object for diagnostics and closing; raw reads/writes and splitting are disabled.

    Cancelling an operation or encountering a native protocol/I/O error makes the
    session terminal. Reconnect and perform a new handshake instead of retrying.
    """

    _native: _NativeSecureConnection

    def __init__(self) -> None:
        raise TypeError("use await SecureConnection.client(...) or .server(...)")

    @classmethod
    def _from_native(cls, native: _NativeSecureConnection) -> SecureConnection:
        result = cls.__new__(cls)
        result._native = native
        return result

    @classmethod
    async def client(
        cls,
        connection: Connection,
        *,
        identity_private_key: bytes,
        server_identity_public_key: bytes,
    ) -> SecureConnection:
        native = await _NativeSecureConnection.client(
            connection,
            identity_private_key=identity_private_key,
            server_identity_public_key=server_identity_public_key,
        )
        return cls._from_native(native)

    @classmethod
    async def server(
        cls,
        connection: Connection,
        *,
        client_identity_public_key: bytes,
        identity_private_key: bytes,
    ) -> SecureConnection:
        native = await _NativeSecureConnection.server(
            connection,
            client_identity_public_key=client_identity_public_key,
            identity_private_key=identity_private_key,
        )
        return cls._from_native(native)

    @property
    def connection(self) -> Connection:
        return self._native.connection

    async def send(self, plaintext: bytes) -> None:
        await self._native.send(plaintext)

    async def receive(self) -> bytes:
        return await self._native.receive()

    async def close(self) -> None:
        await self._native.close()

    def is_closed(self) -> bool:
        return self._native.is_closed()

    async def send_msgpack(self, value: Any) -> None:
        import msgpack

        # Preserve the original Python mapping (especially bytes -> MessagePack bin).
        plaintext = msgpack.packb(value, use_bin_type=True)
        await self.send(plaintext)

    async def receive_msgpack(self) -> Any:
        import msgpack

        # Import first so a missing optional dependency does not consume a message.
        plaintext = await self.receive()
        return msgpack.unpackb(plaintext, raw=False)
