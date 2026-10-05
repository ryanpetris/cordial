# Building Cordial

## Board presets

| Board preset | Bluetooth support | Input profiles |
| --- | --- | --- |
| `pico_w` | Classic HID and BLE through BTstack | No |
| `pico2_w` | Classic HID and BLE through BTstack | Yes |
| `waveshare_rp2350b_plus_w` | Classic HID and BLE through BTstack | Yes |
| `xiao_esp32s3` | BLE through ESP-NimBLE or BTstack | Yes |

Board definitions live in `rust/boards/`. They specify pins, flash allocation, clock settings,
backends and `profile_memory_budget`: the bytes of RAM for loaded [input
profiles](input-profiles.md), or `null` for a board without profile support. Without profile support
the firmware omits profiles and configuration interfaces, so VIA and Vial are unavailable. The Pico
W preset leaves profiles out to keep its limited RAM for connections; the other presets budget room
for two fully remapped VIA profiles per connected device. Pico presets use `pico-sdk-cyw43`;
`embassy-cyw43` is also supported. The XIAO preset uses `esp-nimble` and `esp-idf`.

To use custom wiring or another backend, copy a preset, edit it and pass its path
to `python3 rust/tools/build_firmware.py`. USB and Bluetooth identities are
separate from transient port names. Select an adapter by its reported identity.

## Build

Run commands from the repository root on Linux. Builds run in Docker, so the only
prerequisites are GNU Make, Git and [Docker](https://docs.docker.com/engine/install/)
with your user allowed to run it. `DOCKER=podman` uses Podman instead.

```sh
make desktop
make cli
make firmware BOARD=pico_w
```

Each build runs in a toolchain image from `docker/`, created on first use and
reused afterwards. The build copies the checkout into the image, leaving out the
paths in `.dockerignore` (build outputs, caches, installed dependencies and Git
data), builds there, and copies only its outputs back to the same places a local
build would put them, replacing the earlier copy of each output. Every build
starts from that clean copy, downloading its dependencies and compiling from
scratch; only a build of identical sources can reuse Docker's cached result.
Docker keeps those results until `docker builder prune` removes them, and
firmware builds take several gigabytes each. Docker BuildKit is required.

The CLI executable is `rust/target/release/cordial`. The desktop build is in
`desktop/out/`.

### Building without Docker

`make local-desktop`, `make local-web` and `make local-cli` build the desktop
application, its web version and the CLI with locally installed tools. Install
Python 3.11.4+, Node.js 24 with npm, Rust through [rustup](https://rustup.rs/), and
native build tools. Cargo uses the pinned toolchain in `rust/rust-toolchain.toml`;
the rustup `cargo` and `rustc` commands must be on `PATH`. On Debian and Ubuntu:

```sh
sudo apt-get install build-essential git clang libclang-dev pkg-config python3
```

On Arch, install `base-devel`, `git`, `clang`, `pkgconf` and `python`. Desktop
targets run `npm ci` when the manifests change or dependencies are missing. After
installing desktop dependencies, `npm --prefix desktop run start` builds and starts
the desktop application. The software checks below use the same local tools.

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
The page asks for access to an adapter with **Choose Adapter**; the browser then
remembers it. Only one tab manages adapters at a time. Add `?simulate=2` to the
URL for a demo with two simulated adapters, which needs no Web Serial. The
browser has no tray, notifications appear only while the page is open, and
settings are kept in the browser.

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
adapter. Changes in Settings last until the server stops.

### Firmware

```sh
make firmware BOARD=pico_w
make firmware BOARD=xiao_esp32s3 PROFILE=production
make firmware-all
```

The Pico image has Rust and ARM GCC; the ESP32-S3 image adds Rust, espup and
ldproxy to Espressif's ESP-IDF v6.1 image. Each build fetches the pinned Pico SDK
and BTstack sources inside the image.

`BOARD` defaults to `pico_w`. `PROFILE` is `production`, `debug` or `development`
and defaults to `development`, independently of compiler release optimization.
`PROFILE=production` excludes development commands, including remote bootloader
entry and filesystem inspection. `debug` and `development` include them and run
the same code; only their version and embedded profile name differ. No Make
target installs firmware.

The firmware version is the release version for `production`, with a `-debug`
suffix for `debug` and a `-dev` suffix for `development`, such as `1.2.3-debug`.
Firmware packages are written under
`build/firmware/<firmware-version>/<board>-<bluetooth-backend>-<radio-backend>/`.
They contain the ELF, binary, artifact metadata and checksums. Pico packages also
contain UF2; ESP packages contain bootloader and partition-table images plus
`flash.json`. Packaging verifies image metadata and storage bounds.

`rust/tools/flash_dev.py` inspects Pico UF2 artifacts and supports explicit
storage-preserving development updates. See its `--help` for identity and mount
arguments. ESP32-S3 uses its ROM download protocol and the regions in `flash.json`.
Ordinary firmware updates preserve saved bonds, identities and preferences.

### Versioning and release packages

First-party manifest versions are `0.0.0`. Builds use `CORDIAL_VERSION` when
supplied and otherwise use `0.0.0`; firmware adds its profile suffix. The value is
`MAJOR.MINOR.PATCH`, without leading zeroes or a `v` prefix. Tracked manifests
and native package recipes remain unchanged.

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

`make package-cli` builds the static Rust executable, documentation, protocol definitions and
dependency notices, then creates `build/release/cordial-cli-<version>-linux-<arch>.tar.gz`.
`make package-cli-tar` is the same target. The CLI has no Electron dependency.
`CLI_TARGET` selects `x86_64-unknown-linux-musl` or `aarch64-unknown-linux-musl`;
the default matches the build machine. The app image includes both targets.

`make package-web` writes `build/packages/web/cordial-web-<version>/`, a directory
ready for a static host. Firmware packages are separate from host applications.

### Native distribution packages

Supported native targets are Arch Linux x86-64, Debian 13, Ubuntu 24.04 and
Ubuntu 26.04. Each package is built in an image of its distribution from a
portable archive, which the Make targets build first when none is supplied:

```sh
CORDIAL_VERSION=1.2.3 make package-desktop-arch package-cli-arch
CORDIAL_VERSION=1.2.3 make package-desktop-deb package-cli-deb DEB_DISTRIBUTION=noble
```

Use `DESKTOP_ARCHIVE` and `CLI_ARCHIVE` to package existing archives:

```sh
export CORDIAL_VERSION=1.2.3
make package-desktop-deb DESKTOP_ARCHIVE=cordial-desktop-1.2.3-x64.tar.gz
make package-cli-deb CLI_ARCHIVE=cordial-cli-1.2.3-linux-amd64.tar.gz
```

Archive versions and architectures are checked before packaging. Native desktop
packages copy the bundled distribution to `/opt/cordial-desktop`, with a launcher
in `/usr/bin`, desktop entry, icon and license in the standard locations. This
follows the [Arch Electron guidelines](https://wiki.archlinux.org/title/Electron_package_guidelines)
for applications with bundled Electron. The native packages configure Electron's
sandbox helper; Debian and Ubuntu also install its AppArmor user namespace profile.

Arch packages are built with `makepkg` from `desktop/packaging/arch/` and
`rust/packaging/arch/` into `build/packages/desktop-arch/` and
`build/packages/cli-arch/` as `.pkg.tar.zst` files. Debian and Ubuntu packages are
built with `dpkg-buildpackage` from `desktop/packaging/debian/` and
`rust/packaging/debian/` into `build/packages/<application>-deb/<distribution>/`.
`DEB_DISTRIBUTION` is `trixie`, `noble` or `resolute` and defaults to `trixie`;
`DEB_REVISION` is `1~<distribution>`, producing distinct Debian and Ubuntu files.
Debian's library tools derive dependency versions from the bundled binaries on
each distribution.

`CORDIAL_HOMEPAGE` can override the project homepage in native package metadata.
`SOURCE_DATE_EPOCH` can supply the release timestamp in Unix seconds; otherwise
native packaging uses the archive's `VERSION` timestamp. Supplied archives and
tracked recipes remain unchanged.

The GitHub release workflow resolves the tag once and supplies the same version
to desktop, web, CLI and firmware builds, which use the same Make targets and
images as a local build. It builds the portable archives first, then each native
package from those archives. Installation, native module loading, package checks
and artifact checksums are verified in a clean container of each distribution
before publication. Releases include the AppImage, desktop, CLI and web archives,
both applications' Arch packages, separate Debian and Ubuntu packages, and for each
board a production firmware archive, `cordial-firmware-<version>-<board>.tar.gz`,
and a debug archive, `cordial-firmware-<version>-debug-<board>.tar.gz`, built with
the debug profile. Each archive holds its firmware version's package directory.

Third-party license material is retained in `notices/`. Firmware and CLI packages
include the project license, generated dependency inventories and license notices.

## Software checks

```sh
make check
npm --prefix desktop/packages/protocol run generate
```

`make check` runs Rust tests and Clippy, native Bluetooth regression checks,
Python tool tests, desktop type checks and tests, and the protocol checks:
`tools/check_keys.py`, `buf lint`, and `buf breaking` against the newest earlier release in the
same series as `CORDIAL_VERSION`, or the newest release for a development build
(`PROTOCOL_BASE` overrides it). A series is one minor version before 1.0 and one major version
from 1.0; the first release of a series is not compared. Rust code is generated from `proto/` at build time;
the second command regenerates the checked-in TypeScript messages and key constants
after `proto/cordial.proto` or `proto/keys.toml` changes.
Install Clippy with `(cd rust && rustup component add clippy)` before checking.

Linux CLI terminal checks additionally require `pyte` and a built CLI:

```sh
CORDIAL_TEST_BINARY="$PWD/rust/target/release/cordial" python3 -m unittest discover -s rust/tests -p test_host_terminal.py
```

The ARM allocation check prepares development firmware for each Pico board and runs the
workloads with that board's configuration, such as its profile memory budget, against its linked
heap bounds. It runs in the Pico firmware image, which includes QEMU:

```sh
make check-memory
```

The allocation check runs simulated device workloads on QEMU without accessing
hardware. Clippy for the firmware platform crates also runs in the firmware
images, for one board and profile or for all of them:

```sh
make check-firmware BOARD=xiao_esp32s3 PROFILE=production
make check-firmware-all
```

To exercise the desktop with simulated adapters:

```sh
npm --prefix desktop run simulate
```

## Adapter names

Adapter names are stored on the adapter. Rename through adapter settings in the TUI or desktop app, or run `adapter set name "Desk"` in the shell. Each board configuration supplies `default_adapter_name`, baked into the firmware image and used until a custom name is saved. Use **Reset to Default** in either rename dialog, or `adapter reset name` in the shell, to clear the custom name.

## Repository layout

- `rust/`: firmware, CLI/TUI, shared crates, adapters, board definitions and their tools/tests.
- `desktop/`: Electron, React and TypeScript application, its web version and development server.
- `docs/`: control protocol, storage format and HID++ references.
- `proto/`: serial protocol messages and the information and settings key catalog.
- `docker/`: build images for the applications, firmware and distribution packages.
- `packaging/`: the distribution packaging steps and their installation checks.
- `tools/`: version validation, package checks and their tests.

`make all` builds desktop, CLI and the selected firmware. `make clean` removes
build outputs while preserving downloaded dependencies and toolchains.
