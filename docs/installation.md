# Installation

Release archives, native packages, firmware bundles, and a `SHA256SUMS` checksum file are published on the [latest release](https://github.com/ryanpetris/cordial/releases/latest) page.

## Desktop App and CLI

Cordial ships as a single native Rust executable (`cordial`) that provides the GTK4/libadwaita desktop app, full-screen TUI, interactive shell, and one-shot CLI. Native packages also install the desktop entry, application icons, GSettings schema, documentation, third-party notices, and udev rule. Protocol compatibility checks and storage migrations run in-process.

### Distribution Packages

Native packages target x86-64 systems and link against each distribution's GTK and libadwaita libraries:

- **Arch Linux**: built against the Ubuntu 26.04 baseline (glibc 2.43, GLib 2.88, GTK 4.22, libadwaita 1.9).
- **Ubuntu 26.04 (`resolute`)**: uses the `resolute` `.deb` package.
- **Debian 13 (`trixie`)**: compiled natively on Debian 13 with Debian 13 libraries (`trixie` `.deb` package).

Install on Arch Linux:

```sh
sudo pacman -U cordial-[0-9]*-1-x86_64.pkg.tar.zst
```

Install on Ubuntu 26.04 (substitute `trixie` for Debian 13):

```sh
sudo apt install ./cordial_[0-9]*-1~resolute_amd64.deb
```

To upgrade an existing installation, download the newer package and run the same command. Package upgrades preserve user preferences.

### Other Distributions

#### AppImage

`cordial-<version>-x86_64.AppImage` bundles GTK, libadwaita, text libraries, and the `dconf` GIO settings backend. It requires glibc 2.43 or newer, a session D-Bus, the host's `dconf` service (`dconf-service` on Debian/Ubuntu, `dconf` on Arch, or built into GNOME), and the host's window-system libraries and graphics drivers (EGL, OpenGL, and Vulkan).

Make the file executable and run it:

```sh
chmod +x cordial-<version>-x86_64.AppImage
./cordial-<version>-x86_64.AppImage
```

The AppImage launches the desktop app by default and also accepts CLI subcommands such as `tui` or `adapter list`. If FUSE is unavailable, set `APPIMAGE_EXTRACT_AND_RUN=1`.

#### Portable Tar Archive

`cordial-<version>-linux-amd64.tar.gz` contains the `cordial` executable (Ubuntu 26.04 baseline: glibc 2.43, GLib 2.88, GTK 4.22, libadwaita 1.9), compiled GSettings schemas under `share/`, documentation, notices, and `50-cordial.rules`.

Extract the archive and keep `share/` next to the `cordial` binary (even if invoking it through a symlink), install GTK4 and libadwaita from your distribution, and run `./cordial desktop`, `./cordial tui`, or any CLI command.

### Adapter Access

Arch and Debian/Ubuntu packages install `50-cordial.rules`, granting the active local desktop user access to the adapter's USB serial port and HID interfaces (used by VIA and Vial). If an adapter was plugged in during installation, unplug it and plug it back in.

When using the AppImage or portable tar archive, install the udev rule manually (from the tar archive or [`configs/50-cordial.rules`](../configs/50-cordial.rules)):

```sh
sudo cp 50-cordial.rules /etc/udev/rules.d/
sudo udevadm control --reload-rules
```

Then reconnect the adapter.

Over SSH or without an active local desktop session, add your user to the serial device group instead (`uucp` on Arch Linux, `dialout` on Debian and Ubuntu) and log out and back in.

### Running Cordial

- **Desktop app**: Open **Cordial** from your application menu or run `cordial desktop`. Pass `--hidden` to start minimized to the tray.
- **Start at Login**: Toggle **Start at Login** in Settings to manage `$XDG_CONFIG_HOME/autostart/dev.petris.cordial.desktop` (defaulting to `~/.config`).
- **TUI and CLI**: Run `cordial tui` for the full-screen terminal UI, `cordial --port PORT` for the interactive shell, or `cordial --help` for one-shot commands.
- **Protocol JSON logging**: Pass `--json` to log every raw protocol response and event as JSON lines to standard error while normal output remains on standard output:

```sh
cordial --json tui 2>cordial.jsonl
cordial --json desktop 2>cordial.jsonl
```

Because the desktop JSON stream attaches to the primary process's standard error, quit any running Cordial desktop instance before launching `cordial --json desktop`.

### Tray Icon on GNOME

Cordial uses the StatusNotifierItem protocol for its system tray icon. On GNOME, install a compatible shell extension such as [AppIndicator Support](https://extensions.gnome.org/extension/615/appindicator-support/).

## Firmware

Each supported board has two firmware archives per release:

- **Production** (`cordial-firmware-<version>-<code>.tar.gz`): standard daily-use firmware.
- **Debug** (`cordial-firmware-<version>-debug-<code>.tar.gz`): adds development CLI commands for inspecting adapter files, listing HID++ features, and rebooting into the bootloader from software.

| Board | Firmware Code | Notes |
| --- | --- | --- |
| Raspberry Pi Pico W | `pico_w` | Classic and BLE; no input profiles |
| Raspberry Pi Pico 2 W | `pico2_w` | Classic and BLE; full feature set |
| Pimoroni Pico Plus 2 W | `pimoroni_pico_plus2_w` | PSRAM unused; hardware verification pending |
| Waveshare RP2350B-Plus-W | `waveshare_rp2350b_plus_w` | Classic and BLE; full feature set |
| ESP32-S3 (4 MiB+ flash, native USB) | `esp32s3` | BLE only; no gamepad outputs |

### Storage Initialization and Upgrades

On first boot, Cordial formats empty flash automatically. When upgrading firmware, Cordial preserves paired devices, preferences, and profiles, migrating supported older storage layouts automatically (with host assistance when needed). If firmware encounters newer, unsupported, or corrupt storage, it leaves the data intact rather than erasing it. Update Cordial if a newer firmware requires a newer storage format. See [On-board storage format](storage-format.md) for migration and recovery details.

### RP2040 and RP2350 Boards

1. Download and extract your board's firmware archive.
2. Hold the **BOOTSEL** (or **BOOT**) button while connecting the board to USB. It mounts as a mass-storage drive.
3. Copy the extracted `.uf2` file onto the drive. The board restarts automatically as a Cordial adapter.

### ESP32-S3

The `esp32s3` image requires an ESP32-S3 board with at least 4 MiB of flash and a USB port wired directly to the chip's native USB peripheral. Compatible boards include the [Seeed Studio XIAO ESP32-S3](https://wiki.seeedstudio.com/xiao_esp32s3_getting_started/), [M5Stack Atom S3](https://docs.m5stack.com/en/core/AtomS3), and [M5Stack Atom S3U](https://docs.m5stack.com/en/core/AtomS3U).

Put the board into ROM download mode:

- **XIAO ESP32-S3**: Hold **BOOT** while connecting the USB data cable, then release it.
- **Atom S3 / Atom S3U**: Connect USB, hold **Reset** for about two seconds until the internal green LED turns on, then release it (see M5Stack's [Atom S3](https://docs.m5stack.com/en/core/AtomS3#learn) and [Atom S3U](https://docs.m5stack.com/en/arduino/m5atoms3u/program#2-download-mode) guides).
- **Other boards**: Follow the manufacturer's download-mode instructions, and use the native USB connector for normal operation.

From the extracted `<version>/esp32s3-esp-nimble-esp-idf/` directory (or `<version>-debug/esp32s3-esp-nimble-esp-idf/` for debug builds), flash all three images with [esptool](https://docs.espressif.com/projects/esptool/):

```sh
esptool --chip esp32s3 write_flash \
  0x0 bootloader.bin \
  0x8000 partition-table.bin \
  0x10000 cordial-esp32s3-esp-nimble-esp-idf.bin
```

After flashing completes, unplug the board and plug it back in.
