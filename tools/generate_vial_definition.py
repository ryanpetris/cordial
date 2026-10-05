#!/usr/bin/env python3
"""Embed the Vial keyboard definition using its standard XZ encoding."""
import json
import lzma
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent


def generate():
    definition = json.loads((ROOT / "configs/keyboards/cordial-vial.json").read_text())
    return lzma.compress(json.dumps(definition, separators=(",", ":")).encode())


if __name__ == "__main__":
    (ROOT / "rust/crates/cordial-core/src/vial-definition.xz").write_bytes(generate())
