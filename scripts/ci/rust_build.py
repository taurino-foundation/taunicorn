"""Build once: let Cargo create both halves of the upload against loopback only."""

import hashlib
import json
import os
import secrets
import subprocess
import sys
from http.server import BaseHTTPRequestHandler, HTTPServer
from pathlib import Path
from threading import Thread

from rust_release import NAME, artifact, stable, unpack_capture


def build(folder):
    folder.mkdir(parents=True, exist_ok=False)
    captured = None
    token = secrets.token_hex(32)

    class Capture(BaseHTTPRequestHandler):
        def reply(self, data):
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.end_headers()
            self.wfile.write(json.dumps(data).encode())

        def do_GET(self):
            if self.path == "/config.json":
                self.reply({"api": url, "dl": url})
            elif self.path == "/ta/un/taunicorn" and captured:
                self.reply(captured)
            else:
                self.send_error(404)

        def do_PUT(self):
            nonlocal captured
            if (
                self.path != "/api/v1/crates/new"
                or self.headers.get("Authorization") != token
                or captured
            ):
                return self.send_error(403)
            meta, crate = unpack_capture(
                self.rfile.read(int(self.headers["Content-Length"]))
            )
            v = meta["vers"]
            stable(v)
            if meta["name"] != NAME:
                return self.send_error(400)
            (folder / "metadata.json").write_bytes(
                json.dumps(meta, ensure_ascii=False).encode()
            )
            (folder / f"{NAME}-{v}.crate").write_bytes(crate)
            # Cargo waits for an index entry after a successful upload.
            captured = dict(
                name=NAME,
                vers=v,
                deps=[],
                features={},
                yanked=False,
                cksum=hashlib.sha256(crate).hexdigest(),
            )
            self.reply({})

    with HTTPServer(("127.0.0.1", 0), Capture) as server:
        url = f"http://127.0.0.1:{server.server_port}"
        worker = Thread(target=server.serve_forever)
        worker.start()
        try:
            subprocess.run(
                [
                    "cargo",
                    "publish",
                    "--locked",
                    "-p",
                    NAME,
                    "--registry",
                    "capture",
                    "--config",
                    'registries.capture.credential-provider="cargo:token"',
                ],
                check=True,
                env={
                    **os.environ,
                    "CARGO_REGISTRIES_CAPTURE_INDEX": f"sparse+{url}/",
                    "CARGO_REGISTRIES_CAPTURE_TOKEN": token,
                },
            )
        finally:
            server.shutdown()
            worker.join()
    v, _, crate = artifact(folder)
    print(f"captured {NAME}-{v} sha256={hashlib.sha256(crate).hexdigest()}")


if __name__ == "__main__":
    build(Path(sys.argv[1]))
