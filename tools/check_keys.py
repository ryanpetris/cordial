#!/usr/bin/env python3
"""Validate proto/keys.toml and check that it only grows compared with an earlier version."""

import argparse
import pathlib
import re
import subprocess
import sys
import tomllib

ROOT = pathlib.Path(__file__).resolve().parent.parent
CATALOG = "proto/keys.toml"
LISTS = {"adapter", "device", "setting"}
TYPES = {"bool", "integer", "enum", "text", "color"}
LEVEL = re.compile(r"^(\{n\}|[a-z0-9]+(_[a-z0-9]+)*)$")
MAX_KEY_BYTES = 64


def levels(key):
    return key.split(".")


def problems(catalog):
    """Returns a list of rule violations in a parsed catalog."""
    found = []
    keys = catalog.get("key", [])
    names = {}
    for entry in keys:
        key = entry.get("key")
        if not isinstance(key, str):
            found.append(f"entry without a key: {entry}")
            continue
        if key in names:
            found.append(f"{key}: listed twice")
        names[key] = entry
        parts = levels(key)
        if len(parts) < 2:
            found.append(f"{key}: needs at least two levels")
        if parts[0] == "{n}":
            found.append(f"{key}: the first level names a part, not an index")
        for part in parts:
            if not LEVEL.match(part):
                found.append(f"{key}: level {part!r} is not lowercase words joined by _")
        if len(key.replace("{n}", "65535").encode()) > MAX_KEY_BYTES:
            found.append(f"{key}: longer than {MAX_KEY_BYTES} bytes")
        lists = entry.get("lists")
        if not lists or not isinstance(lists, list) or not set(lists) <= LISTS:
            found.append(f"{key}: lists must be a non-empty subset of {sorted(LISTS)}")
        kind = entry.get("type")
        if kind not in TYPES:
            found.append(f"{key}: type must be one of {sorted(TYPES)}")
        values = entry.get("values")
        if kind == "enum":
            if not values or len(set(values)) != len(values):
                found.append(f"{key}: an enum needs unique values")
            for value in values or []:
                if not LEVEL.match(value) or value == "{n}":
                    found.append(f"{key}: value {value!r} is not lowercase words joined by _")
        elif values is not None:
            found.append(f"{key}: only enum keys have values")
        if "unit" in entry and kind != "integer":
            found.append(f"{key}: only integer keys have a unit")
        extra = set(entry) - {"key", "lists", "type", "unit", "values", "retired", "retired_values"}
        if extra:
            found.append(f"{key}: unknown fields {sorted(extra)}")
    patterns = [levels(key) for key in names]
    for a in patterns:
        for b in patterns:
            if len(a) < len(b) and all(x == y or "{n}" in (x, y) for x, y in zip(a, b)):
                found.append(f"{'.'.join(a)} is a prefix of {'.'.join(b)}")
    for row in catalog.get("row", []):
        for key in row.get("keys", []):
            if "setting" not in names.get(key, {}).get("lists", []):
                found.append(f"row key {key}: not a setting key")
    return found


def breaks(old, new):
    """Returns the changes from an earlier catalog that would break existing clients."""
    found = []
    current = {entry["key"]: entry for entry in new.get("key", [])}
    for entry in old.get("key", []):
        key = entry["key"]
        now = current.get(key)
        if now is None:
            found.append(f"{key}: removed; mark it retired instead")
            continue
        if now.get("type") != entry.get("type"):
            found.append(f"{key}: type changed from {entry.get('type')} to {now.get('type')}")
        if now.get("unit") != entry.get("unit"):
            found.append(f"{key}: unit changed from {entry.get('unit')} to {now.get('unit')}")
        for name in set(entry.get("lists", [])) - set(now.get("lists", [])):
            found.append(f"{key}: no longer listed in {name}")
        kept = set(now.get("values", [])) | set(now.get("retired_values", []))
        for value in entry.get("values", []):
            if value not in kept:
                found.append(f"{key}: value {value} removed; list it in retired_values instead")
        for value in entry.get("retired_values", []):
            if value in now.get("values", []):
                found.append(f"{key}: retired value {value} reused")
    return found


def earlier(ref):
    result = subprocess.run(
        ["git", "show", f"{ref}:{CATALOG}"],
        cwd=ROOT,
        capture_output=True,
        text=True,
    )
    return tomllib.loads(result.stdout) if result.returncode == 0 else None


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--against", help="git revision whose catalog this one must extend")
    args = parser.parse_args(argv)
    catalog = tomllib.loads((ROOT / CATALOG).read_text())
    found = problems(catalog)
    if args.against:
        old = earlier(args.against)
        if old is None:
            print(f"{CATALOG}: no catalog at {args.against}; skipping the comparison")
        else:
            found += breaks(old, catalog)
    for problem in found:
        print(f"{CATALOG}: {problem}", file=sys.stderr)
    return 1 if found else 0


if __name__ == "__main__":
    sys.exit(main())
