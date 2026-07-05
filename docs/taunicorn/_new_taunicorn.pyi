from __future__ import annotations

from typing import Awaitable, Optional

from typing_extensions import Final



class Server:
    def __init__(
        self,
        name: str,
        *,
        mode: Optional[int] = None,
        sddl: Optional[str] = None,
    ) -> None:
        """
        Create a local socket server.

        Parameters
        ----------
        name:
            Socket name (without path).

        mode:
            Unix file permission bits for the socket (e.g. 0o660).
            Only applies on Unix systems.
            If None, a secure default is used.

        sddl:
            Windows Security Descriptor Definition Language (SDDL) string
            controlling named pipe permissions.

            Example:
                "D:(A;;GA;;;WD)"  # Allow Everyone full access

            See:
                https://learn.microsoft.com/en-us/windows/win32/secauthz/security-descriptor-string-format

        Platform behavior
        -----------------
        Windows:
            Socket is created under:
                \\\\.\\pipe\\<name>

        Unix:
            Socket is created under:
                /tmp/<name>
        """
        ...
        
    def serve(self) -> Awaitable[None]:
        """
        Start the server and begin accepting connections.

        This method is async and must be awaited.
        """
        ...

    def stop(self) -> None:
        """
        Stop the server and cancel all pending operations.
        """
        ...

    def send(self, data: bytes) -> Awaitable[None]:
        """
        Send raw bytes to the connected client.

        Raises RuntimeError if no client is connected.
        """
        ...

    def receive(self) -> Awaitable[bytes]:
        """
        Receive raw bytes from the connected client.

        Returns:
            bytes: The received payload.

        Raises RuntimeError if no client is connected or the client disconnects.
        """
        ...

class Client:
    def __init__(self, name: str) -> None:
        """
        Create a local socket client.

        On Windows, connects to a named pipe (\\\\.\\pipe\\<name>).
        On Unix, connects to a socket under /tmp/<name>.
        """
        ...

    def connect(self) -> Awaitable[None]:
        """
        Connect to the server.

        Must be awaited before send/receive can be used.
        """
        ...

    def send(self, data: bytes) -> Awaitable[None]:
        """
        Send raw bytes to the server.

        Raises RuntimeError if not connected.
        """
        ...

    def receive(self) -> Awaitable[bytes]:
        """
        Receive raw bytes from the server.

        Returns:
            bytes: The received payload.

        Raises RuntimeError if not connected or the server disconnects.
        """
        ...
