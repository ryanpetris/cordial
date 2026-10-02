#!/usr/bin/env bash
# Installs, reinstalls and removes the built Arch packages in a clean Arch container, then collects
# them with their checksums in release/. Run as root from the repository root.
set -euxo pipefail
pacman -Syu --noconfirm namcap python
sed -i '/^NoExtract.*usr\/share\/doc/d' /etc/pacman.conf
pacman -U --noconfirm build/packages/cli-arch/cordial-cli-*.pkg.tar.zst
python tools/check_linux_package.py cordial-cli --version "$CORDIAL_VERSION"
test ! -e /usr/bin/cordial-desktop
pacman -U --noconfirm build/packages/desktop-arch/cordial-desktop-*.pkg.tar.zst
python tools/check_linux_package.py cordial-desktop --version "$CORDIAL_VERSION"
# Bundled Electron belongs in /opt under the Arch Electron guidelines.
namcap -e elfpaths build/packages/desktop-arch/cordial-desktop-*.pkg.tar.zst > namcap.log
namcap build/packages/cli-arch/cordial-cli-*.pkg.tar.zst >> namcap.log
cat namcap.log
if grep -q ' E:' namcap.log; then exit 1; fi
pacman -U --noconfirm build/packages/desktop-arch/cordial-desktop-*.pkg.tar.zst build/packages/cli-arch/cordial-cli-*.pkg.tar.zst
pacman -R --noconfirm cordial-cli
test ! -e /usr/bin/cordial
python tools/check_linux_package.py cordial-desktop --version "$CORDIAL_VERSION"
pacman -R --noconfirm cordial-desktop
test ! -e /usr/bin/cordial-desktop
mkdir -p release
cp build/packages/desktop-arch/*.pkg.tar.zst build/packages/cli-arch/*.pkg.tar.zst release/
cd release
sha256sum ./*.pkg.tar.zst > SHA256SUMS-arch
