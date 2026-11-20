# HID Forwarding and Limits

[Protocol index](README.md)

## HID Report Compiler and Limits

Firmware compiles each Bluetooth HID report map independently, forwarding keyboard arrays/bitmaps, report IDs, global Push/Pop, signed/unsigned relative key/button events, absolute and relative pointer axes, 16 mouse buttons, vertical and horizontal wheels, and Consumer-page keys, switches, and linear controls. Relative selector items for keys/buttons emit press/release events; selectors for numeric pointer or Consumer axes are not forwarded.

- **Map limits**: <= 2,048 descriptor bytes, <= 512 report bytes, <= 16 report IDs, <= 96 fields, <= 128 compressed usage spans. Identical ordered usage-span lists share immutable storage.
- **Forwarder limits**: one source slot per compiled runtime connection slot (normal allowances + pairing reserve), 8 simultaneously held Consumer usages (`1..65535`), keyboard usages `1..255`, and a 64-entry transition queue. Overflow returns an error rather than silently dropping keys; pulse bursts verify queue space before enqueueing.
- **Pointer and Consumer axes**: Pointer motion (<= 1,048,576 counts/sample) splits into signed 16-bit USB reports. Relative Consumer report 4 carries 17 signed 32-bit fields in usage order (68-byte payload spanning up to two USB packets; 40-bit accumulator except Volume) for usages `0x0071`, `0x007B`, `0x0086`, `0x00BD`, `0x00BF`, `0x00E0`, `0x00E1`, `0x00E3`, `0x00E4`, `0x0101`, `0x0103`, `0x0105`, `0x0109`, `0x0170`, `0x022F`, `0x0235`, and `0x0238`. Absolute values use 16-bit USB fields with explicit null values for unchanged axes.
- **Relative Volume (`0x00E0`)**: Occupies field 6 (payload offset 20, logical range `-1..1`). Nonzero contributions reduce to direction (`-1` or `+1`) before clamping to `-1..1` without building a backlog of steps.
- **Errors and suspend**: Truncated reports are rejected without mutating held state; bounded trailing padding is ignored. During USB suspend or unconfigured state, input switches to current-state tracking: on resume, currently held controls and latest absolute axes publish while stale motion and taps are discarded.
- **Unsupported syntax**: HID delimiters, reserved long items, and Buffered Bytes fields are unsupported; malformed descriptors fail profile setup explicitly. BLE keeps up to 3 separate HID service maps.

Report decoding follows [USB HID 1.11](https://www.usb.org/sites/default/files/hid1_11.pdf) and [USB HID Usage Tables 1.7](https://usb.org/sites/default/files/hut1_7.pdf).

## Composite USB Interfaces and Boot Keyboard

Cordial presents a single full-speed composite USB device:

- Two CDC interfaces for the serial API (always present).
- One shared desktop HID interface (keyboard, mouse, Consumer, system, radio) when configured devices need it.
- One interface + IN endpoint (22-byte report) per exposed HID gamepad slot.
- One contiguous `FF/5D/81` XInput receiver function with Microsoft OS 2.0 `XUSB20` compatible ID (Windows 8.1+, no Cordial service needed) when XInput channels are exposed.
- One Raw HID interface (IN + OUT, 32-byte reports) when VIA or Vial is active (placed after gamepad functions).

The desktop HID interface advertises the **boot keyboard** subclass/protocol. In report protocol it sends full NKRO bitmaps and all desktop reports; in boot protocol it sends standard 8-byte reports (modifiers + up to 6 keys, emitting ErrorRollOver above 6 while keeping the underlying bitmap intact). Switching protocols reconciles current input; USB reset restores report protocol; lock LEDs and idle reporting work in both modes. Boot mouse protocol is not implemented.

## USB Control Requests

- **Standard validation**: Requests validate full endpoint addresses (direction, reserved bits, active configuration/alternate setting, assigned address) before dispatch. `GET_STATUS`, endpoint HALT, `SET_ADDRESS`, `SET_CONFIGURATION`, and `SET_INTERFACE` check setup fields and state, stalling invalid requests. Full-speed only (`DeviceQualifier` and `OtherSpeedConfiguration` stall).
- **Endpoint HALT and reconfiguration**: Cordial-owned USB wrappers on RP2040, RP2350, and ESP32-S3 preserve queued transfers across `SET_FEATURE`/`CLEAR_FEATURE(ENDPOINT_HALT)`, resetting `DATA0` on clear without flushing or replaying completed packets. `SET_CONFIGURATION` and `SET_INTERFACE` clear HALT and reset `DATA0` on selected endpoints (on ESP32-S3, asserting Global OUT NAK on halt and discarding unread pre-reconfiguration OUT packets while holding unread endpoints at NAK). Interrupted HID reports retry cleanly without dropping undelivered input.
- **Raw HID (VIA/Vial)**: Accepts 32-byte commands on interrupt OUT or `SET_REPORT(Output, ID 0)`. `GET_REPORT(Input, ID 0)` returns a 32-byte snapshot of the last completed interrupt IN reply (zero-initialized after reset/reconfiguration) without consuming queued replies or re-executing commands. Rejects other report types/IDs, zero-length `GET_REPORT`, `GET_IDLE`/`SET_IDLE`, and `GET_PROTOCOL`/`SET_PROTOCOL`.
- **Silicon note**: RP2040 endpoint abort recovery relies on B2 or later silicon.

## USB Remote Wake

RP2040, RP2350, and ESP32-S3 support standard USB remote wake without host drivers when USB is configured, suspended, and armed by the host via `SET_FEATURE(DEVICE_REMOTE_WAKEUP)` (actual wake depends on host OS/hub sleep policy).

- **Triggers**: New key/button presses, pointer motion, scrolling, changes to an observed absolute pointer position, Consumer/system/radio inputs, and native gamepad digital presses (even without a USB gamepad slot). Status/battery reports, releases alone, unchanged held inputs, resyncs, and analog axis jitter do not wake.
- **Timing**: Waits >= 5 ms after suspend before driving a nominal 10 ms resume pulse, then waits for fresh host frames before resuming input. A brief tap released during wake can wake the host without replaying the tap.

## USB Allocation

A single allocator chooses exposed USB interfaces at startup and whenever preferences, connections, or profiles change, respecting interface, endpoint, handler, and descriptor limits. Lower numeric priorities win (ties preserve slot order):

| Priority | Candidate |
| --- | --- |
| `0x00` | Serial (both CDC interfaces) |
| `0x41` | Shared keyboard/mouse interface required by a connected device |
| `0x42` | HID gamepad slot selected by a connected device |
| `0x43` | XInput channel selected by a connected device |
| `0x44` | Enabled editor whose selected profile is used by a connected device |
| `0x81` | Shared keyboard/mouse interface needed only by disconnected configured devices |
| `0x82` | Configured HID slot 0 (when no HID devices are connected) |
| `0x83` | Configured XInput channel 0 (when no XInput devices are connected) |
| `0xC2` | Additional configured HID slots with no connected user |
| `0xC3` | Additional configured XInput channels with no connected user |
| `0xC4` | Enabled editor with no connected device using its profile |
| `0xE2` | Unconfigured extra HID slots (up to compiled max) |
| `0xE3` | Unconfigured extra XInput channels (up to compiled max) |
| `0xFF` | Disabled editor (excluded) |

Rules:

- Valid preferences always save even when capacity omits their interface; saved preferences are never rolled back on reconfiguration failure.
- USB detaches and rebuilds **only when the active interface set changes**, keeping Bluetooth links and held input alive and draining `SessionEnding` first (see [Intentional session endings](transport.md#intentional-session-endings)).
- Omitting any candidate `< 0xC0` sets `ERROR_CODE_USB_REQUIRED_CAPACITY_EXCEEDED`; omitting any in `0xC0..0xDF` sets `ERROR_CODE_USB_RESERVED_CAPACITY_EXCEEDED`; omitting `0xE0..0xFE` reports no error.

## Standard Gamepad Input

The HID compiler recognizes Generic Desktop Joystick (`01:04`) and Game Pad (`01:05`) applications:

- **Buttons**: Button page usages `1..32` (variable or selector), preserving exact usage numbers.
- **Axes**: Absolute variable X, Y, Z, Rx, Ry, Rz (`01:30..35`) and Simulation Brake (`02:C5`) / Accelerator (`02:C4`), linearly normalized to `0..65535` (separate validity bits so unreported/null axes are not mistaken for zero).
- **Hat and explicit controls**: 4-way or 8-way Hat Switch (`01:39`, clockwise from north; out-of-range releases), explicit D-pad Up/Down/Right/Left (`01:90..93`), Start (`01:3D`), Select (`01:3E`), nested System Main Menu (`01:85`), and up to 8 active Consumer usages inside the Game Pad application.

State handling:

- `Map::decode` retains raw gamepad payloads per report ID and service (at most 8,208 bytes/map; keyboard/mouse allocations stay unchanged) and reconstructs a 48-byte `hid::Gamepad` snapshot without allocating.
- Wholly vendor-defined gamepad report IDs are tracked in a 32-byte ignore bitmap and return `hid::Error::Ignored` without clearing held controls or marking first input; the Xbox One S trailing NUL byte is accepted.
- Gamepad applications never directly forward keyboard/mouse/Consumer reports (use profile remaps for cross-collection output). No curves, deadzones, sensitivity, rumble, or proprietary console init protocols are implemented.

## Lock Indicators and BLE Output Reports

- **Lock indicators**: Num/Caps/Scroll Lock, Compose, and Kana support Output and writable Feature reports, bitmaps, selectors, signed on/off, unsigned toggles, Usage Selected/In Use, Multi Mode, Color selectors/bitmaps, RGB + intensity channels, and Ordinal instances. Fresh reads preserve unrelated fields; relative RGB/intensity writes use exact rational conversion against a paired absolute counterpart. Controls lacking usable neutral/visible states, sufficient selector slots, or required absolute feedback remain unavailable.
- **BLE output reports**: Prefer GATT Write Without Response when advertised and within MTU, falling back to acknowledged writes. Feature reports keep their standard write path.

## USB Gamepad Outputs

RP2040 and RP2350 boards expose up to 4 HID and 4 XInput slots (ESP32-S3 exposes none). Both modes encode the same post-profile mapped HID state:

- **HID (22-byte report)**: 32 buttons, signed 16-bit X/Y/Z/Rx/Ry/Rz (centered when invalid), unsigned 16-bit Brake/Accelerator (`0` when invalid), explicit D-pad, Start, Select, and an 8-way hat reconstructed from mapped D-pad (opposing axes cancel in the hat while explicit D-pad preserves both).
- **XInput**: Fixed mapping from canonical HID controls -- Buttons `1/2/5/4/7/8/11/12/14/15/13` to `A/B/X/Y/LB/RB/Back/Start/LS/RS/Guide` (explicit Start/Select also map to Start/Back), mapped D-pad to XInput D-pad, X/Y to left stick, Rx/Ry to right stick (centered on invalid, Y inverted), and Brake/Accelerator (`value / 257`) to `0..255` triggers. Follows [xinput-device](https://github.com/9names/xinput-device/blob/main/xinput-device/src/xinput.rs), [Linux `xpad`](https://github.com/torvalds/linux/blob/master/drivers/input/joystick/xpad.c), and [Microsoft OS 2.0 descriptors](https://learn.microsoft.com/en-us/windows-hardware/drivers/usbcon/microsoft-defined-usb-descriptors).

### Builtin Controller Mappings

When a controller has **no assigned profiles**, firmware matches exact compiled input geometry (single gamepad service, exact report IDs/offsets/widths/ranges/flags, plus freshly read BLE PnP ID where required) against four builtin layouts in `rust/crates/cordial-core/src/gamepad_profiles.rs`:

- **Luna BLE**: Report `0x01` (16 B), 16-bit X/Y/Z/Rz sticks, 10-bit Brake/Accelerator, hat, sparse buttons; routes `Z/Rz -> Rx/Ry`, Consumer AC Back -> Back, report `0x02` AC Home -> Guide. Credits: [ControllersInfo Luna capture](https://github.com/DJm00n/ControllersInfo/blob/3013812fa478e09e70de7dde77b5e74919829177/lunacontroller/luna_bluetoothle_hid_report_descriptor.txt), [SDL Luna decoder](https://github.com/libsdl-org/SDL/blob/release-3.2.0/src/joystick/hidapi/SDL_hidapi_luna.c).
- **Xbox Bluetooth Simulation**: Report `0x01` (16 B), 16-bit X/Y/Z/Rz sticks, 10-bit Brake/Accelerator, hat, 15 buttons; routes `Z/Rz -> Rx/Ry`, buttons `1/2/4/5 -> A/B/X/Y`, AC Back/Home -> Back/Guide. Credits: xpadneo [Series](https://github.com/atar-axis/xpadneo/blob/v0.10.5/docs/descriptors/xbxs.md) & [One S Linux](https://github.com/atar-axis/xpadneo/blob/v0.10.5/docs/descriptors/xb1s_linux.md) descriptors, [SDL Xbox decoder](https://github.com/libsdl-org/SDL/blob/release-2.30.9/src/joystick/hidapi/SDL_hidapi_xboxone.c#L843-L898).
- **Compact Xbox**: Report `0x01` (15 B), X/Y and Rx/Ry sticks, 10-bit Z/Rz triggers, hat, 10 buttons; routes `Z/Rz -> Brake/Accelerator`, buttons `1..10 -> A..RS`, report `0x02` System Main Menu -> Guide. Credit: xpadneo [One S compact descriptor](https://github.com/atar-axis/xpadneo/blob/v0.10.5/docs/descriptors/xb1s_windows.md).
- **DS4 Basic Bluetooth**: Report `0x01` (9 B), 8-bit X/Y/Z/Rz sticks, hat, 14 buttons, 6 pad bits, 8-bit Rx/Ry triggers; simultaneously routes `Z/Rz -> Rx/Ry` and `Rx/Ry -> Brake/Accelerator`, buttons `2/3/1/4/5/6/9/10/11/12/13 -> A..Guide`, disables digital L2/R2 (`7/8`) and touchpad click (keeping analog triggers). Credits: [hid-tools DS4 descriptor](https://github.com/bentiss/hid-tools/blob/0579ecdb4e041ce93c18ca45b3e8a2584f9ce952/hidtools/device/sony_gamepad.py#L716-L900), [Linux `hid-playstation`](https://github.com/torvalds/linux/blob/v6.12/drivers/hid/hid-playstation.c#L2288-L2300).

Unrecognized controllers pass their raw HID usages through unchanged. Assigning any user profile disables builtin matching and passes original controller inputs to the profile layers.

### Slot Transmission

Each USB slot runs an independent writer with a 16-entry digital transition queue and coalesced analog axes (non-allocating; never blocks keyboard, mouse, or serial). If a stalled host fills a slot's queue, firmware clears history, emits neutral digital controls, and restores current state. On Pico, transfer completion polls every 125 us for the first 8 ms, then every 8 ms while idle, resetting to fast checks on each new transfer.
