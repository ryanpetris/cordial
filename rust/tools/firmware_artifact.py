"""Current firmware manifest and Pico UF2 packaging; no hardware access."""
import hashlib
import json
import re
import struct
import sys
from pathlib import Path

MAGIC = b"CORDIAL-META-V1\n"
FAMILIES = {"rp2040": 0xE48BFF56, "rp2350": 0xE48BFF59}
ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT.parent / "tools"))
from version import resolve, RELEASE

PROFILES = ("production", "debug", "development")
# Production versions are bare; debug and development builds carry their profile.
SUFFIXES = {"production": "", "debug": "-debug", "development": "-dev"}
VERSION = rf"{RELEASE}(?:-debug|-dev)?"


def version(profile):
    """The firmware version for a build profile."""
    return resolve() + SUFFIXES[profile]


def manifest(config):
    keys = ("profile", "name", "hardware_digest", "chip", "package", "flash_bytes",
            "bluetooth_backend", "radio_backend", "usb_backend", "storage_backend", "storage_offset", "storage_bytes")
    value = {k: config[k] for k in keys}
    value["hardware"] = value.pop("name")
    value.update(schema=1, erase_bytes=4096, storage_identity=config["storage_identity"].hex(),
                 bootloader=("download" if config["chip"] == "esp32s3" else "bootsel")
                 if config["profile"] != "production" else None,
                 version=version(config["profile"]))
    if config["chip"] == "esp32s3":
        value.update(native_storage_offset=0x9000, native_storage_bytes=0x6000)
    return value


def generate(config, out):
    value = manifest(config)
    blob = MAGIC + json.dumps(value, sort_keys=True, separators=(",", ":")).encode() + b"\0"
    section = ".rodata.cordial_metadata" if config["chip"] == "esp32s3" else ".cordial_metadata"
    (Path(out) / "metadata.rs").write_text(
        f'const FIRMWARE_VERSION: &str = {json.dumps(value["version"])};\n'
        + '#[used]\n#[unsafe(no_mangle)]\n' + f'#[unsafe(link_section = "{section}")]\n'
        + f'pub static CORDIAL_METADATA: [u8; {len(blob)}] = {list(blob)!r};\n')


def unique(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError("Duplicate manifest field")
        result[key] = value
    return result


def inspect(data):
    if data.count(MAGIC) != 1:
        raise ValueError("Artifact requires exactly one embedded Cordial manifest")
    start = data.index(MAGIC) + len(MAGIC)
    end = data.find(b"\0", start, start + 1024)
    if end < 0:
        raise ValueError("Manifest is missing its terminator or too large")
    value = json.loads(data[start:end], object_pairs_hook=unique)
    required = {"schema", "profile", "bootloader", "hardware", "hardware_digest", "chip", "package",
                "flash_bytes", "bluetooth_backend", "radio_backend", "usb_backend", "storage_backend", "storage_offset",
                "storage_bytes", "erase_bytes", "storage_identity", "version"}
    if not isinstance(value, dict):
        raise ValueError("Invalid artifact manifest")
    esp = value.get("chip") == "esp32s3"
    if esp:
        required |= {"native_storage_offset", "native_storage_bytes"}
    if set(value) != required or type(value["schema"]) is not int or value["schema"] != 1:
        raise ValueError("Unsupported artifact manifest")
    if value["profile"] not in PROFILES:
        raise ValueError("Invalid build profile")
    method = ("download" if esp else "bootsel") if value["profile"] != "production" else None
    if value["bootloader"] != method:
        raise ValueError("Inconsistent firmware profile and bootloader capability")
    for field, pattern in (("hardware", r"[a-z][a-z0-9_]{0,47}"), ("hardware_digest", r"[0-9a-f]{64}"),
                           ("storage_identity", r"[0-9a-f]{64}"), ("version", VERSION)):
        if not isinstance(value[field], str) or not re.fullmatch(pattern, value[field]):
            raise ValueError(f"Invalid manifest {field}")
    if (value["chip"], value["package"]) not in (("rp2040", "rp2040"), ("rp2350", "a"), ("rp2350", "b"), ("esp32s3", "esp32s3")):
        raise ValueError("Invalid processor family/package")
    for field in ("flash_bytes", "storage_offset", "storage_bytes", "erase_bytes"):
        if type(value[field]) is not int or value[field] <= 0 or value[field] % 4096:
            raise ValueError(f"Invalid {field}")
    flash, size, offset = (value[k] for k in ("flash_bytes", "storage_bytes", "storage_offset"))
    errata = 4096 if value["chip"] == "rp2350" else 0
    if not 2097152 <= flash <= 16777216 or not 131072 <= size <= flash - (2097152 if esp else 1048576) or offset != flash-size-errata or value["erase_bytes"] != 4096:
        raise ValueError("Invalid storage partition bounds")
    if value["radio_backend"] not in (("esp-idf",) if esp else ("pico-sdk-cyw43", "embassy-cyw43")):
        raise ValueError("Unsupported radio backend")
    if value["usb_backend"] != "embassy":
        raise ValueError("Unsupported USB backend")
    if value["storage_backend"] != "littlefs":
        raise ValueError("Unsupported storage backend")
    if value["bluetooth_backend"] not in (("btstack", "esp-nimble") if esp else ("btstack",)):
        raise ValueError("Unsupported Bluetooth backend")
    layout = dict(format="littlefs-json-1",
                  storage_backend=value["storage_backend"], offset=offset, bytes=size, erase_bytes=4096)
    if esp:
        if flash & (flash-1) or type(value["native_storage_offset"]) is not int or type(value["native_storage_bytes"]) is not int or (value["native_storage_offset"], value["native_storage_bytes"]) != (0x9000, 0x6000):
            raise ValueError("Invalid native storage partition")
        layout.update(guard_bytes=4096, native_offset=0x9000, native_bytes=0x6000)
    identity = hashlib.sha256(json.dumps(layout, sort_keys=True, separators=(",", ":")).encode()).hexdigest()
    if value["storage_identity"] != identity:
        raise ValueError("Storage identity does not match manifest layout")
    return value


def uf2(binary):
    value = inspect(binary)
    chip = value["chip"]
    if chip not in FAMILIES or len(binary) > value["storage_offset"]:
        raise ValueError("Pico image overlaps storage or uses another processor")
    binary += b"\xff" * (-len(binary) % 256)
    count = len(binary) // 256
    blocks = []
    if chip == "rp2350":
        blocks.append(struct.pack("<8I", 0x0A324655, 0x9E5D5157, 0xA000,
                                  0x10000000+value["flash_bytes"]-256, 256, 0, 2, 0xE48BFF57)
                      + b"\xef"*256 + struct.pack("<I", 0x9957E304) + b"\0"*216 + struct.pack("<I", 0x0AB16F30))
    for i in range(count):
        blocks.append(struct.pack("<8I", 0x0A324655, 0x9E5D5157, 0x2000, 0x10000000+256*i,
                                  256, i, count, FAMILIES[chip]) + binary[i*256:(i+1)*256]
                      + b"\0"*220 + struct.pack("<I", 0x0AB16F30))
    return b"".join(blocks)
