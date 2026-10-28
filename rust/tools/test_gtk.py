#!/usr/bin/env python3
"""Build and run ignored desktop tests, each in its own process on one private display."""

import argparse
import json
import os
from pathlib import Path
import shutil
import signal
import subprocess
import sys
import tempfile
import time

ROOT = Path(__file__).resolve().parents[1]


def build_test_binary():
    build = subprocess.run(
        ["cargo", "test", "--locked", "-p", "cordial", "--all-features", "--lib",
         "--no-run", "--message-format=json"],
        cwd=ROOT, stdout=subprocess.PIPE, text=True)
    executables = set()
    for line in build.stdout.splitlines():
        message = json.loads(line)
        if message["reason"] == "compiler-message":
            print(message["message"].get("rendered", ""), end="", file=sys.stderr)
        if (message["reason"] == "compiler-artifact"
                and message["target"]["name"] == "cordial"
                and message["target"]["kind"] == ["lib"]
                and message["profile"]["test"] and message.get("executable")):
            executables.add(message["executable"])
    build.check_returncode()
    if len(executables) != 1:
        raise RuntimeError(f"Expected one Cordial library test executable, found {len(executables)}.")
    return executables.pop()


def environment(root):
    env = dict(os.environ, GDK_BACKEND="x11", GSK_RENDERER="cairo", GTK_A11Y="atspi",
               GTK_USE_PORTAL="0", GIO_USE_VFS="local", GIO_USE_VOLUME_MONITOR="unix",
               GSETTINGS_BACKEND="keyfile", GDK_SCALE="1", GDK_DPI_SCALE="1", LC_ALL="C.UTF-8")
    env.pop("AT_SPI_BUS_ADDRESS", None)
    for kind in ("CONFIG", "CACHE", "DATA", "STATE", "RUNTIME"):
        directory = root / kind.lower()
        directory.mkdir(mode=0o700)
        env[f"XDG_{kind}_HOME" if kind != "RUNTIME" else "XDG_RUNTIME_DIR"] = str(directory)
    temporary = root / "tmp"
    temporary.mkdir()
    env["TMPDIR"] = str(temporary)
    return env


def run_tests(executable):
    listing = subprocess.check_output(
        [executable, "--list", "--ignored", "--format", "terse"], cwd=ROOT, text=True)
    tests = [line.removesuffix(": test") for line in listing.splitlines()
             if line.startswith("desktop::") and line.endswith(": test")]
    if not tests:
        raise RuntimeError("No ignored desktop GTK tests were found.")
    print(f"Found {len(tests)} ignored desktop GTK tests in {executable}.", flush=True)
    started = time.monotonic()
    failed = 0
    for name in tests:
        print(f"RUN {name}", flush=True)
        start = time.monotonic()
        with tempfile.TemporaryDirectory(prefix="cordial-gtk-test-") as directory:
            try:
                result = subprocess.run(
                    [executable, name, "--ignored", "--exact", "--nocapture"],
                    cwd=ROOT, env=environment(Path(directory)), timeout=120)
                passed = result.returncode == 0
                detail = f"exit {result.returncode}"
            except subprocess.TimeoutExpired:
                passed, detail = False, "timed out after 120s"
        failed += not passed
        print(f"{'PASS' if passed else 'FAIL'} {name} ({time.monotonic() - start:.3f}s, {detail})",
              flush=True)
    print(f"GTK tests: {len(tests) - failed} passed, {failed} failed "
          f"({time.monotonic() - started:.3f}s).", flush=True)
    return int(failed != 0)


def stop_session(process):
    try:
        os.killpg(process.pid, signal.SIGTERM)
        deadline = time.monotonic() + 5
        while time.monotonic() < deadline:
            process.poll()
            os.killpg(process.pid, 0)
            time.sleep(0.05)
    except ProcessLookupError:
        return
    finally:
        try:
            os.killpg(process.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--session", metavar="EXECUTABLE", help=argparse.SUPPRESS)
    args = parser.parse_args()
    if args.session:
        return run_tests(args.session)
    started = time.monotonic()
    prerequisites = ("cargo", "xvfb-run", "Xvfb", "xauth", "dbus-run-session", "glib-compile-schemas")
    missing = [name for name in prerequisites if not shutil.which(name)]
    if missing:
        raise RuntimeError("Missing GTK test prerequisites: " + ", ".join(missing) + ".")
    executable = build_test_binary()
    with tempfile.TemporaryDirectory(prefix="cordial-gtk-session-") as directory:
        command = ["xvfb-run", "-a", "-s", "-screen 0 1280x1024x24", "dbus-run-session", "--",
                   sys.executable, str(Path(__file__).resolve()), "--session", executable]
        with subprocess.Popen(command, cwd=ROOT, env=environment(Path(directory)),
                              start_new_session=True) as process:
            try:
                status = process.wait()
            finally:
                stop_session(process)
    print(f"GTK runner: {time.monotonic() - started:.3f}s total.", flush=True)
    return status


if __name__ == "__main__":
    for handled_signal in (signal.SIGTERM, signal.SIGHUP):
        signal.signal(handled_signal, lambda signum, _: sys.exit(128 + signum))
    try:
        sys.exit(main())
    except (OSError, RuntimeError, subprocess.SubprocessError, ValueError) as error:
        print(f"GTK tests failed: {error}", file=sys.stderr)
        sys.exit(1)
    except KeyboardInterrupt:
        sys.exit(130)
