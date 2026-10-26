"""Check copied source freshness and the shared Docker build/export contract."""
import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location("build_cache", ROOT / "docker/build-cache.py")
build_cache = importlib.util.module_from_spec(spec)
spec.loader.exec_module(build_cache)


class BuildCache(unittest.TestCase):
    def test_source_timestamps_follow_contents_across_checkouts(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source, manifest = root / "src", root / "cache/inputs.json"
            source.mkdir()
            path = source / "main.rs"
            path.write_text("first")
            os.utime(path, ns=(1, 1))
            build_cache.prepare(source, manifest)
            first = path.stat().st_mtime_ns
            self.assertGreater(first, 1)
            os.utime(path, ns=(2, 2))
            build_cache.prepare(source, manifest)
            self.assertEqual(path.stat().st_mtime_ns, first)
            path.write_text("other")
            os.utime(path, ns=(1, 1))
            build_cache.prepare(source, manifest)
            self.assertGreater(path.stat().st_mtime_ns, first)
            # Returning to earlier contents must invalidate the later compilation too.
            path.write_text("first")
            os.utime(path, ns=(1, 1))
            build_cache.prepare(source, manifest)
            self.assertGreater(path.stat().st_mtime_ns, first)
            unchanged = path.stat().st_mtime_ns
            path.chmod(0o755)
            build_cache.prepare(source, manifest)
            self.assertGreater(path.stat().st_mtime_ns, unchanged)

    def test_directory_changes_and_cache_mount_boundaries(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source, manifest = root / "src", root / "inputs.json"
            path = source / "rust/src/main.rs"
            path.parent.mkdir(parents=True)
            path.write_text("source")
            for cached in build_cache.CACHES:
                cache = source / cached
                cache.mkdir(parents=True, exist_ok=True)
                (cache / "artifact").write_text("cached")
            link = source / "link"
            link.symlink_to("missing")
            build_cache.prepare(source, manifest)
            first = path.parent.stat().st_mtime_ns
            path.unlink()
            build_cache.prepare(source, manifest)
            self.assertGreater(path.parent.stat().st_mtime_ns, first)
            entries = json.loads(manifest.read_text())
            self.assertIn("link", entries)
            self.assertFalse(any("artifact" in name for name in entries))
            link.unlink()
            link.symlink_to("another")
            build_cache.prepare(source, manifest)
            self.assertNotEqual(entries["link"], json.loads(manifest.read_text())["link"])

    def test_host_package_aliases_share_each_build(self):
        result = subprocess.run(["make", "-n", "package-desktop", "package-cli",
                                 "package-host-arch", "package-host-deb", "DEB_DISTRIBUTION=resolute"],
                                cwd=ROOT, check=True, capture_output=True, text=True)
        for command in ("make local-package-host-tar", "python3 rust/tools/build_appimage.py",
                        "make -f packaging/Makefile arch", "make -f packaging/Makefile deb"):
            self.assertEqual(result.stdout.count("--build-arg COMMAND='" + command), 1, command)

    def test_trixie_builds_its_executable_with_native_libraries(self):
        command = ["make", "-n", "package-host-deb", "DEB_DISTRIBUTION=trixie"]
        result = subprocess.run(command, cwd=ROOT, check=True, capture_output=True, text=True)
        self.assertIn("--build-arg BASE=debian:13 -f docker/host.Dockerfile", result.stdout)
        self.assertIn("--build-arg BASE=cordial-host-trixie", result.stdout)
        self.assertIn("--build-arg COMMAND='make local-package-host-tar", result.stdout)
        self.assertIn("&& make -f packaging/Makefile deb DEB_DISTRIBUTION=trixie", result.stdout)
        self.assertNotIn("/inputs/cordial-", result.stdout)
        self.assertNotIn("-t cordial-host -f", result.stdout)
        rejected = subprocess.run([*command, "HOST_ARCHIVE=release.tar.gz"], cwd=ROOT,
                                  capture_output=True, text=True)
        self.assertNotEqual(rejected.returncode, 0)
        self.assertIn("omit HOST_ARCHIVE", rejected.stderr)

    def test_make_pins_environment_and_exports_only_current_outputs(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            shutil.copy2(ROOT / "Makefile", root / "Makefile")
            dependencies = root / "rust/tools/firmware_dependencies.py"
            dependencies.parent.mkdir(parents=True)
            dependencies.write_text("pinned dependency")
            docker = root / "docker-fixture"
            docker.write_text('''#!/usr/bin/env python3
import json
import os
from pathlib import Path
import sys
args = sys.argv[1:]
with open("calls.jsonl", "a") as log:
    log.write(json.dumps(args) + "\\n")
state = Path("images.json")
images = json.loads(state.read_text()) if state.exists() else {}
image = "sha256:" + os.environ["FIXTURE_IMAGE"]
images[image] = {"id": image, "runtime": json.loads(os.environ["FIXTURE_RUNTIME"])}
images["cordial-host"] = images[image]
if args[0] == "tag":
    images[args[2]] = images[args[1]]
state.write_text(json.dumps(images))
if args[:2] == ["image", "inspect"]:
    if args[-1] not in images:
        raise SystemExit(1)
    inspected = images[args[-1]]
    template = args[args.index("--format") + 1]
    if template == "{{.Id}}":
        print(inspected["id"])
    else:
        assert template == '{{json .RootFS}} {{json .Config}} {{json .Os}} {{json .Architecture}} {{json (or .Variant "")}}'
        runtime = inspected["runtime"]
        print(" ".join(json.dumps(runtime.get(field, ""), sort_keys=True) for field in
                       ("RootFS", "Config", "Os", "Architecture", "Variant")))
elif "--output" in args:
    reference = next(arg.split("=", 1)[1] for arg in args if arg.startswith("IMAGE="))
    assert images[reference]["runtime"] == json.loads(os.environ["FIXTURE_RUNTIME"])
    if os.environ.get("FIXTURE_FAIL"):
        raise SystemExit(1)
    destination = Path(args[args.index("--output") + 1].split("dest=", 1)[1])
    output = destination / "build/release"
    output.mkdir(parents=True)
    (output / "cordial-current-linux-amd64.tar.gz").write_text("current")
''')
            docker.chmod(0o755)

            runtime = dict(RootFS={"Layers": ["fixture"]}, Config={"Env": ["PATH=/bin"]},
                           Os="linux", Architecture="amd64")

            def run(image, contents=runtime):
                subprocess.run(["make", "package-cli", f"DOCKER={docker}"], cwd=root,
                               env=dict(os.environ, FIXTURE_IMAGE=image,
                                        FIXTURE_RUNTIME=json.dumps(contents)), check=True,
                               capture_output=True, text=True)
                calls = [json.loads(line) for line in (root / "calls.jsonl").read_text().splitlines()]
                return next(call for call in reversed(calls) if "--output" in call)

            first = run("a" * 64)
            scratch = root / ".cache/docker/package-host-tar/out/build/release"
            scratch.mkdir(parents=True, exist_ok=True)
            (scratch / "cordial-stale-linux-amd64.tar.gz").write_text("stale")
            same = run("a" * 64)
            self.assertEqual(first, same)
            self.assertFalse((root / "build/release/cordial-stale-linux-amd64.tar.gz").exists())
            second = run("b" * 64)
            key = lambda call: next(arg for arg in call if arg.startswith("CACHE_ID="))
            self.assertEqual(key(first), key(second))
            self.assertEqual(first, second)
            reference = "cordial-toolchain:" + key(first).split("=", 1)[1]
            self.assertIn("IMAGE=" + reference, first)
            state = root / "images.json"
            self.assertEqual(json.loads(state.read_text())[reference]["id"], "sha256:" + "a" * 64)
            calls = [json.loads(line) for line in (root / "calls.jsonl").read_text().splitlines()]
            self.assertEqual([call for call in calls if call[0] == "tag"],
                             [["tag", "sha256:" + "a" * 64, reference]])
            for index, (field, value) in enumerate((
                ("RootFS", {"Layers": ["different toolchain"]}),
                ("Config", {"Env": ["PATH=/usr/bin"]}),
                ("Os", "windows"),
                ("Architecture", "arm64"),
                ("Variant", "v8"),
            )):
                with self.subTest(field=field):
                    changed = run(str(index) * 64, dict(runtime, **{field: value}))
                    self.assertNotEqual(key(second), key(changed))
                    self.assertIn("IMAGE=cordial-toolchain:" + key(changed).split("=", 1)[1], changed)
            images = json.loads(state.read_text())
            images[reference] = images["sha256:" + "0" * 64]
            state.write_text(json.dumps(images))
            repaired = run("b" * 64)
            self.assertEqual(second, repaired)
            self.assertEqual(json.loads(state.read_text())[reference]["id"], "sha256:" + "b" * 64)
            dependencies.write_text("different pinned dependency")
            third = run("b" * 64)
            key = lambda call: next(arg for arg in call if arg.startswith("DEPENDENCY_ID="))
            self.assertNotEqual(key(second), key(third))
            exported = root / "build/release/cordial-current-linux-amd64.tar.gz"
            self.assertEqual(exported.read_text(), "current")
            failed = subprocess.run(["make", "package-cli", f"DOCKER={docker}"], cwd=root,
                                    env=dict(os.environ, FIXTURE_IMAGE="b" * 64,
                                             FIXTURE_RUNTIME=json.dumps(runtime), FIXTURE_FAIL="1"),
                                    capture_output=True, text=True)
            self.assertNotEqual(failed.returncode, 0)
            self.assertEqual(exported.read_text(), "current")


if __name__ == "__main__":
    unittest.main()
