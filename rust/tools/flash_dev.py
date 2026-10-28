#!/usr/bin/env python3
"""Inspect UF2s and optionally flash verified development firmware on Linux.

No production-write path exists. Verification uses embedded metadata, UF2
addresses/family, and the mounted volume's physical USB identity.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import struct
import subprocess
import sys

from firmware_artifact import MAGIC, FAMILIES, image_capacity, inspect as inspect_manifest
FLASH_BASE = 0x10000000


class SubmissionUnconfirmed(OSError):
    """All UF2 bytes were accepted, but the ROM may have disconnected at fsync."""


def unique_object(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError("Duplicate manifest field")
        result[key] = value
    return result


def inspect_uf2(data):
    if not data or len(data) % 512 or len(data) > 32 * 1024 * 1024:
        raise ValueError("UF2 size is invalid")
    pages, numbers, families = {}, set(), set()
    absolute_address = None
    count = len(data) // 512
    # SDK RP2350-E10 workaround is a separate one-block absolute-family stream.
    # Only the exact SDK marker is allowed, never arbitrary absolute writes.
    if count and struct.unpack_from("<I", data, 28)[0] == 0xE48BFF57:
        block = data[:512]
        header = struct.unpack_from("<8I", block)
        if (header[:3] != (0x0A324655, 0x9E5D5157, 0xA000) or
                header[4:] != (256, 0, 2, 0xE48BFF57) or
                block[32:288] != b"\xef" * 256 or
                block[288:292] != struct.pack("<I", 0x9957E304) or
                block[292:508] != b"\0" * 216 or
                struct.unpack_from("<I", block, 508)[0] != 0x0AB16F30):
            raise ValueError("Invalid RP2350-E10 workaround block")
        absolute_address = header[3]
        data = data[512:]
        count -= 1
        if not count:
            raise ValueError("UF2 has no application payload")
    for offset in range(0, len(data), 512):
        block = data[offset:offset + 512]
        magic0, magic1, flags, address, size, number, total, family = struct.unpack_from("<8I", block)
        if (magic0, magic1, struct.unpack_from("<I", block, 508)[0]) != (0x0A324655, 0x9E5D5157, 0x0AB16F30):
            raise ValueError("Invalid UF2 magic")
        if flags != 0x2000 or size != 256:
            raise ValueError("Only plain, family-tagged 256-byte flash blocks are supported")
        if total != count or number >= count or number in numbers:
            raise ValueError("Incomplete or duplicate UF2 block sequence")
        if address % 256 or not FLASH_BASE <= address < FLASH_BASE + 16 * 1024 * 1024:
            raise ValueError("UF2 contains a non-flash or unaligned write")
        if address in pages:
            raise ValueError("Overlapping UF2 writes")
        pages[address] = block[32:288]
        numbers.add(number)
        families.add(family)
    addresses = sorted(pages)
    if addresses[0] != FLASH_BASE or addresses[-1] - FLASH_BASE != (count - 1) * 256:
        raise ValueError("UF2 must contain one contiguous image starting at flash offset zero")
    payload = b"".join(pages[address] for address in addresses)
    manifest = inspect_manifest(payload)
    if manifest["chip"] not in FAMILIES:
        raise ValueError("UF2 requires a Pico processor")
    extent = (addresses[-1] + 256 - FLASH_BASE + 4095) // 4096 * 4096
    if extent > image_capacity(manifest):
        raise ValueError("Image overlaps persistent storage or exceeds flash")
    expected_absolute = (FLASH_BASE + manifest["firmware_bytes"] - 256
                         if manifest["chip"] == "rp2350" else None)
    if absolute_address != expected_absolute:
        raise ValueError("Missing, misplaced, or unexpected RP2350-E10 workaround block")
    if families != {FAMILIES[manifest["chip"]]}:
        raise ValueError("UF2 family does not match the manifest")
    return manifest


def verify_development(data, hardware):
    manifest = inspect_uf2(data)
    if manifest["profile"] == "production" or manifest["bootloader"] != "bootsel":
        raise ValueError("Refusing production firmware: only debug or development firmware with remote BOOTSEL may be flashed")
    if manifest["hardware"] != hardware:
        raise ValueError("Artifact hardware does not match the selected hardware")
    return manifest


def mounted_target(mount, chip):
    mount = Path(mount).resolve(strict=True)
    result = subprocess.run(["findmnt", "--json", "--target", str(mount), "--output", "TARGET,FSTYPE,MAJ:MIN"],
                            check=True, capture_output=True, text=True)
    rows = json.loads(result.stdout)["filesystems"]
    if len(rows) != 1 or Path(rows[0]["target"]) != mount or rows[0]["fstype"] != "vfat":
        raise ValueError("Target must be the root of a mounted BOOTSEL FAT volume")
    major, minor = map(int, rows[0]["maj:min"].split(":"))
    device = Path(f"/sys/dev/block/{major}:{minor}").resolve(strict=True)
    usb = next((p for p in (device, *device.parents) if (p / "idVendor").exists()), None)
    if usb is None:
        raise ValueError("Cannot identify the mounted volume's USB device")
    attributes = {key: (usb / key).read_text().strip() for key in ("idVendor", "idProduct")}
    expected_pid = "0003" if chip == "rp2040" else "000f"
    if attributes["idVendor"] != "2e8a" or attributes["idProduct"] != expected_pid:
        raise ValueError("Mounted volume is not a board in compatible BOOTSEL mode")
    return mount, os.makedev(major, minor), usb


def write_development(data, manifest, mount):
    # Recheck artifact policy inside the sole write function too.
    verified = verify_development(data, manifest["hardware"])
    mount, device_number, usb = mounted_target(mount, verified["chip"])
    directory = os.open(mount, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
    try:
        if os.fstat(directory).st_dev != device_number:
            raise ValueError("Mount changed during verification")
        fd = os.open("CORDIAL.UF2", os.O_WRONLY | os.O_CREAT | os.O_TRUNC | os.O_NOFOLLOW, 0o644, dir_fd=directory)
        try:
            remaining = memoryview(data)
            while remaining:
                written = os.write(fd, remaining)
                if written <= 0:
                    raise OSError("Short UF2 write")
                remaining = remaining[written:]
            try:
                os.fsync(fd)
            except OSError as error:
                raise SubmissionUnconfirmed("UF2 bytes were submitted; the board may have rebooted. Check USB enumeration before doing anything else.") from error
        finally:
            os.close(fd)
    finally:
        os.close(directory)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("uf2", type=Path)
    parser.add_argument("--hardware", required=True, help="board the artifact must be built for, such as pico2_w")
    parser.add_argument("--mount", type=Path)
    parser.add_argument("--write", action="store_true", help="write after artifact and USB identity checks; otherwise inspect only")
    args = parser.parse_args()
    try:
        data = args.uf2.read_bytes()
        manifest = verify_development(data, args.hardware)
        print(json.dumps({"artifact": str(args.uf2), "sha256": hashlib.sha256(data).hexdigest(), "manifest": manifest}, indent=2))
        if args.write:
            if args.mount is None:
                raise ValueError("--write requires --mount")
            write_development(data, manifest, args.mount)
            print("Development UF2 written. Check USB enumeration before attempting another write.")
        return 0
    except SubmissionUnconfirmed as error:
        print(str(error), file=sys.stderr)
        return 2
    except (ValueError, OSError, subprocess.SubprocessError) as error:
        print(f"Refused or failed: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
