# Records

[Protocol index](README.md)

Field definitions are in [`proto/cordial.proto`](../../proto/cordial.proto). This page explains
what the records mean and how their values are chosen.

## Status

`Status` identifies the Dongle and says what it can do: its adapter ID (the USB serial number), the
effective name, the saved host platform, whether Bluetooth and storage have started (`ready`), and
one `TransportSupport` per transport the firmware supports, with `max_enabled` and whether the
transport is enabled (`enabled`, always set; missing means enabled, as firmware that predates the
field omits it). Classic starts disabled and BLE enabled. A disabled transport's devices are
inactive, and the Dongle does not scan, connect or accept connections on it.
`Status.info` holds facts about the Dongle with keys from the [catalog](keys.md): the firmware
version, the hardware configuration (`board.name`), `build.development` on development firmware,
and `storage.full` when there is no room to save another device. The `adapter` event carries a
`Status` whenever any of this changes.

The host platform selects special-key translation and survives removing every device. Until the
saved adapter settings have loaded, the platform reads as Linux, Classic as disabled and BLE as
enabled.

## Devices

A saved device has an opaque ID, such as `d_000000000000004d`, that stays the same across scans,
address changes the Bluetooth stack resolves, reconnects, sessions and restarts, and across
renewing its bond by pairing it again. An unpair removes it, and an ID is never reused. A
peripheral paired over both Classic and BLE has two records. Names are labels, not identities.

- `name` is the name the device reports when it has reported one, otherwise the name seen at
  pairing. A missing or blank report never replaces a known name.
- `kind` comes from the device's HID descriptor roles when known, otherwise from information the
  device reports. It is unknown after a restart until the device connects again.
- `state` is `connected` only once security, HID report setup and input forwarding are all ready,
  not merely when a radio link exists.
- `inactive` says why the Dongle will not use the device, in this order: its transport is not
  supported by this firmware, its transport is disabled on the Dongle, it is blocked, the device
  is disabled, or the stack's bond table is full.
- `error` is the last failed connection attempt, cleared by a successful connection.
- `security` describes the live link while connected; each property is missing when the stack
  cannot report it, which does not mean false. Values describe the negotiated link and key, never
  the requested policy; for example, Secure Connections Just Works reports `encrypted`,
  `secure_connections` and a 16-byte key but not `authenticated`.
- `roles` lists the kinds of input the HID descriptor produces: keyboard, mouse, consumer controls
  such as media and volume keys, and system controls such as power and sleep.

A `device` event carries the whole record whenever anything in it changes, including battery and
other information; `device_removed` follows an unpair or a lost record.

## Integrations

`Device.integrations` lists each integration the device has been detected with or has a saved
preference for. `enabled` is the saved preference. `detected` is present when the device speaks the
integration on the current link, with the protocol version it reported. `status` is one of:

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
saved to flash, and battery values are cleared on disconnect and are unknown after a restart until
read again.

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
values are used, and a failed preferred read lets a valid fallback through. After a disconnect the
last values remain until new reads replace them. Text is at most 64 bytes without control
characters.

Values a device only reports, such as the backlight's current level and status or the wheel's
resolution multiplier, are information too, decoded by the Dongle. A setting the device does not
accept writes for, on its firmware revision, is reported as information under the same key.

## Settings

`Setting` is one value the Dongle can change on the device and save. Its `type` holds the value last
read from the device, the saved value, and the type's limits: integer ranges or choices, enum
choices, or a text length limit. `value` is current while the setting's integration is active and
is otherwise the last reading, shown as possibly out of date; it is missing when nothing has been
read since the Dongle started. `saved` is missing for a setting the Dongle leaves alone, and then
`status` is missing too.

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

A `settings` event carries the device's full list whenever any setting changes.

## Warnings

`DeviceWarnings` lists the HID inputs and outputs the Dongle cannot translate or update. The device
keeps working; only the identified field is affected. Each warning names a code and where it is:
the HID service on the device, the report type and ID, the field's bit offset and its usage page
and usage. Input and indicator limitations last for the connection; indicator read and write
failures clear at the next successful update. A reconnect replaces the earlier connection's
warnings. A `warnings` event carries the full list whenever it changes. See
[HID forwarding](hid-forwarding.md) for what produces each warning.
