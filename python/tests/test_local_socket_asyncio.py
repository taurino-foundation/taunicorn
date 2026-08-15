from __future__ import annotations

import asyncio
import uuid

from taunicorn import Connection, Listener, Server, Stream


async def _round_trip() -> None:
    assert Listener is Server
    assert Stream is Connection

    endpoint = f"taunicorn-test-{uuid.uuid4().hex}"
    server = await Server.start(endpoint)

    async def server_side() -> None:
        connection = await server.accept_timeout(10.0)
        try:
            request = await asyncio.wait_for(connection.receive(4096), 10.0)
            assert request == b"ping"
            await asyncio.wait_for(connection.send(b"pong"), 10.0)
            await asyncio.wait_for(connection.shutdown_write(), 10.0)
        finally:
            await asyncio.wait_for(connection.close(), 10.0)

    task = asyncio.create_task(server_side())
    try:
        client = await Connection.connect_timeout(endpoint, 10.0)
        try:
            await asyncio.wait_for(client.send(b"ping"), 10.0)
            response = await asyncio.wait_for(client.receive(4096), 10.0)
            assert response == b"pong"
            eof = await asyncio.wait_for(client.receive(1), 10.0)
            assert eof == b""
            assert client.at_eof()
            await asyncio.wait_for(client.shutdown_write(), 10.0)
        finally:
            await asyncio.wait_for(client.close(), 10.0)
        await asyncio.wait_for(task, 10.0)
    finally:
        if not task.done():
            task.cancel()
            try:
                await task
            except asyncio.CancelledError:
                pass
        await asyncio.wait_for(server.stop(), 10.0)


def test_asyncio_tokio_local__round_trip() -> None:
    asyncio.run(_round_trip())
