"""
Type declarations for Taunicorn's native local IPC transport.

The module exposes asynchronous, cross-platform local byte-stream
communication backed by the native Taunicorn Rust transport.

Python owns the asyncio event loop. Native I/O is executed by the Rust
runtime and exposed to Python as awaitable operations.

The transport is byte-oriented. It does not provide message framing,
serialization, authentication, routing, or application-level acknowledgements.
"""

import sys
from collections.abc import Awaitable, Sequence
from typing import TypeAlias, final


__version__: str


# ---------------------------------------------------------------------------
# Internal typing helpers
# ---------------------------------------------------------------------------

_EndpointLike: TypeAlias = str | "SocketEndpoint"

# PyO3 extracts Vec[u8] from bytes, bytearray, and compatible integer
# sequences. Individual integer elements must fit into the unsigned-byte
# range accepted by the native binding.
_BytesLike: TypeAlias = bytes | bytearray | memoryview | Sequence[int]


# ---------------------------------------------------------------------------
# Endpoint
# ---------------------------------------------------------------------------


@final
class SocketEndpoint:
    """
    Logical name of a local IPC endpoint.

    An endpoint is an opaque cross-platform identifier. The native transport
    maps the name to the platform-specific local IPC mechanism.

    Parameters
    ----------
    name:
        Non-empty endpoint name.

    Raises
    ------
    ValueError
        If ``name`` is empty.
    """

    def __init__(self, name: str) -> None: ...

    @property
    def name(self) -> str:
        """Return the endpoint name."""
        ...

    def __str__(self) -> str:
        """Return the endpoint name."""
        ...

    def __repr__(self) -> str:
        """Return a diagnostic representation of the endpoint."""
        ...


# ---------------------------------------------------------------------------
# Diagnostic snapshots
# ---------------------------------------------------------------------------


@final
class SocketConnectionInfo:
    """
    Immutable diagnostic snapshot of a socket connection.

    The values describe the connection state at the time the snapshot was
    created. They are not live views of the connection.
    """

    @property
    def id(self) -> int:
        """Return the unique native connection identifier."""
        ...

    @property
    def local_endpoint(self) -> str | None:
        """Return the local endpoint name, if known."""
        ...

    @property
    def peer_endpoint(self) -> str | None:
        """Return the peer endpoint name, if known."""
        ...

    @property
    def is_closed(self) -> bool:
        """Return whether the complete connection has been closed."""
        ...

    @property
    def is_read_shutdown(self) -> bool:
        """Return whether the local receive direction has been shut down."""
        ...

    @property
    def is_write_shutdown(self) -> bool:
        """Return whether the local send direction has been shut down."""
        ...

    @property
    def peer_sent_eof(self) -> bool:
        """Return whether EOF has been observed from the peer."""
        ...

    def __repr__(self) -> str:
        """Return a diagnostic representation of the snapshot."""
        ...


@final
class SocketServerInfo:
    """
    Immutable diagnostic snapshot of a socket server.

    The values describe the server state at the time the snapshot was
    created.
    """

    @property
    def local_endpoint(self) -> str:
        """Return the endpoint on which the server is listening."""
        ...

    @property
    def is_stopped(self) -> bool:
        """Return whether the server has been stopped."""
        ...

    @property
    def is_paused(self) -> bool:
        """Return whether accepting new connections is paused."""
        ...

    def __repr__(self) -> str:
        """Return a diagnostic representation of the snapshot."""
        ...


# ---------------------------------------------------------------------------
# Server
# ---------------------------------------------------------------------------


@final
class SocketServer:
    """
    Asynchronous local IPC server.

    A server owns a local endpoint and accepts full-duplex byte-stream
    connections.

    Instances are created with :meth:`start` or :meth:`bind`.
    """

    @staticmethod
    def start(endpoint: _EndpointLike) -> Awaitable["SocketServer"]:
        """
        Start a server using the platform's default security settings.

        Parameters
        ----------
        endpoint:
            Endpoint name or existing :class:`SocketEndpoint`.

        Returns
        -------
        Awaitable[SocketServer]
            Awaitable resolving to the started server.

        Raises
        ------
        ValueError
            If the endpoint name is empty.
        OSError
            If the native endpoint cannot be created.
        """
        ...

    if sys.platform == "win32":

        @staticmethod
        def bind(
            name: str,
            *,
            mode: str | None = ...,
        ) -> Awaitable["SocketServer"]:
            """
            Bind a Windows local socket with optional security configuration.

            Parameters
            ----------
            name:
                Endpoint name.
            mode:
                Optional Windows security descriptor string. ``None`` uses
                the platform default.

            Returns
            -------
            Awaitable[SocketServer]
                Awaitable resolving to the bound server.
            """
            ...

    else:

        @staticmethod
        def bind(
            name: str,
            *,
            mode: int | None = ...,
        ) -> Awaitable["SocketServer"]:
            """
            Bind a Unix local socket with optional permission bits.

            Parameters
            ----------
            name:
                Endpoint name.
            mode:
                Optional Unix permission mode. ``None`` uses the platform
                default.

            Returns
            -------
            Awaitable[SocketServer]
                Awaitable resolving to the bound server.
            """
            ...

    def accept(self) -> Awaitable["SocketConnection"]:
        """
        Wait for and accept the next connection.

        Returns
        -------
        Awaitable[SocketConnection]
            Awaitable resolving to the accepted connection.

        Raises
        ------
        BlockingIOError
            If the server is paused.
        ConnectionError
            If the server has been stopped.
        OSError
            If the underlying accept operation fails.
        """
        ...

    def accept_timeout(self, timeout: float) -> Awaitable["SocketConnection"]:
        """
        Accept a connection within a timeout.

        Parameters
        ----------
        timeout:
            Maximum number of seconds to wait. Must be finite,
            non-negative, and representable by the native duration type.

        Raises
        ------
        TimeoutError
            If no connection is accepted before the timeout.
        ValueError
            If ``timeout`` is invalid.
        """
        ...

    def stop(self) -> Awaitable[None]:
        """
        Stop accepting new connections.

        Pending native accept operations are interrupted. Existing accepted
        connections remain independent from the server.
        """
        ...

    def close(self) -> None:
        """
        Stop the server synchronously.

        This is the synchronous compatibility form of :meth:`stop`.
        """
        ...

    def pause(self) -> None:
        """Pause acceptance of new connections."""
        ...

    def resume(self) -> None:
        """Resume acceptance of new connections."""
        ...

    def is_paused(self) -> bool:
        """Return whether accepting connections is paused."""
        ...

    def is_stopped(self) -> bool:
        """Return whether the server has been stopped."""
        ...

    def is_closed(self) -> bool:
        """Return whether the server has been stopped."""
        ...

    def is_started(self) -> bool:
        """Return whether the server has not been stopped."""
        ...

    def is_accepting(self) -> bool:
        """
        Return whether the server can currently accept connections.

        This is true only while the server is started and not paused.
        """
        ...

    @property
    def name(self) -> str:
        """Return the server's endpoint name."""
        ...

    @property
    def endpoint(self) -> SocketEndpoint:
        """Return the server's endpoint object."""
        ...

    def info(self) -> SocketServerInfo:
        """Return an immutable snapshot of the current server state."""
        ...

    def __repr__(self) -> str:
        """Return a diagnostic representation of the server."""
        ...


# ---------------------------------------------------------------------------
# Connection
# ---------------------------------------------------------------------------


@final
class SocketConnection:
    """
    Ordered, asynchronous, full-duplex local byte-stream connection.

    Receive and send operations may progress concurrently. Operations in the
    same direction are serialized by the native transport so complete sends
    do not interleave their bytes.

    The connection supports independent read and write shutdown.
    """

    @staticmethod
    def connect(endpoint: _EndpointLike) -> Awaitable["SocketConnection"]:
        """
        Connect to a local IPC server.

        Parameters
        ----------
        endpoint:
            Endpoint name or existing :class:`SocketEndpoint`.

        Returns
        -------
        Awaitable[SocketConnection]
            Awaitable resolving to the established connection.
        """
        ...

    @staticmethod
    def connect_timeout(
        endpoint: _EndpointLike,
        timeout: float,
    ) -> Awaitable["SocketConnection"]:
        """
        Connect to a local IPC server within a timeout.

        Parameters
        ----------
        endpoint:
            Endpoint name or existing :class:`SocketEndpoint`.
        timeout:
            Maximum number of seconds to wait.

        Raises
        ------
        TimeoutError
            If the connection cannot be established before the timeout.
        ValueError
            If ``timeout`` is invalid.
        """
        ...

    def receive(self, max_bytes: int) -> Awaitable[bytes]:
        """
        Receive up to ``max_bytes`` bytes.

        ``b""`` represents EOF when ``max_bytes`` is non-zero. Calling the
        method with ``max_bytes == 0`` also returns ``b""`` immediately;
        use :meth:`at_eof` when that distinction matters.

        Parameters
        ----------
        max_bytes:
            Maximum number of bytes to receive.
        """
        ...

    def read(self, max_bytes: int) -> Awaitable[bytes]:
        """Alias of :meth:`receive`."""
        ...

    def read_bytes(self, max_bytes: int) -> Awaitable[bytes]:
        """Compatibility alias of :meth:`receive`."""
        ...

    def read_timeout(
        self,
        max_bytes: int,
        timeout: float,
    ) -> Awaitable[bytes]:
        """
        Receive bytes within a timeout.

        Raises
        ------
        TimeoutError
            If the operation does not complete before ``timeout``.
        ValueError
            If ``timeout`` is invalid.
        """
        ...

    def send(self, data: _BytesLike) -> Awaitable[None]:
        """
        Send the complete byte sequence.

        The awaitable resolves only after the complete input has been passed
        to the native transport.

        Cancellation is significant: if Python cancels an in-progress send,
        the connection is closed because a prefix of the buffer may already
        have been written and blindly retrying the entire input could
        duplicate data.
        """
        ...

    def write(self, data: _BytesLike) -> Awaitable[int]:
        """
        Perform one possibly partial write.

        Returns
        -------
        Awaitable[int]
            Awaitable resolving to the number of bytes written.

        Notes
        -----
        If Python cancels the operation, the connection is closed because
        partial progress cannot safely be reported to the cancelled caller.
        """
        ...

    def write_bytes(self, data: _BytesLike) -> Awaitable[int]:
        """Compatibility alias of :meth:`write`."""
        ...

    def write_all(self, data: _BytesLike) -> Awaitable[None]:
        """Alias of :meth:`send`."""
        ...

    def write_all_bytes(self, data: _BytesLike) -> Awaitable[None]:
        """Compatibility alias of :meth:`send`."""
        ...

    def write_all_timeout(
        self,
        data: _BytesLike,
        timeout: float,
    ) -> Awaitable[None]:
        """
        Send the complete byte sequence within a timeout.

        If the timeout occurs after partial write progress, the native
        connection is closed rather than leaving the caller with ambiguous
        replay semantics.
        """
        ...

    def flush(self) -> Awaitable[None]:
        """
        Flush locally buffered output.

        Completion is not an acknowledgement that the peer has processed
        the transmitted bytes.
        """
        ...

    def shutdown_read(self) -> Awaitable[None]:
        """
        Shut down the local receive direction.

        The send direction remains independently usable unless it has also
        been shut down or the complete connection has been closed.
        """
        ...

    def shutdown_write(self) -> Awaitable[None]:
        """
        Gracefully shut down the local send direction.

        The receive direction remains independently usable.
        """
        ...

    def close(self) -> Awaitable[None]:
        """
        Close both directions of the connection.

        Pending local native I/O is interrupted. Repeated calls are safe.
        """
        ...

    def into_split(self) -> tuple["SocketReadHalf", "SocketWriteHalf"]:
        """
        Consume this connection wrapper and return independent direction halves.

        Returns
        -------
        tuple[SocketReadHalf, SocketWriteHalf]
            Read and write halves sharing the same native connection state.

        Raises
        ------
        RuntimeError
            If the connection was already split or an asynchronous operation
            still holds the connection.

        Notes
        -----
        After a successful split, this :class:`SocketConnection` wrapper is
        consumed and can no longer be used.
        """
        ...

    def is_closed(self) -> bool:
        """Return whether the complete connection is closed."""
        ...

    def is_read_shutdown(self) -> bool:
        """Return whether local receiving has been shut down."""
        ...

    def is_write_shutdown(self) -> bool:
        """Return whether local sending has been shut down."""
        ...

    def peer_sent_eof(self) -> bool:
        """Return whether EOF has been observed from the peer."""
        ...

    def at_eof(self) -> bool:
        """Return whether EOF has been observed from the peer."""
        ...

    def is_started(self) -> bool:
        """Return whether the connection has not been closed."""
        ...

    def is_active(self) -> bool:
        """Return whether the connection has not been closed."""
        ...

    def is_available(self) -> bool:
        """Return whether the connection is active and not paused."""
        ...

    def is_paused(self) -> bool:
        """Return whether local I/O is paused."""
        ...

    def pause(self) -> None:
        """Pause local connection I/O."""
        ...

    def resume(self) -> None:
        """Resume local connection I/O."""
        ...

    @property
    def id(self) -> int:
        """Return the unique native connection identifier."""
        ...

    @property
    def name(self) -> str:
        """Return the connection endpoint name."""
        ...

    @property
    def endpoint(self) -> SocketEndpoint:
        """Return the logical endpoint associated with the connection."""
        ...

    @property
    def local_endpoint(self) -> str | None:
        """Return the local endpoint name, if known."""
        ...

    @property
    def peer_endpoint(self) -> str | None:
        """Return the peer endpoint name, if known."""
        ...

    def info(self) -> SocketConnectionInfo:
        """Return an immutable snapshot of the connection state."""
        ...

    def __repr__(self) -> str:
        """Return a diagnostic representation of the connection."""
        ...


# ---------------------------------------------------------------------------
# Split connection halves
# ---------------------------------------------------------------------------


@final
class SocketReadHalf:
    """
    Receive half of a split :class:`SocketConnection`.

    Instances are returned by :meth:`SocketConnection.into_split`.
    """

    def receive(self, max_bytes: int) -> Awaitable[bytes]:
        """
        Receive up to ``max_bytes`` bytes.

        ``b""`` represents EOF for non-zero ``max_bytes``.
        """
        ...

    def read(self, max_bytes: int) -> Awaitable[bytes]:
        """Alias of :meth:`receive`."""
        ...

    def shutdown_read(self) -> Awaitable[None]:
        """Shut down the local receive direction."""
        ...

    @property
    def id(self) -> int:
        """Return the shared native connection identifier."""
        ...

    def info(self) -> SocketConnectionInfo:
        """Return an immutable snapshot of the shared connection state."""
        ...

    def __repr__(self) -> str:
        """Return a diagnostic representation of the read half."""
        ...


@final
class SocketWriteHalf:
    """
    Send half of a split :class:`SocketConnection`.

    Instances are returned by :meth:`SocketConnection.into_split`.
    """

    def send(self, data: _BytesLike) -> Awaitable[None]:
        """
        Send the complete byte sequence.

        Cancellation before completion causes the local write direction to
        be shut down so ambiguous partial writes cannot be blindly replayed.
        """
        ...

    def write(self, data: _BytesLike) -> Awaitable[int]:
        """
        Perform one possibly partial write.

        Returns
        -------
        Awaitable[int]
            Awaitable resolving to the number of bytes written.
        """
        ...

    def flush(self) -> Awaitable[None]:
        """Flush locally buffered output."""
        ...

    def shutdown_write(self) -> Awaitable[None]:
        """Gracefully shut down the local send direction."""
        ...

    @property
    def id(self) -> int:
        """Return the shared native connection identifier."""
        ...

    def info(self) -> SocketConnectionInfo:
        """Return an immutable snapshot of the shared connection state."""
        ...

    def __repr__(self) -> str:
        """Return a diagnostic representation of the write half."""
        ...


# ---------------------------------------------------------------------------
# Transport namespace
# ---------------------------------------------------------------------------


@final
class LocalSocketTransport:
    """
    Namespace for creating local IPC servers and connections.

    This class contains only static convenience methods; transport state is
    held by :class:`SocketServer` and :class:`SocketConnection`.
    """

    @staticmethod
    def start(endpoint: _EndpointLike) -> Awaitable[SocketServer]:
        """Start a local IPC server."""
        ...

    @staticmethod
    def connect(endpoint: _EndpointLike) -> Awaitable[SocketConnection]:
        """Connect to a local IPC server."""
        ...


# ---------------------------------------------------------------------------
# Backwards-compatible public aliases
# ---------------------------------------------------------------------------

SocketListener: TypeAlias = SocketServer
SocketStream: TypeAlias = SocketConnection
SocketClient: TypeAlias = SocketConnection


__all__ = [
    "__version__",
    "SocketEndpoint",
    "SocketConnectionInfo",
    "SocketServerInfo",
    "SocketServer",
    "SocketConnection",
    "SocketReadHalf",
    "SocketWriteHalf",
    "LocalSocketTransport",
    "SocketListener",
    "SocketStream",
    "SocketClient",
]