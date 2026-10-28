<p align="center">
  <img src="packaging/icons/app.svg" width="112" alt="">
</p>

<h1 align="center">Cordial</h1>

<p align="center">
  <strong>Your Bluetooth keyboard, mouse and gamepad, as plain USB.</strong>
</p>

<p align="center">
  Cordial turns a small, inexpensive microcontroller board into an adapter that pairs
  with your Bluetooth keyboards, mice and gamepads and presents them to the computer as ordinary
  USB devices. No host Bluetooth, no drivers, no software running in the background.
</p>

<p align="center">
  <a href="https://github.com/ryanpetris/cordial/releases/latest"><img src="https://img.shields.io/github/v/release/ryanpetris/cordial?label=latest" alt="Latest release"></a>
  <img src="https://img.shields.io/badge/desktop-Linux%20x86--64-informational" alt="Desktop app: Linux x86-64">
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-MIT-blue" alt="MIT license"></a>
</p>

<p align="center">
  <a href="https://github.com/ryanpetris/cordial/releases/latest"><b>Download</b></a>
  &nbsp;·&nbsp;
  <a href="docs/installation.md">Install</a>
  &nbsp;·&nbsp;
  <a href="#supported-boards">Supported boards</a>
  &nbsp;·&nbsp;
  <a href="docs/building.md">Build from source</a>
</p>

## Features

- The computer receives input over USB. It needs no Bluetooth radio, no pairing and
  no drivers.
- Pairings and preferences live on the adapter. Move it to another computer and your devices come
  with it.
- Input forwarding happens on the adapter. The app is for setup and status; quit it and your
  keyboard carries on.
- Bluetooth connections survive host sleep, and nothing you typed meanwhile is replayed on wake.
- On Logitech HID++ keyboards and mice, special keys are translated for Linux, Windows or macOS.
  Backlight, DPI, SmartShift and scroll direction are adjustable, and battery level is reported.
- RP2040 and RP2350 boards can forward controllers as HID or XInput [gamepads](docs/gamepads.md).
- [Input profiles](docs/input-profiles.md) remap keys, mouse buttons and controller inputs on the
  adapter. Edit key and mouse rules with VIA or Vial, and any rule with the CLI.
- The native GTK4/libadwaita desktop app shows adapter and device status, settings and profiles.
  The same `cordial` executable provides an interactive shell, one-shot commands with JSON output,
  and a full-screen TUI.

## Supported boards

| Board | Code | Bluetooth | Gamepad | Input profiles |
| --- | --- | --- | --- | --- |
| Raspberry Pi Pico W | `pico_w` | Classic and Low Energy | Yes | No |
| Raspberry Pi Pico 2 W | `pico2_w` | Classic and Low Energy | Yes | Yes |
| Pimoroni Pico Plus 2 W | `pimoroni_pico_plus2_w` | Classic and Low Energy | Yes | Yes |
| Waveshare RP2350B-Plus-W | `waveshare_rp2350b_plus_w` | Classic and Low Energy | Yes | Yes |
| ESP32-S3 with at least 4 MiB flash and native USB | `esp32s3` | Low Energy only | No | Yes |

Each board's firmware is in `cordial-firmware-<version>-<code>.tar.gz`.

## Installation

You need a Linux x86-64 computer to install firmware and pair devices. Once set up, the adapter
works with any computer.

### Desktop app and CLI

Download the package for your distribution from the
[latest release](https://github.com/ryanpetris/cordial/releases/latest). The `cordial` package
includes the GNOME application, CLI and TUI.

On Arch Linux:

```sh
sudo pacman -U cordial-[0-9]*-1-x86_64.pkg.tar.zst
```

On Ubuntu 26.04, use the files marked `resolute`. Substitute `trixie` for Debian 13:

```sh
sudo apt install ./cordial_[0-9]*-1~resolute_amd64.deb
```

Use the same command to update an installed `cordial` package.
See [installation](docs/installation.md#desktop-app-and-cli) for desktop setup.

An AppImage and a unified host tar archive are available for other distributions. See
[installation](docs/installation.md#desktop-app-and-cli) for details.

### Firmware for RP2040 and RP2350 boards

1. Download and extract your board's firmware archive from the
   [latest release](https://github.com/ryanpetris/cordial/releases/latest).
2. Hold the **BOOTSEL** (or **BOOT**) button while plugging the board into USB. It appears as a
   USB drive.
3. Copy the extracted `.uf2` file onto that drive. The board restarts as a Cordial adapter.

For ESP32-S3, see [installation](docs/installation.md#esp32-s3).

## Usage

1. Plug in the adapter and open **Cordial**. The adapter appears in the sidebar.
2. Choose **Add Device**, put your device into pairing mode, and select **Pair** next to it.
3. Follow any on-screen passkey prompt.

From the terminal:

```sh
cordial desktop                 # native GNOME application
cordial adapter list            # find your adapter
cordial --port PORT             # interactive shell
cordial tui                     # full-screen interface for every attached adapter
```

## Documentation

- [Installation](docs/installation.md)
- [Gamepad outputs](docs/gamepads.md)
- [Input profiles](docs/input-profiles.md)
- [HID++ support](docs/hidpp.md)
- [Build from source](docs/building.md)
- [USB control protocol](docs/protocol/README.md)
- [Storage format](docs/storage-format.md)

## License

Cordial is released under the [MIT License](LICENSE). Third-party components keep their
own licenses; see [`notices/`](notices/).
