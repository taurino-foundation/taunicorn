# SPDX-License-Identifier: MIT OR Apache-2.0

"""Compare Taunicorn IPC against TCP loopback."""

import asyncio
import uuid

import pytest
import taunicorn


PAYLOAD = b"x" * 1024
PAYLOAD_SIZE = len(PAYLOAD)

ROUNDS = 1000
WARMUP = 20
HOST = "127.0.0.1"


async def ipc_read_exactly(conn, size):
    """Receive exactly size bytes from Taunicorn."""
    chunks = []
    remaining = size

    while remaining:
        data = await conn.receive(remaining)

        if not data:
            raise EOFError(f"IPC closed with {remaining} bytes remaining")

        chunks.append(data)
        remaining -= len(data)

    return b"".join(chunks)


async def start_taunicorn():
    """Start a persistent Taunicorn echo connection."""
    endpoint = f"taunicorn-bench-{uuid.uuid4().hex}"

    server = await taunicorn.Server.start(endpoint)

    client_task = None
    conn = None
    client = None

    try:
        client_task = asyncio.ensure_future(taunicorn.Connection.connect(endpoint))

        conn = await asyncio.wait_for(
            server.accept(),
            timeout=10,
        )

        client = await asyncio.wait_for(
            client_task,
            timeout=10,
        )

    except BaseException:
        if client_task is not None:
            client_task.cancel()
            await asyncio.gather(
                client_task,
                return_exceptions=True,
            )

        if client is not None:
            await client.close()

        if conn is not None:
            await conn.close()

        await server.stop()
        raise

    async def echo_server():
        while True:
            try:
                request = await ipc_read_exactly(conn, PAYLOAD_SIZE)
            except EOFError:
                return

            await conn.send(request)

    echo_task = asyncio.create_task(echo_server())

    async def roundtrip():
        await client.send(PAYLOAD)

        response = await ipc_read_exactly(client, PAYLOAD_SIZE)

        if response != PAYLOAD:
            raise AssertionError("Invalid IPC response")

        return response

    async def cleanup():
        echo_task.cancel()

        await asyncio.gather(
            echo_task,
            return_exceptions=True,
        )

        try:
            await client.close()
        finally:
            try:
                await conn.close()
            finally:
                await server.stop()

    return roundtrip, cleanup


async def start_tcp():
    """Start a persistent TCP loopback echo connection."""

    async def echo_handler(reader, writer):
        try:
            while True:
                try:
                    request = await reader.readexactly(PAYLOAD_SIZE)
                except asyncio.IncompleteReadError:
                    break

                writer.write(request)
                await writer.drain()

        finally:
            writer.close()
            await writer.wait_closed()

    server = await asyncio.start_server(
        echo_handler,
        HOST,
        0,
    )

    port = server.sockets[0].getsockname()[1]

    try:
        reader, writer = await asyncio.open_connection(
            HOST,
            port,
        )
    except BaseException:
        server.close()
        await server.wait_closed()
        raise

    # Disable Nagle's algorithm for low-latency requests.
    import socket

    sock = writer.get_extra_info("socket")

    if sock is not None:
        sock.setsockopt(
            socket.IPPROTO_TCP,
            socket.TCP_NODELAY,
            1,
        )

    async def roundtrip():
        writer.write(PAYLOAD)
        await writer.drain()

        response = await reader.readexactly(PAYLOAD_SIZE)

        if response != PAYLOAD:
            raise AssertionError("Invalid TCP response")

        return response

    async def cleanup():
        try:
            writer.close()
            await writer.wait_closed()
        finally:
            server.close()
            await server.wait_closed()

    return roundtrip, cleanup


@pytest.fixture(params=["taunicorn", "tcp"])
def transport(request):
    """Provide a persistent transport and event loop."""

    with asyncio.Runner() as runner:
        if request.param == "taunicorn":
            roundtrip, cleanup = runner.run(start_taunicorn())
        else:
            roundtrip, cleanup = runner.run(start_tcp())

        try:
            # Verify both directions before benchmarking.
            assert runner.run(roundtrip()) == PAYLOAD

            # Explicit, identical warmup.
            for _ in range(WARMUP):
                runner.run(roundtrip())

            yield runner, roundtrip

        finally:
            runner.run(cleanup())


def test_benchmark_transport_roundtrip(
    benchmark,
    transport,
):
    """Measure a complete request/response exchange."""

    runner, roundtrip = transport

    def run():
        return runner.run(roundtrip())

    benchmark.pedantic(
        run,
        rounds=ROUNDS,
        iterations=1,
    )
