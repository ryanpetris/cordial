#!/usr/bin/env python3
"""Check a Linux release archive before native packaging and print its timestamp."""
import argparse
from pathlib import PurePosixPath
import tarfile


def check(path, application, version):
    prefix = (f"cordial-cli-{version}-linux-amd64/" if application == "cli"
              else f"cordial-desktop-{version}-x64/")
    executable = "cordial" if application == "cli" else "cordial-desktop"
    with tarfile.open(path, "r:gz") as archive:
        members = {}
        for member in archive:
            name = PurePosixPath(member.name)
            if name.is_absolute() or ".." in name.parts:
                raise ValueError("Archive paths must stay inside the package")
            if not member.isfile() and not member.isdir():
                raise ValueError("Release archives contain only files and directories")
            if member.mode & 0o7022:
                raise ValueError("Archive permissions must not grant special bits or write access to other users")
            normalized = str(name)
            if normalized != prefix.rstrip("/") and not normalized.startswith(prefix):
                raise ValueError("Archive must contain one application directory")
            if normalized in members:
                raise ValueError(f"Duplicate archive path: {normalized}")
            members[normalized] = member
        metadata = members.get(prefix + "VERSION")
        binary = members.get(prefix + executable)
        if not metadata or not metadata.isfile() or metadata.size > 64:
            raise ValueError("Archive must contain a regular VERSION file")
        if archive.extractfile(metadata).read().decode("ascii").strip() != version:
            raise ValueError("Archive version does not match CORDIAL_VERSION")
        rules = members.get(prefix + "50-cordial.rules")
        if not rules or not rules.isfile():
            raise ValueError("Archive must contain the udev rules")
        if not binary or not binary.isfile() or not binary.mode & 0o111:
            raise ValueError("Archive must contain the application executable")
        header = archive.extractfile(binary).read(20)
        if header[:6] != b"\x7fELF\x02\x01" or header[18:20] != b"\x3e\x00":
            raise ValueError("Distribution archives require Linux x86-64 executables")
        return int(metadata.mtime)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("archive")
    parser.add_argument("application", choices=("desktop", "cli"))
    parser.add_argument("version")
    args = parser.parse_args()
    try:
        print(check(args.archive, args.application, args.version))
    except (OSError, ValueError, tarfile.TarError) as error:
        parser.exit(1, f"{error}\n")
