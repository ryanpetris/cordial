"""Check local runners, source freshness, and the Docker build/export contract."""
import importlib.util
import hashlib
import io
import json
import os
from pathlib import Path
import shutil
import shlex
import subprocess
import tempfile
import tarfile
import unittest
from unittest import mock

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location("build_cache", ROOT / "docker/build-cache.py")
build_cache = importlib.util.module_from_spec(spec)
spec.loader.exec_module(build_cache)

CONFIGURATION = (
    "PYTHON", "BUF", "CORDIAL_VERSION", "CORDIAL_HOMEPAGE", "SOURCE_DATE_EPOCH",
    "HOST_TARGET", "BOARD", "PROFILE", "PROTOCOL_STABLE_BASE", "PROTOCOL_BASE", "DOCKER",
    "DEB_DISTRIBUTION", "DEB_REVISION", "HOST_ARCHIVE", "BOARDS", "ESP_BOARDS",
    "FIRMWARE_PLATFORM", "DEB_BASE", "DEB_BASE_trixie", "DEB_BASE_resolute",
    "CHECK_IMAGE_FLAGS",
)


def fixture_environment(**overrides):
    return {**{key: value for key, value in os.environ.items()
               if key not in CONFIGURATION and not key.startswith("JUST_")},
            **overrides}


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


class DockerBuild(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        shutil.copy2(ROOT / "justfile", self.root / "justfile")
        self.dependencies = self.root / "rust/tools/firmware_dependencies.py"
        self.dependencies.parent.mkdir(parents=True)
        self.dependencies.write_text("pinned dependency")
        self.docker = self.root / "docker-fixture"
        self.docker.write_text('''#!/usr/bin/env python3
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
if args[0] == "build" and "-t" in args:
    images[args[args.index("-t") + 1]] = images[image]
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
    with open("environment.jsonl", "a") as log:
        log.write(json.dumps({key: os.environ.get(key) for key in
                              ("CORDIAL_VERSION", "CORDIAL_HOMEPAGE")}) + "\\n")
    reference = next(arg.split("=", 1)[1] for arg in args if arg.startswith("IMAGE="))
    assert images[reference]["runtime"] == json.loads(os.environ["FIXTURE_RUNTIME"])
    if os.environ.get("FIXTURE_FAIL"):
        raise SystemExit(1)
    destination = Path(args[args.index("--output") + 1].split("dest=", 1)[1])
    outputs = next(arg.split("=", 1)[1] for arg in args if arg.startswith("OUTPUTS="))
    for output in outputs.split():
        path = destination / output.replace("*", os.environ["CORDIAL_VERSION"])
        if path.name == "share":
            path.mkdir(parents=True)
        else:
            path.parent.mkdir(parents=True, exist_ok=True)
            if path.name == "cordial":
                path.write_text('#!/usr/bin/env python3\\nimport json, sys\\nprint(json.dumps(sys.argv[1:]))\\n')
                path.chmod(0o755)
            else:
                path.write_text("current")
''')
        self.docker.chmod(0o755)
        self.runtime = dict(RootFS={"Layers": ["fixture"]}, Config={"Env": ["PATH=/bin"]},
                            Os="linux", Architecture="amd64")

    def run_just(self, *args, image="a" * 64, runtime=None, fail=False, check=True, **env):
        return subprocess.run(
            [os.environ.get("JUST", "just"), f"DOCKER={self.docker}", *args], cwd=self.root,
            env=fixture_environment(FIXTURE_IMAGE=image,
                                    FIXTURE_RUNTIME=json.dumps(runtime or self.runtime),
                                    FIXTURE_FAIL="1" if fail else "", **env),
            check=check, capture_output=True, text=True)

    def calls(self):
        return [json.loads(line) for line in (self.root / "calls.jsonl").read_text().splitlines()]

    def builds(self):
        return [call for call in self.calls() if "--output" in call]

    def test_host_packages_share_each_build(self):
        self.run_just("DEB_DISTRIBUTION=resolute", "package-host-appimage", "package-host-tar",
                      "package-host-arch", "package-host-deb")
        commands = [next(arg for arg in call if arg.startswith("COMMAND="))
                    for call in self.builds()]
        self.assertEqual(len(commands), 4)
        for command in ("just HOST_TARGET=", "python3 rust/tools/build_appimage.py",
                        "just --justfile packaging/justfile HOST_ARCHIVE="):
            self.assertEqual(sum(item.startswith("COMMAND=" + command) for item in commands),
                             2 if "packaging/justfile" in command else 1, command)
        images = [call[call.index("-t") + 1] for call in self.calls() if "-t" in call]
        self.assertEqual(images, ["cordial-host", "cordial-arch", "cordial-deb-resolute"])

    def test_distribution_images_keep_packaging_contexts(self):
        self.run_just("DEB_DISTRIBUTION=resolute", "image-arch", "image-deb")
        self.assertEqual(self.calls(), [
            ["build", "-t", "cordial-arch", "--build-context", "tools=docker",
             "-f", "docker/arch.Dockerfile", "packaging/arch"],
            ["build", "-t", "cordial-deb-resolute", "--build-arg", "BASE=ubuntu:26.04",
             "--build-context", "tools=docker", "-f", "docker/deb.Dockerfile", "packaging/debian"],
        ])

    def test_package_checks_share_the_archive_and_use_fresh_prerequisite_containers(self):
        self.run_just("DEB_DISTRIBUTION=resolute", "check-appimage-resolute", "check-appimage-arch",
                      "check-package-arch", "check-package-deb")
        commands = [next(arg for arg in call if arg.startswith("COMMAND=")) for call in self.builds()]
        self.assertEqual(sum("local-package-host-tar" in command for command in commands), 1)
        self.assertEqual(len(commands), 4)
        runs = [call for call in self.calls() if call[0] == "run"]
        self.assertEqual(len(runs), 4)
        self.assertTrue(all(call[1:3] == ["--rm", "--init"] for call in runs))
        self.assertTrue(all(["-v", str(self.root) + ":/src"] == call[3:5] for call in runs))
        self.assertEqual([call[-2:] for call in runs[:2]],
                         [["docker/check-appimage.sh", "resolute"], ["docker/check-appimage.sh", "arch"]])
        self.assertEqual([call[-1] for call in runs[2:]],
                         ["packaging/check-arch.sh", "packaging/check-deb.sh"])
        self.assertIn("DEB_REVISION", runs[-1])
        for call in runs:
            self.assertIn(f"CHECK_UID={os.getuid()}", call)
            self.assertIn(f"CHECK_GID={os.getgid()}", call)

    def test_prerequisite_images_keep_distribution_bases_and_refresh_flags(self):
        self.run_just("CHECK_IMAGE_FLAGS=--pull --no-cache", "image-check-package-deb",
                      "image-check-package-arch", "image-check-appimage-resolute", "image-check-appimage-arch",
                      "image-check-desktop")
        builds = [call for call in self.calls() if call[0] == "build"]
        self.assertEqual(len(builds), 5)
        for call, base, kind in zip(builds, ["debian:13", "archlinux:base-devel", "ubuntu:26.04", "archlinux:base", "ubuntu:26.04"],
                                    ["package-deb", "package-arch", "appimage-resolute", "appimage-arch", "desktop-resolute"]):
            self.assertIn("BASE=" + base, call)
            self.assertIn("KIND=" + kind, call)
            self.assertEqual(call[1:3], ["--pull", "--no-cache"])
            self.assertEqual(call[-3:], ["-f", "docker/check.Dockerfile", "."])
        self.assertEqual(builds[-1][builds[-1].index("--target") + 1], "desktop")

    def test_installed_desktop_checks_build_current_resolute_package(self):
        self.run_just("DEB_DISTRIBUTION=resolute", "check-installed-desktop")
        commands = [next(arg for arg in call if arg.startswith("COMMAND=")) for call in self.builds()]
        self.assertEqual(len(commands), 2)
        self.assertIn("local-package-host-tar", commands[0])
        self.assertIn("DEB_DISTRIBUTION=resolute", shlex.split(commands[1].removeprefix("COMMAND=")))
        self.assertEqual(self.calls()[-1][-3:],
                         ["cordial-check-desktop-resolute", "bash", "docker/check-installed-desktop.sh"])

    def test_installed_desktop_rejects_trixie_before_any_docker_build(self):
        rejected = self.run_just("check-installed-desktop", check=False)
        self.assertNotEqual(rejected.returncode, 0)
        self.assertIn("Installed desktop checks require DEB_DISTRIBUTION=resolute", rejected.stderr)
        self.assertFalse((self.root / "calls.jsonl").exists())

    def test_combined_firmware_runs_clippy_before_build_in_one_export(self):
        self.run_just("BOARD=pico2_w", "PROFILE=production", "check-and-build-firmware")
        self.assertEqual(len(self.builds()), 1)
        call = self.builds()[0]
        command = next(arg for arg in call if arg.startswith("COMMAND="))
        build = "python3 rust/tools/build_firmware.py rust/boards/pico2_w.json --profile production"
        self.assertEqual(command, "COMMAND=time " + build + " --clippy && time " + build)
        self.assertIn("OUTPUTS=build/firmware/*/*", call)
        self.assertEqual(sum("-t" in call for call in self.calls()), 1)

    def test_unknown_debian_distribution_only_blocks_its_image(self):
        self.run_just("DEB_DISTRIBUTION=unknown", "help", "host")
        rejected = self.run_just("DEB_DISTRIBUTION=unknown", "image-deb", check=False)
        self.assertNotEqual(rejected.returncode, 0)
        self.assertIn("Unknown DEB_DISTRIBUTION unknown", rejected.stderr)
        self.run_just("DEB_DISTRIBUTION=unknown", "DEB_BASE=custom:base", "image-deb")
        self.assertIn("BASE=custom:base", self.calls()[-1])

    def test_internal_defaults_ignore_environment_and_cli_overrides_recurse(self):
        self.run_just("firmware-all", "image-host-trixie", "image-deb",
                      BOARDS="other", ESP_BOARDS="pico_w", FIRMWARE_PLATFORM="other",
                      DEB_BASE="other", DEB_BASE_trixie="other", DEB_BASE_resolute="other")
        self.assertEqual(len(self.builds()), 5)
        images = [call for call in self.calls() if "-t" in call]
        self.assertEqual(images[0][images[0].index("-t") + 1], "cordial-pico")
        self.assertEqual(images[4][images[4].index("-t") + 1], "cordial-esp32s3")
        self.assertIn("BASE=debian:13", images[-2])
        self.assertIn("BASE=cordial-host-trixie", images[-1])
        self.run_just("BOARDS=pico_w esp32s3", "ESP_BOARDS=pico_w", "firmware-all")
        self.assertEqual(len(self.builds()), 7)
        images = [call for call in self.calls() if "-t" in call][-2:]
        self.assertEqual([call[call.index("-t") + 1] for call in images],
                         ["cordial-esp32s3", "cordial-pico"])
        start = len(self.calls())
        self.run_just("BOARDS=pico_w", "FIRMWARE_PLATFORM=esp32s3", "check-firmware-all")
        self.assertEqual(len(self.builds()), 9)
        images = [call for call in self.calls()[start:] if "-t" in call]
        self.assertEqual([call[call.index("-t") + 1] for call in images],
                         ["cordial-esp32s3", "cordial-esp32s3"])
        self.run_just("DEB_BASE_trixie=custom:trixie", "image-deb")
        images = [call for call in self.calls() if "-t" in call]
        self.assertIn("BASE=custom:trixie", images[-2])
        self.assertIn("BASE=cordial-host-trixie", images[-1])
        self.run_just("DEB_DISTRIBUTION=resolute", "image-deb",
                      DEB_BASE="other", DEB_BASE_resolute="other")
        self.assertIn("BASE=ubuntu:26.04", self.calls()[-1])
        self.run_just("DEB_DISTRIBUTION=resolute", "DEB_BASE_resolute=custom:resolute", "image-deb")
        self.assertIn("BASE=custom:resolute", self.calls()[-1])

    def test_fixture_defaults_ignore_caller_configuration(self):
        polluted = (*CONFIGURATION, "JUST_JUSTFILE", "JUST_DRY_RUN", "JUST_WORKING_DIRECTORY")
        with mock.patch.dict(os.environ, {key: "unexpected" for key in polluted}):
            self.run_just("package-host-tar", "firmware")
        self.assertTrue((self.root / "build/release/cordial-0.0.0-linux-amd64.tar.gz").is_file())
        command = next(arg for arg in self.builds()[-1] if arg.startswith("COMMAND="))
        self.assertIn("rust/boards/pico_w.json --profile development", command)

    def test_trixie_builds_its_executable_with_native_libraries(self):
        self.run_just("DEB_DISTRIBUTION=trixie", "DEB_REVISION=2~trixie", "package-host-deb")
        images = [call for call in self.calls() if "-t" in call]
        self.assertIn("BASE=debian:13", images[0])
        self.assertIn("BASE=cordial-host-trixie", images[1])
        self.assertEqual(len(self.builds()), 1)
        command = next(arg for arg in self.builds()[0] if arg.startswith("COMMAND="))
        self.assertIn("just HOST_TARGET=x86_64-unknown-linux-gnu local-package-host-tar", command)
        self.assertIn("&& just --justfile packaging/justfile DEB_DISTRIBUTION=trixie", command)
        self.assertIn("DEB_REVISION=2~trixie", command)
        self.assertNotIn("/inputs/cordial-", command)
        rejected = self.run_just("DEB_DISTRIBUTION=trixie", "HOST_ARCHIVE=release.tar.gz",
                                 "package-host-deb", check=False)
        self.assertNotEqual(rejected.returncode, 0)
        self.assertIn("omit HOST_ARCHIVE", rejected.stderr)

    def test_supplied_archive_skips_compilation_and_preserves_its_name(self):
        archive = self.root / "input archive's.tar.gz"
        archive.write_text("supplied")
        self.run_just("HOST_ARCHIVE=" + str(archive), "DEB_DISTRIBUTION=resolute",
                      "package-host-appimage", "package-host-arch", "package-host-deb")
        self.assertEqual(len(self.builds()), 3)
        for build in self.builds():
            command = next(arg for arg in build if arg.startswith("COMMAND="))
            self.assertIn("/inputs/input archive", command)
            context = next(arg for arg in build if arg.startswith("inputs="))
            self.assertEqual((self.root / context.split("=", 1)[1] / archive.name).read_text(),
                             "supplied")
            self.assertIn("/inputs/" + archive.name, [word.removeprefix("HOST_ARCHIVE=")
                                                     for word in shlex.split(command[8:])])

    def test_firmware_matrix_selects_images_and_profiles(self):
        self.run_just("PROFILE=debug", "firmware-all")
        commands = [next(arg for arg in call if arg.startswith("COMMAND="))
                    for call in self.builds()]
        self.assertEqual(len(commands), 5)
        for board in ("pico_w", "pico2_w", "pimoroni_pico_plus2_w",
                      "waveshare_rp2350b_plus_w", "esp32s3"):
            self.assertTrue(any(f"rust/boards/{board}.json --profile debug" in c for c in commands))
        self.run_just("check-firmware-all")
        commands = [next(arg for arg in call if arg.startswith("COMMAND="))
                    for call in self.builds()[5:]]
        self.assertEqual(len(commands), 10)
        self.assertEqual(sum("--profile production --clippy" in c for c in commands), 5)
        self.assertEqual(sum("--profile debug --clippy" in c for c in commands), 5)
        images = [call[call.index("-t") + 1] for call in self.calls() if "-t" in call]
        self.assertEqual(images.count("cordial-pico"), 12)
        self.assertEqual(images.count("cordial-esp32s3"), 3)

    def test_all_builds_host_and_firmware(self):
        self.run_just("all")
        commands = [next(arg for arg in call if arg.startswith("COMMAND="))
                    for call in self.builds()]
        self.assertEqual(commands, [
            "COMMAND=just HOST_TARGET=x86_64-unknown-linux-gnu local-host",
            "COMMAND=python3 rust/tools/build_firmware.py rust/boards/pico_w.json --profile development",
        ])

    def test_build_assignments_override_environment_and_reach_docker(self):
        self.run_just("CORDIAL_VERSION=1.2.3", "CORDIAL_HOMEPAGE=https://example.invalid/project",
                      "SOURCE_DATE_EPOCH=123", "package-host-tar",
                      CORDIAL_VERSION="4.5.6", SOURCE_DATE_EPOCH="456")
        self.assertIn("SOURCE_DATE_EPOCH=123", self.builds()[-1])
        env = json.loads((self.root / "environment.jsonl").read_text().splitlines()[-1])
        self.assertEqual(env, dict(CORDIAL_VERSION="1.2.3",
                                   CORDIAL_HOMEPAGE="https://example.invalid/project"))

    def test_build_pins_environment_and_exports_only_current_outputs(self):
        def run(image, runtime=None):
            self.run_just("package-host-tar", image=image, runtime=runtime)
            return self.builds()[-1]

        release = self.root / "build/release"
        release.mkdir(parents=True)
        (release / "cordial-local-linux-amd64.tar.gz").write_text("local")
        first = run("a" * 64)
        scratch = self.root / ".cache/docker/package-host-tar/out/build/release"
        scratch.mkdir(parents=True, exist_ok=True)
        (scratch / "cordial-stale-linux-amd64.tar.gz").write_text("stale")
        same = run("a" * 64)
        self.assertEqual(first, same)
        self.assertFalse((self.root / "build/release/cordial-stale-linux-amd64.tar.gz").exists())
        second = run("b" * 64)
        key = lambda call: next(arg for arg in call if arg.startswith("CACHE_ID="))
        self.assertEqual(key(first), key(second))
        self.assertEqual(first, second)
        reference = "cordial-toolchain:" + key(first).split("=", 1)[1]
        self.assertIn("IMAGE=" + reference, first)
        state = self.root / "images.json"
        self.assertEqual(json.loads(state.read_text())[reference]["id"], "sha256:" + "a" * 64)
        self.assertEqual([call for call in self.calls() if call[0] == "tag"],
                         [["tag", "sha256:" + "a" * 64, reference]])
        for index, (field, value) in enumerate((
            ("RootFS", {"Layers": ["different toolchain"]}),
            ("Config", {"Env": ["PATH=/usr/bin"]}),
            ("Os", "windows"),
            ("Architecture", "arm64"),
            ("Variant", "v8"),
        )):
            with self.subTest(field=field):
                changed = run(str(index) * 64, dict(self.runtime, **{field: value}))
                self.assertNotEqual(key(second), key(changed))
                self.assertIn("IMAGE=cordial-toolchain:" + key(changed).split("=", 1)[1], changed)
        images = json.loads(state.read_text())
        images[reference] = images["sha256:" + "0" * 64]
        state.write_text(json.dumps(images))
        repaired = run("b" * 64)
        self.assertEqual(second, repaired)
        self.assertEqual(json.loads(state.read_text())[reference]["id"], "sha256:" + "b" * 64)
        self.dependencies.write_text("different pinned dependency")
        third = run("b" * 64)
        key = lambda call: next(arg for arg in call if arg.startswith("DEPENDENCY_ID="))
        self.assertNotEqual(key(second), key(third))
        exported = self.root / "build/release/cordial-0.0.0-linux-amd64.tar.gz"
        self.assertEqual(exported.read_text(), "current")
        failed = self.run_just("package-host-tar", image="b" * 64, fail=True, check=False)
        self.assertNotEqual(failed.returncode, 0)
        self.assertEqual(exported.read_text(), "current")


class LocalRecipes(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        shutil.copy2(ROOT / "justfile", self.root / "justfile")
        shutil.copytree(ROOT / "packaging", self.root / "packaging")
        (self.root / "rust").mkdir()
        commands = self.root / "commands"
        commands.mkdir()
        recorder = commands / "recorder"
        recorder.write_text('''#!/usr/bin/env python3
import json, os
from pathlib import Path
import sys
name = Path(sys.argv[0]).name
with open(os.environ["COMMAND_LOG"], "a") as log:
    log.write(json.dumps(dict(tool=name, args=sys.argv[1:], cwd=str(Path.cwd()),
                             version=os.environ.get("CORDIAL_VERSION"),
                             epoch=os.environ.get("SOURCE_DATE_EPOCH"),
                             schemas=os.environ.get("GSETTINGS_SCHEMA_DIR"),
                             checksum=os.environ.get("CORDIAL_ARCHIVE_SHA256"),
                             homepage=os.environ.get("CORDIAL_HOMEPAGE"))) + "\\n")
if name == "docker":
    raise SystemExit("Local recipes must not invoke Docker")
elif name == "cargo" and "--message-format=json-render-diagnostics" in sys.argv:
    print(json.dumps(dict(reason="compiler-artifact", target=dict(name="dependency"), executable=None)))
    if os.environ.get("FIXTURE_CARGO_FAIL"):
        print("error: fixture compilation failed", file=sys.stderr)
        print(json.dumps(dict(reason="build-finished", success=False)))
        raise SystemExit(101)
    executable = (Path(os.environ.get("CARGO_TARGET_DIR") or "target") / "debug/cordial").resolve()
    executable.parent.mkdir(parents=True, exist_ok=True)
    executable.write_bytes(Path(__file__).read_bytes())
    executable.chmod(0o755)
    print(json.dumps(dict(reason="compiler-artifact", target=dict(name="cordial"), executable=str(executable))))
    print(json.dumps(dict(reason="build-finished", success=True)))
elif name == "cordial" and sys.argv[1:3] == ["file", "put"]:
    print(Path(sys.argv[3]).read_text())
elif name == "python-fixture" and sys.argv[1] == "-c":
    os.execv(sys.executable, [sys.executable, *sys.argv[1:]])
elif name == "python-fixture" and sys.argv[1] == "tools/version.py":
    version = os.environ["CORDIAL_VERSION"]
    print("base-" + version if "--protocol-base" in sys.argv else version)
elif name == "python-fixture" and sys.argv[1] == "tools/check_release_archive.py":
    print("123")
elif name == "dpkg-parsechangelog":
    print("Fixture <fixture@example.invalid>")
''')
        recorder.chmod(0o755)
        for name in ("python-fixture", "buf-fixture", "cargo", "glib-compile-schemas", "makepkg",
                     "fakeroot", "dpkg-buildpackage", "dpkg-parsechangelog", "dch", "docker"):
            (commands / name).symlink_to(recorder.name)
        self.env = fixture_environment(PATH=str(commands) + os.pathsep + os.environ["PATH"],
                                       CARGO_TARGET_DIR=str(self.root / "rust/target"),
                                       COMMAND_LOG=str(self.root / "commands.jsonl"))
        self.env.pop("GSETTINGS_SCHEMA_DIR", None)

    def run_just(self, *args, packaging=False, cwd=None, check=True, **env):
        return subprocess.run([
            os.environ.get("JUST", "just"),
            *(["--justfile", "packaging/justfile"] if packaging else []),
            "PYTHON=" + str(self.root / "commands/python-fixture"), *args],
            cwd=cwd or self.root, env=dict(self.env, **env), check=check, capture_output=True, text=True)

    def calls(self):
        return [json.loads(line) for line in (self.root / "commands.jsonl").read_text().splitlines()]

    def test_gtk_checks_use_the_discovering_python_runner(self):
        self.run_just("check-gtk")
        self.assertEqual(self.calls()[-1]["tool"], "python-fixture")
        self.assertEqual(self.calls()[-1]["args"], ["rust/tools/test_gtk.py"])

    def test_local_packages_share_schema_preparation_and_release_build(self):
        self.run_just("HOST_TARGET=fixture-target", "SOURCE_DATE_EPOCH=321", "local-host",
                      "local-package-host-tar", "local-package-host-appimage", SOURCE_DATE_EPOCH="456")
        cargo = [c for c in self.calls() if c["tool"] == "cargo"]
        self.assertEqual(len(cargo), 1)
        self.assertEqual(cargo[0]["cwd"], str(self.root / "rust"))
        self.assertEqual(cargo[0]["epoch"], "321")
        self.assertEqual(cargo[0]["args"], ["build", "--locked", "--release", "-p", "cordial",
                                           "--bin", "cordial", "--target", "fixture-target"])
        schemas = self.root / "rust/target/fixture-target/release/share/glib-2.0/schemas"
        self.assertEqual((schemas / "dev.petris.cordial.gschema.xml").read_bytes(),
                         (ROOT / "packaging/dev.petris.cordial.gschema.xml").read_bytes())
        packaging = [c for c in self.calls() if c["args"][0] == "rust/tools/package_host.py"]
        self.assertEqual(len(packaging), 1)
        self.assertEqual(packaging[0]["args"], ["rust/tools/package_host.py", "--target", "fixture-target"])

    def test_cordial_runs_local_cargo_and_preserves_arguments(self):
        for args in ([], ["desktop", "--json"],
                     ["", "two words", "quote'and\"", "$HOME", "a;b", "BOARD=app", "--", "-x"]):
            with self.subTest(args=args):
                self.run_just("HOST_TARGET=fixture-target", "SOURCE_DATE_EPOCH=321",
                              "cordial", *args, SOURCE_DATE_EPOCH="456")
                cargo = [c for c in self.calls() if c["tool"] == "cargo"][-1]
                self.assertEqual(cargo["args"],
                                 ["build", "--locked", "-p", "cordial", "--bin", "cordial",
                                  "--message-format=json-render-diagnostics"])
                self.assertEqual(cargo["cwd"], str(self.root / "rust"))
                self.assertEqual(cargo["epoch"], "321")
                application = [c for c in self.calls() if c["tool"] == "cordial"][-1]
                self.assertEqual(application["args"], args)
                self.assertEqual(application["cwd"], str(self.root))
                self.assertEqual(application["epoch"], "321")
                schemas = str(self.root / "rust/target/debug/share/glib-2.0/schemas")
                self.assertIsNone(cargo["schemas"])
                self.assertIsNone(application["schemas"])
                compiler = [c for c in self.calls() if c["tool"] == "glib-compile-schemas"][-1]
                self.assertEqual(compiler["args"], ["--strict", "--targetdir", schemas, "../packaging"])
        self.assertFalse(any(c["tool"] == "docker" for c in self.calls()))
        self.assertFalse((self.root / "build").exists())

    def test_cordial_relative_files_use_root_and_nested_invocation_directories(self):
        nested = self.root / "nested directory/child"
        nested.mkdir(parents=True)
        for directory, contents in ((self.root, "root file"), (nested, "nested file")):
            with self.subTest(directory=directory):
                (directory / "relative file").write_text(contents)
                result = self.run_just("cordial", "file", "put", "relative file", "/data.bin", cwd=directory)
                self.assertEqual(result.stdout.strip(), contents)
                cargo = [c for c in self.calls() if c["tool"] == "cargo"][-1]
                self.assertEqual(cargo["cwd"], str(self.root / "rust"))

    def test_cordial_uses_cargo_artifact_path_with_custom_target_directories(self):
        for target in (str(self.root / "absolute target"), "relative target"):
            with self.subTest(target=target):
                self.run_just("cordial", "--version", CARGO_TARGET_DIR=target)
                executable = (self.root / "rust" / target / "debug/cordial").resolve()
                self.assertTrue(executable.is_file())
                self.assertEqual(self.calls()[-1]["tool"], "cordial")
                self.assertEqual(self.calls()[-1]["cwd"], str(self.root))
                compiler = [c for c in self.calls() if c["tool"] == "glib-compile-schemas"][-1]
                schemas = executable.parent / "share/glib-2.0/schemas"
                self.assertEqual(compiler["args"],
                                 ["--strict", "--targetdir", str(schemas), "../packaging"])
                self.assertTrue(schemas.is_dir())

    def test_cordial_does_not_run_an_artifact_from_a_failed_build(self):
        self.run_just("cordial", "--version")
        (self.root / "commands.jsonl").unlink()
        result = self.run_just("cordial", "--version", FIXTURE_CARGO_FAIL="1", check=False)
        self.assertEqual(result.returncode, 101)
        self.assertIn("error: fixture compilation failed", result.stderr)
        self.assertNotIn("Traceback", result.stderr)
        self.assertNotIn("ValueError", result.stderr)
        self.assertFalse(any(c["tool"] == "cordial" for c in self.calls()))
        self.assertFalse(any(c["tool"] == "glib-compile-schemas" for c in self.calls()))

    def test_cordial_empty_epoch_override_clears_inherited_value(self):
        self.run_just("SOURCE_DATE_EPOCH=", "cordial", "--version", SOURCE_DATE_EPOCH="789")
        cargo = next(c for c in self.calls() if c["tool"] == "cargo")
        self.assertIsNone(cargo["epoch"])

    def test_empty_epoch_override_clears_inherited_value_for_local_artifacts(self):
        self.run_just("SOURCE_DATE_EPOCH=", "local-package-host-appimage", SOURCE_DATE_EPOCH="789")
        artifacts = [c for c in self.calls() if c["tool"] == "cargo" or c["args"][0] in
                     ("rust/tools/package_host.py", "rust/tools/build_appimage.py")]
        self.assertEqual(len(artifacts), 3)
        self.assertTrue(all(c["epoch"] is None for c in artifacts))

    def test_check_commands_keep_workspace_directory_and_protocol_overrides(self):
        self.run_just("BUF=" + str(self.root / "commands/buf-fixture"),
                      "CORDIAL_VERSION=1.2.3", "PROTOCOL_STABLE_BASE=published",
                      "check-rust", "check-protocol", "check-tools")
        calls = self.calls()
        cargo = [c for c in calls if c["tool"] == "cargo"]
        self.assertEqual([c["args"] for c in cargo], [
            ["test", "--locked", "--workspace", "--all-features"],
            ["clippy", "--locked", "--workspace", "--all-targets", "--all-features", "--", "-D", "warnings"]])
        self.assertTrue(all(c["cwd"] == str(self.root / "rust") for c in cargo))
        self.assertTrue(any(c["args"] == ["tools/test_btstack.py"] and
                            c["cwd"] == str(self.root / "rust") for c in calls))
        self.assertTrue(any(c["args"] == ["tools/check_protocol.py", "--base", "base-1.2.3",
                                          "--stable-base", "published"] for c in calls))
        self.assertTrue(any(c["args"] == ["lint", str(self.root / "proto"), "--config", "buf.yaml"]
                            for c in calls))
        self.assertEqual(sum(c["args"][:3] == ["-m", "unittest", "discover"] for c in calls), 2)
        self.run_just("BUF=" + str(self.root / "commands/buf-fixture"),
                      "PROTOCOL_BASE=", "check-protocol")
        self.assertEqual(self.calls()[-1]["args"],
                         ["tools/check_protocol.py", "--base", "", "--stable-base", ""])

    def test_root_command_prefixes_reach_version_and_protocol_baseline(self):
        python = "python3 -B " + str(self.root / "commands/python-fixture")
        result = self.run_just("PYTHON=" + python, "CORDIAL_VERSION=1.2.3", "version")
        self.assertEqual(result.stdout.strip(), "1.2.3")
        self.run_just("PYTHON=" + python, "BUF=env buf-fixture", "CORDIAL_VERSION=1.2.3",
                      "DEB_DISTRIBUTION=unknown", "check-protocol", "check-rust", "check-tools")
        self.assertTrue(any(c["args"] == ["tools/check_protocol.py", "--base", "base-1.2.3",
                                          "--stable-base", ""] for c in self.calls()))
        self.assertTrue(any(c["tool"] == "buf-fixture" and c["args"][0] == "lint"
                            for c in self.calls()))

    def test_packaging_version_defaults_environment_and_cli_overrides(self):
        archive = self.root / "archive.tar.gz"
        archive.write_bytes(b"archive")
        for assignments, environment, expected in (
            ([], {}, "0.0.0"),
            ([], {"CORDIAL_VERSION": "1.2.3"}, "1.2.3"),
            (["CORDIAL_VERSION=2.3.4"], {"CORDIAL_VERSION": "1.2.3"}, "2.3.4"),
        ):
            with self.subTest(expected=expected):
                self.run_just("HOST_ARCHIVE=" + str(archive), *assignments, "arch",
                              packaging=True, **environment)
                self.assertEqual(self.calls()[-1]["version"], expected)

    def test_packaging_distribution_defaults_without_os_release(self):
        justfile = self.root / "packaging/justfile"
        justfile.write_text(justfile.read_text().replace("/etc/os-release", "/missing-os-release"))
        result = subprocess.run(
            [os.environ.get("JUST", "just"), "--justfile", "packaging/justfile",
             "--evaluate", "DEB_DISTRIBUTION"], cwd=self.root, env=self.env,
            check=True, capture_output=True, text=True)
        self.assertEqual(result.stdout.strip(), "unstable")
        archive = self.root / "archive.tar.gz"
        archive.write_bytes(b"archive")
        self.run_just("HOST_ARCHIVE=" + str(archive), "arch", packaging=True)
        self.assertEqual(self.calls()[-1]["tool"], "makepkg")

    def test_native_packaging_keeps_archive_epoch_and_package_configuration(self):
        archive = self.root / "supplied archive.tar.gz"
        with tarfile.open(archive, "w:gz") as tar:
            member = tarfile.TarInfo("cordial-1.2.3-linux-amd64/LICENSE")
            member.size = 7
            tar.addfile(member, io.BytesIO(b"license"))
        self.run_just("CORDIAL_VERSION=1.2.3", "HOST_ARCHIVE=" + str(archive), "DEB_DISTRIBUTION=fixture",
                      "DEB_REVISION=2~fixture", "arch", "deb", packaging=True,
                      CORDIAL_HOMEPAGE="https://example.invalid/project")
        calls = self.calls()
        makepkg = next(c for c in calls if c["tool"] == "makepkg")
        self.assertEqual(makepkg["epoch"], "123")
        self.assertEqual(makepkg["version"], "1.2.3")
        self.assertEqual(makepkg["checksum"], hashlib.sha256(archive.read_bytes()).hexdigest())
        self.assertEqual(makepkg["homepage"], "https://example.invalid/project")
        output = self.root / "build/packages/host-arch"
        self.assertEqual(makepkg["cwd"], str(output))
        self.assertEqual(makepkg["args"], ["--force", "--cleanbuild", "--nosign",
                                          "PKGDEST=" + str(output), "PKGEXT=.pkg.tar.zst"])
        deb = next(c for c in calls if c["tool"] == "dpkg-buildpackage")
        self.assertEqual(deb["cwd"], str(self.root / "build/packages/host-deb/fixture/cordial-1.2.3"))
        self.assertEqual(deb["epoch"], "123")
        self.assertEqual(deb["args"], ["--build=binary", "--no-sign"])
        dch = next(c for c in calls if c["tool"] == "dch")
        self.assertIn("1.2.3-2~fixture", dch["args"])
        self.assertIn("fixture", dch["args"])
        self.run_just("CORDIAL_VERSION=1.2.3", "HOST_ARCHIVE=" + str(output / "cordial-1.2.3-linux-amd64.tar.gz"),
                      "SOURCE_DATE_EPOCH=456", "arch", packaging=True, SOURCE_DATE_EPOCH="789")
        self.assertEqual(self.calls()[-1]["epoch"], "456")


if __name__ == "__main__":
    unittest.main()
