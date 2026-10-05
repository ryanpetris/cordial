#!/usr/bin/env python3
"""Validate the application version supplied through CORDIAL_VERSION, and choose the release the
protocol compatibility checks compare against."""
import os
import re
import subprocess
import sys

NUMBER = r"(?:0|[1-9][0-9]*)"
RELEASE = rf"{NUMBER}\.{NUMBER}\.{NUMBER}"
DEVELOPMENT = "0.0.0"


def resolve():
    value = os.environ.get("CORDIAL_VERSION", DEVELOPMENT)
    if not re.fullmatch(RELEASE, value):
        raise ValueError("CORDIAL_VERSION must be MAJOR.MINOR.PATCH without a v prefix")
    return value


def parse(value):
    return tuple(int(n) for n in value.split("."))


def series(version):
    """Releases that must stay compatible: before 1.0 a minor version may break compatibility,
    from 1.0 only a major version may."""
    major, minor, _ = version
    return (0, minor) if major == 0 else (major,)


def protocol_base(version, tags):
    """The release tag the protocol checks compare against: the newest earlier release in the
    same series as `version`, or the newest release for a development build. None when there is
    no such release."""
    releases = sorted(
        (parse(t[1:]), t) for t in tags if re.fullmatch(rf"v{RELEASE}", t)
    )
    if version == DEVELOPMENT:
        return releases[-1][1] if releases else None
    current = parse(version)
    earlier = [t for v, t in releases if v < current and series(v) == series(current)]
    return earlier[-1] if earlier else None


def released_tags():
    """Release tags reachable from the commit before HEAD, so a tag on HEAD itself is skipped."""
    out = subprocess.run(
        ["git", "tag", "--merged", "HEAD^", "--list", "v*"],
        check=True,
        capture_output=True,
        text=True,
    ).stdout
    return out.split()


if __name__ == "__main__":
    try:
        version = resolve()
        if sys.argv[1:] == ["--protocol-base"]:
            print(protocol_base(version, released_tags()) or "")
        else:
            print(version)
    except (ValueError, subprocess.CalledProcessError) as error:
        sys.exit(str(error))
