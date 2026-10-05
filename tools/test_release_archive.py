"""Validate versions, architectures and extraction boundaries of release archives."""
from io import BytesIO
from pathlib import Path
import tarfile
import tempfile
import unittest

from check_release_archive import check


class ReleaseArchives(unittest.TestCase):
    def test_payload_and_extraction_boundaries(self):
        elf = b"\x7fELF\x02\x01" + b"\0" * 12 + b"\x3e\0"
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "release.tar.gz"
            for application, root, binary in (
                ("desktop", "cordial-desktop-1.2.3-x64", "cordial-desktop"),
                ("cli", "cordial-cli-1.2.3-linux-amd64", "cordial"),
            ):
                for failure in (None, "version", "architecture", "executable", "escape", "duplicate", "link", "permissions",
                                "rules"):
                    with self.subTest(application=application, failure=failure):
                        with tarfile.open(path, "w:gz") as archive:
                            for name, data in (("VERSION", b"4.5.6\n" if failure == "version" else b"1.2.3\n"),
                                               (binary, b"wrong" if failure == "architecture" else elf)):
                                member = tarfile.TarInfo(f"{root}/{name}")
                                member.size, member.mtime = len(data), 123
                                member.mode = 0o644 if failure == "executable" else 0o755
                                if failure == "permissions":
                                    member.mode = 0o777
                                archive.addfile(member, BytesIO(data))
                            if failure != "rules":
                                rules = b'TAG+="uaccess"\n'
                                member = tarfile.TarInfo(f"{root}/50-cordial.rules")
                                member.size, member.mtime, member.mode = len(rules), 123, 0o644
                                archive.addfile(member, BytesIO(rules))
                            if failure in ("escape", "duplicate", "link"):
                                member = tarfile.TarInfo(f"{root}/../outside" if failure == "escape" else f"{root}/VERSION")
                                if failure == "link":
                                    member.type, member.linkname = tarfile.SYMTYPE, "../../outside"
                                archive.addfile(member)
                        if failure:
                            with self.assertRaises(ValueError):
                                check(path, application, "1.2.3")
                        else:
                            self.assertEqual(check(path, application, "1.2.3"), 123)
