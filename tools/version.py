#!/usr/bin/env python3
"""Resolve Cordial's application version without changing tracked manifests."""
import argparse
import os
from pathlib import Path
import re
import subprocess

ROOT = Path(__file__).resolve().parents[1]
NUMBER = r"(?:0|[1-9][0-9]*)"
RELEASE = rf"{NUMBER}\.{NUMBER}\.{NUMBER}"
VERSION = rf"(?:{RELEASE}|0\.0\.0-dev(?:\+g[0-9a-f]+(?:\.dirty)?)?)"


def resolve(root=ROOT, release=False):
    override = os.environ.get("CORDIAL_VERSION")
    if override is not None:
        if not re.fullmatch(RELEASE, override):
            raise ValueError("CORDIAL_VERSION must be MAJOR.MINOR.PATCH without a v prefix")
        return override

    def git(*args):
        return subprocess.run(["git", "-C", str(root), *args], capture_output=True, text=True)

    # A source export inside another repository must not inherit its parent's tag.
    if not (root / ".git").exists():
        if release:
            raise ValueError("Release packaging requires a Git checkout")
        return "0.0.0-dev"
    try:
        head = git("rev-parse", "--short=12", "HEAD")
    except FileNotFoundError:
        if release:
            raise ValueError("Release packaging requires Git on PATH") from None
        return "0.0.0-dev"
    if head.returncode:
        if release:
            raise ValueError("Release packaging requires a committed Git checkout")
        return "0.0.0-dev"
    status = git("status", "--porcelain", "--untracked-files=normal")
    tags = git("tag", "--points-at", "HEAD")
    if status.returncode or tags.returncode:
        raise ValueError("Cannot read Git status or tags")
    matches = sorted(tag[1:] for tag in tags.stdout.splitlines()
                     if re.fullmatch("v" + RELEASE, tag))
    if len(matches) > 1:
        raise ValueError("Multiple release tags point at HEAD")
    if matches and not status.stdout:
        return matches[0]
    if release:
        raise ValueError("Release packaging requires a clean commit tagged vMAJOR.MINOR.PATCH")
    return f"0.0.0-dev+g{head.stdout.strip()}" + (".dirty" if status.stdout else "")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--release", action="store_true")
    args = parser.parse_args()
    try:
        print(resolve(release=args.release))
    except (ValueError, OSError) as error:
        parser.exit(1, f"{error}\n")
