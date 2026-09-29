"""Offline package publication and ESP support-image layout checks."""
import hashlib
from pathlib import Path
import struct
import sys
import tempfile
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "tools"))
import build_firmware as build


class Packaging(unittest.TestCase):
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
        config = {"storage_offset": 0x7e0000, "storage_bytes": 0x20000}
        entries = [(1, 2, 0x9000, 0x6000, b"nvs"), (1, 1, 0xf000, 0x1000, b"phy_init"),
                   (0, 0, 0x10000, 0x7d0000, b"factory"), (1, 0x40, 0x7e0000, 0x1000, b"cordial_layout"),
                   (1, 0x83, 0x7e1000, 0x1f000, b"cordial_app")]
        def table(rows):
            data = b"".join(struct.pack("<HBBII16sI", 0x50aa, *row, 0) for row in rows)
            data += b"\xeb\xeb" + b"\xff" * 14 + hashlib.md5(data).digest()
            return data.ljust(0xc00, b"\xff")
        data = table(entries)
        build.esp_partitions(data, config)
        # A wrong table with a valid checksum is still rejected.
        for malformed in (table(entries[:-1]), table(entries[::-1]),
                          data[:170] + b"\0" + data[171:], data + b"\xff"):
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
