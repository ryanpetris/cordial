#!/usr/bin/env python3
"""Check a Linux release archive before native packaging and print its timestamp."""
import argparse
from pathlib import PurePosixPath
import tarfile


def check(path, application, version):
    if application != "host":
        raise ValueError("Expected a unified host archive")
    prefix = f"cordial-{version}-linux-amd64/"
    executable = "cordial"
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
        required = ("50-cordial.rules", "LICENSE", "DEPENDENCIES.md", "dependencies.json",
                    "SHA256SUMS", "dev.petris.cordial.desktop", "dev.petris.cordial.png",
                    "dev.petris.cordial.svg", "docs/protocol/README.md", "proto/handshake.proto",
                    "proto/serial/current", "share/glib-2.0/schemas/dev.petris.cordial.gschema.xml",
                    "share/glib-2.0/schemas/gschemas.compiled")
        for name in required:
            member = members.get(prefix + name)
            if not member or not member.isfile() or not member.size:
                raise ValueError(f"Archive must contain a nonempty regular {name} file")
        revision = archive.extractfile(members[prefix + "proto/serial/current"]).read(64).decode("ascii").strip()
        if not revision.isdecimal() or len(revision) > 10:
            raise ValueError("Archive must contain a valid serial revision")
        schema = members.get(prefix + f"proto/serial/v{revision}/cordial.proto")
        if not schema or not schema.isfile() or not schema.size:
            raise ValueError("Archive must contain the current serial schema")
        if not any(name.startswith(prefix + "notices/") and member.isfile() and member.size
                   for name, member in members.items()):
            raise ValueError("Archive must contain dependency notices")
        if not binary or not binary.isfile() or not binary.mode & 0o111:
            raise ValueError("Archive must contain the application executable")
        header = archive.extractfile(binary).read(20)
        if header[:6] != b"\x7fELF\x02\x01" or header[18:20] != b"\x3e\x00":
            raise ValueError("Distribution archives require Linux x86-64 executables")
        return int(metadata.mtime)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("archive")
    parser.add_argument("application", choices=("host",))
    parser.add_argument("version")
    args = parser.parse_args()
    try:
        print(check(args.archive, args.application, args.version))
    except (OSError, ValueError, tarfile.TarError) as error:
        parser.exit(1, f"{error}\n")
