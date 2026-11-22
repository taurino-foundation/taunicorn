# SPDX-License-Identifier: MIT OR Apache-2.0

"""Performance benchmarks for Taunicorn IPC transport."""

import asyncio
import uuid

import pytest
import taunicorn


PAYLOAD_SIZE = 1024
PAYLOAD = b"x" * PAYLOAD_SIZE

BENCHMARK_ROUNDS = 1000
WARMUP_ROUNDS = 20


async def receive_exactly(conn, size: int) -> bytes:
    """Read exactly size bytes from the IPC stream."""
    chunks = []
    remaining = size

    while remaining:
        data = await conn.receive(remaining)

        if not data:
            raise EOFError(f"Connection closed with {remaining} bytes remaining")

        chunks.append(data)
        remaining -= len(data)

    return b"".join(chunks)


@pytest.fixture(scope="function")
def server_and_client():
    """Provide connected IPC endpoints using one persistent event loop."""

    endpoint = f"taunicorn-benchmark-{uuid.uuid4().hex}"

    with asyncio.Runner() as runner:

        async def setup():
            server = await taunicorn.Server.start(endpoint)

            client_task = None
            conn = None

            try:
                # PyO3 may return a Future rather than a coroutine.
                client_task = asyncio.ensure_future(
                    taunicorn.Connection.connect(endpoint)
                )

                conn = await server.accept()
                client = await client_task

                return server, conn, client

            except BaseException:
                if client_task is not None:
                    client_task.cancel()

                    await asyncio.gather(
                        client_task,
                        return_exceptions=True,
                    )

                if conn is not None:
                    await conn.close()

                await server.stop()
                raise

        server, conn, client = runner.run(setup())

        try:
            yield runner, conn, client

        finally:

            async def cleanup():
                try:
                    await client.close()
                finally:
                    try:
                        await conn.close()
                    finally:
                        await server.stop()

            runner.run(cleanup())


def test_benchmark_event_loop_overhead(benchmark):
    """Measure Python coroutine execution through asyncio.Runner."""

    async def noop():
        return None

    with asyncio.Runner() as runner:
        # Initialize the loop outside the measurement.
        runner.run(noop())

        def run():
            return runner.run(noop())

        benchmark.pedantic(
            run,
            rounds=BENCHMARK_ROUNDS,
            warmup_rounds=WARMUP_ROUNDS,
        )


def test_benchmark_send(benchmark, server_and_client):
    """Measure completion time for sending 1 KiB."""

    runner, conn, client = server_and_client

    async def send():
        await client.send(PAYLOAD)

    async def receive():
        data = await receive_exactly(conn, PAYLOAD_SIZE)
        assert data == PAYLOAD

    async def verify():
        await send()
        await receive()

    runner.run(verify())

    pending = False

    def setup():
        nonlocal pending

        if pending:
            runner.run(receive())
            pending = False

    def run_send():
        nonlocal pending

        runner.run(send())
        pending = True

    try:
        benchmark.pedantic(
            run_send,
            setup=setup,
            rounds=BENCHMARK_ROUNDS,
            warmup_rounds=WARMUP_ROUNDS,
        )
    finally:
        if pending:
            runner.run(receive())


def test_benchmark_receive(benchmark, server_and_client):
    """Measure receiving an already-sent 1 KiB payload."""

    runner, conn, client = server_and_client

    async def send():
        await client.send(PAYLOAD)

    async def receive():
        data = await receive_exactly(conn, PAYLOAD_SIZE)
        assert data == PAYLOAD
        return data

    async def verify():
        await send()
        await receive()

    runner.run(verify())

    def setup():
        runner.run(send())

    def run_receive():
        return runner.run(receive())

    benchmark.pedantic(
        run_receive,
        setup=setup,
        rounds=BENCHMARK_ROUNDS,
        warmup_rounds=WARMUP_ROUNDS,
    )


def test_benchmark_roundtrip(benchmark, server_and_client):
    """Measure a complete 1 KiB request and response."""

    runner, conn, client = server_and_client

    async def roundtrip():
        async def echo_once():
            request = await receive_exactly(
                conn,
                PAYLOAD_SIZE,
            )

            assert request == PAYLOAD

            await conn.send(request)

        # Simulate an asynchronous server handler.
        server_task = asyncio.create_task(echo_once())

        try:
            # Client sends request.
            await client.send(PAYLOAD)

            # Client waits for echoed response.
            response = await receive_exactly(
                client,
                PAYLOAD_SIZE,
            )

            await server_task

            assert response == PAYLOAD
            return response

        finally:
            if not server_task.done():
                server_task.cancel()

            await asyncio.gather(
                server_task,
                return_exceptions=True,
            )

    # Verify request/response communication.
    runner.run(roundtrip())

    def run_roundtrip():
        return runner.run(roundtrip())

    benchmark.pedantic(
        run_roundtrip,
        rounds=BENCHMARK_ROUNDS,
        warmup_rounds=WARMUP_ROUNDS,
    )
