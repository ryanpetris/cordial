.DEFAULT_GOAL := help
PYTHON ?= python3
BOARD ?= pico_w
PROFILE ?= development
BOARDS := pico_w pico2_w waveshare_rp2350b_plus_w xiao_esp32s3

.PHONY: require-python help all desktop web cli firmware firmware-all package-desktop package-web package-cli package-cli-arch package-cli-deb package-firmware check check-memory check-rust check-desktop check-tools schema version clean
help:
	@echo 'Cordial: all desktop web cli firmware firmware-all check check-memory schema version clean'
	@echo 'Release packages: package-desktop package-web package-cli package-cli-arch package-cli-deb package-firmware (clean vX.Y.Z tag required)'
	@echo 'Firmware options: BOARD=$(BOARD) PROFILE=$(PROFILE)'
	@echo 'Boards: $(BOARDS)'
all: desktop cli firmware

desktop: | require-python
	$(PYTHON) desktop/bootstrap.py build
web: | require-python
	$(PYTHON) desktop/bootstrap.py build:web
cli: | require-python
	$(PYTHON) rust/bootstrap.py cli
firmware: | require-python
	$(PYTHON) rust/bootstrap.py firmware --board $(BOARD) --profile $(PROFILE)
firmware-all:
	@set -e; for board in $(BOARDS); do $(MAKE) firmware BOARD=$$board PROFILE=$(PROFILE); done
package-desktop: | require-python
	$(PYTHON) desktop/bootstrap.py dist
package-web: | require-python
	$(PYTHON) desktop/bootstrap.py dist:web
package-cli: | require-python
	$(PYTHON) rust/bootstrap.py package-cli
package-cli-arch: | require-python
	$(MAKE) -f rust/packaging/Makefile arch
package-cli-deb: | require-python
	$(MAKE) -f rust/packaging/Makefile deb
package-firmware: | require-python
	$(PYTHON) rust/bootstrap.py package-firmware --board $(BOARD) --profile $(PROFILE)

check: check-rust check-desktop check-tools
check-memory: | require-python
	$(PYTHON) rust/bootstrap.py check-memory
check-rust: | require-python
	$(PYTHON) rust/bootstrap.py check
check-desktop: | require-python
	$(PYTHON) desktop/bootstrap.py check
check-tools: | require-python
	$(PYTHON) -m unittest discover -s tools -p 'test_*.py'
	$(PYTHON) -m unittest discover -s rust/tests -p 'test_*.py'
schema: | require-python
	$(PYTHON) rust/bootstrap.py schema
	$(PYTHON) desktop/bootstrap.py generate
version: | require-python
	@$(PYTHON) tools/version.py
clean:
	rm -rf rust/target rust/platforms/pico/target rust/platforms/esp32s3/target rust/tests/memory-arm/target desktop/out desktop/dist build/packages build/firmware build/release target/firmware target/memory-arm target/tmp

require-python:
	@command -v "$(PYTHON)" >/dev/null 2>&1 || { \
		echo 'Missing packages: Arch: python; Debian: python3' >&2; exit 1; }
	@$(PYTHON) -c 'import sys; sys.exit(sys.version_info < (3, 11, 4))' 2>/dev/null || { \
		echo 'Manual download required: Python 3.11.4+' >&2; exit 1; }
