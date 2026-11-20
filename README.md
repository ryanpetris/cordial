<p align="center">
  <img src="packaging/icons/app.svg" width="112" alt="Cordial logo">
</p>

<h1 align="center">Cordial</h1>

<p align="center">
  <strong>Your Bluetooth keyboard, mouse, and gamepad, presented as plain USB.</strong>
</p>

<p align="center">
  Cordial turns an inexpensive microcontroller board into a standalone Bluetooth-to-USB adapter.
  Pair your wireless peripherals once and plug the adapter into any computer, KVM switch, or OS.
  No host Bluetooth, no drivers, and no background software required.
</p>

<p align="center">
  <a href="https://github.com/ryanpetris/cordial/releases/latest"><img src="https://img.shields.io/github/v/release/ryanpetris/cordial?label=latest" alt="Latest release"></a>
  <img src="https://img.shields.io/badge/app-Linux%20x86--64-informational" alt="App: Linux x86-64">
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-MIT-blue" alt="MIT license"></a>
</p>

<p align="center">
  <a href="https://github.com/ryanpetris/cordial/releases/latest"><b>Download</b></a>
  &nbsp;|&nbsp;
  <a href="#quick-start">Quick Start</a>
  &nbsp;|&nbsp;
  <a href="#supported-boards">Supported Boards</a>
  &nbsp;|&nbsp;
  <a href="#documentation">Documentation</a>
</p>

## Why Cordial

- **Plain USB everywhere**: The host sees standard USB keyboards, mice, and gamepads, so your devices work in BIOS/UEFI, disk-encryption prompts, KVM switches, and locked-down machines without host Bluetooth or drivers.
- **Standalone and portable**: Pairings, device preferences, and input profiles are stored on the adapter itself. Quit the app or move the adapter to another computer and everything keeps working.
- **Sleep-friendly**: Bluetooth connections stay alive while the host sleeps, standard USB remote wake is supported, and nothing typed during suspend is replayed on wake.
- **Logitech HID++ integration**: Translates special keys for Linux, Windows, or macOS, reports battery status, and configures DPI, SmartShift, scroll direction, backlighting, Fn-row behavior, and auto power-off on [supported Logitech devices](docs/hidpp.md).
- **USB gamepads**: RP2040 and RP2350 boards forward Bluetooth controllers as standard USB HID or XInput [gamepads](docs/gamepads.md), with automatic builtin mappings for common Xbox, DualShock 4, and Luna layouts.
- **On-adapter input profiles**: Remap keys, mouse buttons, and controller inputs, scale pointer or scroll axes, and stack per-device layers directly on the adapter. Edit keyboard and mouse rules in VIA or Vial, or edit any rule from the CLI; see [Input profiles](docs/input-profiles.md).
- **One native companion tool**: A single `cordial` executable provides a native GTK4/libadwaita desktop app, a full-screen TUI, an interactive shell, and scriptable one-shot CLI commands with JSON protocol logging.

## Supported Boards

| Board | Firmware Code | Bluetooth | Gamepads | Input Profiles |
| --- | --- | --- | --- | --- |
| Raspberry Pi Pico W | `pico_w` | Classic and BLE | HID and XInput | No |
| Raspberry Pi Pico 2 W | `pico2_w` | Classic and BLE | HID and XInput | Yes |
| Pimoroni Pico Plus 2 W | `pimoroni_pico_plus2_w` | Classic and BLE | HID and XInput | Yes |
| Waveshare RP2350B-Plus-W | `waveshare_rp2350b_plus_w` | Classic and BLE | HID and XInput | Yes |
| ESP32-S3 (4 MiB+ flash, native USB) | `esp32s3` | BLE only | No | Yes |

Each board's release archive is named `cordial-firmware-<version>-<code>.tar.gz`.

## Quick Start

A Linux x86-64 computer is used to flash firmware and configure the adapter. Once paired, the adapter works with any operating system without running Cordial.

### 1. Install Cordial

Download the package for your distribution from the [latest release](https://github.com/ryanpetris/cordial/releases/latest). The `cordial` package includes the desktop app, TUI, CLI, and udev rules.

Arch Linux:

```sh
sudo pacman -U cordial-[0-9]*-1-x86_64.pkg.tar.zst
```

Ubuntu 26.04 (`resolute`) or Debian 13 (substitute `trixie` for `resolute`):

```sh
sudo apt install ./cordial_[0-9]*-1~resolute_amd64.deb
```

Using another distribution? Download the AppImage or portable tar archive; see the [Installation guide](docs/installation.md) for setup, udev permissions, and GNOME tray icon details.

### 2. Flash Firmware

For RP2040 and RP2350 boards:

1. Download and extract your board's firmware archive from the [latest release](https://github.com/ryanpetris/cordial/releases/latest).
2. Hold **BOOTSEL** (or **BOOT**) while plugging the board into USB so it mounts as a USB drive.
3. Copy the extracted `.uf2` file onto the drive. The board reboots as a Cordial adapter.

For ESP32-S3 download-mode and `esptool` instructions, or debug firmware builds, see [Firmware installation](docs/installation.md#firmware).

### 3. Pair a Device

1. Plug in the adapter and launch **Cordial**.
2. Open the adapter in the sidebar, select **Add Device**, and put your Bluetooth peripheral into pairing mode.
3. Select **Pair** next to the device and follow any passkey prompt.

You can also manage adapters from a terminal:

```sh
cordial desktop           # launch the GTK4/libadwaita desktop app
cordial tui               # open the full-screen terminal UI
cordial adapter list      # list attached adapters
cordial --port PORT       # open the interactive shell
```

## Documentation

User guides:

- [Installation](docs/installation.md) -- packages, AppImage, udev permissions, tray icon, and firmware flashing
- [Gamepad outputs](docs/gamepads.md) -- HID and XInput controller setup and slot pools
- [Input profiles](docs/input-profiles.md) -- remapping, layers, VIA/Vial editing, and builtin controller rules
- [Logitech HID++ support](docs/hidpp.md) -- feature inventory, special-key translations, and device settings

Developer and technical references:

- [Building from source](docs/building.md) -- Docker and local builds, board presets, packaging, and test suites
- [USB control protocol](docs/protocol/README.md) -- serial API, framing, commands, records, HID forwarding, and client behavior
- [On-board storage format](docs/storage-format.md) -- flash layout, LittleFS records, migrations, and recovery

## License

Cordial is licensed under the [MIT License](LICENSE). Third-party components retain their own licenses; see [`notices/`](notices/).
