import typing
from typing import Any, Callable, Optional

class Sender:
    """Handle for sending data to the connected peer."""

    async def send(self, data: bytes) -> typing.Awaitable[None]:
        """
        Asynchronously send data to the connected peer.

        Args:
            data: The bytes to send.

        Returns:
            Awaitable that resolves when the data has been queued for sending.

        Raises:
            RuntimeError: If the connection is closed or sending fails.
        """
        ...

    def try_send(self, data: bytes) -> bool:
        """
        Try to send data without blocking.

        Args:
            data: The bytes to send.

        Returns:
            True if the data was queued successfully, False if the buffer is full.
        """
        ...

class Receiver:
    """Handle for receiving data from the connected peer."""

    async def recv(self) -> typing.Awaitable[Optional[bytes]]:
        """
        Asynchronously receive data from the connected peer.

        Returns:
            Awaitable that resolves to bytes received, or None if the connection is closed.
        """
        ...

    def try_recv(self) -> Optional[bytes]:
        """
        Try to receive data without blocking.

        Returns:
            Bytes received, or None if no data is available.
        """
        ...

async def connect(path: str, handler: Callable[[Sender, Receiver], Any]) -> typing.Awaitable[None]:
    """
    Connect to a local socket server.

    Args:
        path: The filesystem path (Unix domain socket) or named pipe name (Windows).
        handler: Callback function that receives Sender and Receiver objects.
                 Called when connection is established.

    Returns:
        Awaitable that resolves when the connection is established and handler completes.

    Raises:
        ValueError: If the path is invalid.
        RuntimeError: If connection fails.
    """
    ...

async def server(
    path: str,
    handler: Callable[[Sender, Receiver], Any],
    sddl: Optional[str] = None
) -> typing.Awaitable[None]:
    """
    Start a local socket server and handle incoming connections.

    Args:
        path: The filesystem path (Unix domain socket) or named pipe name (Windows).
        handler: Callback function that receives Sender and Receiver objects for each connection.
        sddl: Windows-only: Security descriptor in SDDL format for the named pipe.

    Returns:
        Awaitable that runs indefinitely, accepting connections.

    Raises:
        ValueError: If the path is invalid or SDDL is malformed.
        RuntimeError: If server creation fails.
    """
    ...

__version__: str
"""The version of the ipc_stream module."""