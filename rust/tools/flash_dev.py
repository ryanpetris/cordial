#!/usr/bin/env python3
"""Inspect UF2s and optionally flash verified development firmware on Linux.

No production-write path exists. Verification uses embedded metadata, UF2
addresses/family, and the mounted volume's physical USB identity.
"""
import argparse
import ctypes
import fcntl
import hashlib
import json
import os
from pathlib import Path
import re
import struct
import subprocess
import sys

from firmware_artifact import MAGIC, FAMILIES, inspect as inspect_manifest
FLASH_BASE = 0x10000000
PROTECTED_SERIAL = "E0C9125B0D9B"
# Independent of the selected build configuration, which might have been edited.
PROTECTED_DIGEST = hashlib.sha256(
    b"rp2040/rp2040/2097152/2/12000000/23/24/24/24/29/25/0/-1/1500000000/6/2/1200000000/5/5/False/False"
).hexdigest()


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
    flash, storage = manifest["flash_bytes"], manifest["storage_offset"]
    errata_bytes = 4096 if manifest["chip"] == "rp2350" else 0
    if addresses[-1] + 256 > FLASH_BASE + storage:
        raise ValueError("Image overlaps persistent storage or exceeds flash")
    expected_absolute = FLASH_BASE + flash - 256 if errata_bytes else None
    if absolute_address != expected_absolute:
        raise ValueError("Missing, misplaced or unexpected RP2350-E10 workaround block")
    if families != {FAMILIES[manifest["chip"]]}:
        raise ValueError("UF2 family does not match the manifest")
    return manifest


def verify_development(data, serial, hardware):
    manifest = inspect_uf2(data)
    if manifest["profile"] == "production" or manifest["bootloader"] != "bootsel":
        raise ValueError("Refusing production firmware: only debug or development firmware with remote BOOTSEL may be flashed")
    if manifest["hardware"] != hardware:
        raise ValueError("Artifact hardware does not match the selected hardware")
    if serial.upper() == PROTECTED_SERIAL:
        if (manifest["hardware"] != "pico_w" or manifest["chip"] != "rp2040" or
                manifest["package"] != "rp2040" or manifest["flash_bytes"] != 2097152 or
                manifest["hardware_digest"] != PROTECTED_DIGEST):
            raise ValueError("Artifact is incompatible with the protected Pico W")
    return manifest


def mounted_target(mount, serial, chip):
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
    attributes = {key: (usb / key).read_text().strip() for key in ("idVendor", "idProduct", "serial")}
    expected_pid = "0003" if chip == "rp2040" else "000f"
    if attributes["idVendor"] != "2e8a" or attributes["idProduct"] != expected_pid or attributes["serial"].upper() != serial.upper():
        raise ValueError("Mounted volume is not the specified board in compatible BOOTSEL mode")
    return mount, os.makedev(major, minor), usb


class _USBControl(ctypes.Structure):
    _fields_ = [("request_type", ctypes.c_uint8), ("request", ctypes.c_uint8),
                ("value", ctypes.c_uint16), ("index", ctypes.c_uint16),
                ("length", ctypes.c_uint16), ("timeout", ctypes.c_uint32),
                ("data", ctypes.c_void_p)]


class _USBBulk(ctypes.Structure):
    _fields_ = [("endpoint", ctypes.c_uint), ("length", ctypes.c_uint),
                ("timeout", ctypes.c_uint), ("data", ctypes.c_void_p)]


class _PicoBoot:
    """Linux usbfs transport for the SDK's boot/picoboot.h ROM protocol."""

    def __init__(self, usb, serial, chip):
        self.fd = None
        self.interface = None
        self.token = 0
        try:
            bus = int((usb / "busnum").read_text())
            address = int((usb / "devnum").read_text())
            major, minor = map(int, (usb / "dev").read_text().split(":"))
            self.fd = os.open(f"/dev/bus/usb/{bus:03}/{address:03}", os.O_RDWR | os.O_NOFOLLOW)
            if os.fstat(self.fd).st_rdev != os.makedev(major, minor):
                raise ValueError("BOOTSEL USB node changed during verification")
            descriptor = self.control(0x80, 6, 0x0100, 0, 18)
            pid = 0x0003 if chip == "rp2040" else 0x000f
            if (len(descriptor) != 18 or descriptor[:2] != b"\x12\x01" or
                    struct.unpack_from("<HH", descriptor, 8) != (0x2e8a, pid) or not descriptor[16]):
                raise ValueError("PICOBOOT handle is not the expected processor")
            languages = self.control(0x80, 6, 0x0300, 0, 255)
            if len(languages) < 4 or languages[1] != 3 or languages[0] != len(languages):
                raise ValueError("Cannot verify BOOTSEL USB serial language")
            language = struct.unpack_from("<H", languages, 2)[0]
            identity = self.control(0x80, 6, 0x0300 | descriptor[16], language, 255)
            if (len(identity) < 2 or identity[1] != 3 or identity[0] != len(identity) or
                    identity[2:].decode("utf-16-le").upper() != serial.upper()):
                raise ValueError("PICOBOOT handle is not the specified board")
            config = self.control(0x80, 6, 0x0200, 0, 9)
            if len(config) != 9 or config[:2] != b"\x09\x02":
                raise ValueError("Invalid BOOTSEL USB configuration")
            size = struct.unpack_from("<H", config, 2)[0]
            if not 9 <= size <= 4096:
                raise ValueError("Invalid BOOTSEL USB configuration length")
            config = self.control(0x80, 6, 0x0200, 0, size)
            if len(config) != size or self.control(0x80, 8, 0, 0, 1) != config[5:6]:
                raise ValueError("BOOTSEL USB configuration changed")
            interface, self.out_ep, self.in_ep = self.find_interface(config)
            fcntl.ioctl(self.fd, 0x8004550f, struct.pack("I", interface))  # USBDEVFS_CLAIMINTERFACE
            self.interface = interface
        except BaseException:
            self.close()
            raise

    @staticmethod
    def find_interface(config):
        matches, endpoints, current = [], [], None
        offset = 0
        while offset < len(config):
            length = config[offset]
            if length < 2 or offset + length > len(config):
                raise ValueError("Malformed BOOTSEL USB descriptor")
            desc = config[offset:offset + length]
            offset += length
            if desc[1] == 4:
                if current is not None:
                    matches.append((current, endpoints))
                current, endpoints = None, []
                if length == 9 and desc[3] == 0 and desc[4:7] == b"\x02\xff\x00" and desc[7] == 0:
                    current = desc[2]
            elif desc[1] == 5 and current is not None:
                if length != 7 or desc[3] & 3 != 2:
                    raise ValueError("PICOBOOT endpoint is not bulk")
                endpoints.append(desc[2])
        if current is not None:
            matches.append((current, endpoints))
        if len(matches) != 1:
            raise ValueError("Cannot identify one PICOBOOT interface")
        interface, endpoints = matches[0]
        outputs = [ep for ep in endpoints if not ep & 0x80 and ep & 15]
        inputs = [ep for ep in endpoints if ep & 0x80 and ep & 15]
        if len(endpoints) != 2 or len(outputs) != 1 or len(inputs) != 1:
            raise ValueError("Invalid PICOBOOT endpoints")
        return interface, outputs[0], inputs[0]

    def control(self, request_type, request, value, index, size):
        buffer = ctypes.create_string_buffer(size)
        transfer = _USBControl(request_type, request, value, index, size, 3000, ctypes.addressof(buffer))
        count = fcntl.ioctl(self.fd, 0xc0005500 | ctypes.sizeof(transfer) << 16, transfer)
        if not 0 <= count <= size:
            raise OSError("Invalid USB control response length")
        return buffer.raw[:count]

    def bulk(self, endpoint, data, size):
        buffer = ctypes.create_string_buffer(data, max(size, 1))
        transfer = _USBBulk(endpoint, size, 10000, ctypes.addressof(buffer))
        count = fcntl.ioctl(self.fd, 0xc0005502 | ctypes.sizeof(transfer) << 16, transfer)
        if not 0 <= count <= size:
            raise OSError("Invalid USB bulk response length")
        return buffer.raw[:count]

    def command(self, command, args=b"", read_size=0):
        self.token += 1
        packet = struct.pack("<IIBBHI16s", 0x431fd10b, self.token, command, len(args), 0, read_size, args)
        if len(self.bulk(self.out_ep, packet, len(packet))) != len(packet):
            raise OSError("Short PICOBOOT command write")
        result = self.bulk(self.in_ep, b"", read_size) if read_size else b""
        if len(result) != read_size:
            raise OSError("Short PICOBOOT read")
        if self.bulk(self.out_ep if command & 0x80 else self.in_ep, b"", 0 if command & 0x80 else 1):
            raise OSError("Invalid PICOBOOT acknowledgement")
        status = self.control(0xc1, 0x42, 0, self.interface, 16)
        if len(status) != 16 or struct.unpack_from("<IIBB", status) != (self.token, 0, command, 0):
            raise OSError("PICOBOOT command did not complete successfully")
        return result

    def close(self):
        if self.fd is not None:
            try:
                if self.interface is not None:
                    fcntl.ioctl(self.fd, 0x80045510, struct.pack("I", self.interface))  # USBDEVFS_RELEASEINTERFACE
            finally:
                os.close(self.fd)
                self.fd = None


def check_development_storage(data, serial, hardware, usb, clear=False):
    verified = verify_development(data, serial, hardware)
    start = FLASH_BASE + verified["storage_offset"]
    size = verified["storage_bytes"]
    boot = _PicoBoot(usb, serial, verified["chip"])
    try:
        for endpoint in (boot.out_ep, boot.in_ep):
            fcntl.ioctl(boot.fd, 0x80045515, struct.pack("I", endpoint))
        boot.control(0x41, 0x41, 0, boot.interface, 0)
        boot.command(0x01, b"\x01")
        try:
            boot.command(0x06)  # ROM flash reads/erases require XIP disabled.
            def read(address, length=4096):
                return boot.command(0x84, struct.pack("<II", address, length), length)
            first = read(FLASH_BASE)
            if first == b"\xff" * 4096:
                # Only a wholly blank board permits first provisioning.
                for offset in range(4096, verified["flash_bytes"], 4096):
                    if read(FLASH_BASE + offset) != b"\xff" * 4096:
                        raise ValueError("Existing flash is unrecognized; use coordinated manual provisioning")
            else:
                try:
                    installed = inspect_manifest(first)
                except (ValueError, UnicodeError) as error:
                    raise ValueError("Installed image has no current manifest; use coordinated manual provisioning") from error
                for field in ("chip", "package", "hardware_digest", "flash_bytes", "bluetooth_backend",
                              "usb_backend", "storage_backend", "storage_offset", "storage_bytes", "storage_identity"):
                    if installed[field] != verified[field]:
                        raise ValueError(f"Installed {field} differs; use coordinated manual provisioning")
                marker = read(start, 32)
                if marker != bytes.fromhex(verified["storage_identity"]):
                    if marker != b"\xff" * 32 or any(read(address) != b"\xff" * 4096
                            for address in range(start, start + size, 4096)):
                        raise ValueError("Stored layout differs; use coordinated manual provisioning")
            if clear:
                # Keep the recognized marker until the data erase completes, so
                # an interrupted explicit clear can be retried normally.
                for address in reversed(range(start, start + size, 4096)):
                    boot.command(0x03, struct.pack("<II", address, 4096))
                    if read(address) != b"\xff" * 4096:
                        raise OSError("Storage erase readback failed; no firmware was written")

        finally:
            boot.command(0x01, b"\x00")
    finally:
        boot.close()


def clear_development_storage(data, serial, hardware, usb):
    check_development_storage(data, serial, hardware, usb, clear=True)


def write_development(data, manifest, mount, serial, clear_storage=False):
    # Recheck artifact policy inside the sole write function too.
    verified = verify_development(data, serial, manifest["hardware"])
    mount, device_number, usb = mounted_target(mount, serial, verified["chip"])
    directory = os.open(mount, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
    try:
        if os.fstat(directory).st_dev != device_number:
            raise ValueError("Mount changed during verification")
        check_development_storage(data, serial, verified["hardware"], usb, clear=clear_storage)
        if mounted_target(mount, serial, verified["chip"]) != (mount, device_number, usb):
            raise ValueError("Mount changed after storage check; no firmware was written")
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
    parser.add_argument("--serial", default=PROTECTED_SERIAL)
    parser.add_argument("--hardware", default="pico_w")
    parser.add_argument("--mount", type=Path)
    parser.add_argument("--write", action="store_true", help="write after artifact and USB identity checks; otherwise inspect only")
    parser.add_argument("--clear-storage", action="store_true",
                        help="with --write, erase saved bonds/configuration using ROM PICOBOOT before writing firmware")
    args = parser.parse_args()
    try:
        if args.clear_storage and not args.write:
            raise ValueError("--clear-storage requires --write")
        data = args.uf2.read_bytes()
        manifest = verify_development(data, args.serial, args.hardware)
        print(json.dumps({"artifact": str(args.uf2), "sha256": hashlib.sha256(data).hexdigest(), "manifest": manifest}, indent=2))
        if args.write:
            if args.mount is None:
                raise ValueError("--write requires --mount")
            write_development(data, manifest, args.mount, args.serial, args.clear_storage)
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
