#!/usr/bin/env python3
"""Check the installed Cordial package and its native dependencies."""
import argparse
import os
from pathlib import Path
import re
import subprocess
import tempfile


def metadata(package, field, debian):
    if debian:
        return subprocess.check_output(
            ["dpkg-query", "-W", f"-f=${{{field}}}", package], text=True).strip()
    fields = {"Version": "Version", "Depends": "Depends On"}
    output = subprocess.check_output(["pacman", "-Qi", package], text=True)
    match = re.search(r"^" + fields[field] + r"\s*:\s*(.*(?:\n +[^\n]*)*)", output, re.MULTILINE)
    if not match:
        raise ValueError(f"Missing {field} metadata for {package}")
    return " ".join(match[1].split())


def check_debian_dependencies(required, declared):
    """Check shlibdeps minimums against declared exact versions or lower bounds."""
    pattern = r"([a-z0-9][a-z0-9+.:_-]*)(?:\s+\((>=|>>|=)\s+([^() ]+)\))?"

    def parse(value):
        match = re.fullmatch(pattern, value.strip())
        return match.groups() if match else None

    declarations = [parse(item) for item in declared.split(",")]
    for requirement in required.split(","):
        parsed = parse(requirement)
        if parsed is None or parsed[1] not in (None, ">=", ">>"):
            raise ValueError(f"Unsupported shared library requirement: {requirement.strip()}")
        name, operation, minimum = parsed
        satisfied = False
        for candidate in declarations:
            if candidate is None or candidate[0] != name:
                continue
            if operation is None:
                satisfied = True
                break
            _, bound, version = candidate
            if bound is None:
                continue
            # A strict declared lower bound at the same version also implies a strict minimum.
            comparison = ">=" if operation == ">>" and bound == ">>" else operation
            result = subprocess.run(["dpkg", "--compare-versions", version, comparison, minimum],
                                    check=False)
            if result.returncode not in (0, 1):
                raise ValueError(f"Invalid Debian dependency version: {candidate}")
            if result.returncode == 0:
                satisfied = True
                break
        if not satisfied:
            raise ValueError(f"Missing or too weak package dependency: {requirement.strip()}")


def check(debian, version):
    package = "cordial"
    listing = ["dpkg-query", "-L", package] if debian else ["pacman", "-Qlq", package]
    files = {Path(line.rstrip("/") or "/") for line in subprocess.check_output(listing, text=True).splitlines()}
    installed = metadata(package, "Version", debian)
    if version and installed.rsplit("-", 1)[0] != version:
        raise ValueError(f"Unexpected package version: {installed}")
    schema = Path("/usr/share/glib-2.0/schemas/dev.petris.cordial.gschema.xml")
    if any(path.name == "gschemas.compiled" for path in files):
        raise ValueError("The GLib schema cache must be maintained by distribution triggers")
    doc = Path("/usr/share/doc/cordial")
    proto = doc / "proto"
    revision = (proto / "serial/current").read_text().strip()
    if not revision.isdecimal():
        raise ValueError("Invalid installed serial revision")
    required = [
        Path("/usr/bin/cordial"), Path("/usr/lib/udev/rules.d/50-cordial.rules"), schema,
        Path("/usr/share/applications/dev.petris.cordial.desktop"),
        Path("/usr/share/icons/hicolor/512x512/apps/dev.petris.cordial.png"),
        Path("/usr/share/icons/hicolor/scalable/apps/dev.petris.cordial.svg"),
        doc / "docs/protocol/README.md", proto / f"serial/v{revision}/cordial.proto",
        proto / "serial/current", proto / "handshake.proto", doc / "DEPENDENCIES.md",
        doc / "dependencies.json", doc / "VERSION",
    ]
    required.append(Path(f"/usr/share/doc/{package}/copyright") if debian
                    else Path(f"/usr/share/licenses/{package}/LICENSE"))
    for path in required:
        if path not in files or not path.is_file():
            raise ValueError(f"Missing package-owned file: {path}")
    default = subprocess.check_output(["gsettings", "get", "dev.petris.cordial",
                                       "low-battery-percent"], text=True,
                                      env=dict(os.environ, GSETTINGS_BACKEND="memory")).strip()
    if default != "20":
        raise ValueError("The installed GSettings schema cache must be readable")
    if not any(path in files and path.is_file() for path in (doc / "notices").rglob("*")):
        raise ValueError("Missing dependency notices")
    if (doc / "VERSION").read_text().strip() != installed.rsplit("-", 1)[0]:
        raise ValueError("Installed VERSION does not match package metadata")
    entry = Path("/usr/share/applications/dev.petris.cordial.desktop").read_text().splitlines()
    if "Exec=cordial desktop" not in entry or "Icon=dev.petris.cordial" not in entry:
        raise ValueError("Desktop entry must launch cordial desktop with the application icon")
    binary = Path("/usr/bin/cordial")
    if binary.is_symlink() or not os.access(binary, os.X_OK):
        raise ValueError("The package must install an executable regular cordial file")
    reported = subprocess.check_output([str(binary), "--version"], text=True).strip()
    if reported != "cordial " + installed.rsplit("-", 1)[0]:
        raise ValueError(f"Unexpected executable version: {reported}")
    elfs = []
    for path in files:
        if not path.exists():
            raise ValueError(f"Missing package-owned path: {path}")
        if path.lstat().st_uid != 0 or path.lstat().st_gid != 0:
            raise ValueError(f"Unexpected package file owner: {path}")
        if path.stat().st_mode & 0o7022:
            raise ValueError(f"Unsafe package permissions: {path}")
        if not path.is_file() or path.is_symlink():
            continue
        if path.suffix in (".uf2", ".elf") or path.name in ("bootloader.bin", "partition-table.bin"):
            raise ValueError(f"Firmware in host package: {path}")
        with path.open("rb") as stream:
            header = stream.read(20)
        if not header.startswith(b"\x7fELF"):
            continue
        if path != binary:
            raise ValueError(f"Unexpected executable: {path}")
        if header[4:6] != b"\x02\x01" or header[18:20] != b"\x3e\x00":
            raise ValueError(f"Unexpected ELF architecture: {path}")
        dynamic = subprocess.check_output(["readelf", "-d", str(path)], text=True)
        if "libgtk-4.so" not in dynamic or "libadwaita-1.so" not in dynamic:
            raise ValueError("The executable must link GTK4 and libadwaita")
        linked = subprocess.run(["ldd", str(path)], capture_output=True, text=True, check=True)
        if "not found" in linked.stdout + linked.stderr:
            raise ValueError(f"Unresolved libraries for {path}:\n{linked.stdout}{linked.stderr}")
        elfs.append(path)
    if elfs != [binary]:
        raise ValueError("The package must contain exactly one native executable")
    if debian and elfs:
        with tempfile.TemporaryDirectory(prefix="cordial-shlibs-") as directory:
            root = Path(directory)
            (root / "debian").mkdir()
            (root / "debian/control").write_text(
                "Source: cordial\n\nPackage: cordial\nArchitecture: amd64\n")
            result = subprocess.check_output([
                "dpkg-shlibdeps", "-O", *(f"-e{path}" for path in elfs),
            ], cwd=root, text=True)
            dependencies = next(line.removeprefix("shlibs:Depends=") for line in result.splitlines()
                                if line.startswith("shlibs:Depends="))
            declared = metadata(package, "Depends", True)
            check_debian_dependencies(dependencies, declared)
            subprocess.run(["dpkg-checkbuilddeps", "-I", "-d", declared], cwd=root, check=True)
    print(f"Checked {package}: {len(elfs)} dynamically linked ELF files")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--debian", action="store_true")
    parser.add_argument("--version", help="expected upstream application version")
    args = parser.parse_args()
    try:
        check(args.debian, args.version)
    except (OSError, ValueError, subprocess.CalledProcessError) as error:
        parser.exit(1, f"{error}\n")
