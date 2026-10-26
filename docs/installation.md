# Installation

Downloads are on the [latest release](https://github.com/ryanpetris/cordial/releases/latest) page.
Every release ships a `SHA256SUMS` file for verifying them.

## Desktop app and CLI

`cordial-desktop` is the desktop tray app. `cordial-cli` is the `cordial` command-line tool and
TUI. You can install either package or both.

The CLI is optional for direct desktop connections. The desktop app invokes `cordial` for assisted
storage migration and for older serial revisions that the helper can convert. A protocol helper
must match the app's current revision and exact operational schema fingerprint. Revision `1` has
no earlier supported revision or historical protocol converter. The CLI and TUI run storage
migration automatically when required.

For protocol proxying on Linux, the desktop runs the first of `~/.local/bin/cordial`,
`/usr/local/bin/cordial` and `/usr/bin/cordial` that exists, or the absolute path in
`CORDIAL_CLI_PATH` when set. The selected file must be executable and match the current protocol
revision and operational schema fingerprint. If it doesn't, the app doesn't look further.

For assisted storage migration, the desktop finds the first executable `cordial` on `PATH`.
`CORDIAL_CLI_PATH` applies only to protocol proxying.

### Arch Linux

```sh
sudo pacman -U cordial-desktop-*-x86_64.pkg.tar.zst
sudo pacman -U cordial-cli-*-x86_64.pkg.tar.zst
```

### Debian and Ubuntu

Choose the files marked `trixie` for Debian 13, `noble` for Ubuntu 24.04, or `resolute` for
Ubuntu 26.04.

```sh
sudo apt install ./cordial-desktop_*_amd64.deb
sudo apt install ./cordial-cli_*_amd64.deb
```

### Other distributions

Download `cordial-desktop-*-x86_64.AppImage` for the desktop app and make it executable with
`chmod +x`. A desktop tar archive, `cordial-desktop-*-x64.tar.gz`, is also available; extract it
and run the included `cordial-desktop`. For the command-line tool, download
`cordial-cli-*-linux-amd64.tar.gz`.

For AppImage and portable desktop installations, put the extracted `cordial` executable on `PATH`
for assisted storage migration. Protocol proxying uses the locations or override listed above.

### Adapter access

The Arch and Debian packages give the signed-in user access to the adapter. If the adapter was
plugged in during installation, unplug it and plug it back in.

With other distributions, copy `50-cordial.rules` from a tar archive (or from `configs/` in this
repository) to `/etc/udev/rules.d/` and reconnect the adapter.

Over SSH or without a desktop session, add yourself to the group that owns `/dev/ttyACM*` instead:
`uucp` on Arch Linux, `dialout` on Debian and Ubuntu. Log out and back in after changing groups.

### Tray icon on GNOME

The tray icon needs the
[AppIndicator extension](https://extensions.gnome.org/extension/615/appindicator-support/).

## Firmware

Download your board's firmware archive from the latest release and extract it. Each board has a
production archive, `cordial-firmware-<version>-<code>.tar.gz`, and a debug archive,
`cordial-firmware-<version>-debug-<code>.tar.gz`. The debug archive adds adapter file access and
restarting into the bootloader from the app.

| Board | Code |
| --- | --- |
| Raspberry Pi Pico W | `pico_w` |
| Raspberry Pi Pico 2 W | `pico2_w` |
| Pimoroni Pico Plus 2 W | `pimoroni_pico_plus2_w` |
| Waveshare RP2350B-Plus-W | `waveshare_rp2350b_plus_w` |
| ESP32-S3 with at least 4 MiB flash and native USB | `esp32s3` |

Pimoroni Pico Plus 2 W hardware verification is outstanding. Cordial leaves its PSRAM unused.

### First installation

Cordial initializes empty storage at startup and migrates supported older storage while preserving
saved devices and preferences. The firmware determines the target storage format. Update the
client if it does not support that target or a migration path to it. Invalid, newer, or unsupported
storage stays intact. See the [storage format](storage-format.md) for migration and recovery.

### RP2040 and RP2350 boards

1. Hold the **BOOTSEL** (or **BOOT**) button while plugging the board into USB. It appears as a
   USB drive.
2. Copy the extracted `.uf2` file onto that drive. The board restarts as a Cordial adapter.

### ESP32-S3

The ESP32-S3 image needs at least 4 MiB flash and a USB connection wired to the chip's native USB
peripheral. Boards that meet these requirements include the
[Seeed Studio XIAO ESP32-S3](https://wiki.seeedstudio.com/xiao_esp32s3_getting_started/),
[M5Stack Atom S3](https://docs.m5stack.com/en/core/AtomS3) and
[Atom S3U](https://docs.m5stack.com/en/core/AtomS3U).

Enter the board's download mode:

- XIAO ESP32-S3: hold **BOOT** while connecting the USB data cable, then release it.
- Atom S3 and Atom S3U: connect USB, hold the **Reset** button for about two seconds until the
  internal green LED lights, then release it. See M5Stack's
  [Atom S3 instructions](https://docs.m5stack.com/en/core/AtomS3#learn) and
  [Atom S3U instructions](https://docs.m5stack.com/en/arduino/m5atoms3u/program#2-download-mode).
- Other boards: follow the manufacturer's download-mode instructions and use the native USB
  connector for normal operation.

Flash it with [esptool](https://docs.espressif.com/projects/esptool/) from the extracted
`esp32s3-esp-nimble-esp-idf` folder:

```sh
esptool --chip esp32s3 write_flash \
  0x0 bootloader.bin \
  0x8000 partition-table.bin \
  0x10000 cordial-esp32s3-esp-nimble-esp-idf.bin
```

After flashing, unplug the board and plug it back in.
