#!/usr/bin/env bash
set -euo pipefail
metadata=$(cd "$(dirname "$0")" && pwd)
export DEBIAN_FRONTEND=noninteractive
install_runtime_dependencies() {
    dependencies=$(python3 - "$metadata/control" <<'PY'
from email.parser import Parser
from pathlib import Path
import sys

binary = Parser().parsestr(Path(sys.argv[1]).read_text().split('\n\n')[1])
print(', '.join(dependency.strip() for dependency in binary['Depends'].split(',')
                if '${' not in dependency))
PY
    )
    apt-get satisfy -y --no-install-recommends "$dependencies"
    if dpkg-query -W cordial; then echo 'Cordial must be absent from prerequisite images' >&2; exit 1; fi
}
case "$1" in
  package-deb)
    apt-get update
    apt-get install -y --no-install-recommends lintian python3 dpkg-dev binutils libglib2.0-bin
    install_runtime_dependencies
    echo 'path-include=/usr/share/doc/cordial/*' > /etc/dpkg/dpkg.cfg.d/zz-cordial-check
    ;;
  package-arch)
    CORDIAL_ARCHIVE_SHA256=SKIP source "$metadata/PKGBUILD"
    pacman -Syu --noconfirm --needed namcap python binutils "${depends[@]}"
    sed -i '/^NoExtract.*usr\/share\/doc/d' /etc/pacman.conf
    if pacman -Q cordial; then echo 'Cordial must be absent from prerequisite images' >&2; exit 1; fi
    ;;
  appimage-resolute)
    apt-get update
    apt-get install -y --no-install-recommends \
      libc6 libgcc-s1 libstdc++6 libx11-6 libxau6 libxcursor1 libxdamage1 libxdmcp6 \
      libxext6 libxfixes3 libxi6 libxinerama1 libxrandr2 libxrender1 libxcb1 \
      libxcb-render0 libxcb-shm0 libwayland-client0 libwayland-cursor0 libwayland-egl1 \
      libvulkan1 libexpat1 libcom-err2 libgmp10 libgpg-error0 zlib1g \
      python3-pyatspi python3-gi xvfb xauth dbus-x11 imagemagick libgl1-mesa-dri \
      libegl1 libgl1 libgles2 libopengl0 libx11-xcb1 libdrm2 libgbm1 libegl-mesa0 \
      mesa-vulkan-drivers libglib2.0-bin dconf-gsettings-backend dconf-service
    ;;
  appimage-arch)
    pacman -Syu --noconfirm --needed \
      libgcc libx11 libxcursor libxdamage libxext libxfixes libxi libxinerama \
      libxrandr libxrender libxcb wayland libglvnd vulkan-icd-loader expat e2fsprogs \
      gmp libgpg-error zlib python-atspi python-gobject xorg-server-xvfb xorg-xauth \
      dbus imagemagick glib2 dconf
    ;;
  desktop-resolute)
    apt-get update
    apt-get install -y --no-install-recommends python3-pyatspi python3-gi gir1.2-adw-1 xvfb xauth dbus-x11 imagemagick
    install_runtime_dependencies
    ;;
  *) echo "Unknown prerequisite kind: $1" >&2; exit 1 ;;
esac
