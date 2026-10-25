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
- **Gamepad outputs on Pico boards.** Choose HID or XInput for each paired controller.
  See [gamepad setup and limits](#gamepad-outputs).
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
| Raspberry Pi Pico W | Classic and Low Energy | `cordial-firmware-<version>-pico_w.tar.gz` |
| Raspberry Pi Pico 2 W | Classic and Low Energy | `cordial-firmware-<version>-pico2_w.tar.gz` |
| Waveshare RP2350B-Plus-W | Classic and Low Energy | `cordial-firmware-<version>-waveshare_rp2350b_plus_w.tar.gz` |
| ESP32-S3 with 8 MiB flash and native USB | Low Energy only | `cordial-firmware-<version>-esp32s3_8mb.tar.gz` |

The ESP32-S3 image requires 8 MiB flash and a USB connection wired to the chip's native USB
peripheral. A USB-to-UART connection alone cannot provide Cordial's USB keyboard and mouse.
Expected compatible boards include the [Seeed Studio XIAO ESP32-S3](https://wiki.seeedstudio.com/xiao_esp32s3_getting_started/),
[M5Stack Atom S3](https://docs.m5stack.com/en/core/AtomS3) and
[Atom S3U](https://docs.m5stack.com/en/core/AtomS3U). Hardware validation of this image is pending.
This image does not use PSRAM or support onboard displays, application buttons or status
LEDs. It does not cover every ESP32-S3 variant.

Each board also has a debug archive, `cordial-firmware-<version>-debug-<board>.tar.gz`, which adds
adapter file access and restarting into the bootloader from the app.

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
```

**Debian 13, Ubuntu 24.04 and Ubuntu 26.04**

```sh
sudo apt install ./cordial-desktop_*_amd64.deb
# Optional command-line tool:
sudo apt install ./cordial-cli_*_amd64.deb
```

Choose the files marked `trixie` for Debian 13, `noble` for Ubuntu 24.04, or
`resolute` for Ubuntu 26.04. If the adapter was plugged in during installation, unplug it and plug
it back in.

The packages give the signed-in user access to the adapter. Over SSH or without a desktop session,
add yourself to the group that owns `/dev/ttyACM*` instead: `uucp` on Arch Linux, `dialout` on
Debian and Ubuntu.

**Other distributions:** download `cordial-desktop-*-x86_64.AppImage` for the desktop
app, or `cordial-cli-*-linux-amd64.tar.gz` for the command-line tool alone. To use the adapter
without root, copy `50-cordial.rules` from a tar archive (or from `configs/` in this repository) to
`/etc/udev/rules.d/` and reconnect the adapter, or add yourself to the group that owns
`/dev/ttyACM*`. Make the AppImage executable with
`chmod +x` before running it. A desktop tar archive is also available as
`cordial-desktop-*-x64.tar.gz`; extract it and run the included `cordial-desktop`.

Log out and back in after changing groups.

> **GNOME users:** the tray icon needs the
> [AppIndicator extension](https://extensions.gnome.org/extension/615/appindicator-support/).

### 2. Flash the adapter

Download your board's firmware from the
[latest release](https://github.com/ryanpetris/cordial/releases/latest) and extract it.

For a first installation, or an update from release 0.6.3 or earlier, erase the
board's flash before flashing Cordial. Use `picotool erase` in BOOTSEL mode or
`esptool --chip esp32s3 erase_flash` in download mode. This removes the existing
firmware and saved data; pair your devices after flashing. Factory firmware and
earlier storage formats can leave data that prevents Cordial from initializing
its storage. Updates within the current storage format preserve saved devices
and preferences without erasing flash. See [building](docs/building.md#firmware).

**Raspberry Pi Pico W, Pico 2 W and Waveshare RP2350B-Plus-W**

1. Hold the **BOOTSEL** (or **BOOT**) button while plugging the board into USB.
   It appears as a USB drive.
2. Copy the extracted `.uf2` file onto that drive. The board restarts as
   a Cordial adapter.

**ESP32-S3 with 8 MiB flash and native USB**

Enter the board's download mode:

- **XIAO ESP32-S3:** hold **BOOT** while connecting the USB data cable, then release it.
- **Atom S3 and Atom S3U:** connect USB, hold the **Reset** button for about two seconds until
  the internal green LED lights, then release it. See M5Stack's
  [Atom S3 instructions](https://docs.m5stack.com/en/core/AtomS3#learn) and
  [Atom S3U instructions](https://docs.m5stack.com/en/arduino/m5atoms3u/program#2-download-mode).
- **Other boards:** follow the manufacturer's download-mode instructions and use the native USB
  connector for normal operation.

Flash it with [esptool](https://docs.espressif.com/projects/esptool/) from the extracted
`esp32s3_8mb-esp-nimble-esp-idf` folder:

```sh
esptool --chip esp32s3 write_flash \
  0x0 bootloader.bin \
  0x8000 partition-table.bin \
  0x10000 cordial-esp32s3_8mb-esp-nimble-esp-idf.bin
```

After flashing, unplug the board and plug it back in.

### 3. Pair your devices

1. Plug in the adapter and open **Cordial**. The adapter appears in the sidebar.
2. Choose **Add Device**, put your device into pairing mode, and select
   **Pair** next to it.
3. Follow any on-screen passkey prompt.

That's it. The device now reconnects to the adapter automatically.

## Gamepad outputs

Pico W, Pico 2 W and Waveshare RP2350B-Plus-W support HID and XInput gamepad
outputs. ESP32-S3 does not support gamepad output. After pairing a controller,
open its Settings tab, select **HID** or **XInput** under **Gamepad Output**,
and save. The control appears when the adapter advertises an enabled mode or a
saved choice needs clearing. The default is **Disabled**, including for existing
pairings without a saved choice. Keyboard and mouse forwarding keeps its existing
behavior.

The adapter saves each controller's choice and forwards input without a Cordial
app or host service running. HID and XInput each have an independent USB pool
of `min(4, paired devices selecting that mode)` slots. Disconnected devices still
count; devices with gamepad output Disabled do not. More than four paired devices
may select a mode. Bluetooth connections have independent allowances per transport,
separate from the USB pools. See [Bluetooth capacity](docs/protocol/limits-and-errors.md#bluetooth-capacity).

Connecting or disconnecting a controller assigns or releases a slot without
reconnecting USB. An empty HID slot stays visible with neutral controls; an empty
XInput channel reports the controller absent. Pairing, unpairing or changing a
saved mode reconnects USB when a pool's size changes. VIA/Vial behavior is
unchanged; configuration-slot resource work is tracked in
[issue 1](https://github.com/ryanpetris/cordial/issues/1).

Controller support depends on its reported HID layout. See the
[mapping limits](docs/protocol/hid-forwarding.md#usb-gamepad-outputs) and
[Linux and Windows verification](docs/protocol/clients.md#gamepad-verification).
Windows native driver binding is implemented but remains unverified on hardware.

## Prefer the terminal?

```sh
cordial adapter list            # find your adapter
cordial --port PORT             # interactive shell
cordial tui                     # full-screen interface for every attached adapter
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
