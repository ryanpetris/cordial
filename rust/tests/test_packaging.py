"""Offline package publication and ESP support-image layout checks."""
import hashlib
import io
import json
import os
from pathlib import Path
import struct
import subprocess
import sys
import tempfile
import unittest
from contextlib import redirect_stdout
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "tools"))
import build_firmware as build
from dependency_notices import newlib_notice


class EspBuild(unittest.TestCase):
    def cargo(self, messages, returncode=0):
        script = ('import sys; assert sys.argv[-1] == "--message-format=json-render-diagnostics"; '
                  'sys.stdout.write(sys.argv[1]); '
                  'sys.stderr.write("Cargo diagnostic\\n" if int(sys.argv[2]) else ""); '
                  'sys.exit(int(sys.argv[2]))')
        return [sys.executable, "-c", script, messages, str(returncode)]

    def test_current_sdk_is_packaged_with_multiple_cached_outputs(self):
        for fresh in (False, True):
            with self.subTest(fresh=fresh), tempfile.TemporaryDirectory() as temp:
                root = Path(temp)
                current = root / "build/esp-idf-sys-current/out/build"
                stale = root / "build/esp-idf-sys-stale/out/build"
                settings = {"flash_size": "8MB", "flash_mode": "dio", "flash_freq": "80m"}
                for sdk, label in ((current, b"current"), (stale, b"stale")):
                    (sdk / "bootloader").mkdir(parents=True)
                    (sdk / "partition_table").mkdir()
                    selected = settings if sdk == current else settings | {"flash_size": "4MB"}
                    (sdk / "flasher_args.json").write_text(json.dumps({"flash_settings": selected}))
                    (sdk.parent / "sdkconfig").write_text(
                        "CONFIG_ESP_REV_MIN_FULL=0\nCONFIG_ESP_REV_MAX_FULL=99\n"
                        "CONFIG_BT_CTRL_BLE_MAX_ACT=10\nCONFIG_BT_NIMBLE_MAX_CONNECTIONS=9\n"
                        "CONFIG_BT_NIMBLE_MAX_BONDS=9\n" if sdk == current else
                        "CONFIG_ESP_REV_MIN_FULL=1\nCONFIG_ESP_REV_MAX_FULL=2\n")
                    (sdk / "bootloader/bootloader.bin").write_bytes(label + b" bootloader")
                    (sdk / "partition_table/partition-table.bin").write_bytes(label + b" partitions")
                os.utime(current / "flasher_args.json", (1, 1))
                os.utime(stale / "flasher_args.json", (2, 2))
                messages = [
                    {"reason": "build-script-executed", "package_id": "registry+test#other@1.0.0",
                     "out_dir": str(stale.parent)},
                    {"reason": "build-script-executed", "package_id": "registry+test#esp-idf-sys@0.38.1",
                     "out_dir": str(current.parent)},
                    {"reason": "compiler-artifact", "package_id": "registry+test#esp-idf-sys@0.38.1",
                     "fresh": fresh},
                    {"reason": "build-finished", "success": True},
                ]
                sdk = build.esp_build(self.cargo("".join(json.dumps(m) + "\n" for m in messages)), os.environ)
                self.assertEqual(sdk, current)
                output = root / "package"
                output.mkdir()
                elf = root / "firmware.elf"
                elf.write_bytes(b"elf")
                def esptool(argv, env):
                    self.assertEqual(argv[argv.index("--min-rev-full") + 1], 0)
                    self.assertEqual(argv[argv.index("--max-rev-full") + 1], 99)
                    Path(argv[argv.index("--output") + 1]).write_bytes(b"application")
                with patch.object(build.subprocess, "check_output", return_value=""), \
                        patch.object(build, "esp_startup_frames", return_value={}), \
                        patch.object(build, "esp_image_header") as header, \
                        patch.object(build, "esp_partitions") as partitions, \
                        patch.object(build, "command", side_effect=esptool), \
                        patch.object(build.artifact, "inspect", return_value={}), \
                        patch.object(build.artifact, "manifest", return_value={}):
                    build.package({"chip": "esp32s3", "bluetooth_backend": "esp-nimble",
                                   "minimum_flash_bytes": 8 * 1048576,
                                   "storage_offset": 0x200000}, sdk, elf, output, "board",
                                  Path("gcc"), {"IDF_PYTHON_ENV_PATH": str(root)})
                self.assertEqual(header.call_args_list[0].args, (b"current bootloader", settings, (0, 99)))
                self.assertEqual(partitions.call_args.args[0], b"current partitions")
                self.assertEqual((output / "bootloader.bin").read_bytes(), b"current bootloader")
                self.assertEqual((output / "partition-table.bin").read_bytes(), b"current partitions")

    def test_invalid_or_missing_sdk_messages_fail_clearly(self):
        message = {"reason": "build-script-executed", "package_id": "registry+test#esp-idf-sys@0.38.1"}
        invalid = [None, "", "relative/out", 5, []]
        cases = [("", "Expected one esp-idf-sys"), ("{invalid\n", "Invalid Cargo JSON")]
        cases += [(json.dumps(message | {"out_dir": value}) + "\n", "invalid esp-idf-sys OUT_DIR")
                  for value in invalid]
        cases.append((json.dumps(message) + "\n", "invalid esp-idf-sys OUT_DIR"))
        valid = json.dumps(message | {"out_dir": str(build.ROOT / "target/out")}) + "\n"
        cases.append((valid * 2, "Expected one esp-idf-sys"))
        for messages, error in cases:
            with self.subTest(messages=messages), self.assertRaisesRegex(ValueError, error):
                build.esp_build(self.cargo(messages), os.environ)

    def test_diagnostics_and_failed_exit_survive_output_discovery(self):
        popen = subprocess.Popen
        with tempfile.TemporaryFile(mode="w+") as diagnostics, redirect_stdout(io.StringIO()) as output, \
                patch.object(build.subprocess, "Popen", side_effect=lambda *args, **kwargs:
                             popen(*args, stderr=diagnostics, **kwargs)) as spawn:
            with self.assertRaises(subprocess.CalledProcessError) as failure:
                build.esp_build(self.cargo("native tool output\n{invalid\n", 101), os.environ)
            self.assertEqual(failure.exception.returncode, 101)
            self.assertNotIn("stderr", spawn.call_args.kwargs)
            self.assertEqual(output.getvalue(), "native tool output\n")
            diagnostics.seek(0)
            self.assertEqual(diagnostics.read(), "Cargo diagnostic\n")

    def test_only_esp_builds_discover_sdk_and_pass_it_to_packaging(self):
        for board in ("pico2_w", "esp32s3"):
            for clippy in (False, True):
                with self.subTest(board=board, clippy=clippy), tempfile.TemporaryDirectory() as temp, \
                        patch.object(build, "ROOT", Path(temp, "rust")), \
                        patch.object(build.firmware_dependencies, "prepare_btstack"), \
                        patch.object(build, "tool", side_effect=Path), \
                        patch.object(build, "command") as command, \
                        patch.object(build, "esp_build", return_value=Path(temp, "sdk/build")) as esp, \
                        patch.object(build, "package") as package, \
                        patch.object(build, "release_docs"), \
                        patch.object(build.dependency_notices, "record"), \
                        patch.dict(os.environ, CORDIAL_VERSION="1.2.3", IDF_PATH=temp,
                                   IDF_PYTHON_ENV_PATH=temp, LIBCLANG_PATH=temp):
                    build.build(build.firmware_config.ROOT / "boards" / f"{board}.json",
                                "production", temp, clippy=clippy)
                    discover = board == "esp32s3" and not clippy
                    self.assertEqual(esp.called, discover)
                    self.assertEqual(command.called, not discover)
                    if clippy:
                        package.assert_not_called()
                    else:
                        self.assertEqual(package.call_args.args[1], esp.return_value if discover else None)
                        backend = "esp-nimble-esp-idf" if discover else "btstack-pico-sdk-cyw43"
                        self.assertEqual(package.call_args.args[4], f"{board}-{backend}")


class Packaging(unittest.TestCase):
    def test_newlib_notice_layouts(self):
        for relative in ('share/licenses/arm-none-eabi-newlib/COPYING.NEWLIB',
                         'share/doc/libnewlib-arm-none-eabi/copyright', 'license.txt'):
            with tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                notice = root / relative
                notice.parent.mkdir(parents=True, exist_ok=True)
                notice.write_text('newlib license')
                self.assertEqual(newlib_notice(root / 'bin/arm-none-eabi-gcc'), notice)

    def test_failed_package_keeps_previous_and_success_replaces_whole_package(self):
        with tempfile.TemporaryDirectory() as temp:
            output = Path(temp, "board")
            output.mkdir()
            (output / "old.bin").write_bytes(b"old")
            def populate(staged):
                (staged / "new.bin").write_bytes(b"new")
                raise ValueError("manifest mismatch")
            with self.assertRaisesRegex(ValueError, "manifest mismatch"):
                build.publish(output, populate)
            self.assertEqual(list(output.iterdir()), [output / "old.bin"])
            self.assertEqual((output / "old.bin").read_bytes(), b"old")
            build.publish(output, lambda staged: (staged / "new.bin").write_bytes(b"new"))
            self.assertEqual({p.name for p in output.iterdir()}, {"new.bin", "SHA256SUMS"})
            self.assertEqual((output / "SHA256SUMS").read_text(),
                             hashlib.sha256(b"new").hexdigest() + "  new.bin\n")

    def test_esp_rejects_wrong_images_and_partitions(self):
        config = {"storage_offset": 0x200000}
        entries = [(1, 1, 0xf000, 0x1000, b"phy_init"),
                   (0, 0, 0x10000, 0x1f0000, b"factory")]
        def table(rows):
            data = b"".join(struct.pack("<HBBII16sI", 0x50aa, *row, 0) for row in rows)
            data += b"\xeb\xeb" + b"\xff" * 14 + hashlib.md5(data).digest()
            return data.ljust(0xc00, b"\xff")
        data = table(entries)
        build.esp_partitions(data, config)
        # A wrong table with a valid checksum is still rejected.
        for malformed in (table(entries[:-1]), table(entries[::-1]),
                          data[:150] + b"\0" + data[151:], data + b"\xff"):
            with self.assertRaises(ValueError):
                build.esp_partitions(malformed, config)
        settings = {"flash_mode": "dio", "flash_freq": "80m", "flash_size": "8MB"}
        header = bytearray(24)
        header[0], header[2], header[3] = 0xe9, 2, 0x3f
        struct.pack_into("<H", header, 12, 9)
        struct.pack_into("<HH", header, 15, 0, 99)
        build.esp_image_header(header, settings, (0, 99))
        for offset in (0, 2, 3, 12, 15, 17):
            wrong = bytearray(header)
            wrong[offset] ^= 1
            with self.assertRaises(ValueError):
                build.esp_image_header(wrong, settings, (0, 99))


if __name__ == "__main__":
    unittest.main()
