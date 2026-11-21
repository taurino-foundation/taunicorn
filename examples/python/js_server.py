"""Taunicorn IPC server — speaks NDJSON with any client.

Run:  python examples/python/js_server.py (local demonstration, not a production listener).
"""

import asyncio
import json
import time

import taunicorn

ENDPOINT = "taunicorn-js-py-bridge"
MAX_LINE = 8192


def handle_request(req: dict) -> dict:
    method = req.get("method")
    params = req.get("params", {})

    if method == "add":
        result = params["a"] + params["b"]
    elif method == "greet":
        result = f"Hallo {params['name']}, grüße aus Python! 🐍"
    elif method == "time":
        result = time.time()
    elif method == "quit":
        result = "bye"
    else:
        result = None

    return {"id": req.get("id"), "result": result}


async def handle_connection(conn: taunicorn.Connection) -> None:
    print("[py-server] connected")
    buf = b""
    try:
        while True:
            data = await conn.receive(4096)
            print(
                f"[py-server] receive -> {type(data).__name__} {len(data) if data else 'EOF'}"
            )
            if not data:  # None oder b"" — beides EOF
                print("[py-server] client sent EOF")
                break
            buf += data

            while b"\n" in buf:
                line, buf = buf.split(b"\n", 1)
                if len(line) > MAX_LINE:
                    return
                if not line.strip():
                    continue
                try:
                    req = json.loads(line)
                except json.JSONDecodeError:
                    print("[py-server] invalid request")
                    continue

                if not isinstance(req, dict) or not isinstance(
                    req.get("params", {}), dict
                ):
                    print("[py-server] invalid request")
                    return
                resp = handle_request(req)
                print(f"[py-server] sending: {resp}")
                await conn.send(json.dumps(resp).encode() + b"\n")

                if req.get("method") == "quit":
                    return
            if len(buf) > MAX_LINE:
                return
    except Exception as e:  # breiter fangen, damit wir es SEHEN
        print(f"[py-server] error: {type(e).__name__}: {e}")
    finally:
        await conn.close()
        print("[py-server] connection closed")


async def main() -> None:
    server = await taunicorn.Server.start(ENDPOINT)
    print("[py-server] ready")
    try:
        while True:
            conn = await server.accept()
            await handle_connection(conn)  # sequenziell: ein Client nach dem anderen
    except ConnectionError:
        pass
    finally:
        await server.stop()
        print("[py-server] stopped")


if __name__ == "__main__":
    try:
        asyncio.run(main())
    except KeyboardInterrupt:
        print("\n[py-server] interrupted")
