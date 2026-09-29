#!/usr/bin/env python3
"""Build the Rust CLI/TUI, dependency notices and checksums. Never opens adapters."""
import argparse
import os
from pathlib import Path
import platform
import shutil
import subprocess
import sys

import build_firmware
import dependency_notices

ROOT = Path(__file__).resolve().parents[1]
TARGETS = {
    "linux/amd64": "x86_64-unknown-linux-musl",
    "linux/arm64": "aarch64-unknown-linux-musl",
    "windows/amd64": "x86_64-pc-windows-msvc",
    "windows/arm64": "aarch64-pc-windows-msvc",
    "darwin/amd64": "x86_64-apple-darwin",
    "darwin/arm64": "aarch64-apple-darwin",
}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--target", action="append", choices=TARGETS,
                        help="repeat for multiple targets; defaults to this OS and CPU")
    parser.add_argument("--release", action="store_true",
                        help="require CORDIAL_VERSION or a clean release tag")
    parser.add_argument("--output", type=Path, default=ROOT.parent / "build" / "release")
    args = parser.parse_args()
    host_os = platform.system().lower()
    host_arch = {"x86_64": "amd64", "AMD64": "amd64", "aarch64": "arm64"}.get(
        platform.machine(), platform.machine())
    targets = list(dict.fromkeys(args.target or [f"{host_os}/{host_arch}"]))
    if any(t not in TARGETS for t in targets):
        parser.error("unsupported native target; choose --target explicitly")
    version = build_firmware.artifact.resolve(release=args.release)
    env = dict(os.environ, CORDIAL_PYTHON=sys.executable)
    for label in targets:
        target = TARGETS[label]
        target_os, arch = label.split("/")
        name = f"cordial-{version}-{target_os}-{arch}"
        executable = "cordial.exe" if target_os == "windows" else "cordial"
        # Linux binaries use musl and Rust's bundled linker/runtime for a single
        # executable with no dependency on the destination's glibc version.
        if target.endswith("linux-musl"):
            sysroot = subprocess.check_output(["rustc", "--print", "sysroot"], text=True).strip()
            compiler_host = next(line.split(": ", 1)[1] for line in subprocess.check_output(
                ["rustc", "-vV"], text=True).splitlines() if line.startswith("host:"))
            linker = Path(sysroot) / "lib/rustlib" / compiler_host / "bin/rust-lld"
            env["CARGO_TARGET_" + target.upper().replace("-", "_") + "_LINKER"] = str(linker)
        if target.endswith("windows-msvc"):
            env["CARGO_TARGET_" + target.upper().replace("-", "_") + "_RUSTFLAGS"] = "-C target-feature=+crt-static"
        subprocess.run(["cargo", "build", "--locked", "--release", "-p", "cordial-client",
                        "--bin", "cordial", "--target", target], cwd=ROOT, env=env, check=True)
        def populate(staged):
            shutil.copy2(ROOT / "target" / target / "release" / executable, staged / executable)
            dependency_notices.record(["cargo"], "host", [], target, env, staged)
            build_firmware.release_docs(staged)
        output = args.output.resolve() / name
        build_firmware.publish(output, populate)
        print(output / executable)


if __name__ == "__main__":
    main()
