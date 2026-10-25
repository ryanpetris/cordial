"""Check pinned dependency preparation with local Git repositories."""
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "tools"))
import firmware_dependencies as dependencies


class FirmwareDependenciesTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.upstream = self.root / "upstream"
        self.upstream.mkdir()
        self.git("init", "-q")
        (self.upstream / "source.c").write_text("int value;\n")
        self.revision = self.commit()
        cache = patch.object(dependencies, "CACHE", self.root / "cache")
        cache.start()
        self.addCleanup(cache.stop)
        self.source = self.prepare()

    def git(self, *args):
        return subprocess.check_output(["git", "-C", str(self.upstream), *args], text=True).strip()

    def commit(self):
        self.git("add", ".")
        self.git("-c", "user.name=Fixture", "-c", "user.email=fixture@example.invalid",
                 "-c", "commit.gpgsign=false", "commit", "-qm", "Dependency fixture")
        return self.git("rev-parse", "HEAD")

    def prepare(self):
        return dependencies.prepare_git("fixture", self.upstream.as_uri(), self.revision)

    def test_cached_dependency_preserves_checkout_mtimes(self):
        os.utime(self.source / "source.c", ns=(1000000000, 1000000000))
        before = {path: path.stat().st_mtime_ns for path in self.source.rglob("*")}
        self.assertEqual(self.prepare(), self.source)
        self.assertEqual({path: path.stat().st_mtime_ns for path in self.source.rglob("*")}, before)

    def test_modified_and_untracked_files_are_rejected(self):
        for name in ("source.c", "untracked.c"):
            with self.subTest(path=name):
                path = self.source / name
                original = path.read_bytes() if path.exists() else None
                path.write_text("int changed;\n")
                with self.assertRaisesRegex(ValueError, "must be unmodified"):
                    self.prepare()
                if original is None:
                    path.unlink()
                else:
                    path.write_bytes(original)

    def test_requested_revision_is_checked_out(self):
        (self.upstream / "source.c").write_text("int updated;\n")
        self.revision = self.commit()
        self.prepare()
        head = subprocess.check_output(["git", "-C", str(self.source), "rev-parse", "HEAD"], text=True)
        self.assertEqual(head.strip(), self.revision)
        self.assertEqual((self.source / "source.c").read_text(), "int updated;\n")


if __name__ == "__main__":
    unittest.main()
