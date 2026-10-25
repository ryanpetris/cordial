"""Offline policy tests. These tests never open a hardware device or mount."""
import importlib.util
import json
from pathlib import Path
import struct
import sys
from types import SimpleNamespace
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).parents[1] / "tools"))

spec = importlib.util.spec_from_file_location("flash_dev", Path(__file__).parents[1] / "tools/flash_dev.py")
flash = importlib.util.module_from_spec(spec)
spec.loader.exec_module(flash)


def image(**changes):
    import firmware_artifact as artifact
    import firmware_config
    config = firmware_config.load(firmware_config.ROOT / "boards/pico_w.json", "development")
    manifest = artifact.manifest(config) | changes
    payload = flash.MAGIC + json.dumps(manifest).encode() + b"\0"
    return artifact.uf2(payload)


class FlashPolicy(unittest.TestCase):
    def test_development(self):
        self.assertEqual(flash.verify_development(image(), "pico_w")["profile"], "development")

    def test_debug(self):
        data = image(profile="debug", version="1.2.3-debug")
        self.assertEqual(flash.verify_development(data, "pico_w")["version"], "1.2.3-debug")

    def test_production_rejected_before_hardware_access(self):
        data = image(profile="production", bootloader=None)
        manifest = flash.inspect_uf2(data)
        with patch.object(flash, "mounted_target", side_effect=AssertionError("hardware access")):
            with self.assertRaisesRegex(ValueError, "Refusing production"):
                flash.write_development(data, manifest, "/unused")

    def test_hardware_and_bootloader(self):
        for changes in (dict(hardware="custom"), dict(bootloader=None)):
            with self.subTest(changes=changes), self.assertRaises(ValueError):
                flash.verify_development(image(**changes), "pico_w")

    def test_disconnect_after_submission_is_uncertain(self):
        data = image(); manifest = flash.inspect_uf2(data)
        with patch.object(flash, "mounted_target", return_value=(Path("/unused"), 42, Path("/usb"))), \
                patch.object(flash.os, "open", return_value=1000), \
                patch.object(flash.os, "fstat", return_value=SimpleNamespace(st_dev=42)), \
                patch.object(flash.os, "write", side_effect=lambda fd, value: len(value)), \
                patch.object(flash.os, "fsync", side_effect=OSError("device disconnected")), \
                patch.object(flash.os, "close"):
            with self.assertRaises(flash.SubmissionUnconfirmed):
                flash.write_development(data, manifest, "/unused")

    def test_changed_mount_prevents_firmware_write(self):
        data = image(); manifest = flash.inspect_uf2(data)
        with patch.object(flash, "mounted_target", return_value=(Path("/unused"), 42, Path("/usb"))), \
                patch.object(flash.os, "open", return_value=1000), \
                patch.object(flash.os, "fstat", return_value=SimpleNamespace(st_dev=99)), \
                patch.object(flash.os, "close"), patch.object(flash.os, "write") as write, \
                self.assertRaisesRegex(ValueError, "Mount changed"):
            flash.write_development(data, manifest, "/unused")
        write.assert_not_called()

    def test_corruption(self):
        data = image()
        variants = [b"", data[:-1], data + data, data[512:], data[:512] + data[:512]]
        for offset, value in ((0, 0), (8, 0), (12, 0x20000000), (16, 512), (20, 500), (28, 123)):
            changed = bytearray(data); struct.pack_into("<I", changed, offset, value); variants.append(changed)
        for corrupted in variants:
            with self.subTest(length=len(corrupted)), self.assertRaises(ValueError):
                flash.inspect_uf2(corrupted)

    def test_rp2350_erratum_and_storage(self):
        import firmware_artifact as artifact
        import firmware_config
        manifest = artifact.manifest(firmware_config.load(firmware_config.ROOT / "boards/pico2_w.json", "development"))
        payload = artifact.MAGIC + json.dumps(manifest).encode() + b"\0"
        data = artifact.uf2(payload)
        self.assertEqual(flash.inspect_uf2(data), manifest)
        self.assertEqual(struct.unpack_from("<I", data, 12)[0], 0x101fff00)
        with self.assertRaisesRegex(ValueError, "workaround"):
            flash.inspect_uf2(data[512:])
        for offset, value in [(12, 0x103fff00), (12, 0x101ff000), (32, 0)]:
            changed = bytearray(data)
            struct.pack_into("<I", changed, offset, value)
            with self.assertRaisesRegex(ValueError, "workaround"):
                flash.inspect_uf2(changed)

    def test_validator_rejects_application_pages_in_e10_sector(self):
        import firmware_artifact as artifact
        import firmware_config
        manifest = artifact.manifest(firmware_config.load(firmware_config.ROOT / "boards/pico2_w.json", "debug"))
        payload = (artifact.MAGIC + json.dumps(manifest).encode() + b"\0").ljust(0x1ff000, b"\xff")
        data = bytearray(artifact.uf2(payload))
        # Extend an otherwise valid stream by one application page into the reserved sector.
        count = (len(data) - 512) // 512
        page = bytearray(data[-512:])
        struct.pack_into("<I", page, 12, 0x101ff000)
        struct.pack_into("<I", page, 20, count)
        data.extend(page)
        for at in range(512, len(data), 512):
            struct.pack_into("<I", data, at + 24, count + 1)
        with self.assertRaisesRegex(ValueError, "overlaps"):
            flash.inspect_uf2(data)


if __name__ == "__main__":
    unittest.main()
