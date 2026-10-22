# Records

[Protocol index](README.md)

Field definitions are in [`proto/cordial.proto`](../../proto/cordial.proto). This page explains
what the records mean and how their values are chosen.

## Status

`Status` identifies the Dongle and says what it can do: its adapter ID (16 uppercase hexadecimal
digits that also begin the USB serial number), the effective name, the saved host platform, whether
Bluetooth and storage have started (`ready`), and one `TransportSupport` per transport the firmware
supports, with `max_enabled` and whether the transport is enabled (`enabled`, always set; missing
means enabled, as firmware that predates the field omits it). Classic starts disabled and BLE
enabled. A disabled transport's devices are inactive, and the Dongle does not scan, connect or
accept connections on it. `Status.info` holds facts about the Dongle with keys from the
[catalog](keys.md): the firmware version, the hardware configuration (`board.name`),
`build.development` on development firmware, and `storage.full` when there is no room to save
another device. The `adapter` event carries a `Status` whenever any of this changes.

The host platform selects special-key translation and survives removing every device. Until the
saved adapter settings have loaded, the platform reads as Linux, Classic as disabled and BLE as
enabled.

`Status.profile_support` says what profile rules this firmware can apply: the input usages and
collections a remap, scale or absolute axis rule can name, the outputs a remap or axis rule can
produce, how many outputs a remap and profiles a layer list can hold, the memory budget for loaded profiles and how much of it is in use. It is missing on
a board configured without profile support, such as the Pico W. `Status.configuration_interfaces`
has one `ConfigurationInterfaceSupport` per configuration interface the firmware supports, enabled
or not: whether it is enabled, its profile, and the interfaces it cannot be enabled alongside.

`Status.output_support` lists implemented USB output backends by role and mode. Unlisted enabled
modes are unsupported. Gamepad Disabled is always accepted, including when no backends are listed.
Each mode reports `max_slots`; `slots` is the enumerated pool size, absent
until known. Disabled has zero capacity and no enumerated slots. Each gamepad HID or XInput pool
has at most four slots. Board compositions advertise modes only when their USB backends are usable.
An actual enumerated pool can differ from the desired pool while USB is being configured. Reset
or suspend makes selected outputs Pending while the enumerated pool sizes remain known.

## Profiles

A profile is a named set of rules on HID usages. Its ID is a positive integer from a sequence
separate from device IDs, stays the same for the profile's life and is never reused. Its name is a
label, not an identity; names need not be unique.

Each rule requires an input usage, its application collection and an effect. A profile has at
most one rule for each input usage and collection together:

- A remap makes an on/off input, such as a key, button, media key or power key, hold a fixed set
  of outputs instead, such as a key with modifiers, a sleep key, or nothing at all.
- A scale multiplies a relative value, such as pointer motion or a wheel, by a ratio; a negative
  ratio inverts it.
- An axis rule routes an absolute Game Pad axis to `destination` in the same Game Pad collection.
  Its `invert` flag reverses the normalized range. It has no deadzones, curves or sensitivity.

An input without a rule passes through unchanged, so a new profile changes nothing.

Each device has its own layers: an ordered list of profiles, empty for a new device. Each profile
applies to the input as the profile before it left it, so a profile that remaps C to B followed by
one that remaps B to A makes C produce A. Scales multiply, and a disabled input reaches no later
profile. An empty list passes everything through.

`Profile.roles` summarizes what a profile's rules change, so a client can show it as a keyboard,
mouse, media, system control or gamepad profile, or several, without reading its rules. Each rule
adds one role, from its explicit input collection; outputs do not count. A rule matching no row
adds none.

| Rule input | Collection | Role |
| --- | --- | --- |
| Keyboard/Keypad usages (`07:xx`) | Keyboard (`01:06`) | Keyboard |
| Button usages (`09:xx`), Generic Desktop X, Y or Wheel (`01:30`, `01:31`, `01:38`) or Consumer AC Pan (`0c:238`) | Mouse (`01:02`) | Mouse |
| Other Consumer usages (`0c:xx`) | Consumer Control (`0c:01`) | Consumer control |
| Other Generic Desktop usages (`01:xx`) | System Control (`01:80`) | System control |
| Supported Game Pad controls and absolute axes | Game Pad (`01:05`) | Gamepad |

A `profile` event carries a profile's record when it is created or copied or its roles change, and
`profile_removed` its ID when it is deleted. `ListProfileRules` lists the rules a page at a time in
input usage page, input usage, collection usage page, then collection usage order. Its `after`
cursor is a `ProfileRuleRef` with both the last rule's `input` and `collection`; omitting the cursor
starts from the beginning.

## Devices

A saved device has a positive integer ID, such as `77`, that stays the same across scans,
address changes the Bluetooth stack resolves, reconnects, sessions and restarts, and across
renewing its bond by pairing it again. An unpair removes it, and an ID is never reused. A
peripheral paired over both Classic and BLE has two records. Names are labels, not identities.

- `name` is the name the device reports when it has reported one, otherwise the name seen at
  pairing. A missing or blank report never replaces a known name.
- `kinds` lists what the device is, such as both a keyboard and a mouse. It comes from the device's
  saved roles when known, where a keyboard role gives keyboard, a mouse role mouse, a gamepad role
  gamepad, and other roles alone other; otherwise from information the device reports on the
  current connection.
- `state` is `connected` only once security, HID report setup and input forwarding are all ready,
  not merely when a radio link exists.
- `inactive` says why the Dongle will not use the device, in this order: its transport is not
  supported by this firmware, its transport is disabled on the Dongle, it is blocked, the device is
  disabled, or the stack's bond table is full.
- `error` is the last failed connection attempt, cleared by a successful connection. The Dongle
  keeps it only while the device is enabled and until it restarts.
- `security` describes the live link while connected; each property is missing when the stack cannot
  report it, which does not mean false. Values describe the negotiated link and key, never the
  requested policy; for example, Secure Connections Just Works reports `encrypted`,
  `secure_connections` and a 16-byte key but not `authenticated`.
- `roles` lists the kinds of input the HID descriptor produces: keyboard, mouse, consumer controls
  such as media and volume keys, system controls such as power and sleep, and gamepads. They are
  saved when the descriptor is read, so they are known while the device is disconnected.
- `profiles` is the device's saved layers, empty for a new device.
- `profile_error` is set while the device is connected without its profiles loaded, because they do
  not fit in the memory budget or one could not be read. Its input then passes through unchanged.

A `device` event carries the whole record whenever anything in it changes, including battery and
other information; `device_removed` follows an unpair or a lost record. `ListDevices` returns saved
devices a page at a time in ascending ID order; a device whose record cannot be read is listed by
ID as an `unreadable` entry.

`Device.output_preferences` holds saved output choices as `role` and `mode`. Only Gamepad is
configurable, with Disabled, HID and XInput choices. A missing gamepad choice means Disabled.
Keyboard and mouse behavior does not depend on this list. Saved choices survive renewing an
existing bond. Unpairing deletes them.

During first-connection setup, a newly paired device with validated gamepad controls automatically
selects HID if the firmware supports it and the saved HID pool has room. Disconnected, disabled,
blocked and nonresident saved HID choices count toward its four-slot limit. Otherwise setup settles
on Disabled. Explicit choices take precedence. Setup saves the choice before applying it and retries
a failed save on a later connection. Completed setup never automatically enables output later.
Gamepad selection does not finish pending HID++ detection. See the
[storage rules](../storage-format.md#devices) for setup persistence.

`Device.outputs` reports output state independently of Bluetooth state and `inactive`:

| State | Meaning |
| --- | --- |
| Unknown | The output state is unknown. |
| Disabled | The selected mode is Disabled. |
| Disconnected | The selected backend is supported and the device has no live connection. |
| Pending | Connection setup or required USB enumeration is incomplete. |
| Active | A supported translation owns an enumerated slot. |
| Unsupported | The selected backend or connected device layout cannot provide translation. |

Only Active carries `slot`, a zero-based temporary slot within its role and mode pool. It is not a
Windows XInput user index. A saved enabled choice whose backend is unavailable reports Unsupported.
A connected layout without supported gamepad controls also reports Unsupported. Connection setup,
unavailable storage and USB setup report Pending. Disabled or blocked devices have no assignment.
Active requires a supported layout, an assigned slot and a ready USB backend.

Each gamepad mode's desired USB pool is the number of paired devices selecting it, capped at four.
HID and XInput have independent pools. Disabled output choices contribute nothing. Disabled or
blocked devices, disabled transports and nonresident records still contribute their saved enabled
choices. Connections and disconnections do not change the desired pools. Pool sizes describe USB
allocation, not the number of Bluetooth connections or support for a particular device layout.

Runtime assignments use the lowest free slot in the selected mode's actual enumerated pool.
Surviving assignments keep their slots when other devices connect or disconnect. Closing a link,
changing its mode, replacing its layout or losing storage readiness releases its controls. A mode
change can move an assignment even when both desired pools remain capped at four. It does not
require USB enumeration when the desired counts stay the same.

The USB owner supplies `outputs::Enumeration` through `Application::set_output_enumeration`, with
an actual `Plan`, a generation that changes when endpoints are rebuilt, and USB readiness. No
capacity is assumed before that call. `Application::gamepad_outputs` reconciles connections and
returns fixed arrays of four HID and four XInput `SlotSnapshot` values. Each has an optional
`LinkId` occupant, a nonwrapping assignment generation and normalized `hid::Gamepad` controls.
An empty slot has neutral controls. Assignment generations change when a slot is assigned again,
including when the same link receives a replacement layout. They are separate from enumeration
generations.

USB class workers own report encoding and transmission. Before sending queued controls, they
compare the current occupant and assignment generation, discard stale work, and send neutral
controls before a replacement occupant's controls. Reading snapshots consumes nothing, so workers
can resend current controls after reset or resume. The application caches controls when reports
arrive and when a slot is assigned. Reconciliation checks at most four connections and eight
slots; surviving assignments do not walk HID fields on each priority poll. The decoder permits
at most sixteen reports and ninety-six fields per map. No USB queues or per-report allocations
are needed by the runtime pools.

## Integrations

`Device.integrations` lists each integration the device has a saved preference for or is detected
with on the current link. `enabled` is the saved preference. `detected` is present when the device
speaks the integration on the current link, with the protocol version it reported. `status` is one
of:

| State | Meaning |
| --- | --- |
| Off | Turned off with `SetDevice`. |
| Disconnected | Turned on, and the device is not connected. |
| Starting | Connected; detecting the integration, reading its settings and applying saved values. |
| Active | Up: its settings have been read and saved values are applied. |
| Unsupported | Connected, and the device cannot use the integration at all. |
| an error code | Connected, and starting failed. |

For HID++, a device qualifies when its HID descriptor has usable HID++ reports. Detection reads the
HID++ version and the device's settings even while HID++ is off; turning it on starts special-key
translation, where the device supports it, and the setting apply, and turning it off resets the
temporary reporting changes translation made. Special-key
translation is never reported on its own: with HID++ on, controls the Dongle can translate are
translated, and the rest behave as the device sends them. Ordinary input never waits for HID++.

## Information

`Device.info` lists what the device reports about itself, from any integration or standard service,
with keys from the [catalog](keys.md); a key whose value is unknown is left out. Nothing here is
saved to flash. Information belongs to the current connection: it is cleared on disconnect and is
unknown until read again on the next connection.

| Connection | Battery source |
| --- | --- |
| BLE or Classic with usable HID++ reports and HID++ on | HID++ |
| Other BLE, including HID++ off | GATT Battery Service |
| Other Classic, including HID++ off | Standard HID battery reports |

No other source fills in a missing or failed battery value. Coarse levels map to full 100,
high/good 75, medium 50, low 20, critical 5, empty 0; a percentage never implies charging. With
several batteries, the explicitly identified main battery is reported, otherwise the lowest known
level, and charging belongs to the same battery.

For other information, valid HID++ values win while HID++ is on; otherwise valid standard BLE
values are used, and a failed preferred read lets a valid fallback through. Text is at most 64
bytes without control characters.

Values a device only reports, such as the backlight's current level and status or the wheel's
resolution multiplier, are information too, decoded by the Dongle. A setting the device does not
accept writes for, on its firmware revision, is reported as information under the same key.

## Settings

`Setting` is one value the Dongle can change on the device and save. Its `type` holds the value last
read from the device, the saved value, and the type's limits: integer ranges or choices, enum
choices, or a text length limit. `value` is current while the setting's integration is active and
is otherwise the last reading on the current connection, shown as possibly out of date; it is
missing when the setting has not been read on the current connection. While the device is
disconnected, the list holds its saved settings, with the limits saved alongside them. `saved` is
missing for a setting the Dongle leaves alone, and then `status` is missing too.

| Status | Meaning |
| --- | --- |
| Pending | Saved, and waiting until the device can take it. |
| Applied | The device reports the saved value. |
| Changed on device | The device reports another value; the next apply writes the saved one back. |
| Unsupported | The device does not support this saved setting. |
| an error code | The last apply failed. |

An enum setting's `value` can hold a choice that is not in `choices`, such as a backlight mode the
device reports but does not accept as a write. Discovery, reads, notifications and applies never
create a saved value; only `SetSettings` does. Unpairing removes a device's saved settings.

A `settings_changed` event follows whenever any setting changes, appears or goes away: it carries
each setting that changed or appeared, whole, and a `SettingRef`, the integration and key, for each
one that went away. Disconnecting, for example, removes the settings that have no saved value and
changes the rest. `ListSettings` lists the settings a page at a time in ascending order of
integration, then key compared bytewise.

## Warnings

`DeviceWarnings` lists the HID inputs and outputs the Dongle cannot translate or update. The device
keeps working; only the identified field is affected. Each warning names a code and where it is: the
HID service on the device, the report type and ID, the field's bit offset and its usage page and
usage. Input and indicator limitations last for the connection; indicator read and write failures
clear at the next successful update. A reconnect replaces the earlier connection's warnings, and the
list is empty while the device is disconnected. `ListWarnings` lists the warnings a page at a time,
and a `warnings_changed` event carries the warnings added and removed whenever the list changes. See
[HID forwarding](hid-forwarding.md) for what produces each warning.
