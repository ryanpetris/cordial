"""Read normal Bluetooth connection allowances for ESP SDK configuration."""
import json
from pathlib import Path


def limits(config):
    values = json.loads(Path(__file__).with_name("bluetooth_limits.json").read_text())
    chip = config.get("chip")
    return tuple(values.get(chip, values["default"]) if isinstance(chip, str) else values["default"])
