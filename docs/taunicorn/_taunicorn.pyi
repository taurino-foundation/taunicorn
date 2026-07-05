from __future__ import annotations

from typing import Any, Dict, Optional,  Tuple


__version__: str




class MPSCChannel:
    """
    Multi-Producer, Single-Consumer (MPSC) asynchronous channel.

    This channel allows multiple producers to send Python dictionaries
    to a single asynchronous consumer using an internal Tokio MPSC queue.
    """

    def __init__(self, buffer: int) -> None:
        """
        Create a new MPSC channel with the given buffer size.

        :param buffer: Maximum number of buffered messages.
        """
        ...

    async def send(self, data: Dict[str, Any]) -> None:
        """
        Asynchronously send a dictionary into the channel.

        This method suspends if the channel buffer is full.

        :param data: Dictionary payload to send.
        :raises RuntimeError: If the channel is closed.
        """
        ...

    def try_send(self, data: Dict[str, Any]) -> bool:
        """
        Attempt to send a dictionary into the channel without blocking.

        :param data: Dictionary payload to send.
        :return: True if the message was sent, False otherwise.
        """
        ...

    async def recv(self) -> Any:
        """
        Asynchronously receive the next message from the channel.

        :return: The received dictionary, or None if the channel is closed.
        """
        ...

    def try_recv(self) -> Any:
        """
        Attempt to receive a message from the channel without blocking.

        :return: The received dictionary, or None if no message is available.
        """
        ...


class BroadcastChannel:
    """
    Asynchronous broadcast channel.

    Messages sent to this channel are delivered to all active subscribers.
    Internally backed by a Tokio broadcast channel.
    """

    def __init__(self, buffer: int) -> None:
        """
        Create a new broadcast channel with the given buffer size.

        :param buffer: Maximum number of buffered messages per subscriber.
        """
        ...

    def subscribe(self) -> BroadcastReceiver:
        """
        Subscribe to this broadcast channel.

        :return: A BroadcastReceiver instance receiving all future messages.
        """
        ...

    async def send(self, data: Dict[str, Any]) -> None:
        """
        Broadcast a dictionary payload to all subscribers.

        :param data: Dictionary payload to broadcast.
        :raises RuntimeError: If there are no active subscribers.
        """
        ...


class BroadcastReceiver:
    """
    Receiver for a BroadcastChannel subscription.

    Each receiver maintains its own cursor and receives all messages
    broadcast after subscription.
    """

    async def recv(self) -> Optional[Any]:
        """
        Asynchronously receive the next broadcast message.

        :return: The received object, or None if the channel is closed.
        :raises RuntimeError: If the receiver has lagged behind.
        """
        ...

class Acceptor:
    def __init__(self, name: str, sddl: Optional[str] = "D:(A;;GA;;;WD)") -> None: #"D:(A;;GA;;;WD)"  => This applies to everyone.
        """
        Create a local socket acceptor with a connection pool.

        :param name: Socket / pipe name
        :param sddl: Windows-only security descriptor (optional)
        """
        ...

    async def start(self) -> None:
        """
        Start listening for incoming clients.

        This method accepts clients indefinitely in the background.
        It must be awaited exactly once before using send/recv.
        """
        ...

    async def recv(self) -> Tuple[int, Dict[str, Any]]:
        """
        Receive the next message from any connected client.

        :return: (client_id, message)
        :raises RuntimeError: if the server is closed
        """
        ...

    async def send(self, client_id: int, data: Dict[str, Any]) -> None:
        """
        Send a JSON message to a specific client.

        :param client_id: ID of the target client
        :param data: JSON-serializable dictionary
        :raises RuntimeError: if the client does not exist
        """
        ...



class Connector:
    def __init__(self, name: str) -> None:
        """
        Creates a client connector for a local socket.

        :param name: Socket / pipe name
        """
        ...

    async def start(self) -> None:
        """
        Connects to the server.
        """
        ...

    async def send(self, data: Dict[str, Any]) -> None:
        """
        Sends a JSON-serializable dictionary to the server.
        """
        ...

    async def recv(self) -> Dict[str, Any]:
        """
        Receives a JSON message from the server.
        Blocks until a message is available.
        """
        ...
