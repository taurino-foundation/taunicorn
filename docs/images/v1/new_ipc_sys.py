from typing import Optional


class Client:
    """
    Active client-side connection to a server.

    A Client represents a bidirectional, asynchronous byte-stream
    connection. All operations are non-blocking and must be awaited.
    """

    async def send(self, data: bytes) -> None:
        """
        Send raw bytes to the connected server.

        :param data: Arbitrary byte payload (sent as a framed message)
        :raises RuntimeError: If the connection is closed
        """
        ...

    async def recv(self) -> bytes:
        """
        Receive raw bytes from the server.

        This method asynchronously blocks until a complete frame
        has been received.

        :return: The received byte payload
        :raises RuntimeError: If the connection is closed
        """
        ...


async def connect(name: str) -> Client:
    """
    Connect to a running local socket server.

    :param name: Local socket / named pipe identifier
    :return: A connected Client instance
    :raises IOError: If the connection cannot be established
    """
    ...


class Connection:
    """
    Server-side connection to a single client.

    Semantically identical to `Client`, but created by the server
    via `Server.accept()`.

    Each Connection is independent and safe to use concurrently.
    """

    async def send(self, data: bytes) -> None:
        """
        Send raw bytes to the connected client.

        :param data: Arbitrary byte payload
        :raises RuntimeError: If the connection is closed
        """
        ...

    async def recv(self) -> bytes:
        """
        Receive raw bytes from the client.

        This method asynchronously blocks until a complete frame
        has been received.

        :return: The received byte payload
        :raises RuntimeError: If the client disconnects
        """
        ...


class Server:
    """
    Local socket server accepting multiple client connections.

    The server itself holds no connection state; each accepted
    client is returned as a `Connection` object.
    """

    async def accept(self) -> Connection:
        """
        Wait for an incoming client connection.

        This method asynchronously blocks until a client connects.

        :return: A new Connection instance
        """
        ...


async def serve(
    name: str,
    *,
    mode: Optional[int] = None,
    sddl: Optional[str] = None,
) -> Server:
    """
    Start a local socket server.

    Platform-specific behavior:
    - Unix: `mode` controls filesystem permissions (e.g. 0o600)
    - Windows: `sddl` controls the security descriptor

    The server remains active until the Python process exits.

    :param name: Local socket / named pipe identifier
    :param mode: (Unix only) File mode for the socket
    :param sddl: (Windows only) Security descriptor string
    :return: A Server instance ready to accept connections
    :raises RuntimeError: If the server cannot be started
    """
    ...
