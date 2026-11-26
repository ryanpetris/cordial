# Building Cordial

This guide covers building the Cordial host application (desktop app, TUI, and CLI), firmware images, distribution packages, and running the validation suites.

## Board Presets

Board definitions live in [`rust/boards/`](../rust/boards/). Each JSON file defines pins, minimum flash size, clock settings, Bluetooth/radio/storage backends, firmware reservation, and `profile_memory_budget` (RAM reserved for loaded [input profiles](input-profiles.md), or `null` to omit profile support).

| Preset | Chip | Bluetooth Backend | Gamepads | Profile RAM | Firmware Reserve | Notes |
| --- | --- | --- | --- | --- | --- | --- |
| `pico_w` | RP2040 | Classic + BLE (`btstack`, `pico-sdk-cyw43`) | HID, XInput | None | 1 MiB | Omits profiles to preserve RAM for connections |
| `pico2_w` | RP2350A | Classic + BLE (`btstack`, `pico-sdk-cyw43`) | HID, XInput | 40 KiB | 2 MiB | Full feature set |
| `pimoroni_pico_plus2_w` | RP2350B | Classic + BLE (`btstack`, `pico-sdk-cyw43`) | HID, XInput | 40 KiB | 2 MiB | 16 MiB flash; 8 MiB PSRAM unused; hardware verification pending |
| `waveshare_rp2350b_plus_w` | RP2350B | Classic + BLE (`btstack`, `pico-sdk-cyw43`) | HID, XInput | 40 KiB | 2 MiB | Full feature set |
| `esp32s3` | ESP32-S3 | BLE (`esp-nimble`, `esp-idf`) | None | 40 KiB | 2 MiB | Requires >= 4 MiB flash and native USB |

Notes on board configuration:

- `minimum_flash_bytes` sets the image's minimum flash requirement and fallback size when runtime capacity discovery is unavailable. Detected flash capacity determines the actual LittleFS size.
- Bluetooth pool sizes are generated for both Rust and C from `CORDIAL_CONFIG` and `rust/tools/bluetooth_limits.json`; see [Bluetooth capacity](protocol/limits-and-errors.md#bluetooth-capacity).
- Pico presets default to `pico-sdk-cyw43` (`embassy-cyw43` is also supported); `esp32s3` uses `esp-nimble` and `esp-idf` (`btstack` is also supported).
- Known compatible `esp32s3` boards include the [Seeed Studio XIAO ESP32-S3](https://wiki.seeedstudio.com/xiao_esp32s3_getting_started/), [M5Stack Atom S3](https://docs.m5stack.com/en/core/AtomS3), and [M5Stack Atom S3U](https://docs.m5stack.com/en/core/AtomS3U); see [ESP32-S3 flashing](installation.md#esp32-s3).
- For custom wiring or alternate backends, copy a preset JSON file, edit it, and pass its path to `python3 rust/tools/build_firmware.py`.

## Building with Docker

Run all `just` commands from the repository root on Linux. Release host builds, firmware builds, and distribution packaging run inside Docker containers with BuildKit enabled.

### Prerequisites

- Bash, Git, Python 3, and `just`
- [Docker](https://docs.docker.com/engine/install/) with BuildKit enabled and user permissions (or Podman via `just DOCKER=podman ...`)

Debian and Ubuntu:

```sh
sudo apt-get install git just python3
```

Arch Linux:

```sh
sudo pacman -S git just python
```

### Common Recipes

Pass configuration via environment variables or assignments before recipe names (command-line assignments take precedence). Run `just` or `just help` to list recipes and defaults.

```sh
just host                                  # build release desktop/CLI/TUI binary
just BOARD=pico2_w PROFILE=production firmware
just firmware-all                          # build firmware for every board preset
just all                                   # build host binary and selected firmware
just clean                                 # remove exported build outputs
```

Key recipe variables:

- `BOARD`: board preset name (default `pico_w`).
- `PROFILE`: `development` (default), `debug`, or `production`.
- `HOST_TARGET`: host target triple (default `x86_64-unknown-linux-gnu`).
- `CORDIAL_VERSION`: release version `MAJOR.MINOR.PATCH` (default `0.0.0`).
- `PYTHON`, `BUF`, `DOCKER`: tool overrides (defaults `python3`, `buf`, `docker`). Root recipes accept prefixes such as `PYTHON='python3 -B'`; native packaging recipes expect a single executable path in `PYTHON`.
- Internal variables `BOARDS`, `ESP_BOARDS`, `FIRMWARE_PLATFORM`, and `DEB_BASE*` accept command-line assignments only.

### Toolchain Images and Caches

`just host`, `just firmware`, and `just package-host-*` build toolchain images from `docker/` on first use and reuse them on subsequent runs. Each build copies the checkout (excluding paths in `.dockerignore`), compiles inside the container, and exports only the built artifacts back to the checkout.

- **BuildKit cache mounts** retain Cargo downloads, Rust target directories, pinned firmware SDK checkouts, and `XDG_CACHE_HOME` caches. Packaged outputs are never cached.
- **Cache isolation**: Cache keys hash the toolchain image's runtime contents (`cordial-toolchain:<runtime-hash>`), OS, architecture, and variant. Firmware caches also include the SHA-256 of `rust/tools/firmware_dependencies.py`.
- **Concurrency**: Builds using the same toolchain runtime serialize access to their cache mounts; different runtimes can build in parallel. Each recipe uses a scratch directory under `.cache/docker/`, so concurrent invocations of the same recipe require separate checkouts.
- **Cleanup**: `just clean` removes exported outputs without clearing BuildKit caches. Reclaim unused cache mounts with `docker builder prune --filter type=exec.cachemount` (note that this affects all projects on the active builder) and remove old runtime tags with `docker image rm cordial-toolchain:<runtime-hash>`.

The `just host` image uses Ubuntu 26.04 Resolute (glibc 2.43, GLib 2.88, GTK 4.22, libadwaita 1.9) and exports `rust/target/x86_64-unknown-linux-gnu/release/cordial`.

## Building Locally (Without Docker)

Local builds are used for day-to-day host development and running `just check` / `just check-gtk`.

### Local Prerequisites

- Rust via [rustup](https://rustup.rs/) (Cargo automatically selects the toolchain pinned in `rust/rust-toolchain.toml`).
- A C compiler, `make`, `clang`/`libclang`, `pkg-config`, Git, `just`, and Python 3.11.4+ (direct Cargo host builds do not require Python; `just` recipes, version tools, packaging, and firmware builds do).
- Native GTK4 and libadwaita development libraries (minimum GTK 4.14 and libadwaita 1.5) and `glib-compile-schemas`.

Debian and Ubuntu:

```sh
sudo apt-get install build-essential clang git just libadwaita-1-dev \
  libclang-dev libglib2.0-bin libgtk-4-dev pkg-config python3
```

Arch Linux:

```sh
sudo pacman -S base-devel clang git glib2 gtk4 just libadwaita pkgconf python
```

### Running Development Builds

Use `just cordial` to compile and run the development binary from your current working directory (preserving relative paths and exact argument boundaries):

```sh
just cordial                 # interactive shell
just cordial desktop         # GTK4/libadwaita desktop app
just cordial tui             # full-screen TUI
just cordial --help
```

`just local-host` builds the release binary locally for `HOST_TARGET`. Both `just cordial` and `just local-host` compile the GSettings schema into `share/glib-2.0/schemas/` next to the built executable so direct launches and Start at Login work without setting `GSETTINGS_SCHEMA_DIR`.

To capture protocol JSON logs cleanly without Cargo output:

```sh
just cordial --version
rust/target/debug/cordial --json desktop 2>cordial.jsonl
rust/target/debug/cordial --json tui 2>cordial.jsonl
```

To build a custom headless CLI/TUI binary without GTK or libadwaita (not used for release packages):

```sh
cd rust
cargo build --locked --release -p cordial --bin cordial --no-default-features
```

### Desktop Preferences (GSettings)

The Linux desktop stores preferences under the `dev.petris.cordial` schema at `/dev/petris/cordial/`. Native packages install the XML schema into the system directory (compiled by distribution GLib triggers). Portable archives place it under `share/glib-2.0/schemas/` beside `cordial`, and AppImages bundle it under `usr/share/glib-2.0/schemas/`. Before launching a desktop binary compiled directly with `cargo`, run `just cordial --version` with the same Cargo configuration to populate its adjacent schema directory. CLI and TUI commands do not require the schema.

## Firmware Details

The Pico toolchain image (`docker/pico.Dockerfile`) provides Rust 1.99.1 and ARM GCC (`arm-none-eabi-gcc`); firmware builds fetch pinned Pico SDK and BTstack sources into the dependency cache as needed. The ESP32-S3 image (`docker/esp32s3.Dockerfile`) adds Espressif Rust 1.97.0.0, `espup`, and `ldproxy` on top of `espressif/idf:v6.1`.

To invoke `python3 rust/tools/build_firmware.py` locally without Docker:

- **RP2040 / RP2350**: Install `arm-none-eabi-gcc`, `cmake`, `ninja`, `clang`/`libclang`, and the pinned Rust toolchain targets from `rust/rust-toolchain.toml` (`thumbv6m-none-eabi` and `thumbv8m.main-none-eabihf`):

  Debian and Ubuntu:

  ```sh
  sudo apt-get install build-essential clang cmake gcc-arm-none-eabi git \
    libclang-dev libnewlib-arm-none-eabi ninja-build pkg-config python3
  ```

  Arch Linux:

  ```sh
  sudo pacman -S arm-none-eabi-gcc arm-none-eabi-newlib base-devel clang cmake \
    git ninja pkgconf python
  ```

  Rust targets:

  ```sh
  (cd rust && rustup target add thumbv6m-none-eabi thumbv8m.main-none-eabihf)
  ```

- **ESP32-S3**: Install ESP-IDF `v6.1`, `ldproxy`, and the `espup` `1.97.0.0` toolchain, then source both ESP-IDF's `export.sh` and `espup`'s `export-esp.sh` so `IDF_PATH`, `IDF_PYTHON_ENV_PATH`, `LIBCLANG_PATH`, and `xtensa-esp32s3-elf-gcc` are active in the environment.

### Profiles and Artifacts

`PROFILE` controls enabled commands and version suffixes independently of compiler optimization:

- `production`: excludes development commands (`ListFeatures`, `EnterBootloader`, and normal-mode file commands). Version has no suffix (for example, `1.2.3`).
- `debug`: includes development commands; version gets `-debug` (for example, `1.2.3-debug`).
- `development` (default): identical code to `debug`; version gets `-dev`.

Packaged firmware is written to `build/firmware/<firmware-version>/<board>-<bluetooth-backend>-<radio-backend>/` containing the ELF, raw binary, artifact metadata, and checksums. Pico builds add a `.uf2` image; ESP32-S3 builds add `bootloader.bin`, `partition-table.bin`, and `flash.json`.

Packaging validates image metadata, storage bounds, and (on ESP32-S3) linked Rust startup stack frames (`main` <= 1 KiB and total initialization frames <= 16 KiB).

### Stack Guards and Flashing

- **Pico stack guard**: Every RP2040 and RP2350 board reserves 65,536 bytes for the core 0 stack and installs `embassy_rp::install_core0_stack_guard` before executor, peripheral, heap, or application initialization (halting on failure). On RP2040, Embassy guards a 32-byte MPU subregion at the stack base; on RP2350, it sets `MSPLIM`. Early Cortex-M reset code runs prior to guard installation.
- **Development flashing**: No `just` recipe flashes hardware automatically. On Pico boards, `rust/tools/flash_dev.py` inspects UF2 artifacts and performs explicit storage-preserving updates (`--help` for options). ESP32-S3 uses `esptool` with the regions in `flash.json`.
- **Adapter names**: Each board preset supplies `default_adapter_name`, baked into firmware and used until customized via the desktop app, TUI, or `adapter set name "Desk"` (cleared with `adapter reset name` or **Reset to Default**).

### USB Gamepad Builds

RP2040 and RP2350 boards enable the `cordial-usb/gamepad` feature, advertising Disabled, HID, and XInput output modes. The largest composite USB configuration uses 12 interfaces and 10 handlers (455 configuration bytes inside a 512-byte buffer, plus 64-byte BOS, 64-byte Microsoft OS 2.0 `XUSB20` descriptor using 46 bytes, and 256-byte control buffers).

The firmware build script sets `EMBASSY_USB_MAX_INTERFACE_COUNT=12` and `EMBASSY_USB_MAX_HANDLER_COUNT=10`. Workspace checks run from `rust/` and read the same settings from `rust/.cargo/config.toml`. When running targeted Cargo commands from the repository root, pass that config explicitly:

```sh
cargo test --manifest-path rust/Cargo.toml --config rust/.cargo/config.toml -p cordial-usb --all-features
cargo clippy --manifest-path rust/Cargo.toml --config rust/.cargo/config.toml -p cordial-usb --all-targets --all-features -- -D warnings
```

## Versioning and Packaging

Tracked manifests use `0.0.0`. Set `CORDIAL_VERSION=MAJOR.MINOR.PATCH` (no `v` prefix or leading zeroes) for release builds; tracked files remain unchanged. `SOURCE_DATE_EPOCH` sets the build timestamp in Unix seconds, and `CORDIAL_HOMEPAGE` overrides the package homepage.

The serial protocol revision (`1`) and storage format (`2`) are versioned independently of `CORDIAL_VERSION`. Breaking protocol or storage changes increment the minor version while major is `0`, or the major version from `1.0` onward. See [Compatibility rules](protocol/compatibility.md).

The GitHub release workflow triggers on `vMAJOR.MINOR.PATCH` tags, runs `just check-protocol`, builds and validates all host packages, AppImages, and board/profile firmware archives, and publishes checksums.

### Portable Host Archives

```sh
export CORDIAL_VERSION=1.2.3
just package-host-tar package-host-appimage
```

- `just package-host-tar` produces `build/release/cordial-<version>-linux-amd64.tar.gz` (Resolute glibc 2.43 baseline, executable, compiled GSettings schema, docs, proto schemas, `50-cordial.rules`, and third-party notices from `notices/`).
- `just package-host-appimage` produces `build/release/cordial-<version>-x86_64.AppImage`, bundling GTK, libadwaita, icons, text libraries, and the `dconf` GIO module (using the host's session D-Bus, `dconf` service, compiler runtime, window system, and graphics drivers).
- Pass `HOST_ARCHIVE=path/to/cordial-<version>-linux-amd64.tar.gz` to repack an existing archive. Local variants (`just local-package-host-tar` and `just local-package-host-appimage`) run without Docker (local AppImage packaging requires the Resolute environment defined in `docker/host.Dockerfile`).

### Native Distribution Packages

Supported native targets are Arch Linux x86-64, Debian 13 (`trixie`), and Ubuntu 26.04 (`resolute`). Each package installs `/usr/bin/cordial`, the `dev.petris.cordial` desktop entry and icons, GSettings schema, `/usr/share/doc/cordial`, notices, license, and `/usr/lib/udev/rules.d/50-cordial.rules`.

```sh
CORDIAL_VERSION=1.2.3 just package-host-arch
CORDIAL_VERSION=1.2.3 just DEB_DISTRIBUTION=resolute package-host-deb
CORDIAL_VERSION=1.2.3 just DEB_DISTRIBUTION=trixie package-host-deb
```

- **Arch** (`packaging/arch/`) writes `build/packages/host-arch/cordial-<version>-1-x86_64.pkg.tar.zst` and accepts `HOST_ARCHIVE`.
- **Ubuntu Resolute** (`packaging/debian/`) writes `build/packages/host-deb/resolute/cordial_<version>-1~resolute_amd64.deb` and accepts `HOST_ARCHIVE`.
- **Debian Trixie** compiles its own executable inside a `debian:13` image and rejects `HOST_ARCHIVE`. `DEB_DISTRIBUTION` defaults to `trixie` and `DEB_REVISION` defaults to `1~<distribution>`.
- To repackage directly with local `makepkg` or `dpkg-buildpackage`, run `just --justfile packaging/justfile arch` or `deb` from the repository root.

## Testing and Validation

See [Required completion checks](../AGENTS.md#required-completion-checks) for the full pre-completion checklist. Documentation-only changes do not require running software suites.

### Core Checks (`just check`)

```sh
just check
```

`just check` runs three suites locally:

1. `check-rust`: workspace tests (`--all-features`), Clippy (`--all-targets --all-features -- -D warnings`; install with `(cd rust && rustup component add clippy)`), and native BTstack C callback fixtures (`rust/tools/test_btstack.py`).
2. `check-protocol`: `buf lint` and `buf breaking` against `buf.yaml` using [Buf 1.75.0](https://github.com/bufbuild/buf/releases/tag/v1.75.0) (`BUF` overrides the binary). Permanent handshake and retained serial revisions compare against every local release tag (overridable via `PROTOCOL_STABLE_BASE`); storage and migration IPC compare against the newest earlier ancestor release in the same application series (overridable via `PROTOCOL_BASE`).
3. `check-tools`: Python unit tests under `tools/test_*.py` and `rust/tests/test_*.py`. Preference and packaging tests require `glib-compile-schemas`; Debian dependency-version tests require `dpkg` (run inside a Debian/Ubuntu container if absent on the host).

Run protocol checks alone with `just check-protocol` (requires only `just`, Python, Git, and `buf`).

### GTK Control Fixtures (`just check-gtk`)

Requires `xvfb-run`, `Xvfb`, `xauth`, `dbus-run-session`, and `glib-compile-schemas`.

Debian and Ubuntu:

```sh
sudo apt-get install dbus-x11 libglib2.0-bin xauth xvfb
```

Arch Linux:

```sh
sudo pacman -S dbus glib2 xorg-server-xvfb xorg-xauth
```

Run the suite:

```sh
just check-gtk
```

`rust/tools/test_gtk.py` builds the all-feature `cordial` library test binary once, discovers every ignored `desktop::` test (currently 20), and runs each in an isolated process with private XDG directories sharing one Xvfb and D-Bus session. All ignored `desktop::` library tests must run unattended; manual test screens live outside that namespace.

### Linux Terminal Fixture

Verifies PTY terminal restoration, interactive shell and TUI operation, `--json` stderr streaming, error preservation, and shutdown behavior against the release binary. Requires `pyte`, Python `protobuf`, `protoc`, and a C compiler.

Debian and Ubuntu:

```sh
sudo apt-get install build-essential protobuf-compiler python3-protobuf python3-pyte
```

Arch Linux:

```sh
sudo pacman -S base-devel protobuf python-protobuf python-pyte
```

Build the release binary and run the fixture:

```sh
just host
CORDIAL_TEST_BINARY="$PWD/rust/target/x86_64-unknown-linux-gnu/release/cordial" \
  python3 -m unittest discover -s rust/tests -p test_host_terminal.py
```

### Firmware Validation

Run Clippy and artifact validation together in a single cached toolchain invocation for each required board and profile (both `pico2_w` profiles are required, plus both profiles for any other board materially affected by a change):

```sh
just BOARD=pico2_w PROFILE=production check-and-build-firmware
just BOARD=pico2_w PROFILE=debug check-and-build-firmware
```

`check-firmware` and `check-firmware-all` run Clippy only and do not replace artifact validation.

### Native Container Validation

Native package lifecycle, installed desktop, and AppImage checks run in disposable containers built from `docker/check.Dockerfile` and `docker/check-prerequisites.sh` with Cordial initially absent. Never install validation packages onto the host system.

Refresh the prerequisite images once against current distribution repositories before running native checks, then reuse them until prerequisite definitions change:

```sh
just DEB_DISTRIBUTION=resolute CHECK_IMAGE_FLAGS='--pull --no-cache' \
  image-check-package-deb image-check-package-arch image-check-desktop \
  image-check-appimage-resolute image-check-appimage-arch
just DEB_DISTRIBUTION=trixie CHECK_IMAGE_FLAGS='--pull --no-cache' image-check-package-deb
```

Run the Resolute checks in one invocation so the current-source host archive and Resolute `.deb` package are built once and shared across checks, then run the Trixie package check and archive validator:

```sh
export CORDIAL_VERSION="${CORDIAL_VERSION:-0.0.0}"
just DEB_DISTRIBUTION=resolute check-package-deb check-installed-desktop \
  check-package-arch check-appimage-resolute check-appimage-arch
just DEB_DISTRIBUTION=trixie check-package-deb
python3 tools/check_release_archive.py \
  "build/release/cordial-$CORDIAL_VERSION-linux-amd64.tar.gz" host "$CORDIAL_VERSION"
```

Validation coverage and rules:

- **Package lifecycle** (`check-package-arch`, `check-package-deb`): clean install, reinstall, native dependencies, GSettings schema registration, file ownership/contents, Namcap or Lintian, removal, and Debian purge.
- **Installed desktop** (`check-installed-desktop`, Resolute only): runs three AT-SPI smoke scenarios against the installed `.deb` (`normal`, `--empty-states`, and `--pairing-reconnect`), writing logs and screenshots to `build/package-smoke`, `build/empty-states-smoke`, and `build/pairing-reconnect-smoke`.
- **AppImage runtime** (`check-appimage-resolute`, `check-appimage-arch`): verifies packaged launch, bundled GIO modules, and persistent default-backend GSettings (using software Mesa on Resolute and Cairo on Arch, with `APPIMAGE_EXTRACT_AND_RUN` fallback when FUSE is absent), writing evidence to `build/appimage-smoke-resolute` and `build/appimage-smoke-arch`.
- **`HOST_ARCHIVE` freshness**: Supply `HOST_ARCHIVE` only when its source, version, target, and build configuration exactly match the sources under test. Never substitute a stale archive.

### Desktop Simulation and Test Ownership

You can launch the desktop app against simulated adapters without hardware:

```sh
CORDIAL_DESKTOP_SIMULATE=2 rust/target/x86_64-unknown-linux-gnu/release/cordial desktop
```

Each simulated adapter starts with a connected mouse and keyboard reporting battery, a connected controller without battery, and a disconnected keyboard. Additional simulation controls used by the smoke fixtures:

| Environment Variable | Purpose |
| --- | --- |
| `CORDIAL_SIM_EMPTY_STATES=1` | Seeds each simulated adapter with one library profile and one device for empty-state transition testing |
| `CORDIAL_SIM_WRITE_DELAY_MS` | Adds a simulated write delay (`0..2000` ms) for delayed/coalesced write and quit-drain tests |
| `CORDIAL_SIM_FAIL_SETTINGS_ONCE=1` | Rejects the first settings write per simulated adapter with a no-effect storage error |
| `CORDIAL_SIM_WRITE_TRACE` | Absolute path to an append-only trace file recording terminal apply jobs (`adapter`/`device`/`settings` + `ok`/`error`/`unknown`) |
| `CORDIAL_SIM_PAIRING_RECONNECT_MS` | Emits `SessionEnding(DEVICE_CONNECTION)` and temporarily disconnects the first simulated adapter after pairing (`100..10000` ms) |
| `CORDIAL_SIM_PAIRING_RECONNECT_TRACE` | Absolute path to a trace file recording `opened`, `done`, `closed`, and `present` reconnect barriers |

Test ownership and timing guidelines:

- Keep exhaustive pure-logic and control matrices in focused Rust unit and `check-gtk` tests. Smoke fixtures own representative native input through protocol writes and readback, settings backends, process restarts, packaged runtime, responsive layout widths (480, 661, and 700 px), and reconnect behavior.
- Wait for observable state rather than fixed sleeps, and reacquire AT-SPI objects after UI mutations.
- Report check time separately from build/setup time and total wall time; `time` output for container recipes includes container startup and fixture setup. Compare timings using the same binary, inputs, cache state, and comparable host load, and never count a skipped fixture as a pass.
- Release workflow validation jobs share a combined 120-second test/validation budget (excluding build, packaging, and publishing; record setup separately). Full regression suites run locally.

## Repository Layout

- `rust/`: firmware, native GTK desktop app, CLI/TUI, shared crates, hardware adapters, board configs, and Rust test tools.
- `docs/`: user guides, build documentation, Logitech HID++ inventory, serial/USB protocol specs, and storage format reference.
- `proto/`: Protocol Buffer definitions for the handshake, serial API, storage formats, and migration IPC.
- `configs/`: udev rules (`50-cordial.rules`) and VIA/Vial virtual keyboard definitions.
- `docker/`: toolchain and container validation Dockerfiles and helper scripts.
- `packaging/`: Arch Linux (`PKGBUILD`), Debian/Ubuntu packaging files, desktop entry, icons, and GSettings schema.
- `tools/`: version, protocol compatibility, and release archive validation scripts and tests.
- `notices/`: retained third-party license notices.
