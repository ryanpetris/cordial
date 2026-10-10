#!/usr/bin/env python3
"""Build the selected firmware and inspect its artifacts. Never accesses hardware."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import struct
import subprocess
import sys
import tempfile

import firmware_artifact as artifact
import firmware_config
import firmware_dependencies
import dependency_notices

ROOT = Path(__file__).resolve().parents[1]


def command(argv, env):
    subprocess.run(list(map(str, argv)), cwd=ROOT, env=env, check=True)


def tool(name):
    found = shutil.which(name)
    if found:
        return Path(found).resolve()
    raise ValueError(f"Missing {name}; install the firmware toolchain described in docs/building.md")


def release_docs(staged):
    """Include the license, documentation and protocol definitions with firmware releases."""
    shutil.copy2(ROOT.parent / "LICENSE", staged / "LICENSE")
    shutil.copytree(ROOT.parent / "docs", staged / "docs")
    shutil.copytree(ROOT.parent / "proto", staged / "proto")


def publish(output, populate):
    """Keep the prior complete package until its replacement passes every check."""
    output.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix=f".{output.name}-", dir=output.parent) as temp:
        staged, backup = Path(temp, "package"), Path(temp, "previous")
        staged.mkdir()
        populate(staged)
        (staged / "SHA256SUMS").write_text("".join(
            f"{hashlib.sha256(path.read_bytes()).hexdigest()}  {path.relative_to(staged)}\n"
            for path in sorted(staged.rglob("*")) if path.is_file()))
        if output.exists():
            output.rename(backup)
        try:
            staged.rename(output)
        except OSError:
            if backup.exists():
                backup.rename(output)
            raise


def esp_image_header(data, settings, revisions):
    mode = {"qio": 0, "qout": 1, "dio": 2, "dout": 3}[settings["flash_mode"]]
    frequency = {"40m": 0, "26m": 1, "20m": 2, "80m": 15}[settings["flash_freq"]]
    size = {f"{1 << i}MB": i for i in range(8)}[settings["flash_size"]]
    if (len(data) < 24 or data[0] != 0xe9 or data[2:4] != bytes([mode, size << 4 | frequency])
            or struct.unpack_from("<H", data, 12)[0] != 9
            or struct.unpack_from("<HH", data, 15) != revisions):
        raise ValueError("ESP image header differs from SDK flash/chip configuration")


def esp_partitions(data, config):
    expected = [
        (1, 1, 0xf000, 0x1000, b"phy_init", 0),
        (0, 0, 0x10000, config["storage_offset"] - 0x10000, b"factory", 0),
        (1, 0x40, config["storage_offset"], 4096, b"cordial_layout", 0),
        (1, 0x83, config["storage_offset"] + 4096, config["storage_bytes"] - 4096, b"cordial_app", 0),
    ]
    if len(data) != 0xc00:
        raise ValueError("ESP partition table has an unexpected size")
    for i, entry in enumerate(expected):
        magic, kind, subtype, start, size, label, flags = struct.unpack_from("<HBBII16sI", data, i * 32)
        if magic != 0x50aa or (kind, subtype, start, size, label.rstrip(b"\0"), flags) != entry:
            raise ValueError("ESP partition table differs from selected storage layout")
    end = len(expected) * 32
    if (data[end:end+16] != b"\xeb\xeb" + b"\xff" * 14
            or data[end+16:end+32] != hashlib.md5(data[:end]).digest()
            or data[end+32:] != b"\xff" * (len(data)-end-32)):
        raise ValueError("ESP partition table checksum or trailing entries are invalid")


def package(config, built, elf, output, name, gcc, env):
    destination = output / f"cordial-{name}"
    shutil.copy2(elf, destination.with_suffix(".elf"))
    if config["chip"] != "esp32s3":
        command([str(gcc).removesuffix("gcc") + "objcopy", "-O", "binary", elf,
                 destination.with_suffix(".bin")], env)
        binary = destination.with_suffix(".bin").read_bytes()
        uf2 = artifact.uf2(binary)
        from flash_dev import inspect_uf2
        if inspect_uf2(uf2) != artifact.manifest(config):
            raise ValueError("Linked Pico metadata differs from selected build")
        destination.with_suffix(".uf2").write_bytes(uf2)
    else:
        sdk_outputs = list(built.glob("build/esp-idf-sys-*/out/build/flasher_args.json"))
        if len(sdk_outputs) != 1:
            raise ValueError("Expected one current ESP SDK build")
        sdk = sdk_outputs[0].parent
        settings = json.loads(sdk_outputs[0].read_text())["flash_settings"]
        if settings["flash_size"] != f"{config['flash_bytes']//1048576}MB":
            raise ValueError("ESP SDK flash size differs from board configuration")
        sdkconfig = dict(line.split("=", 1) for line in (sdk.parent / "sdkconfig").read_text().splitlines()
                         if line.startswith("CONFIG_"))
        revisions = tuple(int(sdkconfig[f"CONFIG_ESP_REV_{bound}_FULL"]) for bound in ("MIN", "MAX"))
        bootloader = sdk / "bootloader/bootloader.bin"
        partitions = sdk / "partition_table/partition-table.bin"
        esp_image_header(bootloader.read_bytes(), settings, revisions)
        esp_partitions(partitions.read_bytes(), config)
        python = Path(env["IDF_PYTHON_ENV_PATH"]) / "bin/python"
        command([python, "-m", "esptool", "--chip", "esp32s3", "elf2image",
                 "--flash-mode", settings["flash_mode"], "--flash-freq", settings["flash_freq"],
                 "--flash-size", settings["flash_size"], "--elf-sha256-offset", "0xb0",
                 "--min-rev-full", revisions[0], "--max-rev-full", revisions[1],
                 "--output", destination.with_suffix(".bin"), elf], env)
        binary = destination.with_suffix(".bin").read_bytes()
        esp_image_header(binary, settings, revisions)
        if 0x10000 + len(binary) > config["storage_offset"]:
            raise ValueError("ESP application image overlaps storage")
        shutil.copy2(bootloader, output / "bootloader.bin")
        shutil.copy2(partitions, output / "partition-table.bin")
        (output / "flash.json").write_text(json.dumps({"chip": "esp32s3", "files": {
            "0x0": "bootloader.bin", "0x8000": "partition-table.bin", "0x10000": destination.name + ".bin"}}, indent=2) + "\n")
    actual = artifact.inspect(binary)
    if actual != artifact.manifest(config):
        raise ValueError("Linked artifact metadata differs from selected build")
    destination.with_suffix(".json").write_text(json.dumps(actual, indent=2) + "\n")


def build(config_path, profile, output, clippy=False):
    version = artifact.version(profile)
    config = firmware_config.load(config_path, profile)
    # All board values and the profile affect this directory, including settings
    # that don't affect physical wiring identity (backend and storage layout).
    raw = json.loads(Path(config_path).read_text(), object_pairs_hook=firmware_config.unique_object)
    digest = hashlib.sha256(json.dumps([raw, profile], sort_keys=True).encode()).hexdigest()[:12]
    name = f"{config['name']}-{config['bluetooth_backend']}-{config['radio_backend']}"
    directory = ROOT.parent / "target/firmware" / f"{name}-{digest}"
    generated = directory / "config"
    # esp-idf-sys regenerates its SDK configuration from these tracked defaults
    # whenever they change. It keeps the result under its own Cargo OUT_DIR.
    firmware_config.generate(config, generated)
    (generated / "board.json").write_text(json.dumps(raw, sort_keys=True))
    env = dict(os.environ, CORDIAL_PYTHON=sys.executable)
    env["CORDIAL_VERSION"] = artifact.resolve()
    temporary = ROOT.parent / "target/tmp"
    temporary.mkdir(parents=True, exist_ok=True)
    env.update(TMPDIR=str(temporary), CORDIAL_CONFIG=str(generated / "board.json"),
               CARGO_TARGET_DIR=str(directory / "cargo"))
    triple = config["target"]
    if config["bluetooth_backend"] == "btstack":
        firmware_dependencies.prepare_btstack()
    if config["chip"] == "esp32s3":
        platform = "esp32s3"
        if not all(env.get(name) for name in ("IDF_PATH", "IDF_PYTHON_ENV_PATH", "LIBCLANG_PATH")):
            raise ValueError("Activate the espup and ESP-IDF environments described in docs/building.md")
        gcc = tool("xtensa-esp32s3-elf-gcc")
        linker = tool("ldproxy")
        env.update(MCU="esp32s3",
                   CARGO_WORKSPACE_DIR=str(ROOT / "platforms/esp32s3"),
                   ESP_IDF_SDKCONFIG=str(generated / "sdkconfig"),
                   ESP_IDF_SDKCONFIG_DEFAULTS=str(generated / "sdkconfig.defaults"),
                   CARGO_TARGET_XTENSA_ESP32S3_ESPIDF_LINKER=str(linker),
                   CARGO_TARGET_XTENSA_ESP32S3_ESPIDF_RUSTFLAGS="--cfg espidf_time64")
        features = [config["bluetooth_backend"], profile, "firmware"]
        cargo = ["cargo", "+esp"]
        extra = ["-Z", "build-std=std,panic_abort"]
    else:
        platform = "pico"
        gcc = tool("arm-none-eabi-gcc")
        features = [config["feature"], config["radio_backend"], profile, "firmware"]
        cargo, extra = ["cargo"], []
    env[f"CC_{triple.replace('-', '_').replace('.', '_')}"] = str(gcc)
    env[f"AR_{triple.replace('-', '_').replace('.', '_')}"] = str(gcc).removesuffix("gcc") + "ar"
    command(cargo + ["clippy" if clippy else "build", "--locked", "--manifest-path",
                    ROOT / f"platforms/{platform}/Cargo.toml", "--target", triple,
                    "--release", "--no-default-features", "--features", ",".join(features)]
            + extra + (["--", "-D", "warnings"] if clippy else []), env)
    if clippy:
        return
    built = directory / "cargo" / triple / "release"
    elf = built / f"cordial-{platform}"
    output = Path(output).resolve() / version / name
    def populate(staging):
        package(config, built, elf, staging, name, gcc, env)
        release_docs(staging)
        sdk = Path(env["IDF_PATH"]) if platform == "esp32s3" else None
        dependency_notices.record(cargo, platform, features, triple, env, staging, sdk)
    publish(output, populate)
    print(output)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("config", type=Path)
    parser.add_argument("--profile", choices=artifact.PROFILES, default="development")
    parser.add_argument("--output", type=Path, default=ROOT.parent / "build/firmware")
    parser.add_argument("--clippy", action="store_true")
    args = parser.parse_args()
    build(args.config.resolve(), args.profile, args.output, args.clippy)


if __name__ == "__main__":
    main()
