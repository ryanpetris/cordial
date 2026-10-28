#!/usr/bin/env bash
set -euo pipefail
source "$(dirname "$0")/check-output.sh"
check_output build/package-smoke build/empty-states-smoke build/pairing-reconnect-smoke
export DEBIAN_FRONTEND=noninteractive
apt-get install -y --no-install-recommends "./build/packages/host-deb/resolute/cordial_$CORDIAL_VERSION-${DEB_REVISION:-1~resolute}_amd64.deb"
export CORDIAL_TEST_BINARY=/usr/bin/cordial
for scenario in package empty-states pairing-reconnect; do
  arguments=()
  if [ "$scenario" != package ]; then arguments=("--$scenario"); fi
  xvfb-run -a -s '-screen 0 1280x1024x24' dbus-run-session -- python3 rust/tests/gtk_smoke.py "build/$scenario-smoke" "${arguments[@]}"
done
