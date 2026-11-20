# Input Profiles

Input profiles let the adapter remap, scale, or reroute HID inputs before combining them and sending them over USB. Everything runs on the adapter with no host software required after configuration. The host OS's keyboard layout still interprets the resulting USB keycodes.

Profiles are supported on RP2350 and ESP32-S3 boards (`pico2_w`, `pimoroni_pico_plus2_w`, `waveshare_rp2350b_plus_w`, and `esp32s3`). The Pico W omits profile support to preserve RAM for Bluetooth connections.

## How Profiles Work

A profile is a named, sparse collection of rules keyed by HID usage and application collection. Profiles are not bound to a specific device type: the adapter applies whichever rules match the inputs a device produces. Any input without a matching rule passes through unchanged, so a newly created profile changes nothing.

Three rule types are supported:

- **Remap**: Replaces a held on/off input (key, mouse button, media key, system control, or gamepad button/D-pad/Start/Select) with a fixed set of held outputs, or disables it. Outputs can cross application collections -- for example, mapping a gamepad button to a mouse button or a keyboard shortcut chord.
- **Scale**: Multiplies a relative input (such as pointer motion or a scroll wheel) by a rational ratio `N/D`. A negative numerator inverts the direction.
- **Axis**: Routes an absolute gamepad axis to another axis within the Game Pad collection, with an optional `invert` flag that reverses its normalized range. Axis rules do not add deadzones, curves, or sensitivity scaling.

### Collections and Roles

Every rule explicitly specifies both its input usage and its application collection (`PAGE:USAGE@COLLECTION_PAGE:COLLECTION_USAGE`). The adapter never guesses a collection from a usage number -- for instance, Mouse button 1 (`09:01` in Mouse `01:02`) and Game Pad button 1 (`09:01` in Game Pad `01:05`) are distinct inputs. A profile holds at most one rule per `(input, collection)` pair.

An output collection can be omitted only when a single advertised output collection carries that usage; the adapter resolves and stores the explicit collection.

The adapter derives a profile's displayed roles (keyboard, mouse, consumer control, system control, gamepad) strictly from its rules' input collections, never from outputs. Remapping a gamepad button to a mouse button gives the profile the gamepad role.

Clients inspect `Status.profile_support.ranges` (keyed by `UsageRange.Kind`, collection, and usage) to see which inputs and outputs the firmware accepts for remap, scale, and axis rules. Saving a no-op rule -- mapping an input only to itself in the same collection, scaling by `1`, or routing an axis to itself without inversion -- deletes that rule instead.

## Builtin Controller Mappings

When a controller connects with **no assigned profiles**, firmware automatically applies a builtin mapping if its HID descriptor matches a recognized controller layout (Xbox Bluetooth, DualShock 4, or Amazon Luna).

- Assigning **any** profile -- including an empty profile or one with only keyboard or mouse rules -- disables the builtin mapping and feeds the controller's raw inputs into the profile layers.
- Assign an empty profile if you want unmapped raw gamepad passthrough; remove all profiles to restore automatic builtin mapping.
- HID and XInput outputs share the same mapped state; XInput simply encodes the canonical HID controls with a fixed layout. Builtins live only in firmware and never appear in the user profile list.

## Layers and Memory Budget

Each paired device has an ordered list of profile **layers** (empty by default). Layers run sequentially:

- Each layer receives the output of the previous layer (if layer 1 remaps C to B and layer 2 remaps B to A, pressing C sends A).
- Remapped outputs carry their output usage and collection into the next layer.
- Scale ratios across layers multiply together.
- Within a single profile, axis rules read the axes entering that profile simultaneously, so two rules (`X -> Y` and `Y -> X`) swap axes cleanly without overwriting each other.

Saved profiles are limited only by flash space, while **loaded** profiles share a RAM budget configured per board (40 KiB on supported presets). A profile is loaded into RAM only while used by at least one connected device or an active external editor, and multiple devices share a single loaded copy.

- When a device connects, the adapter loads its layers from flash before forwarding its first input.
- If a device's layers do not fit in the remaining memory budget, none of its profiles load, its input passes through unchanged, and Cordial notifies the user (and warns when budget usage reaches 85%).
- Layer changes and rule edits take effect on the next input; any key or button already held keeps its original outputs until released.

## Managing and Deleting Profiles

The desktop app and TUI manage profiles, per-device layer ordering, and VIA/Vial interface assignments, while rule editing happens through VIA, Vial, or the CLI (see [Profile commands](protocol/clients.md#profile-commands)).

Assigning a profile to a configuration interface exposes it to an external editor without applying it to any device.

The adapter refuses `DeleteProfile` while a profile is still referenced by any device layer or configuration interface. When you delete an in-use profile from Cordial:

1. Cordial removes the profile from each affected device's layers (preserving the order of remaining layers).
2. Cordial turns off and clears every configuration interface pointing to it, waiting for the adapter to reconnect if turning off an active USB interface triggers a USB rebuild.
3. Once no device or interface references the profile, Cordial deletes it.

In the desktop app and TUI, these steps are listed in order for confirmation. If dependencies change or a step fails mid-way, deletion stops cleanly with completed steps kept and remaining steps shown for review; a disconnect before the final delete is confirmed is reported as an unknown outcome rather than retried automatically.

## External Editors (VIA and Vial)

Configuration interfaces expose a single chosen profile over a 32-byte USB Raw HID interface so you can edit keyboard, mouse, media, and system control remaps visually:

- **VIA** (protocol 9): Load [`configs/keyboards/cordial-via.json`](../configs/keyboards/cordial-via.json) as a VIA v2 definition in VIA's **Design** tab. Remap uses the same interface and definition.
- **Vial** (protocol 6): When Vial is active, the adapter appends `-vial:f64c2b3c` to its USB serial number so Vial discovers it automatically and loads the embedded definition generated from [`configs/keyboards/cordial-vial.json`](../configs/keyboards/cordial-vial.json). After editing that JSON file in development, regenerate its compressed bytes with `python3 tools/generate_vial_definition.py`.

VIA and Vial share a Raw HID usage and conflict with each other, so only one can be enabled at a time. When both are disabled, no editor USB interface is allocated and all profiles continue translating input normally. Saved enablement is independent of active USB exposure; if higher-priority USB interfaces occupy the available slots, the preference stays saved and USB reconnects only when the active interface allocation changes (see [USB allocation](protocol/hid-forwarding.md#usb-allocation)).

### Virtual Matrix and Keycode Limits

Both editors present a stable 14x16 virtual matrix mapping input `(usage, collection)` pairs (not physical keyboard switch locations):

- Keyboard/Keypad usages `0x04..0xA4` and the eight modifiers `0xE0..0xE7` (collection `01:06`)
- Seven standard media keys (Next, Previous, Stop, Play/Pause, Mute, Volume Up, Volume Down) and 16 additional QMK basic Consumer controls (collection `0c:01`)
- System Power Down, Sleep, and Wake Up (collection `01:80`)
- Mouse buttons 1 through 16 (collection `01:02`)
- The final 13 matrix positions are unused, read as disabled, and ignore writes.

Key editing semantics:

- **Single-profile view**: Editors display only the selected profile's own remap rules, not stacked device layers. Gamepad, scale, axis, and other non-matrix rules remain intact during normal edits.
- **No-op and reset**: Unmapped inputs read as their own usage (or transparent if unrepresentable). Mapping an input to itself or transparent forgets its rule. **Resetting the keymap clears every rule in the profile**, including gamepad, scale, and axis rules outside the matrix.
- **Keycode tables**: VIA (protocol 9) uses legacy QMK keycodes (mouse buttons 1..5); Vial (protocol 6) uses current QMK keycodes (mouse buttons 1..8). Supported editor actions are disabled, single keyboard keys, the System/Consumer controls above, mouse buttons, and QMK left/right modifier + key chords.
- **Unrepresentable rules**: Rules created via the CLI that an editor cannot encode (such as mouse buttons 9..16 as outputs, extra Consumer usages, or modifier + mouse chords) keep working on the adapter untouched, but reading a keymap chunk containing one fails in that editor (returning `0xFF` for that chunk). Enabling Vial reports the interface as unlocked.

### Saving, Limits, and Error Handling

- **Immediate apply, batched flash write**: Supported editor packets update loaded rules atomically and take effect immediately. Edits flush to flash once editing pauses, within about 2 seconds, and before USB re-enumeration or bootloader entry (see [When files are written](storage-format.md#when-files-are-written)). Power loss before flush can lose recent edits.
- **Rejection and retry**: Invalid actions or edits exceeding the profile memory budget return VIA's `0xFF` unhandled response without changing runtime rules. If a deferred flash write fails after acknowledgement, the edit stays active and retries in the background (reporting `STORAGE_FULL` if flash is full, or marking storage unready on an unknown write outcome). Multi-packet imports are not atomic; read the keymap back to verify.
- **Consumer chord limit**: USB Consumer reports carry at most 8 simultaneous distinct usages across all connected devices (including remaps). Exceeding 8 triggers an input-overflow disconnect rather than silently dropping part of a chord.
- **Raw HID control reports**: Commands reply over interrupt IN; `GET_REPORT` returns a snapshot without consuming a reply (see [USB control requests](protocol/hid-forwarding.md#usb-control-requests)).

### Linux Permissions

External editors access the adapter's `/dev/hidraw*` node. Arch and Debian/Ubuntu packages install [`configs/50-cordial.rules`](../configs/50-cordial.rules) to grant access to the active seat user automatically. Otherwise, copy `50-cordial.rules` to `/etc/udev/rules.d/`, reload udev rules, and reconnect the adapter.

### Protocol References and Credits

- Editor protocols: [QMK VIA commands](https://github.com/qmk/qmk_firmware/blob/master/quantum/via.h), [Vial VIA implementation](https://github.com/vial-kb/vial-qmk/blob/vial/quantum/via.c), and [Vial extensions](https://github.com/vial-kb/vial-qmk/blob/vial/quantum/vial.c).
- Controller research: Builtin gamepad mappings draw on HID descriptor captures and documentation from [ControllersInfo](https://github.com/DJm00n/ControllersInfo) by [DJm00n](https://github.com/DJm00n) and contributors.
