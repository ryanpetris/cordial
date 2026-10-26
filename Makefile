.DEFAULT_GOAL := help
SHELL := /bin/bash
.SHELLFLAGS := -eu -o pipefail -c
.ONESHELL:
PYTHON ?= python3
export BUF ?= buf
export CORDIAL_VERSION ?= 0.0.0
HOST_TARGET ?= x86_64-unknown-linux-gnu
BOARD ?= pico_w
PROFILE ?= development
PROTOCOL_STABLE_BASE ?=
PROTOCOL_BASE ?= $(shell $(PYTHON) tools/version.py --protocol-base 2>/dev/null)
BOARDS := pico_w pico2_w pimoroni_pico_plus2_w waveshare_rp2350b_plus_w esp32s3
ESP_BOARDS := esp32s3
DOCKER ?= docker
DEB_DISTRIBUTION ?= trixie
DEB_BASE_trixie := debian:13
DEB_BASE_resolute := ubuntu:26.04
DEB_BASE = $(or $(DEB_BASE_$(DEB_DISTRIBUTION)),$(error Unknown DEB_DISTRIBUTION $(DEB_DISTRIBUTION)))
FIRMWARE_PLATFORM = $(if $(filter $(ESP_BOARDS),$(BOARD)),esp32s3,pico)

.PHONY: help all desktop cli local-desktop local-cli firmware firmware-all \
	package-desktop package-desktop-tar \
	package-cli package-cli-tar package-firmware \
	local-package-desktop local-package-desktop-tar local-package-cli \
	image-host image-host-trixie image-pico image-esp32s3 image-arch image-deb host local-host \
	package-host-tar local-package-host-tar package-host-appimage local-package-host-appimage \
	package-host-arch package-host-deb \
	check check-firmware check-firmware-all check-rust check-protocol check-tools version clean
help:
	@echo 'Cordial: all desktop cli firmware firmware-all check check-firmware check-firmware-all version clean'
	@echo 'Without Docker: local-desktop local-cli'
	@echo 'Release packages: package-desktop package-cli package-firmware (CORDIAL_VERSION, default 0.0.0)'
	@echo 'Distribution packages: package-host-arch package-host-deb (DEB_DISTRIBUTION=$(DEB_DISTRIBUTION))'
	@echo 'Firmware options: BOARD=$(BOARD) PROFILE=$(PROFILE)'
	@echo 'Boards: $(BOARDS)'
all: desktop cli firmware

# Toolchain images from docker/. Each build copies the checkout into one of them (without the
# paths in .dockerignore), runs there, and exports only its outputs back into the checkout.
image-host:
	$(DOCKER) build -t cordial-host -f docker/host.Dockerfile docker
image-host-trixie:
	$(DOCKER) build -t cordial-host-trixie --build-arg BASE=$(DEB_BASE_trixie) -f docker/host.Dockerfile docker
image-pico:
	$(DOCKER) build -t cordial-pico -f docker/pico.Dockerfile docker
image-esp32s3:
	$(DOCKER) build -t cordial-esp32s3 -f docker/esp32s3.Dockerfile docker
image-arch:
	$(DOCKER) build -t cordial-arch -f docker/arch.Dockerfile packaging/arch
image-deb: $(if $(filter trixie,$(DEB_DISTRIBUTION)),image-host-trixie)
	$(DOCKER) build -t cordial-deb-$(DEB_DISTRIBUTION) --build-arg BASE=$(if $(filter trixie,$(DEB_DISTRIBUTION)),cordial-host-trixie,$(DEB_BASE)) -f docker/deb.Dockerfile packaging/debian
comma := ,
# $(call build,IMAGE,COMMAND,OUTPUTS[,INPUT]) runs COMMAND with bash in the image and exports the
# paths matching OUTPUTS, relative to the checkout; each replaces the checkout's copy, so nothing
# from an earlier build is left inside it. INPUT is a file the build reads from /inputs. Each
# target has its own working directories under .cache/docker/. Concurrent invocations of the same
# target must use separate checkouts. The runtime hash selects the retained toolchain reference
# and caches independently of image export metadata.
define build
rm -rf .cache/docker/$@
mkdir -p .cache/docker/$@/inputs
$(if $(4),cp --reflink=auto '$(4)' .cache/docker/$@/inputs/)
image=$$($(DOCKER) image inspect --format '{{.Id}}' cordial-$(1))
runtime_format='{{json .RootFS}} {{json .Config}} {{json .Os}} {{json .Architecture}} {{json (or .Variant "")}}'
runtime=$$($(DOCKER) image inspect --format "$$runtime_format" "$$image")
cache_key=$$(printf '%s\n' "$$runtime" | sha256sum | cut -d' ' -f1)
image_ref="cordial-toolchain:$$cache_key"
cached_runtime=$$($(DOCKER) image inspect --format "$$runtime_format" "$$image_ref" 2>/dev/null || true)
if [ "$$cached_runtime" != "$$runtime" ]; then $(DOCKER) tag "$$image" "$$image_ref"; fi
dependency_key=$$(sha256sum rust/tools/firmware_dependencies.py | cut -d' ' -f1)
$(DOCKER) build --progress=plain -f docker/build.Dockerfile --build-arg IMAGE="$$image_ref" \
  --build-arg CACHE_ID="$$cache_key" --build-arg DEPENDENCY_ID="$$dependency_key" \
  --build-arg CORDIAL_VERSION --build-arg SOURCE_DATE_EPOCH --build-arg CORDIAL_HOMEPAGE \
  --build-arg COMMAND='$(2)' --build-arg OUTPUTS='$(3)' --build-context inputs=.cache/docker/$@/inputs \
  --output type=local$(comma)dest=.cache/docker/$@/out .
for pattern in $(foreach p,$(3),'$(p)'); do
  for exported in .cache/docker/$@/out/$$pattern; do
    path=$${exported#.cache/docker/$@/out/}
    rm -rf "$$path"
    mkdir -p "$$(dirname "$$path")"
    mv "$$exported" "$$path"
  done
done
endef

desktop cli: host
host: image-host
	$(call build,host,make local-host HOST_TARGET=$(HOST_TARGET),rust/target/$(HOST_TARGET)/release/cordial rust/target/$(HOST_TARGET)/release/share)
local-desktop local-cli: local-host
local-host:
	$(PYTHON) tools/version.py
	schemas=rust/target/$(HOST_TARGET)/release/share/glib-2.0/schemas
	mkdir -p "$$schemas"
	cp packaging/dev.petris.cordial.gschema.xml "$$schemas/"
	glib-compile-schemas --strict "$$schemas"
	cd rust
	cargo build --locked --release -p cordial --bin cordial --target $(HOST_TARGET)

firmware: image-$(FIRMWARE_PLATFORM)
	$(call build,$(FIRMWARE_PLATFORM),python3 rust/tools/build_firmware.py rust/boards/$(BOARD).json --profile $(PROFILE),build/firmware/*/*)
firmware-all:
	@set -e; for board in $(BOARDS); do $(MAKE) firmware BOARD=$$board PROFILE=$(PROFILE); done
package-firmware: firmware

package-desktop: package-host-appimage
package-desktop-tar package-cli package-cli-tar: package-host-tar
package-host-tar: image-host
	$(call build,host,make local-package-host-tar HOST_TARGET=$(HOST_TARGET),build/release/cordial-*-linux-amd64.tar.gz)
package-host-appimage: image-host $(if $(HOST_ARCHIVE),,package-host-tar)
	$(call build,host,python3 rust/tools/build_appimage.py --archive /inputs/$(notdir $(host_archive)),build/release/cordial-desktop-*-x86_64.AppImage,$(host_archive))
local-package-desktop: local-package-host-appimage
local-package-desktop-tar local-package-cli: local-package-host-tar
local-package-host-tar: local-host
	$(PYTHON) rust/tools/package_host.py --target $(HOST_TARGET)
local-package-host-appimage: local-package-host-tar
	version=$$($(PYTHON) tools/version.py)
	$(PYTHON) rust/tools/build_appimage.py --archive "build/release/cordial-$$version-linux-amd64.tar.gz"

# Each distribution build exports one Cordial package.
host_archive = $(or $(HOST_ARCHIVE),build/release/cordial-$(CORDIAL_VERSION)-linux-amd64.tar.gz)
package-host-arch: image-arch $(if $(HOST_ARCHIVE),,package-host-tar)
	$(call build,arch,make -f packaging/Makefile arch HOST_ARCHIVE=/inputs/$(notdir $(host_archive)),build/packages/host-arch/cordial-$(CORDIAL_VERSION)-1-x86_64.pkg.tar.zst,$(host_archive))
ifeq ($(DEB_DISTRIBUTION),trixie)
# Trixie uses its own executable because Resolute's glibc is newer.
package-host-deb: image-deb
	$(if $(HOST_ARCHIVE),$(error Trixie packages build their executable on Debian 13; omit HOST_ARCHIVE))
	$(call build,deb-trixie,make local-package-host-tar HOST_TARGET=$(HOST_TARGET) && make -f packaging/Makefile deb DEB_DISTRIBUTION=trixie$(if $(DEB_REVISION), DEB_REVISION=$(DEB_REVISION)),build/packages/host-deb/trixie/cordial_$(CORDIAL_VERSION)-$(or $(DEB_REVISION),1~trixie)_amd64.deb)
else
package-host-deb: image-deb $(if $(HOST_ARCHIVE),,package-host-tar)
	$(call build,deb-$(DEB_DISTRIBUTION),make -f packaging/Makefile deb HOST_ARCHIVE=/inputs/$(notdir $(host_archive)) DEB_DISTRIBUTION=$(DEB_DISTRIBUTION)$(if $(DEB_REVISION), DEB_REVISION=$(DEB_REVISION)),build/packages/host-deb/$(DEB_DISTRIBUTION)/cordial_$(CORDIAL_VERSION)-$(or $(DEB_REVISION),1~$(DEB_DISTRIBUTION))_amd64.deb,$(host_archive))
endif

check: check-rust check-protocol check-tools
# Clippy on the firmware platform crates for BOARD and PROFILE, in the board's image.
check-firmware: image-$(FIRMWARE_PLATFORM)
	$(call build,$(FIRMWARE_PLATFORM),python3 rust/tools/build_firmware.py rust/boards/$(BOARD).json --profile $(PROFILE) --clippy)
check-firmware-all:
	@set -e; for board in $(BOARDS); do for profile in production debug; do $(MAKE) check-firmware BOARD=$$board PROFILE=$$profile; done; done
check-rust:
	$(PYTHON) rust/tools/firmware_dependencies.py --btstack
	cd rust
	cargo test --locked --workspace --all-features
	cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
	$(PYTHON) tools/test_btstack.py
check-tools:
	$(PYTHON) -m unittest discover -s tools -p 'test_*.py'
	$(PYTHON) -m unittest discover -s rust/tests -p 'test_*.py'
# Check permanent serial contracts across releases and storage/migration IPC within a release series.
check-protocol:
	$(BUF) lint "$(CURDIR)/proto" --config buf.yaml
	$(PYTHON) tools/check_protocol.py --base "$(PROTOCOL_BASE)" --stable-base "$(PROTOCOL_STABLE_BASE)"
version:
	@$(PYTHON) tools/version.py
clean:
	rm -rf rust/target rust/platforms/pico/target rust/platforms/esp32s3/target build/packages build/firmware build/release target/firmware target/tmp
