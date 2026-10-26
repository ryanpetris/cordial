SHELL := /bin/bash
.SHELLFLAGS := -eu -o pipefail -c
.ONESHELL:
PYTHON ?= python3
export CORDIAL_HOMEPAGE ?= https://github.com/ryanpetris/cordial
DEB_DISTRIBUTION ?= $(shell . /etc/os-release; echo "$${VERSION_CODENAME:-unstable}")
DEB_REVISION ?= 1~$(DEB_DISTRIBUTION)

define prepare
test "$$(uname -sm)" = 'Linux x86_64' || { echo 'Distribution packages require Linux x86-64' >&2; exit 1; }
version=$$("$(PYTHON)" tools/version.py)
export CORDIAL_VERSION="$$version"
archive="$(HOST_ARCHIVE)"
if test -z "$$archive"; then archive="build/release/cordial-$$version-linux-amd64.tar.gz"; fi
epoch=$$("$(PYTHON)" tools/check_release_archive.py "$$archive" host "$$version")
export SOURCE_DATE_EPOCH=$${SOURCE_DATE_EPOCH:-$$epoch}
output="$$PWD/build/packages/host-$@$(if $(filter deb,$@),/$(DEB_DISTRIBUTION))"
mkdir -p "$$output"
endef

.PHONY: arch deb
arch:
	command -v makepkg >/dev/null
	command -v fakeroot >/dev/null
	$(prepare)
	cp packaging/arch/PKGBUILD "$$output/"
	filename="cordial-$$version-linux-amd64.tar.gz"
	test "$$archive" -ef "$$output/$$filename" || cp -- "$$archive" "$$output/$$filename"
	export CORDIAL_ARCHIVE_SHA256=$$(sha256sum "$$output/$$filename" | cut -d' ' -f1)
	cd "$$output"
	makepkg --force --cleanbuild --nosign "PKGDEST=$$output" PKGEXT=.pkg.tar.zst

deb:
	command -v dpkg-buildpackage >/dev/null
	command -v dch >/dev/null
	$(prepare)
	source="$$output/cordial-$$version"
	rm -rf -- "$$source"
	mkdir -p "$$source"
	tar --no-same-owner -C "$$source" --strip-components=1 -xzf "$$archive"
	cp -R packaging/debian "$$source/debian"
	cd "$$source"
	cp LICENSE debian/copyright
	DEBEMAIL="$$(dpkg-parsechangelog -S Maintainer)" dch --no-auto-nmu --preserve --force-bad-version --newversion "$$version-$(DEB_REVISION)" --distribution '$(DEB_DISTRIBUTION)' "Cordial $$version."
	dpkg-buildpackage --build=binary --no-sign
