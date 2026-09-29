<p align="center">
  <img src="desktop/assets/icons/app.svg" width="112" alt="">
</p>

<h1 align="center">Cordial</h1>

<p align="center">
  <strong>Your Bluetooth keyboard and mouse, as plain USB.</strong>
</p>

<p align="center">
  Cordial turns a small, inexpensive microcontroller board into an adapter that pairs
  with your Bluetooth keyboards and mice and presents them to the computer as ordinary
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
  <a href="#get-started">Get started</a>
  &nbsp;·&nbsp;
  <a href="#supported-boards">Supported boards</a>
  &nbsp;·&nbsp;
  <a href="docs/building.md">Build from source</a>
</p>

## Why Cordial

- **Works wherever USB works.** The computer sees a standard USB keyboard and mouse.
  It needs no Bluetooth radio, no pairing, and no drivers.
- **Pair once.** Pairings and preferences are stored on the adapter itself. Move it to
  another computer and your devices come with it.
- **Nothing to keep running.** Input forwarding happens on the adapter. The app is for
  setup and status; quit it and your keyboard carries on.
- **Sleeps politely.** Bluetooth connections survive host sleep, and nothing you typed
  meanwhile is replayed on wake.
- **Special keys that just work.** On Logitech HID++ keyboards and mice, media, brightness
  and other special keys are translated for Linux, Windows or macOS. Backlight, DPI,
  SmartShift and scroll direction are adjustable, and battery level is reported.
- **Tray app or terminal.** A desktop tray app shows battery status at a glance. The
  `cordial` CLI offers an interactive shell, one-shot commands with JSON output, and a
  full-screen TUI.

## What you need

- One of the [supported boards](#supported-boards).
- A Linux x86-64 computer to install firmware and pair devices. Once set up, the adapter
  works with any computer.

## Supported boards

| Board | Bluetooth | Firmware download |
| --- | --- | --- |
| Raspberry Pi Pico W | Classic and Low Energy | `cordial-firmware-*-pico_w-production.tar.gz` |
| Raspberry Pi Pico 2 W | Classic and Low Energy | `cordial-firmware-*-pico2_w-production.tar.gz` |
| Waveshare RP2350B-Plus-W | Classic and Low Energy | `cordial-firmware-*-waveshare_rp2350b_plus_w-production.tar.gz` |
| Seeed Studio XIAO ESP32S3 | Low Energy only | `cordial-firmware-*-xiao_esp32s3-production.tar.gz` |

## Get started

### 1. Install the app

Download the package for your distribution from the
[latest release](https://github.com/ryanpetris/cordial/releases/latest). Choose `cordial-desktop` for the desktop app or `cordial-cli` for the `cordial`
command-line tool. You can install either package or both.

**Arch Linux**

```sh
sudo pacman -U cordial-desktop-*-x86_64.pkg.tar.zst
# Optional command-line tool:
sudo pacman -U cordial-cli-*-x86_64.pkg.tar.zst
sudo usermod -aG uucp "$USER"
```

**Debian 13, Ubuntu 24.04 and Ubuntu 26.04**

```sh
sudo apt install ./cordial-desktop_*_amd64.deb
# Optional command-line tool:
sudo apt install ./cordial-cli_*_amd64.deb
sudo usermod -aG dialout "$USER"
```

Choose the files marked `trixie` for Debian 13, `noble` for Ubuntu 24.04, or
`resolute` for Ubuntu 26.04.

**Other distributions:** download `cordial-desktop-*-x86_64.AppImage` for the desktop
app, or `cordial-cli-*-linux-amd64.tar.gz` for the command-line tool alone, and add
yourself to the group that owns `/dev/ttyACM*`. Make the AppImage executable with
`chmod +x` before running it. A desktop tar archive is also available as
`cordial-desktop-*-x64.tar.gz`; extract it and run the included `cordial-desktop`.

Log out and back in after changing groups.

> **GNOME users:** the tray icon needs the
> [AppIndicator extension](https://extensions.gnome.org/extension/615/appindicator-support/).

### 2. Flash the adapter

Download your board's firmware from the
[latest release](https://github.com/ryanpetris/cordial/releases/latest) and extract it.

**Raspberry Pi Pico W, Pico 2 W and Waveshare RP2350B-Plus-W**

1. Hold the **BOOTSEL** (or **BOOT**) button while plugging the board into USB.
   It appears as a USB drive.
2. Copy the `.uf2` file from the extracted folder onto that drive. The board restarts as
   a Cordial adapter.

**Seeed Studio XIAO ESP32S3**

Hold **BOOT** while plugging the board in, then flash it with
[esptool](https://docs.espressif.com/projects/esptool/):

```sh
cd xiao_esp32s3-esp-nimble-esp-idf-production
esptool --chip esp32s3 write_flash \
  0x0 bootloader.bin \
  0x8000 partition-table.bin \
  0x10000 cordial-xiao_esp32s3-esp-nimble-esp-idf-production.bin
```

### 3. Pair your devices

1. Plug in the adapter and open **Cordial**. The adapter appears in the sidebar.
2. Choose **Add Device**, put your keyboard or mouse into pairing mode, and select
   **Pair** next to it.
3. Follow any on-screen passkey prompt.

That's it. The device now reconnects to the adapter automatically.

## Prefer the terminal?

```sh
cordial adapter list            # find your adapter
cordial --port PORT             # interactive shell
cordial --port PORT tui         # full-screen interface
```

Only one app can manage an adapter at a time. Quit the desktop app, or disconnect the
adapter in it, before using the CLI. See the
[command reference](docs/protocol/clients.md) for everything the CLI can do.

## Learn more

- [Build from source](docs/building.md): toolchains, custom boards, and checks.
- [USB control protocol](docs/protocol/README.md)
- [HID++ support](docs/hidpp.md)
- [Storage format](docs/storage-format.md)

Every release ships a `SHA256SUMS` file for verifying downloads.

## License

Cordial is released under the [MIT License](LICENSE). Third-party components keep their
own licenses; see [`notices/`](notices/).
