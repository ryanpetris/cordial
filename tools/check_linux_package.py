#!/usr/bin/env python3
"""Check an installed Cordial desktop or CLI package, its ELF files and Debian dependencies."""
import argparse
from pathlib import Path
import subprocess
import tempfile

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("package", choices=["cordial-desktop", "cordial-cli"])
parser.add_argument("--debian", action="store_true")
args = parser.parse_args()
listing = ["dpkg-query", "-L", args.package] if args.debian else ["pacman", "-Qlq", args.package]
files = [Path(line) for line in subprocess.check_output(listing, text=True).splitlines()]
if args.package == "cordial-cli":
    required = ["/usr/bin/cordial", "/usr/share/doc/cordial-cli/docs/protocol/README.md",
                "/usr/share/doc/cordial-cli/schema/README.md", "/usr/share/doc/cordial-cli/DEPENDENCIES.md"]
    subprocess.run(["cordial", "--version"], check=True)
    if "INTERP" in subprocess.check_output(["readelf", "-l", "/usr/bin/cordial"], text=True):
        raise SystemExit("The CLI must be statically linked")
else:
    required = ["/usr/bin/cordial-desktop", "/usr/share/applications/cordial-desktop.desktop",
                "/opt/Cordial/LICENSE"]
    subprocess.run(["udevadm", "--version"], check=True)
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
            "dpkg-shlibdeps", "--ignore-missing-info", "-O", "-l/opt/Cordial",
            *(f"-e{path}" for path in elfs),
        ], cwd=root, text=True)
        dependencies = next(line.removeprefix("shlibs:Depends=") for line in result.splitlines()
                            if line.startswith("shlibs:Depends="))
        declared = subprocess.check_output(["dpkg-query", "-W", "-f=${Depends}", args.package], text=True)
        declared_names = {item.strip().split()[0] for item in declared.split(",")}
        for requirement in dependencies.split(","):
            if requirement.strip().split()[0] not in declared_names:
                raise SystemExit(f"Missing package dependency: {requirement.strip()}")
        subprocess.run(["dpkg-checkbuilddeps", "-I", "-d", dependencies], cwd=root, check=True)
        print(dependencies)
print(f"Checked {len(elfs)} dynamically linked ELF files")
