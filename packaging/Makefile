SHELL := /bin/bash
.SHELLFLAGS := -eu -o pipefail -c
.ONESHELL:
PYTHON ?= python3
APP ?= desktop
export CORDIAL_HOMEPAGE ?= https://github.com/ryanpetris/cordial
DEB_DISTRIBUTION ?= $(shell . /etc/os-release; echo "$${VERSION_CODENAME:-unstable}")
DEB_REVISION ?= 1~$(DEB_DISTRIBUTION)
RECIPE := $(if $(filter desktop,$(APP)),desktop,rust)/packaging
ARCHIVE := $(if $(filter desktop,$(APP)),$(DESKTOP_ARCHIVE),$(CLI_ARCHIVE))

ifeq ($(filter $(APP),desktop cli),)
$(error APP must be desktop or cli)
endif

define prepare
test "$$(uname -sm)" = 'Linux x86_64' || { echo 'Distribution packages require Linux x86-64' >&2; exit 1; }
version=$$("$(PYTHON)" tools/version.py)
export CORDIAL_VERSION="$$version"
archive="$(ARCHIVE)"
if test -z "$$archive"; then
    if test '$(APP)' = desktop; then
        archive="desktop/dist/cordial-desktop-$$version-x64.tar.gz"
    else
        archive="build/release/cordial-cli-$$version-linux-amd64.tar.gz"
    fi
fi
epoch=$$("$(PYTHON)" tools/check_release_archive.py "$$archive" '$(APP)' "$$version")
export SOURCE_DATE_EPOCH=$${SOURCE_DATE_EPOCH:-$$epoch}
output="$$PWD/build/packages/$(APP)-$@$(if $(filter deb,$@),/$(DEB_DISTRIBUTION))"
mkdir -p "$$output"
endef

.PHONY: arch deb
arch:
	command -v makepkg >/dev/null
	command -v fakeroot >/dev/null
	$(prepare)
	cp "$(RECIPE)/arch/PKGBUILD" "$$output/"
	if test '$(APP)' = desktop; then filename="cordial-desktop-$$version-x64.tar.gz"; else filename="cordial-cli-$$version-linux-amd64.tar.gz"; fi
	test "$$archive" -ef "$$output/$$filename" || cp -- "$$archive" "$$output/$$filename"
	export CORDIAL_ARCHIVE_SHA256=$$(sha256sum "$$output/$$filename" | cut -d' ' -f1)
	cd "$$output"
	makepkg --force --cleanbuild --nosign "PKGDEST=$$output" PKGEXT=.pkg.tar.zst

deb:
	command -v dpkg-buildpackage >/dev/null
	command -v dch >/dev/null
	$(prepare)
	source="$$output/cordial-$(APP)-$$version"
	rm -rf -- "$$source"
	mkdir -p "$$source"
	if test '$(APP)' = desktop; then
	    mkdir "$$source/payload"
	    tar --no-same-owner --strip-components=1 -C "$$source/payload" -xzf "$$archive"
	    cp "$$source/payload/LICENSE" "$$source/LICENSE"
	else
	    tar --no-same-owner -C "$$source" --strip-components=1 -xzf "$$archive"
	fi
	cp -R "$(RECIPE)/debian" "$$source/debian"
	cd "$$source"
	cp LICENSE debian/copyright
	DEBEMAIL="$$(dpkg-parsechangelog -S Maintainer)" dch --no-auto-nmu --preserve --force-bad-version --newversion "$$version-$(DEB_REVISION)" --distribution '$(DEB_DISTRIBUTION)' "Cordial $$version."
	dpkg-buildpackage --build=binary --no-sign
