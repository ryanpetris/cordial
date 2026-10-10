"""Offline policy tests. These tests never open a hardware device or mount."""
import importlib.util
import ctypes
import contextlib
import json
from pathlib import Path
import struct
import sys
import hashlib
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).parents[1] / "tools"))

spec = importlib.util.spec_from_file_location("flash_dev", Path(__file__).parents[1] / "tools/flash_dev.py")
flash = importlib.util.module_from_spec(spec)
spec.loader.exec_module(flash)


def image(**changes):
    manifest = dict(schema=1, profile="development", bootloader="bootsel", hardware="pico_w",
                    hardware_digest="0" * 64, chip="rp2040", package="rp2040",
                    flash_bytes=2097152, storage_offset=2097152 - 131072, version="0.0.0", storage_bytes=131072, erase_bytes=4096,
                    bluetooth_backend="btstack", radio_backend="pico-sdk-cyw43",
                    usb_backend="embassy", storage_backend="littlefs")
    manifest.update(changes)
    layout = dict(format="littlefs-2",
                  storage_backend=manifest["storage_backend"], offset=manifest["storage_offset"],
                  bytes=manifest["storage_bytes"], erase_bytes=4096)
    manifest["storage_identity"] = hashlib.sha256(json.dumps(layout, sort_keys=True, separators=(",", ":")).encode()).hexdigest()
    payload = flash.MAGIC + json.dumps(manifest).encode() + b"\0"
    payload += b"\0" * (-len(payload) % 256)
    blocks = []
    for i in range(len(payload) // 256):
        blocks.append(struct.pack("<8I", 0x0A324655, 0x9E5D5157, 0x2000, 0x10000000 + 256 * i,
                                  256, i, len(payload) // 256, flash.FAMILIES[manifest["chip"]]) +
                      payload[i * 256:(i + 1) * 256] + b"\0" * 220 + struct.pack("<I", 0x0AB16F30))
    return b"".join(blocks)


class FakeROM:
    """Emulate USB descriptors, ROM acknowledgements and the flash address space."""

    def __init__(self, manifest):
        self.manifest = manifest
        self.memory = bytearray(b"\xa5" * manifest["flash_bytes"])
        metadata = flash.MAGIC + json.dumps(manifest).encode() + b"\0"
        self.memory[:len(metadata)] = metadata
        start = manifest["storage_offset"]
        self.memory[start:start+32] = bytes.fromhex(manifest["storage_identity"])
        self.commands, self.writes = [], []
        self.token, self.command, self.args = 0, 0, b""
        self.fail_erase = False
        self.fail_erase_after = None
        self.erases = 0
        self.bad_status = False
        self.xip = True

    def ioctl(self, fd, request, transfer):
        if isinstance(transfer, flash._USBControl):
            value = b""
            if transfer.request == 6:
                if transfer.value == 0x0100:
                    descriptor = bytearray(18); descriptor[:2] = b"\x12\x01"
                    pid = 3 if self.manifest["chip"] == "rp2040" else 15
                    struct.pack_into("<HH", descriptor, 8, 0x2e8a, pid)
                    value = bytes(descriptor)
                elif transfer.value == 0x0200:
                    value = (bytes((9, 2, 32, 0, 1, 1, 0, 0x80, 50)) +
                             bytes((9, 4, 0, 0, 2, 0xff, 0, 0, 0)) +
                             bytes((7, 5, 0x03, 2, 64, 0, 0)) + bytes((7, 5, 0x84, 2, 64, 0, 0)))
            elif transfer.request == 8:
                value = b"\x01"
            elif transfer.request == 0x42:
                value = struct.pack("<IIBB6x", self.token, int(self.bad_status), self.command, 0)
            value = value[:transfer.length]
            ctypes.memmove(transfer.data, value, len(value))
            return len(value)
        if isinstance(transfer, flash._USBBulk):
            if transfer.endpoint == 3 and transfer.length == 32:
                packet = ctypes.string_at(transfer.data, 32)
                magic, self.token, self.command, size, reserved, count, args = struct.unpack("<IIBBHI16s", packet)
                assert magic == 0x431fd10b and reserved == 0
                self.args = args[:size]
                self.commands.append((self.command, self.args, count))
                if self.command == 6:
                    self.xip = False
                elif self.command == 7:
                    # Model command intent, not RP2040's standalone no-op.
                    self.xip = True
                if self.command in (3, 0x84):
                    assert not self.xip, "ROM flash access requires EXIT_XIP"
                if self.command == 3:
                    if self.fail_erase or self.erases == self.fail_erase_after:
                        raise OSError("simulated erase failure")
                    self.erases += 1
                    start, length = struct.unpack("<II", self.args)
                    offset = start - flash.FLASH_BASE
                    assert offset >= 0 and offset + length <= len(self.memory)
                    self.memory[offset:offset + length] = b"\xff" * length
                return 32
            if transfer.endpoint == 0x84 and self.command == 0x84 and transfer.length > 1:
                address, size = struct.unpack("<II", self.args)
                offset = address - flash.FLASH_BASE
                value = bytes(self.memory[offset:offset + size])
                ctypes.memmove(transfer.data, value, len(value))
                return len(value)
            return 0
        return 0

    @contextlib.contextmanager
    def environment(self):
        with tempfile.TemporaryDirectory() as temporary:
            usb = Path(temporary)
            for name, value in (("busnum", "3"), ("devnum", "5"), ("dev", "189:260")):
                (usb / name).write_text(value)
            def open_file(path, flags, *args, **kwargs):
                if path == "CORDIAL.UF2":
                    self.writes.append("uf2-open")
                    return 1002
                return 1001 if str(path).startswith("/dev/bus/usb/") else 1000
            with patch.object(flash, "mounted_target", return_value=(Path("/unused"), 42, usb)), \
                    patch.object(flash.os, "open", side_effect=open_file), \
                    patch.object(flash.os, "fstat", return_value=SimpleNamespace(st_dev=42, st_rdev=flash.os.makedev(189, 260))), \
                    patch.object(flash.os, "write", side_effect=lambda fd, value: len(value)), \
                    patch.object(flash.os, "fsync"), patch.object(flash.os, "close"), \
                    patch.object(flash.fcntl, "ioctl", side_effect=self.ioctl):
                yield


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
                patch.object(flash.os, "close"), patch.object(flash, "check_development_storage"):
            with self.assertRaises(flash.SubmissionUnconfirmed):
                flash.write_development(data, manifest, "/unused")

    def test_storage_erase_is_explicit_and_confined(self):
        data = image(); manifest = flash.inspect_uf2(data)
        rom = FakeROM(manifest)
        original = bytes(rom.memory)
        with rom.environment():
            flash.write_development(data, manifest, "/unused")
        self.assertNotIn(3, [cmd for cmd, _, _ in rom.commands])
        rom.writes.clear()
        with rom.environment():
            flash.write_development(data, manifest, "/unused", clear_storage=True)
        start = manifest["storage_offset"]
        self.assertEqual(rom.memory[:start], original[:start])
        self.assertEqual(rom.memory[start:], b"\xff" * 131072)
        erases = [args for cmd, args, _ in rom.commands if cmd == 3]
        self.assertEqual(erases, [struct.pack("<II", flash.FLASH_BASE + start + offset, 4096)
                                  for offset in reversed(range(0, 131072, 4096))])
        self.assertTrue(set(cmd for cmd, _, _ in rom.commands) <= {1, 3, 6, 7, 0x84})
        self.assertEqual(rom.writes, ["uf2-open"])

    def test_interrupted_explicit_clear_can_be_retried(self):
        data = image(); manifest = flash.inspect_uf2(data)
        start = manifest["storage_offset"]
        for completed in (1, 31):
            rom = FakeROM(manifest)
            rom.fail_erase_after = completed
            with rom.environment(), self.assertRaisesRegex(OSError, "erase failure"):
                flash.write_development(data, manifest, "/unused", clear_storage=True)
            self.assertEqual(rom.writes, [])
            self.assertEqual(rom.memory[start:start+32], bytes.fromhex(manifest["storage_identity"]))
            rom.fail_erase_after = None
            with rom.environment():
                flash.write_development(data, manifest, "/unused", clear_storage=True)
            self.assertEqual(rom.memory[start:], b"\xff" * manifest["storage_bytes"])
            self.assertEqual(rom.writes, ["uf2-open"])

    def test_storage_check_prevents_unknown_or_changed_layout_writes(self):
        data = image(); manifest = flash.inspect_uf2(data)
        for clear, offset in ((clear, offset) for clear in (False, True)
                              for offset in (0, manifest["storage_offset"])):
            rom = FakeROM(manifest)
            rom.memory[offset] ^= 1
            with rom.environment(), self.assertRaises(ValueError):
                flash.write_development(data, manifest, "/unused", clear_storage=clear)
            self.assertEqual(rom.writes, [])
            self.assertNotIn(3, [cmd for cmd, _, _ in rom.commands])

    def test_blank_provision_requires_entire_flash_blank(self):
        data = image(); manifest = flash.inspect_uf2(data)
        rom = FakeROM(manifest)
        rom.memory[:] = b"\xff" * len(rom.memory)
        with rom.environment():
            flash.write_development(data, manifest, "/unused")
        self.assertEqual(rom.writes, ["uf2-open"])
        rom.writes.clear()
        rom.memory[8192] = 0
        with rom.environment(), self.assertRaisesRegex(ValueError, "unrecognized"):
            flash.write_development(data, manifest, "/unused")
        self.assertEqual(rom.writes, [])

    def test_erase_failure_prevents_firmware_write(self):
        data = image(); manifest = flash.inspect_uf2(data)
        for fail_erase, bad_status in ((True, False), (False, True)):
            rom = FakeROM(manifest); rom.fail_erase, rom.bad_status = fail_erase, bad_status
            with rom.environment(), self.assertRaises(OSError):
                flash.write_development(data, manifest, "/unused", clear_storage=True)
            self.assertEqual(rom.writes, [])

    def test_clear_rejects_production_before_usb_access(self):
        data = image(profile="production", bootloader=None)
        with patch.object(flash, "_PicoBoot", side_effect=AssertionError("USB opened")), \
                self.assertRaisesRegex(ValueError, "Refusing production"):
            flash.clear_development_storage(data, "pico_w", Path("/unused"))

    def test_erase_preserves_rp2350_reserved_sector(self):
        flash_bytes = 2097152
        data = image(chip="rp2350", package="a", hardware="pico2_w", flash_bytes=flash_bytes,
                     storage_offset=flash_bytes - 131072 - 4096)
        marker = (struct.pack("<8I", 0x0A324655, 0x9E5D5157, 0xA000, flash.FLASH_BASE + flash_bytes - 256,
                              256, 0, 2, 0xE48BFF57) + b"\xef" * 256 + struct.pack("<I", 0x9957E304) +
                  b"\0" * 216 + struct.pack("<I", 0x0AB16F30))
        data = marker + data; manifest = flash.inspect_uf2(data)
        rom = FakeROM(manifest)
        original = bytes(rom.memory)
        with rom.environment():
            flash.write_development(data, manifest, "/unused", clear_storage=True)
        start = manifest["storage_offset"]
        self.assertEqual(rom.memory[:start], original[:start])
        self.assertEqual(rom.memory[start:start + 131072], b"\xff" * 131072)
        self.assertEqual(rom.memory[-4096:], b"\xa5" * 4096)

    def test_changed_mount_prevents_erase(self):
        data = image(); manifest = flash.inspect_uf2(data)
        rom = FakeROM(manifest)
        with rom.environment(), patch.object(flash.os, "fstat", return_value=SimpleNamespace(st_dev=99)), \
                self.assertRaisesRegex(ValueError, "Mount changed"):
            flash.write_development(data, manifest, "/unused", clear_storage=True)
        self.assertEqual(rom.commands, [])
        self.assertEqual(rom.writes, [])

    def test_clear_requires_write(self):
        with patch.object(flash.sys, "argv", ["flash_dev.py", "/unused", "--hardware", "pico_w", "--clear-storage"]), \
                patch.object(flash.Path, "read_bytes", side_effect=AssertionError("file read")):
            self.assertEqual(flash.main(), 1)

    def test_corruption(self):
        data = image()
        variants = [b"", data[:-1], data + data, data[512:], data[:512] + data[:512]]
        for offset, value in ((0, 0), (8, 0), (12, 0x20000000), (16, 512), (20, 500), (28, 123)):
            changed = bytearray(data); struct.pack_into("<I", changed, offset, value); variants.append(changed)
        for corrupted in variants:
            with self.subTest(length=len(corrupted)), self.assertRaises(ValueError):
                flash.inspect_uf2(corrupted)

    def test_rp2350_erratum_and_storage(self):
        data = image(chip="rp2350", package="b", hardware="custom", flash_bytes=16777216,
                     storage_offset=16777216 - 131072 - 4096)
        marker = (struct.pack("<8I", 0x0A324655, 0x9E5D5157, 0xA000, 0x10FFFF00, 256, 0, 2, 0xE48BFF57) +
                  b"\xef" * 256 + struct.pack("<I", 0x9957E304) + b"\0" * 216 + struct.pack("<I", 0x0AB16F30))
        self.assertEqual(flash.inspect_uf2(marker + data)["chip"], "rp2350")
        with self.assertRaisesRegex(ValueError, "workaround"):
            flash.inspect_uf2(data)
        changed = bytearray(marker); struct.pack_into("<I", changed, 12, 0x10FFE000)
        with self.assertRaisesRegex(ValueError, "workaround"):
            flash.inspect_uf2(changed + data)
        changed = bytearray(marker); changed[32] = 0
        with self.assertRaisesRegex(ValueError, "workaround"):
            flash.inspect_uf2(changed + data)
        with self.assertRaises(ValueError):
            flash.inspect_uf2(marker + image(chip="rp2350", package="b", flash_bytes=16777216,
                                            storage_offset=16777216 - 131072))


if __name__ == "__main__":
    unittest.main()
