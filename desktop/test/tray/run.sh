#!/bin/sh
# Runs the built app on a private session bus with a mock tray watcher,
# printing what the tray exports. Needs xvfb-run. SIMULATE sets the number of
# simulated adapters (default 2; 0 uses attached adapters). APP selects the
# executable, for example dist/linux-unpacked/cordial-desktop.
cd "$(dirname "$0")/../.." || exit 1
exec dbus-run-session -- sh -c '
  python3 test/tray/watcher.py 12 &
  sleep 1
  profile=$(mktemp -d)
  CORDIAL_DESKTOP_SIMULATE=${SIMULATE:-2} timeout 11 xvfb-run -a ${APP:-npx electron .} --user-data-dir="$profile" --hidden >/dev/null 2>&1
  rm -rf "$profile"
  wait'
