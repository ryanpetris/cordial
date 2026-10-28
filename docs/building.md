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

Run commands from the repository root on Linux. Builds run in Docker.
Install [just 1.59.0](https://github.com/casey/just/releases/tag/1.59.0), Git and [Docker](https://docs.docker.com/engine/install/)
with your user allowed to run it. `just DOCKER=podman host` uses Podman instead.
The host and distribution packaging images and CI install the pinned just release with `docker/install-just.sh`,
which verifies the official archive's SHA-256. To install it in a local directory,
run `sh docker/install-just.sh .cache/just/bin` and add that directory to `PATH`.

Configuration values use environment variables or assignments before the recipe names,
such as `just BOARD=pico2_w PROFILE=production firmware`. Command-line assignments
win over the environment. `PYTHON` defaults to `python3`, `BUF` to `buf`, and
`DOCKER` to `docker`. Root recipes accept command prefixes such as
`PYTHON='python3 -B'`. Native packaging recipes use `PYTHON` as one executable path.
The internal `BOARDS`, `ESP_BOARDS`, `FIRMWARE_PLATFORM`, and `DEB_BASE` variables,
including `DEB_BASE_trixie` and `DEB_BASE_resolute`, accept command-line assignments only.
`just` and `just help` list the recipes and current firmware options.

```sh
just host                   # one executable for desktop, CLI and TUI
just BOARD=pico_w firmware
```

To build and run locally with Cargo's development profile, pass arguments after `cordial`:

```sh
just cordial desktop
just cordial --help
```

`just cordial` without arguments opens the interactive shell. It runs Cargo from `rust/`
with the pinned toolchain and workspace configuration, then launches the built executable
from the directory where you invoked `just`. Relative file paths use that directory,
including when you run `just cordial` from a subdirectory. The recipe uses the executable
path reported by Cargo. The default is `rust/target/debug/cordial`; Cargo configuration
and environment settings such as `CARGO_TARGET_DIR` can change the output directory.
Application arguments keep their original boundaries, including spaces, empty values and
`NAME=value` strings. Put recipe configuration before `cordial`.
The recipe compiles the desktop preference schema into `share/glib-2.0/schemas`
beside that executable without installing it on the system. Direct launches and
Start at Login use that schema without `GSETTINGS_SCHEMA_DIR`.

To capture protocol JSON without build progress, build first and redirect only the executable:

```sh
just cordial --version
rust/target/debug/cordial --json desktop 2>cordial.jsonl
rust/target/debug/cordial --json tui 2>cordial.jsonl
```

`just host`, `just firmware` and the `package-host-*` recipes run in toolchain images
from `docker/`, created on first use and reused afterwards.
The build copies the checkout into the image, leaving out the
paths in `.dockerignore` (build outputs, caches, installed dependencies and Git
data), builds there, and copies only its outputs back to the same places a local
build would put them, replacing the earlier copy of each output. Docker BuildKit
is required.

BuildKit cache mounts retain Cargo downloads, Rust compilation outputs, pinned
firmware dependencies and tools that use `XDG_CACHE_HOME`. Packaged outputs are
not cached. Firmware keeps its board configuration and profile hash directories
under `target/firmware/`; host target triples remain separate under `rust/target/`.

Caches are shared by checkouts using the same Docker builder and toolchain runtime.
The cache key hashes the image's filesystem layers, runtime configuration, OS,
architecture and variant. Image export metadata, such as provenance, can change
the image ID without changing this key. Changes to native libraries, compilers,
runtime configuration or platform get separate caches. Firmware compilation and
dependency caches also include the SHA-256 of `rust/tools/firmware_dependencies.py`,
so different dependency pins never switch a shared SDK checkout. Cargo handles
Rust dependency and compiler-option changes.
Copied input files retain timestamps by content, so Cargo detects source changes
even when another checkout supplies older timestamps. Build commands and artifact
validation still run when the Docker result layer is not reusable.

Builds using the same toolchain runtime serialize access to their cache mounts.
This protects Cargo outputs, generated configuration and mutable SDK checkouts
across concurrent builds. Different toolchain runtimes can build in parallel.
Each recipe has an export scratch directory under `.cache/docker/`;
concurrent invocations of the same recipe must use separate checkouts. Toolchain
images also receive `cordial-toolchain:<runtime-hash>` tags. Each tag retains the
first image with matching runtime contents, so provenance changes and rebuilds
of the regular image tag preserve the selected environment. An existing runtime
tag with different contents is replaced.

`just clean` removes exported outputs but leaves BuildKit caches intact. To clear
unused cache mounts for the active Docker builder, run
`docker builder prune --filter type=exec.cachemount`. This affects all projects
using that builder, so coordinate cleanup with other builds. Docker may also
reclaim cache mounts during normal garbage collection; a missing cache triggers
a rebuild. Pruning also recovers dependency caches left incomplete by an
interrupted checkout. Cache mounts can use several gigabytes per toolchain and
firmware configuration. Content-tagged toolchain images remain until their tags
are removed with `docker image rm cordial-toolchain:<runtime-hash>`.

`just host` exports `rust/target/x86_64-unknown-linux-gnu/release/cordial`.
`HOST_TARGET` defaults to `x86_64-unknown-linux-gnu`, the release architecture,
and applies to host release builds and packages. The host image uses Ubuntu 26.04
Resolute with glibc 2.43, GLib 2.88, GTK 4.22 and libadwaita 1.9. It installs the current
distribution packages, including updates and security fixes.

### Building without Docker

`just cordial` builds and runs the native desktop, CLI and TUI for development.
`just local-host` builds the same executable locally with Cargo's release profile and
the selected `HOST_TARGET`. Install Python 3.11.4+, Rust through
[rustup](https://rustup.rs/) and native build tools. Cargo uses the pinned
toolchain in `rust/rust-toolchain.toml`;
the rustup `cargo` and `rustc` commands must be on `PATH`. On Debian and Ubuntu:

```sh
sudo apt-get install build-essential git clang libclang-dev pkg-config python3 libgtk-4-dev libadwaita-1-dev libglib2.0-bin
```

On Arch, install `base-devel`, `git`, `clang`, `pkgconf`, `python`, `gtk4` and
`libadwaita`.
The software checks below use the same local tools.

The native GTK application requires GTK 4.14 or newer and libadwaita 1.5 or newer.
Run the desktop with `just cordial desktop`, or run the local release build with
`rust/target/x86_64-unknown-linux-gnu/release/cordial desktop`.
Direct Cargo runs need the schema setup described under Desktop preferences.
Direct desktop, CLI and TUI Cargo builds do not require Python. The just recipes, version tools
and packaging workflow still require Python. Firmware builds also require Python for board
configuration and BTstack's ATT database compiler.
The desktop, CLI and TUI share the Rust controller and run protocol compatibility and storage
migration in-process. They do not launch another `cordial` process.

A custom headless build can omit GTK and libadwaita:

```sh
cd rust
cargo build --locked --release -p cordial --bin cordial --no-default-features
```

This custom executable provides CLI and TUI commands and is not the release package payload.

### Firmware

```sh
just BOARD=pico_w firmware
just BOARD=esp32s3 PROFILE=production firmware
just firmware-all
```

The Pico image has Rust 1.99.1 and ARM GCC; the ESP32-S3 image adds Espressif's
Rust 1.97.0.0, espup and ldproxy to its ESP-IDF v6.1 image. Builds fetch the pinned
Pico SDK and BTstack sources into the dependency cache as needed.

`BOARD` defaults to `pico_w`. `PROFILE` is `production`, `debug` or `development`
and defaults to `development`, independently of compiler release optimization.
`PROFILE=production` excludes development commands, including remote bootloader
entry and filesystem inspection. `debug` and `development` include them and run
the same code; only their version and embedded profile name differ. No recipe installs firmware.

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
for reservations and recovery.

### Versioning and release packages

First-party manifest versions are `0.0.0`. Builds use `CORDIAL_VERSION` when
supplied and otherwise use `0.0.0`; firmware adds its profile suffix. The value is
`MAJOR.MINOR.PATCH`, without leading zeroes or a `v` prefix. Tracked manifests
and native package recipes remain unchanged.

The serial protocol revision is independent of `CORDIAL_VERSION` and the storage
format version. The current serial revision is `1`. Clients require its
handshake, which older firmware lacks, so the next release must increment the minor version; development builds and tracked
manifest templates remain `0.0.0`. See [protocol compatibility](protocol/compatibility.md)
for revision matching and schema fingerprints.

```sh
export CORDIAL_VERSION=1.2.3
just BOARD=pico_w package-host-appimage package-host-tar firmware
```

The tag-triggered release workflow reads `GITHUB_REF_NAME`, validates
`vMAJOR.MINOR.PATCH`, and runs `just check-protocol` before starting build jobs.
It supplies the numeric version and tagged commit timestamp to every build job.

### Portable archives

`just package-host-tar` builds the GNU/Linux native executable with documentation, protocol
definitions and dependency notices in `build/release/cordial-<version>-linux-amd64.tar.gz`.
`just package-host-appimage` builds this archive and an AppImage at
`build/release/cordial-<version>-x86_64.AppImage`. The AppImage bundles GTK, libadwaita,
icons and their redistribution notices. Both formats use the Resolute glibc 2.43 baseline.
The AppImage bundles GTK, libadwaita, text libraries and the dconf GIO module with its dependencies
and redistribution notices. It loads GIO modules only from the bundle and uses the host's
session D-Bus and dconf service. The compiler runtime, window-system
libraries and graphics drivers, including Vulkan, come from the host.

`just HOST_ARCHIVE=path/to/cordial-1.2.3-linux-amd64.tar.gz package-host-appimage` repacks an
existing unified archive. `just local-package-host-tar` and `just local-package-host-appimage`
use local build tools instead of Docker. Local AppImage packaging needs the Resolute library
and package metadata environment supplied by `docker/host.Dockerfile`.

Firmware packages remain separate from host application packages.

### Native distribution packages

Supported native targets are Arch Linux x86-64, Debian 13 and Ubuntu 26.04.
Arch and Ubuntu packages use the Resolute host archive. Debian 13 builds its own executable
and archive inside a Debian 13 image using the same host Dockerfile with `BASE=debian:13`.
This keeps its glibc and GTK dependencies compatible with Debian 13. Each package owns
`/usr/bin/cordial`, the `dev.petris.cordial` desktop entry and icons, the GSettings schema,
`/usr/share/doc/cordial`, dependency notices, the license and
`/usr/lib/udev/rules.d/50-cordial.rules`. The desktop entry runs `cordial desktop`.
Distribution triggers maintain the GSettings schema cache.

```sh
CORDIAL_VERSION=1.2.3 just package-host-arch
CORDIAL_VERSION=1.2.3 just DEB_DISTRIBUTION=resolute package-host-deb
```

Supply an existing Resolute archive to Arch or Ubuntu packaging with `HOST_ARCHIVE`:

```sh
CORDIAL_VERSION=1.2.3 just DEB_DISTRIBUTION=resolute \
  HOST_ARCHIVE=build/release/cordial-1.2.3-linux-amd64.tar.gz package-host-deb
```

Archive versions, architectures, permissions and extraction boundaries are checked before
packaging. The Arch recipe in `packaging/arch/` exports
`build/packages/host-arch/cordial-<version>-1-x86_64.pkg.tar.zst`. Debian recipes in
`packaging/debian/` export
`build/packages/host-deb/<distribution>/cordial_<version>-1~<distribution>_amd64.deb`.
`DEB_DISTRIBUTION` is `trixie` or `resolute` and defaults to `trixie`.
Trixie packaging rejects `HOST_ARCHIVE` and builds against the current Debian 13 libraries.
`DEB_REVISION` defaults to `1~<distribution>`. Debian's library tools derive native dependency
versions on each distribution.

`CORDIAL_HOMEPAGE` overrides the native package homepage. `SOURCE_DATE_EPOCH` supplies the release
timestamp in Unix seconds. Both accept environment variables or assignments before recipe names.
Native repackaging otherwise uses the archive's `VERSION` timestamp.
Supplied archives and tracked recipes remain unchanged.

To repackage locally with native distribution tools, run
`just --justfile packaging/justfile arch` or `just --justfile packaging/justfile deb`
from the repository root. These recipes validate the archive before invoking `makepkg`
or `dpkg-buildpackage`. The local Debian recipe defaults to the running distribution's
codename. Set `DEB_DISTRIBUTION` and `DEB_REVISION` before the recipe to override it.

The release workflow validates one tag and supplies the same version to all builds. It checks
clean installation, reinstallation, package file ownership, native dependencies, schema registration,
removal and Debian purge in disposable distribution containers, including Namcap and Lintian checks.
It checks the actual AppImage's desktop launch.
Release assets include the unified host archive, AppImage, one native package
for each distribution, production and debug firmware archives for every board, and checksums.
Each firmware archive holds its firmware version's package directory.

Release builds reuse Docker toolchain images and BuildKit caches within each job.
Each firmware job reuses preparation between checking and packaging its board and
profile. Production and debug profiles run in parallel. These caches last only for
that job and are not transferred to other runners or later releases.
Arch and Ubuntu packaging jobs reuse the portable archive through workflow artifacts.
The Debian job compiles its executable in its distribution image.

Third-party license material is retained in `notices/`. Firmware and native host packages
include the project license, generated dependency inventories and license notices.

## Software checks

The release workflow's `validate` job checks the tag and protocol schemas. Run the
full software suite and the GTK checks below locally.

```sh
just check
```

`just check` runs Rust tests and Clippy, native Bluetooth regression checks,
Python tool tests and the protocol checks:
`buf lint` and `buf breaking`. The permanent handshake and serial revisions present in the current
tree compare against every local release tag, regardless of application version or branch ancestry,
except the release identified by `CORDIAL_VERSION`. Revisions released by ancestors must remain
present; revisions released only on other branches need not be added. `PROTOCOL_STABLE_BASE`
overrides these comparisons with one Git revision. Storage and migration IPC use the newest earlier
ancestor release in the same series as `CORDIAL_VERSION`; `PROTOCOL_BASE` overrides it. A series is
one minor version before 1.0 and one major version from 1.0. A series' first release skips only the
storage and migration IPC comparisons. Development builds use the newest ancestor release for
these comparisons. Selection uses local tags without fetching. Rust code is generated from
`proto/` at build time, including the operational schema fingerprint.
Install Clippy with `(cd rust && rustup component add clippy)` before checking.
Preference and host packaging fixtures also require `glib-compile-schemas`, supplied by
`libglib2.0-bin` on Debian and Ubuntu or `glib2` on Arch.

Protocol checks require the native Buf 1.75.0 executable from the
[official release](https://github.com/bufbuild/buf/releases/tag/v1.75.0) on `PATH`.
The release workflow installs and verifies the Linux x86-64 binary. `BUF` can
select another executable for `just check-protocol` and the protocol fixtures.
The lint and breaking rules live in `buf.yaml`.

To run only the protocol checks, use `just check-protocol`. It requires just,
Python and Git in addition to Buf, and does not compile Rust or native code.

Run the GTK control checks with a display:

```sh
(cd rust && xvfb-run -a dbus-run-session -- cargo test --locked -p cordial --all-features --lib \
  desktop::settings::tests::integer_rows_commit_accepted_numbers -- --ignored --exact)
(cd rust && xvfb-run -a dbus-run-session -- cargo test --locked -p cordial --all-features --lib \
  desktop::settings::tests::integer_rows_take_only_numbers -- --ignored --exact)
(cd rust && xvfb-run -a dbus-run-session -- cargo test --locked -p cordial --all-features --lib \
  desktop::settings::tests::unknown_switches_offer_off_and_on -- --ignored --exact)
(cd rust && xvfb-run -a dbus-run-session -- cargo test --locked -p cordial --all-features --lib \
  desktop::settings::tests::unknown_choices_show_unknown_without_listing_it -- --ignored --exact)
(cd rust && xvfb-run -a dbus-run-session -- cargo test --locked -p cordial --all-features --lib \
  desktop::settings::tests::every_saved_state_has_an_icon -- --ignored --exact)
(cd rust && xvfb-run -a dbus-run-session -- cargo test --locked -p cordial --all-features --lib \
  desktop::settings::tests::free_spin_threshold_follows_its_switch_while_on -- --ignored --exact)
(cd rust && xvfb-run -a dbus-run-session -- cargo test --locked -p cordial --all-features --lib \
  desktop::settings::tests::switch_and_number_rows_line_up_in_banded_pairs -- --ignored --exact)
(cd rust && xvfb-run -a dbus-run-session -- cargo test --locked -p cordial --all-features --lib \
  desktop::settings::tests::automatic_power_off_turns_on_with_its_last_timeout -- --ignored --exact)
(cd rust && xvfb-run -a dbus-run-session -- cargo test --locked -p cordial --all-features --lib \
  desktop::window::tests::search_opens_the_expander_row_holding_its_result -- --ignored --exact)
(cd rust && xvfb-run -a dbus-run-session -- cargo test --locked -p cordial --all-features --lib \
  desktop::add::tests::pairing_centers_every_step_at_one_size -- --ignored --exact)
(cd rust && xvfb-run -a dbus-run-session -- cargo test --locked -p cordial --all-features --lib \
  desktop::add::tests::code_entry_takes_passkey_digits_and_pin_text -- --ignored --exact)
(cd rust && xvfb-run -a dbus-run-session -- cargo test --locked -p cordial --all-features --lib \
  desktop::dialogs::tests::a_replaced_dialog_cancels_nothing -- --ignored --exact)
(cd rust && xvfb-run -a dbus-run-session -- cargo test --locked -p cordial --all-features --lib \
  desktop::profiles::tests::cleaned_empty_profile_names_use_id_fallbacks -- --ignored --exact)
(cd rust && xvfb-run -a dbus-run-session -- cargo test --locked -p cordial --all-features --lib \
  desktop::adapter::tests::platform_banner_accessibility_follows_revelation -- --ignored --exact)
(cd rust && xvfb-run -a dbus-run-session -- cargo test --locked -p cordial --all-features --lib \
  desktop::dialogs::tests::a_changed_deletion_is_answered_once_its_changes_are_painted -- --ignored --exact)
(cd rust && xvfb-run -a dbus-run-session -- cargo test --locked -p cordial --all-features --lib \
  desktop::dialogs::tests::a_deletion_is_wider_than_other_confirmations -- --ignored --exact)
(cd rust && xvfb-run -a dbus-run-session -- cargo test --locked -p cordial --all-features --lib \
  desktop::dialogs::tests::a_deletion_marks_its_changes_in_place -- --ignored --exact)
(cd rust && xvfb-run -a dbus-run-session -- cargo test --locked -p cordial --all-features --lib \
  desktop::dialogs::tests::a_change_found_while_a_deletion_ran_is_answered_once_painted -- --ignored --exact)
(cd rust && xvfb-run -a dbus-run-session -- cargo test --locked -p cordial --all-features --lib \
  desktop::services::prefs::tests::dialog_rolls_back_controls_when_gsettings_rejects_saves -- --ignored --exact)
(cd rust && xvfb-run -a dbus-run-session -- cargo test --locked -p cordial --all-features --lib \
  desktop::services::prefs::tests::low_battery_level_takes_typed_levels_in_range -- --ignored --exact)
```

Each command runs one GTK test in its own process. The preference rollback test constructs a
Settings page in a window and checks all five controls against a read-only GSettings backend.
The normal Rust preference tests cover external updates
and persistence. The low battery input test covers typed levels from 1 through 100, including 3
and 60, and rejects invalid text and out-of-range values. Its percent unit is outside the entry.

Install `xvfb`, `xauth` and `dbus-x11` for the display checks. Linux terminal fixtures need `pyte`,
Python `protobuf`, `protoc`, a C compiler and an actual release `cordial` executable. Debian dependency-version tests need `dpkg`; run them
in a Debian or Ubuntu container when it is unavailable on the host. A missing prerequisite or skipped fixture does not
complete validation.

The build-cache fixtures check the local Cargo runner and its arguments, source timestamp
reuse, cache boundaries and clean exports for native host and firmware builds.

Linux CLI terminal checks use a simulated adapter over a PTY. They verify terminal restoration,
interactive shell and TUI operation with JSON on redirected stderr, and complete file responses
alongside ordinary CLI output. They check original wire errors in JSON, deferred terminal
diagnostics, and shutdown with unread stderr.
They require `pyte`, Python `protobuf`, `protoc`, a C compiler
and a built CLI:

```sh
CORDIAL_TEST_BINARY="$PWD/rust/target/x86_64-unknown-linux-gnu/release/cordial" python3 -m unittest discover -s rust/tests -p test_host_terminal.py
```

Clippy for the firmware platform crates runs in the firmware images, for one board and
profile or for all of them:

```sh
just BOARD=esp32s3 PROFILE=production check-firmware
just check-firmware-all
```

To exercise the desktop with simulated adapters:

```sh
CORDIAL_DESKTOP_SIMULATE=2 rust/target/x86_64-unknown-linux-gnu/release/cordial desktop
```

Each simulated adapter starts with a connected mouse and keyboard that report a battery, a
connected Example Controller that reports none, and a disconnected Travel Keyboard.

### Desktop preferences

The Linux desktop stores preferences in the `dev.petris.cordial` GSettings schema at
`/dev/petris/cordial/`. Reset individual preference keys to restore their defaults.
Open Settings from the sidebar, Ctrl+, or the tray menu. Settings occupies the regular content
pane and saves each change immediately. Its controls follow external GSettings changes.

Native packages install the schema XML with the common `cordial` package. Distribution GLib
triggers compile the shared schema cache. Portable archives include XML and a compiled cache
under `share/glib-2.0/schemas`, next to the executable. AppImages include the same schema under
`usr/share/glib-2.0/schemas`.

`just cordial desktop` compiles the schema beside Cargo's reported development executable.
`just local-host` prepares the schema beside the release executable. Both executables find
the schema there without `GSETTINGS_SCHEMA_DIR`, including when Start at Login launches them.
Before running the desktop directly through Cargo, use `just cordial --version` with the
same Cargo configuration to prepare its schema.

No schema is needed for CLI or TUI commands. The GTK smoke fixture uses GLib's persistent
keyfile backend under a private `XDG_CONFIG_HOME`. The AppImage fixture uses the default
settings backend, private XDG directories, a private dconf profile and a private session D-Bus.
It checks the bundled GIO module paths, reads preferences with the host's `gsettings` command
before and after quitting, and checks preferences and autostart across a restart. It also changes
and resets all five preferences with the host command while Settings stays open, checks the
controls and host readback, and checks that external Start at Login changes update autostart.
Both fixtures open the regular Settings pane through Ctrl+, and through its sidebar row.

### Completion checks

For host changes, run `just check`, the explicit GTK checks above, and the terminal fixture.
Build and validate the unified archive and AppImage:

```sh
just package-host-appimage package-host-tar
python3 tools/check_release_archive.py "build/release/cordial-$CORDIAL_VERSION-linux-amd64.tar.gz" host "$CORDIAL_VERSION"
```

Run `packaging/check-arch.sh` in a disposable Arch container after `just package-host-arch`.
Build `package-host-deb` and run `packaging/check-deb.sh` in a matching disposable container for
each of `trixie` and `resolute`. Pass `CORDIAL_VERSION` and `DEB_DISTRIBUTION` to the
Debian fixture. These fixtures install and remove packages, so use clean containers rather than
a development system. Preserve Namcap and Lintian checks.

In a disposable Resolute environment, install the matching `cordial` package, plus
`python3-pyatspi`, `python3-gi`, `gir1.2-adw-1`, `xvfb`, `xauth`, `dbus-x11`, `imagemagick`,
`adwaita-icon-theme` and `librsvg2-common`. The AppImage fixture also needs `libglib2.0-bin`,
`dconf-gsettings-backend` and `dconf-service` for independent settings readback. Run:

```sh
CORDIAL_TEST_BINARY=/usr/bin/cordial xvfb-run -a -s '-screen 0 1280x1024x24' \
  dbus-run-session -- python3 rust/tests/gtk_smoke.py build/package-smoke
xvfb-run -a -s '-screen 0 1280x1024x24' dbus-run-session -- python3 tools/check_appimage.py \
  "build/release/cordial-$CORDIAL_VERSION-x86_64.AppImage" build/appimage-smoke
```

The installed fixture checks immediate adapter settings, transport and configuration interface
changes, device connection switches, settings, Gamepad Output and profile layer changes. It keeps
numeric edits focused through pauses and redraws, checks that the pre-edit numeric value and write
count stay unchanged until commit, and checks persistence after committing an edit. The pre-edit
text and numeric value are recorded in `numeric-baseline.json`.
Numeric input uses native key events, checks that a typed letter is rejected, and finds the DPI
unit outside the entry. Low Battery Level is typed as 3 and 60, checked against isolated saved
preferences, and restored as 60 and then 3 across separate relaunches. Its percent unit is a
separate label.
Before each normal configuration edit it records the matching simulator completion count, then
waits for a new successful completion before Refresh or reconnect. Save and Discard are absent.
Each configuration interface has a card with its enable switch above a native Profile selection
row. The fixture selects VIA and Vial profiles by their accessible names and checks them after
reconnect. While VIA has no profile, and while Vial's switch waits for VIA, searching for the
interface focuses its Profile selector. The adapter library and Add Profile picker expose all
twelve seeded profiles without Next or Previous. The fixture scrolls to profiles 11 and 12,
adds profile 11 through the picker,
checks it after Refresh, and removes it before the three-layer drag checks.
Device profile layers move by dragging a row onto another row. A three-layer pointer
drag checks the final insertion order and exactly one completed write, then verifies the order
after Refresh and reconnect. `device-profiles-drag-pointer.png` captures the pressed pointer,
`device-profiles-drag-readback.png` captures the reconnected list, and
`device-profiles-drag.json` records the pointer coordinates, write result and both readbacks.
Each layer row has a drag handle and Remove. The fixture checks that the rows stay enabled and
keep their Remove buttons while verifying the layer order. A second pointer drag moves a layer up
and checks one completed write and the order after Refresh and reconnect in
`device-profiles-drag-up.json`.
Deleting an in-use profile checks that the numbered confirmation names the affected device and VIA,
keeps wrapped lines aligned with their text, and preserves its step rows through completion.
Once deletion starts, its introduction reads "Changes, in order:" through completion or failure.
The completed dialog is 420px wide at the normal window size, marks every step Done and stays open
with only a full-width Close button. After Close, the fixture verifies the remaining layer order,
disabled VIA and cleared interface profile after reconnect. `profile-delete-confirmation.png`,
`profile-delete-completed.png` and `profile-delete-readback.json` record these checks.
Turning off an active configuration interface may reconnect the adapter. Profile deletion
continues only after the same adapter returns, its current device and interface settings are
read, the listed interfaces are off with no assigned profile, and no device or interface uses
the profile. If another assignment now uses it, deletion stops and completed changes remain.
A connection lost before the final deletion is confirmed leaves its outcome unknown.
The GTK dialog tests cover the active spinner, completed checkmarks, failed and unconfirmed
markers, stable step placement, and confirmation of a changed remaining plan before retry.
The narrow layouts also check the Profile Memory meter and the bootloader confirmation's heading,
Cancel and Enter Bootloader buttons, then dismiss that confirmation with Escape.

Run the same GTK smoke command with `--empty-states` and a separate output directory to check
empty Profiles and sidebar groups. This flow removes the mouse's last profile, checks the large
icon, No Profiles title and centered Add Profile button at 980px and 480px. It deletes every profile
in that adapter's library, opens the empty Add Profile picker and checks No Profiles, New Profile
and disabled Choose. Creating a profile reopens the picker with that profile selected. Choosing it
adds the layer and persists after Refresh. It forgets every device on one adapter while the other
stays populated, then empties both adapters and checks their separate Add Device rows. Pairing through
each adapter's row verifies the target with the paired device's Adapter field. Screenshots,
accessibility trees and `empty-profiles-*-bounds.json` record the empty layouts.

The delayed flow checks rapid numeric edits while a write is pending, editable controls, retained
values and the absence of apply progress and success indicators. Closing the window lets writes
continue, and reopening retains the latest value. Explicit Quit commits the focused numeric edit,
shows a separate Cordial window with "Settings are being applied.", and exits after pending writes
drain. Process exit is the completion barrier for coalesced edits, which can produce one or two
jobs. Two fresh simulated sessions reject a settings write after Quit shows pending progress.
These two sessions use a 2000ms write delay to allow the pending window's accessibility tree to
be read before rejection. The quiet settings and profile drag flows use 800ms.
Continue Editing restores a usable main window with the same error and no replay, then a deliberate
control edit saves successfully and is read back after Refresh. Quit Anyway exits cleanly without
replaying the rejected write. Both flows record their outcomes in `failed-quit-*.json` and capture
pending and failed Quit windows. These GUI failures confirm that nothing was saved. Headless
engine tests check the exact persisted latest values and known, unknown and detached write outcomes;
unknown write outcomes are not injected by the GUI fixture.
With delayed profile writes, a second pointer drag restores the original order while the first
write is pending. The fixture checks that the rows stay editable and quiet, then reads the order
after Refresh. `device-profiles-delayed.json` records both drags and the completed writes.
Profile creation, copying and deletion, pairing and Forget Device retain their
confirmation actions. It also covers preferences, autostart and lifecycle. Settings opens in the
content pane with its sidebar row selected, without a modal dialog. Ctrl+N on Settings cannot
pair through the adapter or device page it replaced. Adapter and device
settings share their main Details page, without tab controls, and Profiles and Diagnostics open
from links near the bottom. Navigation waits for the selected sidebar row and the requested page's
visible header. The sidebar starts with Overview and Settings, then each adapter followed by its
devices, including repeated device names across adapters. Separators divide the groups without
Devices or Adapters heading rows. Battery icons expose their reading as accessible image names;
the exact charge and full-width battery bar appear on the device page. The smoke fixture checks
that a retained battery reading remains visible after Disconnect.

Sidebar unit tests cover connection styling. Connected devices and adapters use normal colors;
disconnected rows have subdued icons and names. Battery readings remain until the adapter reports
none, including while an idle device sends no new reports. An adapter waiting for an announced
restart dims without a spinner, while an ordinary connection attempt shows a spinner.
Overview lists adapters
first without an Adapters heading, followed by Needs Attention and Connected when populated.
Failure dumps include selected states and accessible action names.
The About dialog check opens Legal and checks its combined copyright and license text. It reads
the Website destination through Copy Link and the MIT License destination through the Legal
text's Hypertext interface without launching a browser. `about-main.png` and `about-legal.png`
capture both views.
Search coverage opens the sidebar header's Search button and uses Ctrl+F with the sidebar visible.
At narrow widths, it checks that Ctrl+F leaves the content page in place, then uses Back to search.
It checks case-insensitive words across result titles and contexts, opens app, adapter and device controls,
and checks their focus and scroll position without moving them from the fixture. Page results
cover Overview, Settings, adapters, devices, Profiles and Diagnostics, with entry Enter and
Down/Enter result activation. Clearing or replacing a matching query followed immediately by
Enter cannot open its previous result. A matching replacement opens its current first result.
Advertised VIA and Vial settings open Profiles and focus their closed profile selectors.
Information searches open the mouse's Battery and Ratchets per Rotation rows, the adapter's
Firmware row on Details, and the mouse's Device ID row on Diagnostics. Active Devices results
name Bluetooth Classic or Bluetooth LE in their context and focus the row in the matching
transport group. Each requires focus, full row visibility and no writes without scrolling from
the fixture. `search-information.png` captures the diagnostic result.
Profile searches open Typing in each adapter's library and its assigned layer on the second
adapter's keyboard. Example Profile 11 opens its library row at normal and narrow widths. These
checks require focus and full row visibility without scrolling from the fixture. They verify the
selected adapter or device in the visible sidebar. At narrow widths, they check the requested
page header on activation and sidebar selection on returning from content.
No-results shows "No Results Found" and "Try a different search". Escape restores navigation.
The same flow checks a device control, a late library profile and no-results at 480 pixels.
Simulator writes, saved app preferences and autostart stay unchanged throughout search.
`search-results.png`, `search-control-480.png`, `search-profile-480.png` and
`search-no-results*.png` capture these states.
Pairing opens through the Details header's Add Device button from both an adapter page and an
attached device's page, with two adapters present. Profiles and Diagnostics have no Add Device
button. After pairing, the fixture checks the new
device's Adapter row against the chosen adapter before forgetting it. The scan dialog has no
adapter selector or searching text, and its passive spinner is in the header. Scan screenshots
named `pairing-scan-*` capture the spinner's spacing for visual review. Enter-code, compare-code,
display-code cancellation and automatic pairing remain covered. The fixture reacquires a distinct
native dialog when moving between the device picker and pairing, including Search Again and
Add Another. Prompt and result screenshots have matching `*-bounds.json` files. The fixture
waits for every answer to fit inside the pairing dialog, which is 420 by 372 pixels and narrows
to preserve the native sheet's 30-pixel side margins. It checks full-width buttons
with equal widths for two choices, and checks that its outer size and answer placement stay
unchanged between a prompt and its result.
The ignored `pairing_centers_every_step_at_one_size` GTK test also covers progress, paired results,
PIN, passkey, long device names and scrolling long errors at normal and narrow window widths.
Pairing hides the scan header's title and uses a centered Code entry.
Passkey entry uses native key events and checks that a typed letter is rejected.
The fixture keeps each successful result open for more than three seconds,
captures its layout, and closes it with Dismiss to open the new device's page. Automatic pairing
uses Add Another to return to a new device picker. The observed times and chosen actions are
recorded in `*-dismissal.json`. Rejecting passkey entry returns to the device picker without
Pairing Failed, and the fixture starts another pairing from that picker. Rejecting a code
comparison shows a failure that stays open until Close or Search Again. Cancel Pairing closes
the dialog within one second, without a cancellation screen or later pairing failure.
The display-code and cancellation flow
runs inside a 480-pixel window.

Run the pairing reconnect fixture separately in the same disposable environment:

```sh
CORDIAL_TEST_BINARY=/usr/bin/cordial xvfb-run -a -s '-screen 0 1280x1024x24' \
  dbus-run-session -- python3 rust/tests/gtk_smoke.py build/pairing-reconnect-smoke --pairing-reconnect
```

It pairs through the existing passkey and code comparison controls. Each prompt stays open longer
than three seconds before the fixture answers it. After `PairingDone`, the simulated adapter sends
`SessionEnding` with `DEVICE_CONNECTION`, closes its framed stream, leaves discovery for 1500
milliseconds and returns with the same adapter ID. The fixture checks the dialog, adapter and new
device in the native accessibility tree during that gap and checks that a different session opens.
It retains the same native pairing dialog throughout the reconnect and checks that the prompt
and result have the same outer size and answer placement.
Both Paired results stay open for more than three seconds until the fixture presses Dismiss,
which opens the new device's page. The code comparison flow also checks persistence after keyboard
interaction. Screenshots, accessibility dumps, dismissal measurements and `pairing-reconnect.txt`
capture both flows.

`CORDIAL_SIM_PAIRING_RECONNECT_MS` enables this behavior only in desktop simulation, for the first
adapter and after successful pairing. Positive integers are clamped to `100` through `10000`
milliseconds; zero, missing or invalid values leave it disabled. The normal fixture leaves it
disabled. `CORDIAL_SIM_PAIRING_RECONNECT_TRACE` names a private trace file. With the switch enabled,
the simulator appends lines in this format:

```text
<opened|done|closed|present> <adapter-id> <session> <device-id>
```

`done` follows the encoded `PairingDone`. `closed` follows the final framed `SessionEnding` and
closure of the adapter's stream writer, which delivers EOF to the client. `present` follows the
port's return to discovery. `opened` follows construction of a new framed connection, before its
handshake. The fixture checks the native interface as well as these transport barriers. The trace
adds no production interface controls or indicators.

The fixture checks the visible header, device menus, settings rows, capacity rows, page menu
items, profile actions and the Settings pane at 480, 661 and 700 pixels. Settings Back returns to
the sidebar at 480 pixels. It scrolls rows into view through
the enclosing GTK scrollbar's accessible Value interface before checking bounds or interacting.
Header and control bounds use the accessible window's actual origin. The fixture logs raw,
window and relative coordinates, and rejects negative relative bounds and window overflow.
Setting markers are found by names such as "Pointer Sensitivity: Saved"; their visual states use
symbolic icons with menu arrows and tooltips, without visible status words. The fixture checks that
each marker follows its setting's control on the right.

The focused GTK control tests cover Automatic Free Spin's threshold and Automatic Power-Off's
timeout in sibling rows below their switches, shown only while the switch is on. A continuous
3-pixel colored band on the left joins each pair. Bands follow build order through blue, purple,
teal, orange, green and pink, then repeat. The numeric row reserves the saved-state menu's width
so its controls align with the switch. Numbers have separate unit labels and show empty text
while unknown. Choice menus list supported values without an Unknown option.
Readings without saved-state menus occupy a separate
list within their category. Shared formatting tests cover units in numeric choices and device-value notes.
A device's header shows its status without repeating its battery reading, and its
page has no Status row. Device Menu contains Refresh followed by Connect or Disconnect on Details
and Diagnostics; Profiles has no Device Menu. The fixture opens each menu at all three widths,
checks that its items fit, and exercises Connect, Disconnect and Refresh through it. The Battery
row retains its exact percentage and full-width meter. Adapter Menu offers Rename followed by
Connect or Disconnect; the adapter's Details page has no Status row. Maintenance actions open
from Adapter Menu on Diagnostics. New Profile sits beside the scrolling profile list.
The last seeded library rows and profile 11 in the picker fit at all three
widths. Layer rows and Remove fit at all three widths.
Forget Device is at the end of the device's scrolling page.
The seeded Active Devices rows must each contain a capacity bar. Its bounds must span the text
line below the name and usage, relative to the row rather than a fixed pixel width.
The AppImage fixture checks its packaged desktop launch with two simulated adapters using
extraction when FUSE is unavailable. Both write screenshots and logs.
The release workflow uploads AppImage evidence on failure.

Native simulation accepts `CORDIAL_SIM_WRITE_DELAY_MS` as an integer from `0` through `2000`.
The GTK fixture uses `0` for the normal flow, `800` for delayed settings and profile changes,
and `2000` for failed Quit.
`CORDIAL_SIM_FAIL_SETTINGS_ONCE=1` rejects the first settings write on each simulated adapter with
a storage failure that confirms no effect. Other values leave failure injection off. The failure
uses the configured write delay, leaves saved settings unchanged, and is consumed once per adapter
for that simulated session. It does not open hardware or retry a rejected write. The GTK fixture
enables it only for its two failed-Quit sessions, both with a `2000` millisecond delay.
`CORDIAL_SIM_WRITE_TRACE` names an absolute private file. After the engine handles a terminal
apply result and updates the controller view, it appends one UTF-8 line per actual job:

```text
adapter <adapter-id> <ok|error|unknown>
device <adapter-id> <device-id> <details|profiles|settings> <ok|error|unknown>
settings <adapter-id> <device-id> <ok|error|unknown>
```

Staging, coalescing and unchanged values add no records. A session drop records an unknown result
once, including when the job later returns. The fixture matches only seeded simulated identifiers,
`SIMULATED0000001` and `SIMULATED0000002`, and copies the trace to `writes.txt` with its other artifacts.
The trace supplies fixture completion barriers without adding indicators to the interface.
An `ok` record means the adapter confirmed the write. The fixture reconnects for independent
readback where needed.

Run the AppImage fixture in a disposable Arch environment as well. Its prerequisites are
`libgcc`, `libx11`, `libxcursor`, `libxdamage`, `libxext`, `libxfixes`, `libxi`, `libxinerama`,
`libxrandr`, `libxrender`, `libxcb`, `wayland`, `libglvnd`, `vulkan-icd-loader`, `expat`, `e2fsprogs`, `gmp`,
`libgpg-error`, `zlib`, `python-atspi`, `python-gobject`, `xorg-server-xvfb`, `xorg-xauth`, `dbus`,
`imagemagick`, `glib2` and `dconf`. Use the same command with `--renderer cairo`. The release
workflow runs Arch with Cairo and Resolute with software Mesa and GTK's default renderer.
All AppImage runs use the default settings backend.

Before completing a code change, also run Pico 2 W firmware Clippy and artifact builds for both
`production` and `debug`. Run the same checks for any other board directly affected. Reuse passing
results while their inputs remain unchanged. When assembling a release, verify every asset and
checksum listed in `.github/workflows/release.yml`.

## Adapter names

Adapter names are stored on the adapter. Rename through adapter settings in the TUI or desktop app, or run `adapter set name "Desk"` in the shell. Each board configuration supplies `default_adapter_name`, baked into the firmware image and used until a custom name is saved. Use **Reset to Default** in either rename dialog, or `adapter reset name` in the shell, to clear the custom name.

## Repository layout

- `rust/`: firmware, native GTK desktop, CLI/TUI, shared crates, adapters and their tools/tests.
- `docs/`: control protocol, storage format and HID++ references.
- `proto/`: serial protocol messages and persistent storage records.
- `docker/`: build images for the applications, firmware and distribution packages.
- `packaging/`: the distribution packaging steps and their installation checks.
- `tools/`: version validation, package checks and their tests.

`just all` builds the unified host and the selected firmware. `just clean` removes
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
