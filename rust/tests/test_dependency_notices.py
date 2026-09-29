"""Exercise notice collection with Cargo and an isolated, file-backed registry."""
import hashlib
import io
import json
import os
from pathlib import Path
import subprocess
import sys
import tarfile
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "tools"))
import dependency_notices


class DependencyNotices(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        index = self.root / "index"
        index.mkdir()
        downloads = self.root / "downloads"
        downloads.mkdir()
        (index / "config.json").write_text(json.dumps({
            "dl": downloads.as_uri() + "/{crate}-{version}.crate"}))
        for name in ("common", "default-only", "feature-only", "build-only",
                     "dev-only", "platform-only", "unused"):
            archive = downloads / f"{name}-1.0.0.crate"
            files = {
                "Cargo.toml": f'[package]\nname="{name}"\nversion="1.0.0"\nedition="2021"\nlicense="MIT"\n',
                "src/lib.rs": "pub fn example() {}\n",
                "LICENSE": f"Licence text for {name}\n",
            }
            with tarfile.open(archive, "w:gz") as tar:
                for relative, contents in files.items():
                    data = contents.encode()
                    info = tarfile.TarInfo(f"{name}-1.0.0/{relative}")
                    info.size = len(data)
                    tar.addfile(info, io.BytesIO(data))
            entry = index / name[:2] / name[2:4] / name
            entry.parent.mkdir(parents=True, exist_ok=True)
            entry.write_text(json.dumps({
                "name": name, "vers": "1.0.0", "deps": [],
                "cksum": hashlib.sha256(archive.read_bytes()).hexdigest(),
                "features": {}, "yanked": False}) + "\n")
        for arguments in (("init", "-q"), ("add", "."),
                          ("-c", "user.name=Fixture", "-c", "user.email=fixture@example.invalid",
                           "-c", "commit.gpgsign=false", "commit", "-qm", "Registry fixture")):
            subprocess.run(["git", "-C", str(index), *arguments], check=True, capture_output=True)
        self.home = self.root / "cargo-home"
        self.home.mkdir()
        (self.home / "config.toml").write_text(
            '[source.crates-io]\nreplace-with="fixture"\n[source.fixture]\nregistry='
            + json.dumps(index.as_uri()) + "\n")
        self.env = dict(os.environ, CARGO_HOME=str(self.home),
                        CARGO_TARGET_DIR=str(self.root / "target"), CARGO_NET_OFFLINE="false")
        self.workspace = self.root / "workspace"
        self.workspace.mkdir()
        (self.workspace / "rust-toolchain.toml").write_text(
            (dependency_notices.deps.ROOT / "rust-toolchain.toml").read_text())
        self.compiler = subprocess.check_output(["rustc", "--version", "--verbose"],
                                                cwd=self.workspace, env=self.env, text=True).strip()
        self.target = next(line.removeprefix("host: ") for line in self.compiler.splitlines()
                           if line.startswith("host: "))
        (self.workspace / "Cargo.toml").write_text(
            '[workspace]\nresolver="3"\nmembers=["crates/cordial-client", "other"]\n'
            'exclude=["platforms/esp32s3"]\n')
        self.manifest = '''[package]
name="cordial-client"
version="0.0.0"
edition="2021"
[features]
default=["dep:default-only"]
selected=["dep:feature-only"]
[dependencies]
common="=1.0.0"
default-only={version="=1.0.0", optional=true}
feature-only={version="=1.0.0", optional=true}
[build-dependencies]
build-only="=1.0.0"
[dev-dependencies]
dev-only="=1.0.0"
[target.'cfg(any())'.dependencies]
platform-only="=1.0.0"
'''
        self.package("crates/cordial-client", self.manifest)
        self.package("other", '[package]\nname="other"\nversion="0.0.0"\nedition="2021"\n'
                     '[dependencies]\nunused="=1.0.0"\n')

    def package(self, relative, manifest):
        root = self.workspace / relative
        (root / "src").mkdir(parents=True)
        (root / "Cargo.toml").write_text(manifest)
        (root / "src/main.rs").write_text("fn main() {}\n")
        (root / "build.rs").write_text("fn main() {}\n")

    def cargo(self, *arguments, check=True):
        return subprocess.run(["cargo", *arguments], cwd=self.workspace, env=self.env,
                              check=check, capture_output=True, text=True)

    def collect(self, platform, features, lockfile):
        locked = lockfile.read_bytes()
        output = self.root / "output"
        output.mkdir()
        with patch.object(dependency_notices.deps, "ROOT", self.workspace):
            dependency_notices.record(["cargo"], platform, features, self.target, self.env, output)
        self.assertEqual(lockfile.read_bytes(), locked)
        inventory = json.loads((output / "dependencies.json").read_text())
        self.assertEqual(inventory["rustc"], self.compiler)
        entries = inventory["crates"]
        for entry in entries:
            self.assertEqual(entry["version"], "1.0.0")
            self.assertEqual(len(entry["notices"]), 1)
            notice = entry["notices"][0]
            data = (output / notice["file"]).read_bytes()
            self.assertEqual(data, f"Licence text for {entry['name']}\n".encode())
            self.assertEqual(notice["sha256"], hashlib.sha256(data).hexdigest())
        return {entry["name"] for entry in entries}

    def test_cli_collects_notices_after_build_with_fresh_cache(self):
        manifest = "crates/cordial-client/Cargo.toml"
        self.cargo("generate-lockfile")
        self.cargo("build", "--locked", "-p", "cordial-client", "--bin", "cordial-client",
                   "--target", self.target)
        self.assertFalse(list(self.home.glob("registry/src/*/unused-*")))
        offline = self.cargo("metadata", "--locked", "--offline", "--format-version", "1",
                             "--manifest-path", manifest, "--filter-platform", self.target, check=False)
        self.assertNotEqual(offline.returncode, 0)
        self.assertIn("--offline was specified", offline.stderr)
        self.assertEqual(self.collect("host", [], self.workspace / "Cargo.lock"),
                         {"common", "default-only", "build-only"})
        self.assertTrue(list(self.home.glob("registry/src/*/unused-*")))

    def test_firmware_collects_explicit_features_without_defaults(self):
        manifest = "platforms/esp32s3/Cargo.toml"
        self.package("platforms/esp32s3", self.manifest + '\n[workspace]\nresolver="3"\n')
        self.cargo("generate-lockfile", "--manifest-path", manifest)
        self.cargo("build", "--locked", "--manifest-path", manifest, "--target", self.target,
                   "--no-default-features", "--features", "selected")
        self.assertEqual(self.collect("esp32s3", ["selected"],
                                      self.workspace / "platforms/esp32s3/Cargo.lock"),
                         {"common", "feature-only", "build-only"})


if __name__ == "__main__":
    unittest.main()
