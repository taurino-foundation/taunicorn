"""Verify the bridge's NDJSON end record and privacy of its example logs."""

import asyncio
import contextlib
import importlib.util
import io
import json
import unittest
import uuid
from pathlib import Path

from taunicorn import Client, Server


path = Path(__file__).resolve().parents[2] / "examples/python/js_server.py"
spec = importlib.util.spec_from_file_location("bridge", path)
assert spec is not None and spec.loader is not None
bridge = importlib.util.module_from_spec(spec)
spec.loader.exec_module(bridge)


class BridgeTest(unittest.IsolatedAsyncioTestCase):
    async def test_final_reply_then_eof_without_logging_payload(self):
        endpoint = "taunicorn-test-" + uuid.uuid4().hex
        server = await Server.listen(endpoint)
        try:
            client, peer = await asyncio.wait_for(
                asyncio.gather(Client.connect(endpoint), server.accept()), 5
            )
            logs = io.StringIO()
            with contextlib.redirect_stdout(logs):
                handler = asyncio.create_task(bridge.handle_connection(peer))
                try:
                    await client.send(
                        b'{"id":1,"method":"greet","params":{"name":"SYNTHETIC_CANARY"}}\n'
                    )
                    await client.send(b'{"id":2,"method":"quit"}\n')

                    async def through_eof():
                        data = bytearray()
                        while chunk := await client.receive(7):
                            data.extend(chunk)
                        return [json.loads(line) for line in data.splitlines()]

                    responses = await asyncio.wait_for(through_eof(), 5)
                    self.assertIn("SYNTHETIC_CANARY", responses[0]["result"])
                    self.assertEqual(responses[1], {"id": 2, "result": "bye"})
                    await asyncio.wait_for(handler, 5)
                finally:
                    handler.cancel()
                    await client.close()
                    await asyncio.gather(handler, return_exceptions=True)
            self.assertNotIn("SYNTHETIC_CANARY", logs.getvalue())
        finally:
            await server.stop()


if __name__ == "__main__":
    unittest.main()
