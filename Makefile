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
PROTOCOL_BASE ?= $(shell git describe --tags --abbrev=0 HEAD^ 2>/dev/null)
BOARDS := pico_w pico2_w waveshare_rp2350b_plus_w xiao_esp32s3
ESP_BOARDS := xiao_esp32s3
DOCKER ?= docker
FIRMWARE_PLATFORM = $(if $(filter $(ESP_BOARDS),$(BOARD)),esp32s3,pico)

.PHONY: help all desktop web cli firmware firmware-all docker-firmware docker-firmware-all docker-image check-memory-local package-desktop package-desktop-tar package-desktop-arch package-desktop-deb package-web package-cli package-cli-tar package-cli-arch package-cli-deb package-firmware check check-memory check-rust check-desktop check-protocol check-tools version clean
help:
	@echo 'Cordial: all desktop web cli firmware firmware-all check check-memory version clean'
	@echo 'Firmware in Docker: docker-firmware docker-firmware-all (DOCKER=$(DOCKER))'
	@echo 'Release packages: package-desktop package-web package-cli package-firmware (CORDIAL_VERSION, default 0.0.0)'
	@echo 'Distribution packages: package-desktop-arch package-desktop-deb package-cli-arch package-cli-deb'
	@echo 'Firmware options: BOARD=$(BOARD) PROFILE=$(PROFILE)'
	@echo 'Boards: $(BOARDS)'
all: desktop cli docker-firmware

desktop/node_modules/.package-lock.json: desktop/package.json desktop/package-lock.json $(wildcard desktop/packages/*/package.json)
	npm ci --include=dev --prefix desktop
desktop: desktop/node_modules/.package-lock.json
	npm --prefix desktop run build
web: desktop/node_modules/.package-lock.json
	npm --prefix desktop run build:web
cli:
	$(PYTHON) tools/version.py
	cd rust
	cargo build --locked --release -p cordial-cli --bin cordial
firmware:
	$(PYTHON) rust/tools/build_firmware.py rust/boards/$(BOARD).json --profile $(PROFILE)
firmware-all:
	@set -e; for board in $(BOARDS); do $(MAKE) firmware BOARD=$$board PROFILE=$(PROFILE); done
# The firmware toolchains live in an image per platform. The checkout is mounted at its own path
# and the build runs as the calling user, so outputs and caches are the same as a local build.
docker-image:
	$(DOCKER) build -t cordial-firmware-$(FIRMWARE_PLATFORM) -f rust/docker/$(FIRMWARE_PLATFORM).Dockerfile rust/docker
DOCKER_RUN = mkdir -p .cache/docker/home .cache/docker/cargo && \
	$(DOCKER) run --rm -u "$$(id -u):$$(id -g)" -v "$(CURDIR):$(CURDIR)" -w "$(CURDIR)" \
	  -e HOME="$(CURDIR)/.cache/docker/home" -e CARGO_HOME="$(CURDIR)/.cache/docker/cargo" \
	  -e CORDIAL_VERSION -e GIT_CONFIG_COUNT=1 -e GIT_CONFIG_KEY_0=safe.directory -e GIT_CONFIG_VALUE_0='*' \
	  cordial-firmware-$(FIRMWARE_PLATFORM)
docker-firmware: docker-image
	$(DOCKER_RUN) make firmware BOARD=$(BOARD) PROFILE=$(PROFILE)
docker-firmware-all:
	@set -e; for board in $(BOARDS); do $(MAKE) docker-firmware BOARD=$$board PROFILE=$(PROFILE); done
package-desktop-tar: DESKTOP_TARGETS = tar.gz
package-desktop package-desktop-tar: desktop
	version=$$($(PYTHON) tools/version.py)
	printf '%s\n' "$$version" > desktop/out/VERSION
	cd desktop
	npm exec -- electron-builder --linux $(DESKTOP_TARGETS) --publish never --config.extraMetadata.version="$$version"
package-desktop-arch: $(if $(DESKTOP_ARCHIVE),,package-desktop-tar)
	$(MAKE) -f packaging/Makefile arch APP=desktop
package-desktop-deb: $(if $(DESKTOP_ARCHIVE),,package-desktop-tar)
	$(MAKE) -f packaging/Makefile deb APP=desktop
package-web: web
	version=$$($(PYTHON) tools/version.py)
	destination="build/packages/web/cordial-web-$$version"
	rm -r -f "$$destination"
	mkdir -p "$$destination"
	cp -r desktop/out/web/. "$$destination/"
	cp LICENSE "$$destination/"
	printf '%s\n' "$$version" > "$$destination/VERSION"
package-cli: package-cli-tar
package-cli-tar:
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
	cp -r LICENSE docs proto "$$temporary/$$name/"
	printf '%s\n' "$$version" > "$$temporary/$$name/VERSION"
	$(PYTHON) rust/tools/dependency_notices.py --target $(CLI_TARGET) --output "$$temporary/$$name"
	(cd "$$temporary/$$name" && find . -type f ! -name SHA256SUMS -print0 | sort -z | xargs -0 sha256sum > SHA256SUMS)
	tar -C "$$temporary" -czf "$$temporary/$$name.tar.gz" "$$name"
	mv "$$temporary/$$name.tar.gz" "build/release/$$name.tar.gz"
package-cli-arch: $(if $(CLI_ARCHIVE),,package-cli-tar)
	$(MAKE) -f packaging/Makefile arch APP=cli
package-cli-deb: $(if $(CLI_ARCHIVE),,package-cli-tar)
	$(MAKE) -f packaging/Makefile deb APP=cli
package-firmware: firmware

check: check-rust check-desktop check-protocol check-tools
check-memory: override BOARD = pico_w
check-memory: docker-image
	$(DOCKER_RUN) make check-memory-local
check-memory-local:
	$(MAKE) firmware BOARD=pico_w PROFILE=development
	version=$$($(PYTHON) tools/version.py)
	$(PYTHON) rust/tools/check_memory.py "build/firmware/$$version/pico_w-btstack-pico-sdk-cyw43-development/cordial-pico_w-btstack-pico-sdk-cyw43-development.elf"
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
