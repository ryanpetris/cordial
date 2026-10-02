#!/usr/bin/env bash
# Installs, reinstalls and removes the built packages for DEB_DISTRIBUTION in a clean container of
# that distribution, then collects them with their checksums in release/. Run as root from the
# repository root.
set -euxo pipefail
export DEBIAN_FRONTEND=noninteractive
packages=build/packages
apt-get update
apt-get install -y --no-install-recommends lintian python3
echo 'path-include=/usr/share/doc/cordial-*' > /etc/dpkg/dpkg.cfg.d/zz-cordial-check
apt-get install -y --no-install-recommends ./$packages/cli-deb/"$DEB_DISTRIBUTION"/cordial-cli_*_amd64.deb
python3 tools/check_linux_package.py cordial-cli --debian --version "$CORDIAL_VERSION"
test ! -e /usr/bin/cordial-desktop
apt-get install -y --no-install-recommends ./$packages/desktop-deb/"$DEB_DISTRIBUTION"/cordial-desktop_*_amd64.deb
python3 tools/check_linux_package.py cordial-desktop --debian --version "$CORDIAL_VERSION"
# Vendor ELF files retain their published bytes under /opt.
lintian --suppress-tags dir-or-file-in-opt,embedded-library,shared-library-is-executable,unstripped-binary-or-object $packages/desktop-deb/"$DEB_DISTRIBUTION"/*.deb
lintian --suppress-tags unstripped-binary-or-object $packages/cli-deb/"$DEB_DISTRIBUTION"/*.deb
apt-get install -y --reinstall ./$packages/cli-deb/"$DEB_DISTRIBUTION"/*.deb ./$packages/desktop-deb/"$DEB_DISTRIBUTION"/*.deb
apt-get purge -y cordial-cli
test ! -e /usr/bin/cordial
python3 tools/check_linux_package.py cordial-desktop --debian --version "$CORDIAL_VERSION"
apt-get purge -y cordial-desktop
test ! -e /usr/bin/cordial-desktop
mkdir -p release
cp $packages/desktop-deb/"$DEB_DISTRIBUTION"/*.deb $packages/cli-deb/"$DEB_DISTRIBUTION"/*.deb release/
cd release
sha256sum ./*.deb > "SHA256SUMS-$DEB_DISTRIBUTION"
