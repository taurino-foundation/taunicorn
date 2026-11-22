import hashlib
import json
import os
import struct
import unittest
from http.client import HTTPConnection
from http.server import BaseHTTPRequestHandler, HTTPServer
from pathlib import Path
from threading import Thread
from unittest.mock import patch

import rust_release as release


class ReleaseTests(unittest.TestCase):
    @unittest.skipUnless(
        os.environ.get("RUST_RELEASE_ARTIFACT"), "requires a real build artifact"
    )
    def test_publish_staged_file_and_confirm_registry_checksum(self):
        folder = Path(os.environ["RUST_RELEASE_ARTIFACT"])
        v, metadata, archive = release.artifact(folder)
        row = dict(vers=v, yanked=False, cksum=hashlib.sha256(archive).hexdigest())
        received = []

        class Registry(BaseHTTPRequestHandler):
            def do_PUT(self):
                received.append(self.rfile.read(int(self.headers["Content-Length"])))
                self.send_response(200)
                self.end_headers()
                self.wfile.write(b"{}")

        with HTTPServer(("127.0.0.1", 0), Registry) as server:
            server.timeout = 5
            worker = Thread(target=server.handle_request)
            worker.start()
            local = HTTPConnection("127.0.0.1", server.server_port, timeout=5)
            with (
                patch.object(release, "HTTPSConnection", return_value=local),
                patch.object(
                    release, "history", side_effect=[[dict(vers="0.0.0")], [row]]
                ),
                patch.dict(
                    os.environ, RELEASE_VERSION=v, CARGO_REGISTRY_TOKEN="test-only"
                ),
            ):
                release.publish(folder)
            worker.join()
        self.assertEqual(len(received), 1)
        data = received[0]
        (size,) = struct.unpack_from("<I", data)
        self.assertEqual(data[4 : 4 + size], metadata)
        self.assertEqual(struct.unpack_from("<I", data, 4 + size), (len(archive),))
        self.assertEqual(data[8 + size :], archive)
        with (
            patch.dict(os.environ, RELEASE_VERSION="wrong"),
            self.assertRaisesRegex(ValueError, "dispatch version"),
        ):
            release.preflight(folder)

    def test_cargo_framing_and_crates_io_dependency_metadata(self):
        crate = b"\x1f\x8b\x00\xff"
        dep = dict(
            name="serde",
            explicit_name_in_toml="alias",
            version_req="^1",
            registry="https://github.com/rust-lang/crates.io-index",
            target="cfg(unix)",
        )
        meta = dict(name="taunicorn", vers="1.0.0", deps=[dep], readme="snowman ☃")
        raw = json.dumps(meta).encode()
        body = struct.pack("<I", len(raw)) + raw + struct.pack("<I", len(crate)) + crate
        converted, archive = release.unpack_capture(body)
        self.assertEqual(archive, crate)
        self.assertEqual(converted, dict(meta, deps=[dict(dep, registry=None)]))
        for invalid in [body[:-1], body + b"x", b""]:
            with self.assertRaises(ValueError):
                release.unpack_capture(invalid)
        for source in [None, "https://elsewhere.example/index"]:
            dep["registry"] = source
            with self.assertRaises(ValueError):
                release.unpack_capture(release.frame(json.dumps(meta).encode(), crate))

    def test_rollback_and_publish_version_guards(self):
        def row(v, yanked=False):
            return dict(vers=v, name="taunicorn", yanked=yanked, cksum="0" * 64)

        rows = [row("1.2.9"), row("1.2.10"), row("1.2.11", True), row("1.3.0")]
        self.assertEqual(release.previous(rows, "1.3.0")["vers"], "1.2.10")
        with self.assertRaises(ValueError):
            release.previous(rows, "1.2.9")
        for v in ["1.3.0", "1.2.11", "1.2.8", "../2.0.0", "01.3.0"]:
            with self.assertRaises(ValueError):
                release.require_new(rows, v)
        release.require_new(rows, "1.10.0")

    def test_put_exact_bytes_once_and_rejection_even_with_http_200(self):
        requests = []

        class Registry(BaseHTTPRequestHandler):
            def do_PUT(self):
                requests.append(
                    (
                        self.path,
                        self.headers["Authorization"],
                        self.headers["Content-Type"],
                        self.rfile.read(int(self.headers["Content-Length"])),
                    )
                )
                self.send_response(status)
                self.end_headers()
                self.wfile.write(reply)

        with HTTPServer(("127.0.0.1", 0), Registry) as server:
            server.timeout = 4
            for status, reply, accepted in [
                (200, b"{}", True),
                (200, b'{"errors":[{"detail":"taken"}]}', False),
                (403, b"forbidden", False),
                (302, b"{}", False),
                (200, b"oops", False),
            ]:
                t = Thread(target=server.handle_request)
                t.start()
                con = HTTPConnection("127.0.0.1", server.server_port, timeout=3)
                try:
                    if accepted:
                        release.put(con, b"payload", "local-only")
                    else:
                        with self.assertRaises(ValueError):
                            release.put(con, b"payload", "local-only")
                finally:
                    con.close()
                    t.join(4)
            self.assertEqual(
                requests,
                [
                    (
                        "/api/v1/crates/new",
                        "local-only",
                        "application/octet-stream",
                        b"payload",
                    )
                ]
                * 5,
            )


if __name__ == "__main__":
    unittest.main()
