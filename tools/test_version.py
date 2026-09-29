"""Version policy checks in disposable Git repositories."""
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

from version import resolve


class Versions(unittest.TestCase):
    def test_missing_git(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / ".git").mkdir()
            with patch("version.subprocess.run", side_effect=FileNotFoundError):
                self.assertEqual(resolve(root), "0.0.0-dev")
                with self.assertRaises(ValueError):
                    resolve(root, release=True)

    def test_git_states_and_strict_tags(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            self.assertEqual(resolve(root), "0.0.0-dev")
            with self.assertRaises(ValueError):
                resolve(root, release=True)
            def git(*args):
                subprocess.run(["git", "-C", directory, *args], check=True, capture_output=True)
            git("init", "-q")
            git("config", "user.name", "Version test")
            git("config", "user.email", "version@example.invalid")
            file = root / "source"
            file.write_text("initial")
            git("add", ".")
            git("commit", "-qm", "initial")
            development = resolve(root)
            self.assertRegex(development, r"^0\.0\.0-dev\+g[0-9a-f]{12}$")
            for invalid in ["v01.2.3", "v1.2.3-rc1", "1.2.3", "v1.2.3junk"]:
                git("tag", invalid)
            self.assertEqual(resolve(root), development)
            git("tag", "v1.2.3")
            self.assertEqual(resolve(root, release=True), "1.2.3")
            (root / "untracked").write_text("dirty")
            self.assertEqual(resolve(root), development + ".dirty")
            with self.assertRaises(ValueError):
                resolve(root, release=True)
            (root / "untracked").unlink()
            file.write_text("changed")
            self.assertEqual(resolve(root), development + ".dirty")
            git("add", ".")
            self.assertEqual(resolve(root), development + ".dirty")
            git("commit", "-qm", "next")
            self.assertNotEqual(resolve(root), development)
            with self.assertRaises(ValueError):
                resolve(root, release=True)
            git("tag", "-a", "v2.3.4", "-m", "release")
            self.assertEqual(resolve(root, release=True), "2.3.4")
            git("tag", "v2.3.5")
            with self.assertRaises(ValueError):
                resolve(root)
            export = root / "export"
            export.mkdir()
            self.assertEqual(resolve(export), "0.0.0-dev")
