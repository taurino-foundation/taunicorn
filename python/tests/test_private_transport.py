"""The private factories verify OS peers; the resulting transport is still a byte stream."""

import asyncio
import os
import sys
import tempfile
import uuid
from pathlib import Path

import pytest

from taunicorn import Client, Endpoint, Server


@pytest.fixture
def private_runtime(monkeypatch):
    # A 0700 child under a world-writable /tmp is not a trusted socket path.
    with tempfile.TemporaryDirectory(
        prefix="tu-", dir=os.environ.get("XDG_RUNTIME_DIR") or Path.home().resolve()
    ) as directory:
        monkeypatch.setenv("XDG_RUNTIME_DIR", directory)
        previous = os.umask(0o077)
        try:
            yield
        finally:
            os.umask(previous)


async def test_private_transport(private_runtime):
    name = Endpoint(f"tu-{uuid.uuid4().hex[:12]}")
    try:
        server = await Server.listen_private(name)
    except RuntimeError as exc:
        if (
            sys.platform in ("linux", "win32")
            or os.environ.get("GITHUB_ACTIONS") == "true"
            or str(exc) != "UnsupportedPlatform"
        ):
            raise
        with pytest.raises(RuntimeError, match="^UnsupportedPlatform$"):
            await Client.connect_private(name)
        pytest.skip("Core cannot verify this runtime directory's ACL")
    try:
        client, peer = await asyncio.wait_for(
            asyncio.gather(Client.connect_private(name), server.accept()), 10
        )
        try:
            await client.send(b"\xff")
            assert await asyncio.wait_for(peer.receive(1), 10) == b"\xff"
            await peer.send(b"\x00")
            assert await asyncio.wait_for(client.receive(1), 10) == b"\x00"
        finally:
            await asyncio.gather(client.close(), peer.close())
    finally:
        await server.stop()


@pytest.mark.parametrize("factory", ["listen_private", "connect_private"])
async def test_private_factory_rejects_invalid_service_name(factory):
    open_endpoint = getattr(Server if factory == "listen_private" else Client, factory)
    with pytest.raises(RuntimeError, match="InvalidEndpoint"):
        await open_endpoint("invalid service name")


@pytest.mark.skipif(os.name != "posix", reason="Unix runtime-directory policy")
async def test_private_factories_refuse_missing_runtime(monkeypatch):
    monkeypatch.delenv("XDG_RUNTIME_DIR", raising=False)
    for factory in (Server.listen_private, Client.connect_private):
        with pytest.raises(RuntimeError, match="^InsecureSocketPermissions$"):
            await factory("tu-no-runtime")


@pytest.mark.parametrize("method", ["receive", "read_timeout", "split"])
async def test_raw_read_preserves_bytes_across_bounded_chunks(method):
    name = f"tu-{uuid.uuid4().hex[:12]}"
    server = await Server.listen(name)
    try:
        client, peer = await asyncio.wait_for(
            asyncio.gather(Client.connect(name), server.accept()), 10
        )
        try:
            if method == "split":
                reader, _ = peer.into_split()
                read = reader.receive
            elif method == "read_timeout":

                def read(size):
                    return peer.read_timeout(size, 10)
            else:
                read = peer.receive
            assert await read(0) == b""
            payload = bytes(range(256)) * 1024

            async def drain():
                received = bytearray()
                while len(received) < len(payload):
                    chunk = await read(len(payload))
                    assert 0 < len(chunk) <= 65536
                    received.extend(chunk)
                assert received == payload

            await asyncio.wait_for(asyncio.gather(client.send(payload), drain()), 10)
        finally:
            await client.close()
            if method != "split":
                await peer.close()
    finally:
        await server.stop()
