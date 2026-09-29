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

Run commands from the repository root on Arch Linux or Debian, with Python
3.11.4+ available:

```sh
make all
```

Each target runs its ecosystem's bootstrap automatically. `desktop/bootstrap.py`
prepares Node/npm and installs the locked npm dependencies, including build-time
dependencies. `rust/bootstrap.py` uses rustup and the pinned Rust toolchain,
and prepares the toolchain for the selected board. An installed rustup takes
precedence over system Rust. If missing, rustup is installed locally without
changing the default toolchain or shell profile.

Missing system prerequisites stop the build with package names for Arch or
Debian, or the required manual download. The bootstraps never run a system
package manager. Automatic tool downloads support Linux x86-64 and ARM64;
compatible installed Node and ARM compilers are reused. Project-managed tools and
sources live in ignored `.tools/`, `.cache/` and `target/` directories. An existing
rustup keeps its toolchains in its own home. Archives fetched directly by the
bootstraps have pinned versions and SHA-256 checksums; SDK installers manage
their own downloads. Repeated builds reuse completed installations;
changes to the npm manifests or Node/npm version trigger `npm ci` again.
Parallel Make targets serialize builds within each ecosystem. Running the desktop
application releases the build lock after startup preparation.

```sh
make desktop
make cli
```

The CLI executable is `rust/target/release/cordial`. The desktop build is in
`desktop/out/`. `python3 desktop/bootstrap.py start` starts the desktop with its
prepared tool environment.

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
python3 desktop/bootstrap.py serve              # adapters on this machine
python3 desktop/bootstrap.py serve --simulate   # two simulated adapters
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

Pico builds automatically prepare ARM GCC with newlib/nano and the selected
Rust target. ESP32-S3 builds prepare Espressif's Xtensa Rust, ldproxy, ESP-IDF
and its C tools and Python environment. No manual environment sourcing is
required. CMake 3.24+, Ninja and native build tools are system prerequisites.
Pico builds and Rust checks also require Clang and libclang.
Firmware source dependencies remain pinned and unmodified.

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

First-party manifest versions are `0.0.0`. `tools/version.py` derives the
application version from Git:

| Source state | Application version |
| --- | --- |
| Clean commit with one exact `vMAJOR.MINOR.PATCH` tag | `MAJOR.MINOR.PATCH` |
| Other commit | `0.0.0-dev+g<commit>` |
| Commit with local changes | `0.0.0-dev+g<commit>.dirty` |
| No Git metadata or executable | `0.0.0-dev` |

Multiple release tags on the same commit are rejected. Direct Cargo and npm
builds use the same resolver. Set `CORDIAL_PYTHON` to select a Python executable;
otherwise the version launchers try `python3`, `python`, then `py -3`.

```sh
make version
make package-cli
make package-desktop
make package-web
make package-firmware BOARD=pico_w
```

Release packaging requires a clean, exactly tagged commit and leaves tracked
manifests unchanged. CLI packages go to `build/release/`; desktop packages go to
`desktop/dist/`. The web version goes to `build/packages/web/cordial-web-<version>/`,
a directory to upload as is to a static host such as GitHub Pages or Cloudflare
Pages; it is not part of the desktop packages. The independent distribution packages are `cordial-desktop`
and `cordial-cli`. CLI packages include protocol documentation, schemas and
dependency notices.
Firmware is distributed separately.

```sh
CORDIAL_HOMEPAGE=https://github.com/OWNER/REPOSITORY python3 desktop/bootstrap.py dist -- --arch --deb
make package-cli-arch
make package-cli-deb
```

The Debian package also supports Ubuntu. Distribution packages target x86-64.
Desktop distribution packaging requires the project homepage in `CORDIAL_HOMEPAGE`.
Electron Builder produces the desktop Arch and Debian packages, AppImage and tar
archive. Arch packaging requires `bsdtar` from `libarchive-tools` on Debian/Ubuntu
or `libarchive` on Arch. CLI packages go to `build/packages/arch/` and
`build/packages/deb/`; their builders require `makepkg` plus `fakeroot` on Arch, and
`build-essential`, `dpkg-dev` and `debhelper` on Debian/Ubuntu. The native recipes live in
`rust/packaging/arch/` and `rust/packaging/debian/`.

Both Make targets build the CLI release files before packaging. To package an
existing release archive, pass `CLI_ARCHIVE=path/to/cordial-VERSION-linux-amd64.tar.gz`.
Packaging runs under `build/packages/`; tracked recipes remain unchanged.
The release workflow packages the same static CLI binary in every format.
`python3 desktop/bootstrap.py package:arch` and `package:deb` build individual
desktop package formats with the same homepage setting.
The CLI uses its existing static Rust build and has no Electron dependency.
Both applications use the repository root `LICENSE`.
The desktop application ID is `dev.petris.cordial`.

The GitHub release workflow validates `vMAJOR.MINOR.PATCH` tags and publishes
Arch and Debian packages for each application, an AppImage, desktop and CLI tar archives, and production
firmware archives for all four board presets. Tests, package installation checks
and artifact checksum verification must pass before publication.

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
python3 desktop/bootstrap.py simulate
```

## Adapter names

Adapter names are stored on the adapter. Rename through adapter settings in the TUI or desktop app, or run `adapter name set "Desk"` in the shell. Each board configuration supplies `default_adapter_name`, baked into the firmware image and used until a custom name is saved. Use **Reset to default** in either rename dialog, or `adapter name reset` in the shell, to clear the custom name.

## Repository layout

- `rust/`: firmware, CLI/TUI, shared crates, adapters, board definitions and their tools/tests.
- `desktop/`: Electron, React and TypeScript application, its web version and development server.
- `docs/`: control protocol, storage format and HID++ references.
- `schema/`: generated JSON wire schema and command catalog.
- `tools/`: shared version resolver and its checks.

`make all` builds desktop, CLI and the selected firmware. `make clean` removes
build outputs while preserving downloaded dependencies and toolchains.
