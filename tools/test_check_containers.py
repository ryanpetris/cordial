"""Exercise smoke launch commands without installing packages or launching a desktop."""
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]


class SmokeLaunch(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        commands = self.root / "commands"
        commands.mkdir()
        recorder = commands / "recorder"
        recorder.write_text('''#!/usr/bin/env python3
import json, os, sys
from pathlib import Path
with open(os.environ["COMMAND_LOG"], "a") as log:
    log.write(json.dumps(dict(tool=Path(sys.argv[0]).name, args=sys.argv[1:],
        environment={key: os.environ.get(key) for key in (
            "CORDIAL_TEST_BINARY", "LIBGL_ALWAYS_SOFTWARE", "GALLIUM_DRIVER", "VK_DRIVER_FILES",
            "GSETTINGS_BACKEND", "HOME")})) + "\\n")
if Path(sys.argv[0]).name == "xvfb-run":
    output = Path(next(arg for arg in sys.argv if arg.startswith("build/") and "-smoke" in arg))
    output.mkdir(parents=True, exist_ok=True)
    (output / "evidence.txt").write_text("evidence")
    sys.exit(int(os.environ.get("FIXTURE_EXIT", "0")))
''')
        recorder.chmod(0o755)
        for name in ("apt-get", "xvfb-run"):
            (commands / name).symlink_to(recorder.name)
        self.log = self.root / "commands.jsonl"
        self.env = {key: value for key, value in os.environ.items()
                    if key not in ("LIBGL_ALWAYS_SOFTWARE", "GALLIUM_DRIVER", "VK_DRIVER_FILES", "GSETTINGS_BACKEND",
                                   "CHECK_UID", "CHECK_GID")}
        self.env.update(PATH=str(commands) + os.pathsep + os.environ["PATH"],
                        CORDIAL_VERSION="1.2.3", DEB_REVISION="2~resolute", COMMAND_LOG=str(self.log))

    def calls(self):
        return [json.loads(line) for line in self.log.read_text().splitlines()]

    def test_appimage_launch_preserves_distribution_renderers_and_default_backend(self):
        image = self.root / "build/release/cordial-1.2.3-x86_64.AppImage"
        image.parent.mkdir(parents=True)
        image.touch()
        vulkan = self.root / "vulkan"
        vulkan.mkdir()
        driver = vulkan / "lvp_icd.fixture.json"
        driver.touch()
        script = self.root / "check-appimage.sh"
        shutil.copy2(ROOT / "docker/check-output.sh", self.root / "check-output.sh")
        script.write_text((ROOT / "docker/check-appimage.sh").read_text().replace(
            "/usr/share/vulkan/icd.d", str(vulkan)))
        for distribution in ("resolute", "arch"):
            subprocess.run(["bash", str(script), distribution], cwd=self.root, env=self.env, check=True)
        resolute, arch = self.calls()
        self.assertEqual(resolute["args"], [
            "-a", "-s", "-screen 0 1280x1024x24", "dbus-run-session", "--", "python3",
            "tools/check_appimage.py", str(image.relative_to(self.root)), "build/appimage-smoke-resolute"])
        self.assertEqual(resolute["environment"]["LIBGL_ALWAYS_SOFTWARE"], "1")
        self.assertEqual(resolute["environment"]["GALLIUM_DRIVER"], "llvmpipe")
        self.assertEqual(resolute["environment"]["VK_DRIVER_FILES"], str(driver))
        self.assertEqual(arch["args"][-3:], ["build/appimage-smoke-arch", "--renderer", "cairo"])
        self.assertIsNone(arch["environment"]["VK_DRIVER_FILES"])
        self.assertTrue(all(call["environment"]["GSETTINGS_BACKEND"] is None for call in self.calls()))
        self.assertTrue(os.access(image, os.X_OK))
        self.assertTrue(all(not Path(call["environment"]["HOME"]).exists() for call in self.calls()))
        failed = subprocess.run(["bash", str(script), "arch"], cwd=self.root,
                                env=dict(self.env, FIXTURE_EXIT="17"))
        self.assertEqual(failed.returncode, 17)
        self.assertTrue((self.root / "build/appimage-smoke-arch/evidence.txt").is_file())
        self.assertFalse(Path(self.calls()[-1]["environment"]["HOME"]).exists())

    def test_installed_desktop_checks_all_scenarios_with_installed_binary(self):
        subprocess.run(["bash", str(ROOT / "docker/check-installed-desktop.sh")],
                       cwd=self.root, env=self.env, check=True)
        install, *launches = self.calls()
        self.assertEqual(install["args"], ["install", "-y", "--no-install-recommends",
            "./build/packages/host-deb/resolute/cordial_1.2.3-2~resolute_amd64.deb"])
        self.assertEqual([call["args"][7:] for call in launches], [
            ["build/package-smoke"],
            ["build/empty-states-smoke", "--empty-states"],
            ["build/pairing-reconnect-smoke", "--pairing-reconnect"],
        ])
        self.assertTrue(all(call["args"][:7] == [
            "-a", "-s", "-screen 0 1280x1024x24", "dbus-run-session", "--", "python3", "rust/tests/gtk_smoke.py"
        ] for call in launches))
        self.assertTrue(all(call["environment"]["CORDIAL_TEST_BINARY"] == "/usr/bin/cordial" for call in launches))
        self.assertTrue(all(not Path(call["environment"]["HOME"]).exists() for call in launches))

    def test_failed_smoke_keeps_evidence_and_cleans_its_home(self):
        result = subprocess.run(["bash", str(ROOT / "docker/check-installed-desktop.sh")],
                                cwd=self.root, env=dict(self.env, FIXTURE_EXIT="17"))
        self.assertEqual(result.returncode, 17)
        self.assertTrue((self.root / "build/package-smoke/evidence.txt").is_file())
        self.assertFalse(Path(self.calls()[-1]["environment"]["HOME"]).exists())


class OutputOwnership(unittest.TestCase):
    def test_cleanup_limits_ownership_changes_and_preserves_exit_status(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            commands = root / "commands"
            commands.mkdir()
            (commands / "id").write_text("#!/bin/sh\necho 0\n")
            (commands / "chown").write_text('#!/bin/sh\nprintf "%s\\n" "$*" >> "$COMMAND_LOG"\n')
            for command in commands.iterdir():
                command.chmod(0o755)
            env = dict(os.environ, PATH=str(commands) + os.pathsep + os.environ["PATH"],
                       CHECK_UID="1234", CHECK_GID="5678", COMMAND_LOG=str(root / "ownership.log"))
            for status in (0, 17):
                (root / "ownership.log").unlink(missing_ok=True)
                result = subprocess.run(["bash", "-euo", "pipefail", "-c", '''
source "$1"
check_output build/smoke release/package release/SHA256SUMS
mkdir -p build/smoke release
touch build/smoke/evidence release/package release/SHA256SUMS
printf '%s' "$HOME" > temporary-home
cd release
exit "$2"
''', "check-output", str(ROOT / "docker/check-output.sh"), str(status)],
                    cwd=root, env=env)
                self.assertEqual(result.returncode, status)
                self.assertFalse(Path((root / "temporary-home").read_text()).exists())
                changes = (root / "ownership.log").read_text().splitlines()
                self.assertTrue({f"-R 1234:5678 {root}/build/smoke", f"-R 1234:5678 {root}/release/package",
                                 f"-R 1234:5678 {root}/release/SHA256SUMS", f"1234:5678 {root}/build",
                                 f"1234:5678 {root}/release"}.issubset(changes))
                self.assertFalse(any(line in changes for line in
                                     (f"-R 1234:5678 {root}/build", f"-R 1234:5678 {root}/release",
                                      f"-R 1234:5678 {root}", f"1234:5678 {root}")))


if __name__ == "__main__":
    unittest.main()
