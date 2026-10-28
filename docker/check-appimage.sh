#!/usr/bin/env bash
set -euo pipefail
source "$(dirname "$0")/check-output.sh"
image="build/release/cordial-$CORDIAL_VERSION-x86_64.AppImage"
check_output "$image" "build/appimage-smoke-$1"
chmod +x "$image"
case "$1" in
  resolute)
    lavapipe=(/usr/share/vulkan/icd.d/lvp_icd*.json)
    test -f "${lavapipe[0]}"
    export LIBGL_ALWAYS_SOFTWARE=1 GALLIUM_DRIVER=llvmpipe VK_DRIVER_FILES="${lavapipe[0]}"
    renderer=()
    ;;
  arch) renderer=(--renderer cairo) ;;
  *) echo "Unknown AppImage distribution: $1" >&2; exit 1 ;;
esac
xvfb-run -a -s '-screen 0 1280x1024x24' dbus-run-session -- \
  python3 tools/check_appimage.py "$image" "build/appimage-smoke-$1" "${renderer[@]}"
