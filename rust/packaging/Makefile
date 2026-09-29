SHELL := /bin/bash
.SHELLFLAGS := -eu -o pipefail -c
.ONESHELL:
PYTHON ?= python3
export CLI_ARCHIVE ?=
export CORDIAL_HOMEPAGE ?= https://github.com/ryanpetris/cordial

# Run from the repository root, through the top-level Makefile.
define prepare
test "$$(uname -sm)" = 'Linux x86_64' || { echo 'CLI packages require Linux x86-64' >&2; exit 1; }
[[ "$$CORDIAL_HOMEPAGE" =~ ^https?://[^/[:space:][:cntrl:]]+ && ! "$$CORDIAL_HOMEPAGE" =~ [[:space:][:cntrl:]] ]] || { echo 'CORDIAL_HOMEPAGE must be an HTTP(S) URL' >&2; exit 1; }
CORDIAL_VERSION=$$("$(PYTHON)" tools/version.py --release)
export CORDIAL_VERSION
export SOURCE_DATE_EPOCH=$${SOURCE_DATE_EPOCH:-$$(git show -s --format=%ct HEAD)}
output="$$PWD/build/packages/$@"
release="cordial-$$CORDIAL_VERSION-linux-amd64"
mkdir -p "$$output"
if test -n "$$CLI_ARCHIVE"; then
    test "$$CLI_ARCHIVE" -ef "$$output/$$release.tar.gz" || cp -- "$$CLI_ARCHIVE" "$$output/$$release.tar.gz"
else
    "$(PYTHON)" rust/bootstrap.py package-cli
    tar -C build/release -czf "$$output/$$release.tar.gz" "$$release"
fi
endef

.PHONY: arch deb
arch:
	command -v makepkg >/dev/null
	command -v fakeroot >/dev/null
	$(prepare)
	cp rust/packaging/arch/PKGBUILD "$$output/"
	cd "$$output"
	CORDIAL_ARCHIVE_SHA256="$$(sha256sum "$$release.tar.gz" | cut -d' ' -f1)" makepkg --force --nosign "PKGDEST=$$output" PKGEXT=.pkg.tar.zst

deb:
	command -v dpkg-buildpackage >/dev/null
	command -v dh >/dev/null
	$(prepare)
	rm -rf -- "$$output/$$release"
	tar -C "$$output" -xzf "$$output/$$release.tar.gz"
	cp -R rust/packaging/debian "$$output/$$release/debian"
	cd "$$output/$$release"
	cp LICENSE debian/copyright
	printf 'cordial-cli (%s) unstable; urgency=medium\n\n  * Cordial %s.\n\n -- Ryan Petris <ryan@petris.net>  %s\n' "$$CORDIAL_VERSION" "$$CORDIAL_VERSION" "$$(LC_ALL=C date -u -R -d "@$$SOURCE_DATE_EPOCH")" > debian/changelog
	dpkg-buildpackage --build=binary --no-sign
