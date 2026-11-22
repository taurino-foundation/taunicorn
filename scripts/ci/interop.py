"""Exercise the public bindings in separate processes; never log payloads."""

import asyncio
import os
import shutil
import sys
import tempfile
import uuid
from pathlib import Path

from taunicorn import Client, Server

PEER = """
import assert from 'node:assert/strict';
import { Client, Server } from 'taunicorn';
const [name, role, mode] = process.argv.slice(1);
if (mode === 'unsupported') {
  await assert.rejects(Server.listenPrivate(name), { message: 'UnsupportedPlatform' });
  await assert.rejects(Client.connectPrivate(name), { message: 'UnsupportedPlatform' });
  process.exit(0);
}
const server = role === 'server' ? await Server.listenPrivate(name) : null;
try {
  if (server) console.log('ready');
  const conn = server ? await server.accept() : mode === 'private'
    ? await Client.connectPrivate(name) : await Client.connect(name);
  try {
    await conn.send(Buffer.from([255]));
    assert.deepEqual(await conn.receive(1), Buffer.from([0]));
  } finally { await conn.close(); }
} finally { if (server) await server.stop(); }
"""


async def exchange(node, role, mode):
    endpoint = f"tu-{uuid.uuid4().hex[:12]}"
    server = None
    process = None
    try:
        if role == "client":
            listen = Server.listen_private if mode == "private" else Server.listen
            server = await listen(endpoint)
        process = await asyncio.create_subprocess_exec(
            node,
            "--input-type=module",
            "--eval",
            PEER,
            endpoint,
            role,
            mode,
            stdout=asyncio.subprocess.PIPE,
        )
        if server is None:
            assert process.stdout is not None
            assert (await process.stdout.readline()).strip() == b"ready"
            conn = await Client.connect_private(endpoint)
        else:
            conn = await server.accept_timeout(10)
        try:
            assert await conn.receive(1) == b"\xff"
            await conn.send(b"\0")
            assert await process.wait() == 0
        finally:
            await conn.close()
    finally:
        if process is not None and process.returncode is None:
            process.kill()
            await process.wait()
        if server is not None:
            await server.stop()


async def main():
    node = shutil.which("node")
    if not node:
        raise RuntimeError("Node.js required")
    await exchange(node, "client", "raw")
    with tempfile.TemporaryDirectory(
        prefix="tu-", dir=Path.home().resolve()
    ) as directory:
        os.environ["XDG_RUNTIME_DIR"] = directory
        previous = os.umask(0o077)
        try:
            try:
                await exchange(node, "client", "private")
            except RuntimeError as exc:
                if (
                    sys.platform in ("linux", "win32")
                    or str(exc) != "UnsupportedPlatform"
                ):
                    raise
                process = await asyncio.create_subprocess_exec(
                    node,
                    "--input-type=module",
                    "--eval",
                    PEER,
                    "tu-check",
                    "client",
                    "unsupported",
                )
                assert await process.wait() == 0
                print(
                    "Private interop unavailable: runtime ACL unsupported; Node rejected both roles"
                )
                return
            await exchange(node, "server", "private")
        finally:
            os.umask(previous)


if __name__ == "__main__":
    asyncio.run(asyncio.wait_for(main(), 30))
