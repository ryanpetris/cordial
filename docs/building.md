# Building Cordial

## Board presets

| Board preset | Bluetooth support | Input profiles |
| --- | --- | --- |
| `pico_w` | Classic HID and BLE through BTstack | No |
| `pico2_w` | Classic HID and BLE through BTstack | Yes |
| `pimoroni_pico_plus2_w` | Classic HID and BLE through BTstack | Yes |
| `waveshare_rp2350b_plus_w` | Classic HID and BLE through BTstack | Yes |
| `esp32s3` | BLE through ESP-NimBLE or BTstack | Yes |

Board definitions live in `rust/boards/`. They specify pins, minimum flash capacity, clock settings,
backends and `profile_memory_budget`: the bytes of RAM for loaded [input
profiles](input-profiles.md), or `null` to build without them. The Pico W preset leaves profiles
out to keep its limited RAM for connections. The other presets budget 40 KiB for loaded profiles,
shared across connections.
`minimum_flash_bytes` is the image's minimum flash requirement and the fallback when runtime
capacity discovery is unsupported. Detected capacity determines the filesystem size.
Bluetooth pool sizes derive from the same `CORDIAL_CONFIG` for Rust and C. See
[Bluetooth capacity](protocol/limits-and-errors.md#bluetooth-capacity) for the independent transport
allowances and pairing reserve. Pico presets use `pico-sdk-cyw43`; `embassy-cyw43` is also supported.
The `esp32s3` preset uses `esp-nimble` and `esp-idf`.

The `pimoroni_pico_plus2_w` preset targets the Pimoroni Pico Plus 2 W with an RP2350B and
16 MiB flash. Cordial leaves its 8 MiB PSRAM unused. Hardware verification is outstanding.

`esp32s3` targets ESP32-S3 boards with at least 4 MiB flash and native USB connected to the host.
It reserves 2 MiB for firmware and budgets 40 KiB of internal RAM for loaded profiles.

The [Seeed Studio XIAO ESP32-S3](https://wiki.seeedstudio.com/xiao_esp32s3_getting_started/),
[M5Stack Atom S3](https://docs.m5stack.com/en/core/AtomS3) and
[Atom S3U](https://docs.m5stack.com/en/core/AtomS3U) meet this preset's requirements. See the
[flashing steps](installation.md#esp32-s3) for each board's download mode.

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
build would put them, replacing the earlier copy of each output. Docker BuildKit
is required.

BuildKit cache mounts retain Cargo downloads, Rust compilation outputs, pinned
firmware dependencies, npm downloads, Electron and electron-builder downloads,
and tools that use `XDG_CACHE_HOME`. Desktop, web and their portable packages
share a Docker layer containing the complete npm installation, including native
modules, workspace links and nested dependencies. The layer runs `npm ci` with
development dependencies before copying application sources. Its inputs are
`desktop/package.json`, `desktop/package-lock.json`, all workspace manifests,
and `desktop/.npmrc` and `desktop/npm-shrinkwrap.json` when present.
Changing those inputs or the toolchain runtime creates a new installation;
application source edits reuse it. If installation hooks need repository files,
include those files in the `npm_inputs` extraction in `docker/build-cache.py`.
CLI, firmware and native archive repackaging skip this layer. Packaged outputs
are not cached. Firmware keeps its board configuration and profile hash
directories under `target/firmware/`; CLI target triples remain separate under
`rust/target/`.

Caches are shared by checkouts using the same Docker builder and toolchain runtime.
The cache key hashes the image's filesystem layers, runtime configuration, OS,
architecture and variant. Image export metadata, such as provenance, can change
the image ID without changing this key. Changes to native libraries, compilers,
runtime configuration or platform get separate caches. Firmware compilation and
dependency caches also include the SHA-256 of `rust/tools/firmware_dependencies.py`,
so different dependency pins never switch a shared SDK checkout. Cargo handles
Rust dependency and compiler-option changes.
Copied input files retain timestamps by content, so Cargo detects source changes
even when another checkout supplies older timestamps. Installed `node_modules`
trees are excluded from source hashing. The npm installation marker is refreshed
after timestamp normalization so Make reuses the prepared installation. Build
commands and artifact validation still run when the Docker result layer is not
reusable.

Builds using the same toolchain runtime serialize access to their cache mounts.
This protects Cargo outputs, generated configuration and mutable SDK checkouts
across concurrent builds. Different toolchain runtimes can build in parallel.
Each Make target has an export scratch directory under `.cache/docker/`;
concurrent invocations of the same target must use separate checkouts. Toolchain
images also receive `cordial-toolchain:<runtime-hash>` tags. Each tag retains the
first image with matching runtime contents, so provenance changes preserve the
npm layer and a rebuild of the regular image tag preserves the selected
environment. An existing runtime tag with different contents is replaced.

`make clean` removes exported outputs but leaves BuildKit caches intact. To clear
unused cache mounts for the active Docker builder, run
`docker builder prune --filter type=exec.cachemount`. This affects all projects
using that builder, so coordinate cleanup with other builds. Docker may also
reclaim cache mounts during normal garbage collection; a missing cache triggers
a rebuild. Pruning also recovers dependency caches left incomplete by an
interrupted checkout. Cache mounts can use several gigabytes per toolchain and
firmware configuration. Content-tagged toolchain images remain until their tags
are removed with `docker image rm cordial-toolchain:<runtime-hash>`.

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
make firmware BOARD=esp32s3 PROFILE=production
make firmware-all
```

The Pico image has Rust and ARM GCC; the ESP32-S3 image adds Rust, espup and
ldproxy to Espressif's ESP-IDF v6.1 image. Builds fetch the pinned Pico SDK and
BTstack sources into the dependency cache as needed.

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
`flash.json`. Packaging verifies image metadata and storage bounds. ESP packaging
also checks the linked Rust startup frames: `main` has at most 1 KiB, and the
initialization frames together have at most 16 KiB. This catches large constructor
temporaries.

Every Pico board reserves 65536 bytes for the core 0 stack. Its synchronous entry
installs `embassy_rp::install_core0_stack_guard` before executor or task
construction, peripheral initialization, heap initialization and application
construction. A separate, non-inlined function starts the executor so its frames
are allocated after guard installation. Firmware halts if installation fails.
The Cortex-M reset handler initializes RAM before this entry runs, so that early
runtime code is outside this protection.

On RP2040, Embassy uses the MPU to deny access to a 32-byte subregion at the
bottom of the stack reservation. Access to that subregion faults, but a large
stack adjustment can jump over it and corrupt memory below the reservation
without touching the guard. On RP2350, Embassy sets `MSPLIM` to the bottom of
the reservation so main stack pointer updates below that limit fault. The guards
provide overflow detection.

`rust/tools/flash_dev.py` inspects Pico UF2 artifacts and supports explicit
storage-preserving development updates. See its `--help` for identity and mount
arguments. ESP32-S3 uses its ROM download protocol and the regions in `flash.json`.

Startup mounts storage and checks the shared version in `Format.format` before
using saved configuration. Invalid filesystems or essential roots and incompatible formats
reset the filesystem automatically. Driver I/O failures keep storage unavailable.
Compatible updates at the same filesystem start preserve saved data and can grow
the filesystem to the detected capacity. See the [storage format](storage-format.md)
for reservations, recovery and the one-time Pico storage reset on chips of at
least 4 MiB.

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

Release builds reuse Docker toolchain images and BuildKit caches within each job.
Each firmware job reuses preparation between checking and packaging its board and
profile. Production and debug profiles run in parallel. These caches last only for
that job and are not transferred to other runners or later releases. Native
application packaging jobs reuse the portable archives through workflow artifacts.

Third-party license material is retained in `notices/`. Firmware and CLI packages
include the project license, generated dependency inventories and license notices.

## Software checks

```sh
make check
npm --prefix desktop/packages/protocol run generate
```

`make check` runs Rust tests and Clippy, native Bluetooth regression checks,
Python tool tests, desktop type checks and tests, and the protocol checks:
`buf lint` and `buf breaking` against the newest earlier release in the
same series as `CORDIAL_VERSION`, or the newest release for a development build
(`PROTOCOL_BASE` overrides it). A series is one minor version before 1.0 and one major version
from 1.0; the first release of a series is not compared. Rust code is generated from `proto/` at build time;
the second command regenerates the checked-in TypeScript messages after a schema changes.
Install Clippy with `(cd rust && rustup component add clippy)` before checking.

The build-cache tests also have a tiny npm workspace fixture that runs in Docker.
Build the app image before running it:

```sh
make image-app
CORDIAL_TEST_DOCKER=docker python3 -m unittest discover -s tools -p test_build_cache.py
```

`CORDIAL_TEST_APP_IMAGE` selects an alternative app toolchain image. The fixture
checks installation reuse, source and manifest edits, runtime changes, workspace
links, nested dependencies, failed-install retries and clean exports.

Linux CLI terminal checks additionally require `pyte` and a built CLI:

```sh
CORDIAL_TEST_BINARY="$PWD/rust/target/release/cordial" python3 -m unittest discover -s rust/tests -p test_host_terminal.py
```

Clippy for the firmware platform crates runs in the firmware images, for one board and
profile or for all of them:

```sh
make check-firmware BOARD=esp32s3 PROFILE=production
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
- `proto/`: serial protocol messages and persistent storage records.
- `docker/`: build images for the applications, firmware and distribution packages.
- `packaging/`: the distribution packaging steps and their installation checks.
- `tools/`: version validation, package checks and their tests.

`make all` builds desktop, CLI and the selected firmware. `make clean` removes
build outputs while preserving downloaded dependencies and toolchains.

## USB gamepad builds

Pico W, Pico 2 W, Pimoroni Pico Plus 2 W and Waveshare RP2350B-Plus-W enable the
`cordial-usb/gamepad` feature. These builds advertise
Disabled, HID and XInput gamepad output modes. USB gamepad slot limits are separate from the
board's Bluetooth transport allowances.

The largest USB allocation needs 12 interfaces and 10 handlers. The firmware
build script sets `EMBASSY_USB_MAX_INTERFACE_COUNT=12` and
`EMBASSY_USB_MAX_HANDLER_COUNT=10` for Cargo. Workspace checks run from `rust/`
and read the same values from `rust/.cargo/config.toml`. Cargo does not discover
that file when invoked from the repository root with only `--manifest-path`.
For a targeted check from the root, pass it explicitly:

```sh
cargo test --manifest-path rust/Cargo.toml --config rust/.cargo/config.toml -p cordial-usb --all-features
cargo clippy --manifest-path rust/Cargo.toml --config rust/.cargo/config.toml -p cordial-usb --all-targets --all-features -- -D warnings
```

USB fixtures exercise all 25 HID/XInput pool combinations through the actual
composite builder, including the VIA/Vial descriptor variants. The descriptor
buffers hold 512 configuration bytes, 64 BOS bytes, 64 Microsoft OS descriptor
bytes and 256 control bytes. The maximum configuration uses 455 bytes, and the
receiver's Microsoft OS descriptor uses 46 bytes.

Windows driver binding uses the receiver function's `XUSB20` compatible ID and
does not require a Cordial service.
