#!/usr/bin/env bash
# Run in a disposable DEB_DISTRIBUTION container as root from the repository root.
set -euxo pipefail
export DEBIAN_FRONTEND=noninteractive
package="./build/packages/host-deb/$DEB_DISTRIBUTION/cordial_$CORDIAL_VERSION-${DEB_REVISION:-1~$DEB_DISTRIBUTION}_amd64.deb"
test -f "$package"
apt-get update
apt-get install -y --no-install-recommends lintian python3 dpkg-dev binutils libglib2.0-bin
echo 'path-include=/usr/share/doc/cordial/*' > /etc/dpkg/dpkg.cfg.d/zz-cordial-check
temporary=$(mktemp -d)
trap 'rm -rf "$temporary"' EXIT
apt-get install -y --no-install-recommends "$package"
python3 tools/check_linux_package.py --debian --version "$CORDIAL_VERSION"
# The package retains the release executable and its symbol table.
lintian --suppress-tags unstripped-binary-or-object "$package"
apt-get install -y --reinstall "$package"
python3 tools/check_linux_package.py --debian --version "$CORDIAL_VERSION"
dpkg --verify cordial
apt-get remove -y cordial
test ! -e /usr/bin/cordial
test ! -e /usr/share/glib-2.0/schemas/dev.petris.cordial.gschema.xml
schemas=$(GSETTINGS_BACKEND=memory gsettings list-schemas)
if grep -Fxq dev.petris.cordial <<< "$schemas"; then exit 1; fi
test ! -e /usr/lib/udev/rules.d/50-cordial.rules
test ! -e /usr/share/doc/cordial
test ! -e /usr/share/applications/dev.petris.cordial.desktop
test ! -e /usr/share/icons/hicolor/512x512/apps/dev.petris.cordial.png
test ! -e /usr/share/icons/hicolor/scalable/apps/dev.petris.cordial.svg
apt-get install -y --no-install-recommends "$package"
apt-get purge -y cordial
test ! -e /usr/share/doc/cordial
test ! -e /usr/share/glib-2.0/schemas/dev.petris.cordial.gschema.xml
mkdir -p release
cp "$package" release/
cd release
sha256sum "$(basename "$package")" > "SHA256SUMS-$DEB_DISTRIBUTION"
