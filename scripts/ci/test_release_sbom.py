"""Exercise the archive boundary without network access or a native compiler."""

import hashlib
import io
import json
import subprocess
import sys
import tarfile
import tempfile
import unittest
import zipfile
from pathlib import Path


class ReleaseSbomTest(unittest.TestCase):
    def test_root_tarball_uses_packed_manifest_not_workspace(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            archive = root / "loader-2.0.0.tgz"
            manifest = json.dumps(
                dict(
                    name="loader",
                    version="2.0.0",
                    license="MIT",
                    optionalDependencies={"loader-win": "2.0.0"},
                )
            ).encode()
            with tarfile.open(archive, "w:gz") as tar:
                member = tarfile.TarInfo("package/package.json")
                member.size = len(manifest)
                tar.addfile(member, io.BytesIO(manifest))
            digest = hashlib.sha256(archive.read_bytes()).hexdigest()
            result = subprocess.run(
                [
                    sys.executable,
                    str(Path(__file__).with_name("release_sbom.py")),
                    str(root / "reports"),
                    str(archive),
                ],
                capture_output=True,
                text=True,
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            bom = json.loads(
                (root / "reports" / (archive.name + ".cdx.json")).read_bytes()
            )
            subject = bom["metadata"]["component"]
            self.assertEqual(subject["hashes"], [{"alg": "SHA-256", "content": digest}])
            self.assertEqual(subject["purl"], "pkg:npm/loader@2.0.0")
            properties = {p["name"]: p["value"] for p in bom["metadata"]["properties"]}
            self.assertEqual(
                properties["taunicorn:commit"],
                subprocess.check_output(
                    ["git", "rev-parse", "HEAD"], text=True
                ).strip(),
            )
            self.assertEqual(
                [c["purl"] for c in bom["components"]], ["pkg:npm/loader-win@2.0.0"]
            )
            self.assertEqual(hashlib.sha256(archive.read_bytes()).hexdigest(), digest)

    def test_embedded_inventory_must_cover_locked_versions(self):
        from release_sbom import verify_embedded

        with tempfile.TemporaryFile() as stream, zipfile.ZipFile(stream, "w") as wheel:
            wheel.writestr(
                "fixture.dist-info/sboms/native.cyclonedx.json",
                json.dumps(
                    {
                        "bomFormat": "CycloneDX",
                        "metadata": {"component": {"name": "binding", "version": "1"}},
                        "components": [
                            {
                                "name": "dep",
                                "version": "1",
                                "licenses": [{"license": {"id": "MIT"}}],
                            }
                        ],
                    }
                ),
            )
            verify_embedded(
                wheel, "binding", {("binding", "1"): "", ("dep", "1"): "MIT"}
            )
            with self.assertRaisesRegex(ValueError, "dep"):
                verify_embedded(
                    wheel, "binding", {("binding", "1"): "", ("dep", "2"): "MIT"}
                )
            with self.assertRaisesRegex(ValueError, "dep"):
                verify_embedded(
                    wheel, "binding", {("binding", "1"): "", ("dep", "1"): "Apache-2.0"}
                )


if __name__ == "__main__":
    unittest.main()
