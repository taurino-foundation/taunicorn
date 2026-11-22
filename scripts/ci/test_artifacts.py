import tempfile
import unittest
from pathlib import Path

from artifacts import seal, verify


class ArtifactTests(unittest.TestCase):
    def setUp(self):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.root = Path(tmp.name)
        self.package = self.root / "taunicorn-1.0.0.tgz"
        self.package.write_bytes(b"original")

    def test_handoff(self):
        seal(self.root)
        verify(self.root)
        with self.assertRaises(FileExistsError):
            seal(self.root)

    def test_corrupt_missing_extra(self):
        seal(self.root)
        self.package.write_bytes(b"changed")
        with self.assertRaises(ValueError):
            verify(self.root)
        self.package.unlink()
        with self.assertRaises(ValueError):
            verify(self.root)
        self.package.write_bytes(b"original")
        (self.root / "unexpected.whl").touch()
        with self.assertRaises(ValueError):
            verify(self.root)

    def test_manifest_path_is_never_followed(self):
        seal(self.root)
        (self.root / "SHA256SUMS").write_text("0" * 64 + "  ../outside\n")
        with self.assertRaises(ValueError):
            verify(self.root)
