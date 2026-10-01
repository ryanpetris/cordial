#!/usr/bin/env python3
"""Check an installed Cordial desktop or CLI package, its ELF files and Debian dependencies."""
import argparse
import json
import os
from pathlib import Path
import subprocess
import tempfile

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("package", choices=["cordial-desktop", "cordial-cli"])
parser.add_argument("--debian", action="store_true")
parser.add_argument("--version", help="expected upstream application version")
args = parser.parse_args()
listing = ["dpkg-query", "-L", args.package] if args.debian else ["pacman", "-Qlq", args.package]
files = [Path(line) for line in subprocess.check_output(listing, text=True).splitlines()]
if args.package == "cordial-cli":
    required = ["/usr/bin/cordial", "/usr/share/doc/cordial-cli/docs/protocol/README.md",
                "/usr/share/doc/cordial-cli/proto/cordial.proto", "/usr/share/doc/cordial-cli/DEPENDENCIES.md"]
    reported = subprocess.check_output(["cordial", "--version"], text=True).strip()
    if args.version and reported != f"cordial {args.version}":
        raise SystemExit(f"Unexpected CLI version: {reported}")
    if "INTERP" in subprocess.check_output(["readelf", "-l", "/usr/bin/cordial"], text=True):
        raise SystemExit("The CLI must be statically linked")
else:
    required = ["/usr/bin/cordial-desktop", "/usr/share/applications/cordial-desktop.desktop",
                "/opt/cordial-desktop/LICENSE", "/opt/cordial-desktop/VERSION",
                "/opt/cordial-desktop/resources/app.asar", "/opt/cordial-desktop/chrome-sandbox"]
    subprocess.run(["udevadm", "--version"], check=True)
    root = Path("/opt/cordial-desktop")
    if Path("/usr/bin/cordial-desktop").resolve() != root / "cordial-desktop":
        raise SystemExit("Desktop launcher must use /opt/cordial-desktop")
    if (root / "chrome-sandbox").stat().st_mode & 0o7777 != 0o4755:
        raise SystemExit("Electron sandbox helper must have mode 4755")
    script = """
const root = '/opt/cordial-desktop/resources/app.asar';
const version = require(root + '/package.json').version;
require(root + '/node_modules/usb');
require(root + '/node_modules/serialport').SerialPort.list().then(() => console.log(JSON.stringify({version})));
"""
    result = subprocess.check_output([str(root / "cordial-desktop"), "-e", script],
                                     env=dict(os.environ, ELECTRON_RUN_AS_NODE="1"), text=True)
    reported = json.loads(result)["version"]
    if reported != (root / "VERSION").read_text().strip() or (args.version and reported != args.version):
        raise SystemExit(f"Unexpected desktop version: {reported}")
    if args.debian:
        profile = Path("/etc/apparmor.d/cordial-desktop").read_text()
        if "/opt/cordial-desktop/cordial-desktop" not in profile or "userns," not in profile:
            raise SystemExit("Missing Electron user namespace AppArmor profile")
required.append(f"/usr/share/doc/{args.package}/copyright" if args.debian
                else f"/usr/share/licenses/{args.package}/LICENSE")
for path in required:
    if not Path(path).is_file():
        raise SystemExit(f"Missing installed file: {path}")
elfs = []
for path in files:
    if path.exists() and (path.lstat().st_uid != 0 or path.lstat().st_gid != 0):
        raise SystemExit(f"Unexpected package file owner: {path}")
    if not path.is_file() or path.is_symlink():
        continue
    if path.suffix in (".uf2", ".elf") or path.name in ("bootloader.bin", "partition-table.bin"):
        raise SystemExit(f"Firmware in host package: {path}")
    with path.open("rb") as stream:
        header = stream.read(20)
    if not header.startswith(b"\x7fELF"):
        continue
    if header[4:6] != b"\x02\x01" or header[18:20] != b"\x3e\x00":
        raise SystemExit(f"Unexpected ELF architecture: {path}")
    dynamic = subprocess.check_output(["readelf", "-d", str(path)], text=True)
    if "(NEEDED)" not in dynamic:
        continue
    linked = subprocess.run(["ldd", str(path)], capture_output=True, text=True, check=True)
    if "not found" in linked.stdout + linked.stderr:
        raise SystemExit(f"Unresolved libraries for {path}:\n{linked.stdout}{linked.stderr}")
    elfs.append(path)
if args.debian and elfs:
    # Bundled Electron libraries have no dpkg owner; their system dependencies do.
    with tempfile.TemporaryDirectory(prefix="cordial-shlibs-") as directory:
        root = Path(directory)
        (root / "debian").mkdir()
        (root / "debian/control").write_text(f"Source: {args.package}\n\nPackage: {args.package}\nArchitecture: amd64\n")
        result = subprocess.check_output([
            "dpkg-shlibdeps", "--ignore-missing-info", "-O", "-l/opt/cordial-desktop",
            *(f"-e{path}" for path in elfs),
        ], cwd=root, text=True)
        dependencies = next(line.removeprefix("shlibs:Depends=") for line in result.splitlines()
                            if line.startswith("shlibs:Depends="))
        declared = subprocess.check_output(["dpkg-query", "-W", "-f=${Depends}", args.package], text=True)
        declared_requirements = {item.strip() for item in declared.split(",")}
        for requirement in dependencies.split(","):
            if requirement.strip() not in declared_requirements:
                raise SystemExit(f"Missing package dependency: {requirement.strip()}")
        subprocess.run(["dpkg-checkbuilddeps", "-I", "-d", dependencies], cwd=root, check=True)
        subprocess.run(["dpkg-checkbuilddeps", "-I", "-d", declared], cwd=root, check=True)
        print(dependencies)
if args.version:
    if args.debian:
        installed = subprocess.check_output(["dpkg-query", "-W", "-f=${Version}", args.package], text=True)
    else:
        installed = subprocess.check_output(["pacman", "-Q", args.package], text=True).split()[1]
    if installed.rsplit("-", 1)[0] != args.version:
        raise SystemExit(f"Unexpected package version: {installed}")
print(f"Checked {len(elfs)} dynamically linked ELF files")
