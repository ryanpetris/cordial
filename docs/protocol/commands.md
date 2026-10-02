# Commands

[Protocol index](README.md)

Each command is one `Request.command` variant; its message in
[`proto/cordial.proto`](../../proto/cordial.proto) documents its fields. This page describes what
each command does. Commands that need Bluetooth or storage return `ERROR_CODE_NOT_READY` until both
have started.

## Validation

A command is refused only when carrying it out would fail or corrupt something: a missing required
field, a value of the wrong type or outside the device's range, an ID that does not exist, a second
pairing, no room to save a new device. A command that can be carried out as given is accepted, even
when the value has no effect in the current state; the Dongle applies it when it can. Clients check
the refusals they can predict before sending (see [Limits and errors](limits-and-errors.md)), and
show the Dongle's error without refreshing when it refuses anyway.

## Adapter

| Command | Result | Behavior |
| --- | --- | --- |
| `GetStatus` | `Status` | Refreshes the free-storage estimate first. |
| `SetAdapter` | `Status` | Partial update of the name, host platform and enabled transports, saved together. A name is 1..64 UTF-8 bytes without control characters, trimmed; `""` restores the firmware default. Transport updates apply in order; one for a transport the firmware does not support returns `ERROR_CODE_UNSUPPORTED` and changes nothing. Any transport may be disabled, including all of them. An unchanged value writes nothing. A platform change reconfigures special-key translation on every connected device with HID++ on and re-applies its saved settings. Disabling a transport closes its links, fails a pairing over it that has not saved its bond with `ERROR_CODE_UNSUPPORTED`, drops it from a running scan (ending the scan when no transport is left), stops reconnecting its devices and declines their connections, and makes its saved devices inactive with `INACTIVE_REASON_TRANSPORT_DISABLED`. Enabling it makes them eligible again. A transport change that is saved but whose saved devices cannot then be read returns `ERROR_CODE_STORAGE_FAILED`. |
| `EnterBootloader` | none | Development firmware only. Refused with `ERROR_CODE_BUSY` while a pairing or setup link is open. After responding, the Dongle stops reading requests, releases held input, and reboots into ROM programming mode (BOOTSEL on Pico, download mode on ESP32-S3) within about 250 ms. Saved data is untouched. |

An `adapter` event follows any change to the name, platform, enabled transports, readiness or
adapter information.

## Discovery and pairing

| Command | Result | Behavior |
| --- | --- | --- |
| `StartScan` | none | Scans the listed transports for 1..60 seconds (10 when zero). Listed transports the firmware does not support or has disabled are left out; when none is left, the scan returns `ERROR_CODE_UNSUPPORTED`. Starting a new scan discards earlier candidates, except one a pairing already captured. |
| `StopScan` | none | Ends the scan early; candidates already found stay usable. |
| `StartPairing` | none | Starts pairing a candidate. Only one pairing runs at a time. |
| `AcceptPrompt` | none | Answers the open prompt, with the passkey or PIN when the step is `EnterCode`. |
| `RejectPrompt` | none | Rejects the open prompt; pairing fails with `ERROR_CODE_REJECTED`. |
| `CancelPairing` | none | Cancels a pairing that has not saved its bond. |

Candidates arrive as `scan_found` events, again whenever a candidate's name, kind or signal
changes, and the scan ends with a `scan_done` event. At most 32 candidates are kept; past that,
`scan_done` reports `truncated`. Candidate IDs stay valid until the next scan starts or the session
ends. Results are candidates, not a promise of HID support. A saved BLE device advertising its
identity alone is not a fresh candidate.

`StartPairing` refuses a blocked saved device, a transport the firmware cannot pair or has
disabled, a full flash (`ERROR_CODE_NO_CAPACITY` with `CAPACITY_REASON_STORAGE`, also reported as `storage.full`) and a
full connection table (`CAPACITY_REASON_CONNECTIONS`). It closes the selected device's own link and
any setup link first, and never an unrelated working device.

Progress arrives as `pairing` events: `connecting`, then `enter_code`, `confirm_code` or
`show_code` when the method needs the user, then `done` with the saved device ID or `failed` with
an error code. Just Works pairing goes straight from `connecting` to `done`. A prompt expires after
30 seconds and the whole pairing after 120.

The bond is committed only once pairing completes. Failure, cancellation, the session ending or a
restart before that leaves every existing bond unchanged. Pairing a device that is already saved
renews its bond in place, keeping its ID, name and preferences.

A newly paired device is saved trusted and unblocked, with no integration enabled; its first
connection turns on each integration it detects. It is saved enabled when fewer than `max_enabled`
devices of its transport are enabled, and disabled otherwise. An enabled device then connects on
its own; there is no separate connect step.

## Devices

| Command | Result | Behavior |
| --- | --- | --- |
| `ListDevices` | `DeviceList` | Every saved device. |
| `GetDevice` | `Device` | One saved device. |
| `SetDevice` | `Device` | Partial update of `enabled`, `trusted`, `blocked` and integration preferences, saved in one write. Turning `enabled` off or `blocked` on closes the device's link. |
| `ConnectDevice` | `Device` | Clears a disconnect pause and starts connecting, or joins an attempt already running. Refused for a disabled or blocked device and while a pairing runs. |
| `DisconnectDevice` | `Device` | Closes the link and pauses automatic reconnection until `ConnectDevice` or a restart. |
| `UnpairDevice` | none | Pauses and disconnects the device, then deletes its bond and settings once the link is gone; `device_removed` follows. |
| `RefreshDevice` | none | Re-reads device information and settings from a connected device. |
| `ListWarnings` | `DeviceWarnings` | The device's current HID warnings. |

`device`, `settings` and `warnings` events report every change. Connection progress and failure
show up in `Device.state` and `Device.error`; a connection attempt times out after 30 seconds and
records `ERROR_CODE_TIMEOUT` without pausing automatic reconnection.

### Enabled, trusted and blocked

Enabled means the device is loaded into the Bluetooth stack's bond table, which lets it reconnect,
be recognized when it connects in, and encrypt its link; a disabled device stays saved but cannot
connect. The table has a fixed size per transport, reported as `TransportSupport.max_enabled` (the
stack's bond table less one entry kept free for pairing). Enabling a device beyond it is refused
with `ERROR_CODE_NO_CAPACITY` and `CAPACITY_REASON_ENABLED`. If firmware with a smaller table starts
with more devices enabled, nothing is disabled or deleted: the lowest device IDs are loaded and the
rest report `INACTIVE_REASON_CAPACITY` until the user disables enough of them.

Separately, the Dongle holds a fixed number of live connections, shared across transports, and
keeps one free for pairing. Any enabled device connects whenever one is free.

Automatic reconnection and incoming connections need an enabled, trusted, unblocked device without
a disconnect pause. While eligible BLE devices are disconnected, the Dongle uses the controller's
accept list to connect whichever advertises first; a sleeping device holds no connection. Classic
devices get timed attempts, the first two seconds after a connected device goes away cleanly. A
connected BLE device that goes away, such as by sleeping or being switched off, may reconnect at
once. From its third consecutive drop within a second of connecting, it waits one second, doubling
up to five seconds. Every failure, including an authentication failure or a device the Dongle
cannot use, backs off from two seconds to five minutes and records its error on the device; only
the user's own settings stop reconnection. An explicit connect starts the backoff again from two
seconds. Discovery runs beside accept-list reconnection when the controller supports scanning
while initiating; otherwise BLE discovery and reconnection alternate in one-second windows.

Untrusting a device prevents future unattended connections without closing the current one.
Blocking keeps the bond and any disconnect pause. All of this is enforced on the Dongle without a
client.

### Lost records

Every saved device has a bond. If a device's saved record turns out to be missing, undecodable or
holding a bond that does not belong to it, at startup or when the Dongle next reads it, the Dongle
deletes the device and its settings as an unpair would and sends `device_removed`; the user pairs it
again as a new device.

A storage read that fails is an error, never proof that data is missing: the operation that needed
the data fails with `ERROR_CODE_STORAGE_FAILED` and nothing is deleted. A read error while loading at
startup leaves storage not ready, so `Status.ready` stays false.

## Settings

| Command | Result | Behavior |
| --- | --- | --- |
| `ListSettings` | `DeviceSettings` | Every setting the Dongle knows for the device, across integrations: those read this boot plus every saved value. Sends nothing to the device. |
| `SetSettings` | `DeviceSettings` | Saves one or more values in one storage write. |
| `ForgetSettings` | `DeviceSettings` | Removes one or more saved values in one storage write. |

A valid value is always saved, whatever the device's current state: disconnected, busy, its
integration off, or in a mode where the value does not apply. It stays pending until an apply writes
it. If any change in a request is invalid, nothing is saved. A forgotten setting is never written to
the device again; the device keeps whatever value it has.

### Applying saved settings

The Dongle's saved settings are the truth for the device, and an apply syncs them to it. One routine
does every apply, for one integration: it reads the device's current values, skips those already
equal to the saved ones, writes the differences one setter at a time (fields that share a setter
go out together), and reads back to confirm. Only saved settings are written.

The routine runs when an integration comes up (on connect, or when it is turned on), when the
adapter platform changes, and after every `SetSettings` or `ForgetSettings` for each integration in
the request, once the command has responded. A failure sets the affected settings' status to an
error code in a `settings` event, keeps their saved values, and is tried again at the next apply.
Different setters and integrations are separate device writes, so one failing does not undo the
others. A write the device does not answer is reported as `ERROR_CODE_TIMEOUT`; it may or may not
have taken effect, and the next apply settles it by reading first.

A setting that only applies while another has a particular value is still saved: a configured
backlight level saved while the mode is automatic waits as pending and is written once the mode
becomes permanent manual. If the user changes a saved setting on the device itself, the setting
shows `SETTING_STATE_CHANGED_ON_DEVICE` until the next apply writes the saved value back.

See [HID++](../hidpp.md) for the device operations behind each HID++ setting.

## Development

See [Development commands](development.md).
