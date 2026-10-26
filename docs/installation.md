# Installation

Downloads are on the [latest release](https://github.com/ryanpetris/cordial/releases/latest) page.
Every release ships a `SHA256SUMS` file for verifying them.

## Desktop app and CLI

Cordial uses one native Rust executable for the GTK4/libadwaita desktop app, CLI and TUI.
The `cordial` package includes the executable, application menu entry, icons, settings schema,
documentation, notices and adapter access rule.
Protocol compatibility and storage migration run inside the application.

Launch **Cordial** from the application menu or run `cordial desktop`. Use `cordial tui` for the
terminal interface. `cordial desktop --hidden` starts without presenting a window.

Native packages target Arch Linux, Debian 13 and Ubuntu 26.04 on x86-64, using each
distribution's GTK and libadwaita libraries. The unified host archive uses the Ubuntu 26.04
baseline of glibc 2.43, GLib 2.88, GTK 4.22 and libadwaita 1.9. The Debian 13 package builds
its executable on Debian 13 with that distribution's libraries.
The AppImage bundles GTK, libadwaita, text libraries and the dconf settings backend.
It requires a session D-Bus and the host's dconf service, provided by GNOME desktops. Install
`dconf-service` on Debian or Ubuntu, or `dconf` on Arch, when using another desktop.
The AppImage requires glibc 2.43 or newer and uses the host's compiler runtime,
window-system libraries and graphics drivers,
including EGL, OpenGL and Vulkan.

### Arch Linux

```sh
sudo pacman -U cordial-[0-9]*-1-x86_64.pkg.tar.zst
```

### Debian and Ubuntu

Choose the package marked `trixie` for Debian 13 or `resolute` for Ubuntu 26.04.
This command uses Ubuntu 26.04; substitute your distribution's codename:

```sh
sudo apt install ./cordial_[0-9]*-1~resolute_amd64.deb
```

### Upgrading

To update an installed `cordial` package, download its new version and use the same installation
command.

Enable Start at Login in Settings to launch Cordial automatically.
Package installation leaves user configuration unchanged.

### Other distributions

Download `cordial-desktop-<version>-x86_64.AppImage`, make it executable with `chmod +x`, and
run it to open the desktop app. It also accepts CLI arguments, such as `tui`. If FUSE is
unavailable, set `APPIMAGE_EXTRACT_AND_RUN=1` when launching it.

The unified `cordial-<version>-linux-amd64.tar.gz` archive contains the `cordial` executable,
documentation, notices, adapter access rule and GSettings schemas. Keep `share/` beside the
executable, including when launching it through a symlink. Extract it and run `./cordial desktop`,
`./cordial tui` or another command. Install the required GTK and libadwaita runtime libraries
with your distribution's package manager.

### Adapter access

The Arch and Debian packages give the signed-in user access to the adapter. If the adapter was
plugged in during installation, unplug it and plug it back in.

With other distributions, copy `50-cordial.rules` from a tar archive (or from `configs/` in this
repository) to `/etc/udev/rules.d/` and reconnect the adapter.

Over SSH or without a desktop session, add yourself to the group that owns `/dev/ttyACM*` instead:
`uucp` on Arch Linux, `dialout` on Debian and Ubuntu. Log out and back in after changing groups.

### Tray icon on GNOME

Cordial uses StatusNotifierItem for its tray icon. GNOME needs a supporting extension, such as the
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
