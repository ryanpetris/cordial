#!/usr/bin/env python3
"""Validate the application version supplied through CORDIAL_VERSION."""
import os
import re
import sys

NUMBER = r"(?:0|[1-9][0-9]*)"
RELEASE = rf"{NUMBER}\.{NUMBER}\.{NUMBER}"


def resolve():
    value = os.environ.get("CORDIAL_VERSION", "0.0.0")
    if not re.fullmatch(RELEASE, value):
        raise ValueError("CORDIAL_VERSION must be MAJOR.MINOR.PATCH without a v prefix")
    return value


if __name__ == "__main__":
    try:
        print(resolve())
    except ValueError as error:
        sys.exit(str(error))
