#!/usr/bin/env bash
# Activates the Rust Xtensa toolchain and ESP-IDF, then runs the build command.
set -e
. /opt/export-esp.sh
. "$IDF_PATH/export.sh" >/dev/null
exec "$@"
