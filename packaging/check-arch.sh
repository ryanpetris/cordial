#!/usr/bin/env bash
# Run in a disposable Arch container as root from the repository root.
set -euxo pipefail
pacman -Syu --noconfirm namcap python binutils
sed -i '/^NoExtract.*usr\/share\/doc/d' /etc/pacman.conf
package="build/packages/host-arch/cordial-$CORDIAL_VERSION-1-x86_64.pkg.tar.zst"
test -f "$package"
temporary=$(mktemp -d)
trap 'rm -rf "$temporary"' EXIT
pacman -U --noconfirm "$package"
python tools/check_linux_package.py --version "$CORDIAL_VERSION"
namcap "$package" > "$temporary/namcap.log"
cat "$temporary/namcap.log"
if grep -q ' E:' "$temporary/namcap.log"; then exit 1; fi
pacman -U --noconfirm "$package"
python tools/check_linux_package.py --version "$CORDIAL_VERSION"
pacman -Qkk cordial
pacman -R --noconfirm cordial
test ! -e /usr/bin/cordial
test ! -e /usr/share/glib-2.0/schemas/dev.petris.cordial.gschema.xml
schemas=$(GSETTINGS_BACKEND=memory gsettings list-schemas)
if grep -Fxq dev.petris.cordial <<< "$schemas"; then exit 1; fi
test ! -e /usr/lib/udev/rules.d/50-cordial.rules
test ! -e /usr/share/doc/cordial
test ! -e /usr/share/applications/dev.petris.cordial.desktop
test ! -e /usr/share/icons/hicolor/512x512/apps/dev.petris.cordial.png
test ! -e /usr/share/icons/hicolor/scalable/apps/dev.petris.cordial.svg
test ! -e /usr/share/licenses/cordial
mkdir -p release
cp "$package" release/
cd release
sha256sum "$(basename "$package")" > SHA256SUMS-arch
