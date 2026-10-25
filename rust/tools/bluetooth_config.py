"""Compile the board's independent normal Bluetooth connection allowances."""
import json
import os
from pathlib import Path
import sys


def limits(config):
    if config.get("chip") == "esp32s3":
        return (0, 8)
    if config.get("chip") == "rp2040":
        return (2, 8)
    return (6, 14)


def generate(out):
    path = os.environ.get("CORDIAL_CONFIG")
    config = json.loads(Path(path).read_text()) if path else {}
    classic, ble = limits(config)
    values = {"CLASSIC_CONNECTIONS": classic, "BLE_CONNECTIONS": ble,
              "ACTIVE_CONNECTIONS": classic + ble + 1}
    out = Path(out)
    (out / "bluetooth.rs").write_text("".join(
        f"pub const {key}: usize = {value};\n" for key, value in values.items()))
    (out / "cordial_bluetooth.h").write_text("#pragma once\n" + "".join(
        f"#define CORDIAL_{key} {value}\n" for key, value in values.items()))


if __name__ == "__main__":
    generate(sys.argv[1])
