"""Validate host payloads and extraction boundaries of release archives."""
from io import BytesIO
from pathlib import Path
import tarfile
import tempfile
import unittest

from check_release_archive import check


class ReleaseArchives(unittest.TestCase):
    def test_payload_and_extraction_boundaries(self):
        elf = b"\x7fELF\x02\x01" + b"\0" * 12 + b"\x3e\0"
        files = {name: b"content" for name in (
            "50-cordial.rules", "LICENSE", "DEPENDENCIES.md", "dependencies.json", "SHA256SUMS",
            "dev.petris.cordial.desktop", "dev.petris.cordial.png", "dev.petris.cordial.svg",
            "docs/protocol/README.md", "proto/handshake.proto", "proto/serial/v1/cordial.proto",
            "share/glib-2.0/schemas/dev.petris.cordial.gschema.xml",
            "share/glib-2.0/schemas/gschemas.compiled", "notices/crates/fixture/LICENSE")}
        files.update(VERSION=b"1.2.3\n", cordial=elf)
        files["proto/serial/current"] = b"1\n"
        failures = (None, "version", "architecture", "executable", "escape", "absolute", "duplicate",
                    "link", "hardlink", "fifo", "permissions", "special", "prefix", "revision")
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "release.tar.gz"
            for failure in (*failures, *("missing:" + name for name in files)):
                with self.subTest(failure=failure):
                    root = "cordial-1.2.3-linux-amd64"
                    with tarfile.open(path, "w:gz") as archive:
                        for name, data in files.items():
                            if failure == "missing:" + name:
                                continue
                            if name == "VERSION" and failure == "version":
                                data = b"4.5.6\n"
                            if name == "cordial" and failure == "architecture":
                                data = b"wrong"
                            if name == "proto/serial/current" and failure == "revision":
                                data = b"../outside"
                            member = tarfile.TarInfo(f"{root}/{name}")
                            member.size, member.mtime = len(data), 123
                            member.mode = 0o755 if name == "cordial" and failure != "executable" else 0o644
                            if failure in ("permissions", "special"):
                                member.mode = 0o777 if failure == "permissions" else 0o4755
                            archive.addfile(member, BytesIO(data))
                        if failure in ("escape", "absolute", "duplicate", "link", "hardlink", "fifo", "prefix"):
                            name = {"escape": f"{root}/../outside", "absolute": "/outside",
                                    "prefix": "other/file"}.get(failure, f"{root}/VERSION")
                            member = tarfile.TarInfo(name)
                            if failure in ("link", "hardlink"):
                                member.type = tarfile.SYMTYPE if failure == "link" else tarfile.LNKTYPE
                                member.linkname = "../../outside"
                            if failure == "fifo":
                                member.type = tarfile.FIFOTYPE
                            archive.addfile(member)
                    if failure:
                        with self.assertRaises(ValueError):
                            check(path, "host", "1.2.3")
                    else:
                        self.assertEqual(check(path, "host", "1.2.3"), 123)
