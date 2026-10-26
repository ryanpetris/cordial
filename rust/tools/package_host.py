#!/usr/bin/env python3
"""Package the GNU/Linux desktop, CLI and TUI executable and its notices."""
import argparse
import gzip
import hashlib
import os
from pathlib import Path
import shutil
import subprocess
import tarfile
import tempfile
import time

import dependency_notices

ROOT = Path(__file__).resolve().parents[2]
TARGET = "x86_64-unknown-linux-gnu"


def schemas(destination):
    destination.mkdir(parents=True, exist_ok=True)
    shutil.copy2(ROOT / "packaging/dev.petris.cordial.gschema.xml", destination)
    subprocess.run(["glib-compile-schemas", "--strict", str(destination)], check=True)


def package(target):
    version = subprocess.check_output(
        ["python3", "tools/version.py"], cwd=ROOT, text=True).strip()
    binary = ROOT / "rust/target" / target / "release/cordial"
    with binary.open("rb") as stream:
        header = stream.read(20)
    if header[:6] != b"\x7fELF\x02\x01" or header[18:20] != b"\x3e\x00":
        raise ValueError("Host packages require a Linux x86-64 executable")
    output = ROOT / "build/release"
    output.mkdir(parents=True, exist_ok=True)
    name = f"cordial-{version}-linux-amd64"
    epoch = int(os.environ.get("SOURCE_DATE_EPOCH", time.time()))
    with tempfile.TemporaryDirectory(prefix=".cordial-", dir=output) as temporary:
        root = Path(temporary) / name
        root.mkdir()
        shutil.copy2(binary, root / "cordial")
        schemas(root / "share/glib-2.0/schemas")
        for directory in ("docs", "proto"):
            shutil.copytree(ROOT / directory, root / directory)
        for source in ("LICENSE", "configs/50-cordial.rules",
                       "packaging/dev.petris.cordial.desktop"):
            shutil.copy2(ROOT / source, root / Path(source).name)
        shutil.copy2(ROOT / "packaging/icons/app-512.png", root / "dev.petris.cordial.png")
        shutil.copy2(ROOT / "packaging/icons/app.svg", root / "dev.petris.cordial.svg")
        (root / "VERSION").write_text(version + "\n", encoding="ascii")
        dependency_notices.record(["cargo"], "host", ["desktop"], target, os.environ, root)
        files = sorted(path for path in root.rglob("*") if path.is_file())
        (root / "SHA256SUMS").write_text("".join(
            f"{hashlib.sha256(path.read_bytes()).hexdigest()}  ./{path.relative_to(root).as_posix()}\n"
            for path in files), encoding="utf-8")

        def metadata(member):
            member.uid = member.gid = 0
            member.uname = member.gname = ""
            member.mtime = epoch
            member.mode = 0o755 if member.isdir() or member.name == name + "/cordial" else 0o644
            return member

        archive = Path(temporary) / (name + ".tar.gz")
        with archive.open("wb") as stream, gzip.GzipFile(
                filename="", mode="wb", fileobj=stream, mtime=epoch) as compressed:
            with tarfile.open(fileobj=compressed, mode="w") as tar:
                tar.add(root, arcname=name, filter=metadata)
        archive.replace(output / archive.name)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--target", default=TARGET, choices=(TARGET,))
    package(parser.parse_args().target)
