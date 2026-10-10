.DEFAULT_GOAL := help
SHELL := /bin/bash
.SHELLFLAGS := -eu -o pipefail -c
.ONESHELL:
PYTHON ?= python3
export CORDIAL_VERSION ?= 0.0.0
CLI_TARGET ?= $(shell uname -m)-unknown-linux-musl
DESKTOP_TARGETS ?= AppImage tar.gz
BOARD ?= pico_w
PROFILE ?= development
PROTOCOL_BASE ?= $(shell $(PYTHON) tools/version.py --protocol-base 2>/dev/null)
BOARDS := pico_w pico2_w waveshare_rp2350b_plus_w xiao_esp32s3
ESP_BOARDS := xiao_esp32s3
PICO_BOARDS := $(filter-out $(ESP_BOARDS),$(BOARDS))
DOCKER ?= docker
DEB_DISTRIBUTION ?= trixie
DEB_BASE_trixie := debian:13
DEB_BASE_noble := ubuntu:24.04
DEB_BASE_resolute := ubuntu:26.04
DEB_BASE = $(or $(DEB_BASE_$(DEB_DISTRIBUTION)),$(error Unknown DEB_DISTRIBUTION $(DEB_DISTRIBUTION)))
FIRMWARE_PLATFORM = $(if $(filter $(ESP_BOARDS),$(BOARD)),esp32s3,pico)

.PHONY: help all desktop web cli local-desktop local-web local-cli firmware firmware-all \
	package-desktop package-desktop-tar package-desktop-arch package-desktop-deb package-web \
	package-cli package-cli-tar package-cli-arch package-cli-deb package-firmware \
	local-package-desktop local-package-desktop-tar local-package-web local-package-cli \
	image-app image-pico image-esp32s3 image-arch image-deb \
	check check-memory check-firmware check-firmware-all check-rust check-desktop check-protocol check-tools version clean
help:
	@echo 'Cordial: all desktop web cli firmware firmware-all check check-memory check-firmware check-firmware-all version clean'
	@echo 'Without Docker: local-desktop local-web local-cli'
	@echo 'Release packages: package-desktop package-web package-cli package-firmware (CORDIAL_VERSION, default 0.0.0)'
	@echo 'Distribution packages: package-desktop-arch package-desktop-deb package-cli-arch package-cli-deb (DEB_DISTRIBUTION=$(DEB_DISTRIBUTION))'
	@echo 'Firmware options: BOARD=$(BOARD) PROFILE=$(PROFILE)'
	@echo 'Boards: $(BOARDS)'
all: desktop cli firmware

# Toolchain images from docker/. Each build copies the checkout into one of them (without the
# paths in .dockerignore), runs there, and exports only its outputs back into the checkout.
image-app:
	$(DOCKER) build -t cordial-app -f docker/app.Dockerfile docker
image-pico:
	$(DOCKER) build -t cordial-pico -f docker/pico.Dockerfile docker
image-esp32s3:
	$(DOCKER) build -t cordial-esp32s3 -f docker/esp32s3.Dockerfile docker
image-arch:
	$(DOCKER) build -t cordial-arch -f docker/arch.Dockerfile desktop/packaging/arch
image-deb:
	$(DOCKER) build -t cordial-deb-$(DEB_DISTRIBUTION) --build-arg BASE=$(DEB_BASE) -f docker/deb.Dockerfile desktop/packaging/debian
comma := ,
# $(call build,IMAGE,COMMAND,OUTPUTS[,INPUT]) runs COMMAND with bash in the image and exports the
# paths matching OUTPUTS, relative to the checkout; each replaces the checkout's copy, so nothing
# from an earlier build is left inside it. INPUT is a file the build reads from /inputs. Each
# target has its own working directories under .cache/docker/, so parallel builds stay apart.
define build
rm -rf .cache/docker/$@
mkdir -p .cache/docker/$@/inputs
$(if $(4),cp --reflink=auto '$(4)' .cache/docker/$@/inputs/)
$(DOCKER) build --progress=plain -f docker/build.Dockerfile --build-arg IMAGE=cordial-$(1) \
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

desktop: image-app
	$(call build,app,make local-desktop,desktop/out/main desktop/out/preload desktop/out/renderer)
web: image-app
	$(call build,app,make local-web,desktop/out/web)
cli: image-app
	$(call build,app,make local-cli,rust/target/release/cordial)

desktop/node_modules/.package-lock.json: desktop/package.json desktop/package-lock.json $(wildcard desktop/packages/*/package.json)
	npm ci --include=dev --prefix desktop
local-desktop: desktop/node_modules/.package-lock.json
	npm --prefix desktop run build
local-web: desktop/node_modules/.package-lock.json
	npm --prefix desktop run build:web
local-cli:
	$(PYTHON) tools/version.py
	cd rust
	cargo build --locked --release -p cordial-cli --bin cordial

firmware: image-$(FIRMWARE_PLATFORM)
	$(call build,$(FIRMWARE_PLATFORM),python3 rust/tools/build_firmware.py rust/boards/$(BOARD).json --profile $(PROFILE),build/firmware/*/*)
firmware-all:
	@set -e; for board in $(BOARDS); do $(MAKE) firmware BOARD=$$board PROFILE=$(PROFILE); done
package-firmware: firmware

package-desktop: image-app
	$(call build,app,make local-package-desktop,desktop/dist/*.AppImage desktop/dist/*.tar.gz)
package-desktop-tar: image-app
	$(call build,app,make local-package-desktop-tar,desktop/dist/*.tar.gz)
package-web: image-app
	$(call build,app,make local-package-web,build/packages/web/*)
package-cli package-cli-tar: image-app
	$(call build,app,make local-package-cli CLI_TARGET=$(CLI_TARGET),build/release/cordial-cli-*.tar.gz)
local-package-desktop-tar: DESKTOP_TARGETS = tar.gz
local-package-desktop local-package-desktop-tar: local-desktop
	version=$$($(PYTHON) tools/version.py)
	printf '%s\n' "$$version" > desktop/out/VERSION
	cd desktop
	npm exec -- electron-builder --linux $(DESKTOP_TARGETS) --publish never --config.extraMetadata.version="$$version"
local-package-web: local-web
	version=$$($(PYTHON) tools/version.py)
	destination="build/packages/web/cordial-web-$$version"
	rm -r -f "$$destination"
	mkdir -p "$$destination"
	cp -r desktop/out/web/. "$$destination/"
	cp LICENSE "$$destination/"
	printf '%s\n' "$$version" > "$$destination/VERSION"
local-package-cli:
	version=$$($(PYTHON) tools/version.py)
	case '$(CLI_TARGET)' in
	  x86_64-unknown-linux-musl) arch=amd64 ;;
	  aarch64-unknown-linux-musl) arch=arm64 ;;
	  *) echo 'CLI_TARGET must be x86_64-unknown-linux-musl or aarch64-unknown-linux-musl' >&2; exit 1 ;;
	esac
	(cd rust && cargo build --locked --release -p cordial-cli --bin cordial --target $(CLI_TARGET))
	name="cordial-cli-$$version-linux-$$arch"
	mkdir -p build/release
	temporary=$$(mktemp -d "$$PWD/build/release/.cordial-XXXXXX")
	trap 'rm -r "$$temporary"' EXIT
	mkdir "$$temporary/$$name"
	install -m 755 rust/target/$(CLI_TARGET)/release/cordial "$$temporary/$$name/cordial"
	cp -r LICENSE docs proto configs/50-cordial.rules "$$temporary/$$name/"
	printf '%s\n' "$$version" > "$$temporary/$$name/VERSION"
	$(PYTHON) rust/tools/dependency_notices.py --target $(CLI_TARGET) --output "$$temporary/$$name"
	(cd "$$temporary/$$name" && find . -type f ! -name SHA256SUMS -print0 | sort -z | xargs -0 sha256sum > SHA256SUMS)
	tar -C "$$temporary" -czf "$$temporary/$$name.tar.gz" "$$name"
	mv "$$temporary/$$name.tar.gz" "build/release/$$name.tar.gz"
# Distribution packages repackage the portable archives, which are built first unless
# DESKTOP_ARCHIVE or CLI_ARCHIVE names an existing one.
desktop_archive = $(or $(DESKTOP_ARCHIVE),desktop/dist/cordial-desktop-$(CORDIAL_VERSION)-x64.tar.gz)
cli_archive = $(or $(CLI_ARCHIVE),build/release/cordial-cli-$(CORDIAL_VERSION)-linux-amd64.tar.gz)
package-desktop-arch: image-arch $(if $(DESKTOP_ARCHIVE),,package-desktop-tar)
	$(call build,arch,make -f packaging/Makefile arch APP=desktop DESKTOP_ARCHIVE=/inputs/$(notdir $(desktop_archive)),build/packages/desktop-arch/*.pkg.tar.zst,$(desktop_archive))
package-cli-arch: image-arch $(if $(CLI_ARCHIVE),,package-cli-tar)
	$(call build,arch,make -f packaging/Makefile arch APP=cli CLI_ARCHIVE=/inputs/$(notdir $(cli_archive)),build/packages/cli-arch/*.pkg.tar.zst,$(cli_archive))
package-desktop-deb: image-deb $(if $(DESKTOP_ARCHIVE),,package-desktop-tar)
	$(call build,deb-$(DEB_DISTRIBUTION),make -f packaging/Makefile deb APP=desktop DESKTOP_ARCHIVE=/inputs/$(notdir $(desktop_archive)) DEB_DISTRIBUTION=$(DEB_DISTRIBUTION)$(if $(DEB_REVISION), DEB_REVISION=$(DEB_REVISION)),build/packages/desktop-deb/$(DEB_DISTRIBUTION)/*.deb,$(desktop_archive))
package-cli-deb: image-deb $(if $(CLI_ARCHIVE),,package-cli-tar)
	$(call build,deb-$(DEB_DISTRIBUTION),make -f packaging/Makefile deb APP=cli CLI_ARCHIVE=/inputs/$(notdir $(cli_archive)) DEB_DISTRIBUTION=$(DEB_DISTRIBUTION)$(if $(DEB_REVISION), DEB_REVISION=$(DEB_REVISION)),build/packages/cli-deb/$(DEB_DISTRIBUTION)/*.deb,$(cli_archive))

check: check-rust check-desktop check-protocol check-tools
# Clippy on the firmware platform crates for BOARD and PROFILE, in the board's image.
check-firmware: image-$(FIRMWARE_PLATFORM)
	$(call build,$(FIRMWARE_PLATFORM),python3 rust/tools/build_firmware.py rust/boards/$(BOARD).json --profile $(PROFILE) --clippy)
check-firmware-all:
	@set -e; for board in $(BOARDS); do for profile in production debug; do $(MAKE) check-firmware BOARD=$$board PROFILE=$$profile; done; done
# The allocation check builds each Pico board's development firmware and runs the workloads with
# that board's configuration and heap under QEMU in the Pico image.
check-memory: image-pico
	$(call build,pico,for board in $(PICO_BOARDS); do python3 rust/tools/build_firmware.py rust/boards/$$board.json --profile development; python3 rust/tools/check_memory.py build/firmware/$$CORDIAL_VERSION-dev/$$board-btstack-pico-sdk-cyw43/cordial-$$board-btstack-pico-sdk-cyw43.elf; done)
check-rust:
	$(PYTHON) rust/tools/firmware_dependencies.py --btstack
	cd rust
	cargo test --locked --workspace --all-features
	cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
	$(PYTHON) tools/test_btstack.py
check-desktop: desktop/node_modules/.package-lock.json
	npm --prefix desktop run check
check-tools:
	$(PYTHON) -m unittest discover -s tools -p 'test_*.py'
	$(PYTHON) -m unittest discover -s rust/tests -p 'test_*.py'
# Lint and breaking checks cover every schema in proto/. A schema the base release lacks, such as
# storage.proto before its first release, is new to buf breaking and has nothing to break.
check-protocol: desktop/node_modules/.package-lock.json
	$(PYTHON) tools/check_keys.py $(if $(PROTOCOL_BASE),--against $(PROTOCOL_BASE))
	npm --prefix desktop/packages/protocol run lint
	if [ -n "$(PROTOCOL_BASE)" ] && git cat-file -e "$(PROTOCOL_BASE):proto/cordial.proto" 2>/dev/null; then
	  npm --prefix desktop/packages/protocol run breaking -- "../../../.git#ref=$(PROTOCOL_BASE),subdir=proto"
	fi
version:
	@$(PYTHON) tools/version.py
clean:
	rm -rf rust/target rust/platforms/pico/target rust/platforms/esp32s3/target rust/tests/memory-arm/target desktop/out desktop/dist build/packages build/firmware build/release target/firmware target/memory-arm target/tmp
