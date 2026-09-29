# Building Cordial

## Board presets

| Board preset | Bluetooth support |
| --- | --- |
| `pico_w` | Classic HID and BLE through BTstack |
| `pico2_w` | Classic HID and BLE through BTstack |
| `waveshare_rp2350b_plus_w` | Classic HID and BLE through BTstack |
| `xiao_esp32s3` | BLE through ESP-NimBLE or BTstack |

Board definitions live in `rust/boards/`. They specify pins, flash allocation,
clock settings and backends. Pico presets use `pico-sdk-cyw43`; `embassy-cyw43`
is also supported. The XIAO preset uses `esp-nimble` and `esp-idf`.

To use custom wiring or another backend, copy a preset, edit it and pass its path
to `python3 rust/tools/build_firmware.py`. USB and Bluetooth identities are
separate from transient port names. Select an adapter by its reported identity.

## Build

Run commands from the repository root on Linux. Install Python 3.11.4+, Node.js
24 with npm, Rust through [rustup](https://rustup.rs/), and native build tools.
Cargo uses the pinned toolchain in `rust/rust-toolchain.toml`. The rustup `cargo`
and `rustc` commands must be on `PATH`.

On Debian and Ubuntu, install the native prerequisites with:

```sh
sudo apt-get install build-essential git clang libclang-dev pkg-config python3
```

On Arch, install `base-devel`, `git`, `clang`, `pkgconf` and `python`.
Desktop targets run `npm ci` when the manifests change or dependencies are
missing, then invoke the Vite CLI. Rust targets invoke Cargo directly. Install
the board tools below before building firmware.

```sh
make desktop
make cli
```

The CLI executable is `rust/target/release/cordial`. The desktop build is in
`desktop/out/`. After installing desktop dependencies, `npm --prefix desktop run
start` builds and starts the desktop application.

### Web version

The desktop window also runs in a browser, with the controller in the page and
adapters reached through Web Serial. Web Serial exists only in Chromium-based
browsers (Chrome, Edge) and only on `https://` pages or `http://localhost`.

```sh
make web
```

The build is in `desktop/out/web/`, a static site with relative URLs that works
under any path. Serve it locally with, for example,
`python3 -m http.server 8000 --bind 127.0.0.1 --directory desktop/out/web`.
The page asks for access to an adapter with **Choose Adapter…**; the browser then
remembers it. Only one tab manages adapters at a time. Add `?simulate=2` to the
URL for a demo with two simulated adapters, which needs no Web Serial. The
browser has no tray, notifications appear only while the page is open, and
preferences are kept in the browser.

Web Serial can't read USB serial numbers, so the page identifies adapters by the
protocol handshake alone. A disconnected adapter is remembered by its port.
When it is plugged in again, the page briefly opens the new port to identify it,
then leaves it disconnected until you choose Connect.

### Development server

For testing, the desktop window can run in a browser while the adapters and the
controller stay on another machine, such as a remote development host. It is
never built into the application or packaged.

```sh
npm --prefix desktop run serve                  # adapters on this machine
npm --prefix desktop run serve -- --simulate    # two simulated adapters
```

The server listens on `localhost:5180` only (`--port` changes it) and reloads
the window when its sources change. From another machine, forward the port and
open `http://localhost:5180`:

```sh
ssh -L 5180:localhost:5180 devhost
```

It refuses requests with another host name or origin, and actions that aren't
JSON, so other web pages can't use it. It has no other authentication; anyone
who can reach the port on the development host can manage its adapters. Quit the
desktop application on that host first, since only one program can open an
adapter. Preferences last until the server stops.

### Firmware toolchains

All firmware builds require CMake 3.24+, Ninja and native build tools.
Pico builds require ARM GCC with newlib/nano and its redistribution notices.
Install `gcc-arm-none-eabi` and `libnewlib-arm-none-eabi` on Debian and Ubuntu,
or `arm-none-eabi-gcc` and `arm-none-eabi-newlib` on Arch. Rustup installs the
Pico Rust targets listed in `rust/rust-toolchain.toml`.

ESP32-S3 uses [espup](https://github.com/esp-rs/espup) and an activated
[ESP-IDF v6.1 environment](https://docs.espressif.com/projects/esp-idf/en/v6.1/esp32s3/get-started/).
Install `python3-venv` and `libusb-1.0-0` on Debian and Ubuntu, or `python` and
`libusb` on Arch, then prepare the vendor tools:

```sh
cargo install espup --version 0.17.1 --locked
cargo install ldproxy --version 0.3.5 --locked
espup install --toolchain-version 1.97.0.0 --targets esp32s3 --std
git clone --branch v6.1 --depth 1 --recursive --shallow-submodules https://github.com/espressif/esp-idf.git ../esp-idf
../esp-idf/install.sh esp32s3
. "$HOME/export-esp.sh"
. ../esp-idf/export.sh
```

Source both export files in each shell that builds ESP firmware. The firmware
builder uses the exported compiler, SDK and Python environment. Pico SDK and
BTstack source dependencies remain pinned and unmodified in `.cache/dependencies/`.

```sh
make firmware BOARD=pico_w
make firmware BOARD=xiao_esp32s3
make firmware-all
```

`BOARD` defaults to `pico_w`. `PROFILE` defaults to `development`, independently
of compiler release optimization. `PROFILE=production` excludes development
commands, including remote bootloader entry and filesystem inspection. No Make
target installs firmware.

Firmware packages are written under
`build/firmware/<version>/<board>-<bluetooth-backend>-<radio-backend>-<profile>/`.
They contain the ELF, binary, artifact metadata and checksums. Pico packages also
contain UF2; ESP packages contain bootloader and partition-table images plus
`flash.json`. Packaging verifies image metadata and storage bounds.

`rust/tools/flash_dev.py` inspects Pico UF2 artifacts and supports explicit
storage-preserving development updates. See its `--help` for identity and mount
arguments. ESP32-S3 uses its ROM download protocol and the regions in `flash.json`.
Ordinary firmware updates preserve saved bonds, identities and preferences.

### Versioning and release packages

First-party manifest versions are `0.0.0`. Builds use `CORDIAL_VERSION` when
supplied and otherwise use `0.0.0`. The value is `MAJOR.MINOR.PATCH`, without
leading zeroes or a `v` prefix. Tracked manifests and native package recipes
remain unchanged.

```sh
export CORDIAL_VERSION=1.2.3
make package-desktop package-cli package-web package-firmware BOARD=pico_w
```

The tag-triggered release workflow reads `GITHUB_REF_NAME`, validates
`vMAJOR.MINOR.PATCH`, and supplies the numeric version to every build job.

### Portable archives

`make package-desktop` uses electron-builder for AppImage and `.tar.gz` output in
`desktop/dist/`. `make package-desktop-tar` builds just the archive. Both formats
bundle Electron and the application's native modules.

`make package-cli` builds the static Rust executable, documentation, schemas and
dependency notices, then creates `build/release/cordial-<version>-linux-<arch>.tar.gz`.
`make package-cli-tar` is the same target. The CLI has no Electron dependency.
Install its musl target once with rustup before packaging:

```sh
(cd rust && rustup target add x86_64-unknown-linux-musl)
```

On ARM64 use `aarch64-unknown-linux-musl`. `CLI_TARGET` can select either target;
the default matches the build machine.

`make package-web` writes `build/packages/web/cordial-web-<version>/`, a directory
ready for a static host. Firmware packages are separate from host applications.

### Native distribution packages

Build each package on its target distribution. Supported native targets are
Arch Linux x86-64, Debian 13, Ubuntu 24.04 and Ubuntu 26.04. Each native recipe
consumes a portable archive. The Make targets build that archive when none is
supplied:

```sh
CORDIAL_VERSION=1.2.3 make package-desktop-arch package-cli-arch
CORDIAL_VERSION=1.2.3 make package-desktop-deb package-cli-deb
```

Use `DESKTOP_ARCHIVE` and `CLI_ARCHIVE` to package existing archives. This path
requires Python and the native packaging tools, without Node, Cargo, firmware
tools or Git:

```sh
export CORDIAL_VERSION=1.2.3
make package-desktop-deb DESKTOP_ARCHIVE=cordial-desktop-1.2.3-x64.tar.gz
make package-cli-deb CLI_ARCHIVE=cordial-1.2.3-linux-amd64.tar.gz
```

Archive versions and architectures are checked before packaging. Native desktop
packages copy the bundled distribution to `/opt/cordial-desktop`, with a launcher
in `/usr/bin`, desktop entry, icon and license in the standard locations. This
follows the [Arch Electron guidelines](https://wiki.archlinux.org/title/Electron_package_guidelines)
for applications with bundled Electron. The native packages configure Electron's
sandbox helper; Debian and Ubuntu also install its AppArmor user namespace profile.

Arch uses `makepkg` and `fakeroot`. Recipes are in `desktop/packaging/arch/` and
`rust/packaging/arch/`; output goes to `build/packages/desktop-arch/` and
`build/packages/cli-arch/` as `.pkg.tar.zst` files. Run `makepkg` as a regular user
with the recipe's dependencies installed.

Debian and Ubuntu use `dpkg-buildpackage`, debhelper and `dch`. Install
`build-essential`, `debhelper`, `devscripts`, `dh-apparmor`, `equivs` and `python3`,
then install the desktop recipe's build dependencies:

```sh
sudo mk-build-deps --install --remove --tool 'apt-get -y --no-install-recommends' desktop/packaging/debian/control
```

Recipes are in `desktop/packaging/debian/` and `rust/packaging/debian/`. Package
output goes to `build/packages/<application>-deb/<distribution>/`. By default,
`DEB_DISTRIBUTION` is the build system's codename and `DEB_REVISION` is
`1~<distribution>`, producing distinct Debian and Ubuntu files. Debian's library
tools derive dependency versions from the bundled binaries on each distribution.

`CORDIAL_HOMEPAGE` can override the project homepage in native package metadata.
`SOURCE_DATE_EPOCH` can supply the release timestamp in Unix seconds; otherwise
native packaging uses the archive's `VERSION` timestamp. Supplied archives and
tracked recipes remain unchanged.

The GitHub release workflow resolves the tag once and supplies the same version
to desktop, web, CLI and firmware builds. It builds the portable archives first,
then builds each native package on its target distribution from those archives.
Installation, native module loading, package checks and artifact checksums must
pass before publication. Releases include the AppImage, desktop, CLI and web
archives, both applications' Arch packages, separate Debian and Ubuntu packages,
and production firmware archives for all four boards.

Third-party license material is retained in `notices/`. Firmware and CLI packages
include the project license, generated dependency inventories and license notices.

## Software checks

```sh
make check
make schema
```

`make check` runs Rust tests and Clippy, native Bluetooth regression checks,
Python tool tests, and desktop schema/type checks and tests. `make schema`
regenerates the shared schemas and TypeScript wire types.
Install Clippy with `(cd rust && rustup component add clippy)` before checking.

Linux CLI terminal checks additionally require `pyte` and a built CLI:

```sh
CORDIAL_TEST_BINARY="$PWD/rust/target/release/cordial" python3 -m unittest discover -s rust/tests -p test_host_terminal.py
```

The ARM allocation check prepares Pico W development firmware and uses its linked
heap bounds. It requires the system package `qemu-system-arm`:

```sh
make check-memory
```

The allocation check runs simulated device workloads on QEMU without accessing
hardware. To exercise the desktop with simulated adapters:

```sh
npm --prefix desktop run simulate
```

## Adapter names

Adapter names are stored on the adapter. Rename through adapter settings in the TUI or desktop app, or run `adapter name set "Desk"` in the shell. Each board configuration supplies `default_adapter_name`, baked into the firmware image and used until a custom name is saved. Use **Reset to default** in either rename dialog, or `adapter name reset` in the shell, to clear the custom name.

## Repository layout

- `rust/`: firmware, CLI/TUI, shared crates, adapters, board definitions and their tools/tests.
- `desktop/`: Electron, React and TypeScript application, its web version and development server.
- `docs/`: control protocol, storage format and HID++ references.
- `schema/`: generated JSON wire schema and command catalog.
- `tools/`: version validation, package checks and their tests.

`make all` builds desktop, CLI and the selected firmware. `make clean` removes
build outputs while preserving downloaded dependencies and toolchains.
