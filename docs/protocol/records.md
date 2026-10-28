# Records

[Protocol index](README.md)

Field definitions are in [`proto/serial/v1/cordial.proto`](../../proto/serial/v1/cordial.proto). This page explains
what the records mean and how their values are chosen.

## Status

`Status` identifies the Dongle and reports its readiness, saved host platform, supported transports,
output backends, profiles, and configuration interfaces. The adapter ID is 16 uppercase hexadecimal
digits and begins the USB serial number. Classic starts disabled and BLE enabled. A disabled
transport does not scan, connect, or accept connections. Until saved adapter settings load, the host
platform reads as Linux and the transport defaults apply.

`Status.information` holds optional `firmware_version`, `board_name`,
`development_build`, `storage_format`, and `target_storage_format`, each with
independent presence. Firmware always sets `target_storage_format`, including
when storage is blocked or unreadable. Production firmware reports
`development_build = false`. The current `storage_format` is the version declared
by the committed `/format.pb`, omitted when the root is missing, malformed, or unreadable. It does
not prove dataset validity or readiness. `target_storage_format` is the operating
format required by the firmware, currently `2`, independently of release version.
A radio failure does not make a known root format unknown.
`Status.errors` is the complete current set of adapter problems as full `Error`
messages, ordered and deduplicated by complete error as defined in
[Errors](limits-and-errors.md#errors). Empty clears all adapter problems. An `adapter` event
replaces the complete `Status`, including its errors and resource counts.

`ready` means Bluetooth and storage are both usable. False gives no claim about which subsystem
is ready, and pairing remains blocked while storage is unavailable. Ordinary startup reports no
error merely because `ready` is false. A radio fault reports its known cause with reason `RADIO`,
or code `NOT_READY` when only unavailability is known. Storage initialization or access faults
report `STORAGE_FAILED` with reason `STORAGE`. Each source error clears independently when its
subsystem recovers. An unresolved save reports outcome `UNKNOWN` and keeps storage not ready
until its outcome is resolved or a restart successfully reloads saved state; radio recovery does
not clear it. Storage readiness and storage space are separate: `STORAGE_FULL` uses reason
`STORAGE`, follows its own clearing rule, and does not make storage unready.

After a compatible permanent hello and `GetStatus`, clients verify the presence and exact support
of `target_storage_format` before any other command. An absent or zero
target prevents migration and normal commands, even when information is present
and the adapter reports readiness. Assisted mode reports `ready = false`
and `STORAGE_MIGRATION_REQUIRED` with reason `STORAGE`, outcome `NO_EFFECT`, and no related
errors. This exact error authorizes assisted migration only when there are no
other errors. `STORAGE_FAILED` blocks it regardless of outcome. The informational
`storage_format` never selects migration, its source, reconnect behavior, or success.
The downloaded `/format.pb` selects the source and the host validates the complete
dataset before writes. Without the marker, a fresh client reports readiness and
errors without file access, even when the informational formats differ or are absent.

Assisted mode and its migration-required marker remain latched until hardware reset,
including after intermediate and final root uploads. A fresh host can therefore
validate an already converted dataset and restart without conversion or file writes.
Root telemetry failures report `STORAGE_FAILED` and do not authorize migration.
Normal operation never reports the migration-required marker.
Blocked mode allows `GetStatus` and `Restart`. General
`Restart` acknowledges before resetting the MCU and reruns ordinary startup
without a target or strict dataset validator. An active migration worker confirms
success after reconnecting by stable ID only when the target is unchanged, the
marker is absent, and `ready = true`, with no errors other than the normal warnings
`STORAGE_FULL`, `PROFILE_MEMORY_LOW`, `USB_REQUIRED_CAPACITY_EXCEEDED`, and
`USB_RESERVED_CAPACITY_EXCEEDED`, each without related errors. While normal startup
is unready with nothing but those warnings, an active worker continues status checks
without file access. See [storage migration](commands.md#file-access-and-storage-migration).

### Resource capacities

Every resource entry is a `ResourceCapacity` with a `ResourceType` and independently optional
`count` and `limit`. An absent count is unknown, not zero. An absent limit is unknown, not
unlimited. Explicit zero remains zero. Each containing object reports at most one entry per type;
clients ignore unknown types and leave missing types unreported. The containing object defines
what is counted and its units:

| Container | Type | Count | Limit |
| --- | --- | --- | --- |
| `Status.resources` | `INTERFACES` | USB interfaces actually exposed, including each interface of a multi-interface function | Compiled USB interface maximum |
| `Status.resources` | `STORAGE` | Best-effort filesystem allocation in use, in raw bytes | Total bytes in the mounted filesystem |
| `TransportSupport.resources` | `DEVICES` | Saved devices enabled for that transport, even while the transport is disabled; excludes pairing | Enabled-device allowance, equal to the normal connection allowance |
| `TransportSupport.resources` | `CONNECTIONS` | Occupied normal connection slots, including connecting and closing slots until freed; excludes pairing | Normal connection allowance, equal to the enabled-device allowance |
| `OutputModeSupport.resources` | `SLOTS` | Enumerated pool size for that role and mode, absent until known | Compiled slot maximum, at most four per gamepad mode |
| `ProfileSupport.resources` | `MEMORY` | Bytes used by loaded profiles, shared by ID | Shared loaded-profile budget in bytes |
| `ProfileSupport.resources` | `OUTPUTS` | Omitted | Maximum outputs in one remap and after every applied layer for one held input |
| `ProfileSupport.resources` | `LAYERS` | Omitted | Maximum profile IDs in one device's layer list |

Storage usage is the mounted filesystem size minus its best-effort free-space estimate. It includes
filesystem overhead, does not subtract pairing or maintenance reserves, and is not an exact sum of
file lengths. The total excludes firmware, guard regions, and the RP2350 E10 reservation. It uses
the mounted filesystem's actual block count, which can be smaller than the detected flash allows.
A failed free-space measurement omits `count` while a known `limit` remains available. `GetStatus`
refreshes these readings. Existing recounts after storage mutations or generation changes can send
an `adapter` event when a reading or its presence changes, even without a `STORAGE_FULL` change.
Updates are best effort and do not promise a notification for every write.

Saved enabled-device count can exceed its allowance; the saved preferences remain intact.
Connection counts describe occupied slots, not just fully connected devices. Firmware reports its
current transport allowances; a client does not infer them from board names or USB slot counts.
The allowances are independent per transport, so Classic enablement does not lower BLE capacity.
One additional pairing connection is reserved overall, outside both transport allowances. See
[Bluetooth capacity](limits-and-errors.md#bluetooth-capacity) for the configured board limits.

`Status.profile_support.ranges` describes supported rule inputs and outputs by kind, collection,
and usage. A board without profile support, such as Pico W, omits `profile_support` and reports
no configuration interfaces. Each `ConfigurationInterfaceSupport` separates the saved `enabled`
preference from `active`, which says whether USB currently exposes it. Its profile stays selected
while disabled or omitted for lack of USB capacity. `conflicts` names interfaces that cannot be
enabled together.

`Status.output_support` lists implemented output backends by role and mode. Unlisted enabled
modes are unsupported. Gamepad Disabled is always accepted, including when no backends are listed;
its `SLOTS` count and limit are both zero. USB reset or suspend can make device outputs Pending
while their enumerated slot counts remain known. USB resources other than interfaces can limit an
allocation even when `INTERFACES.count` is below `INTERFACES.limit`. See
[USB allocation](hid-forwarding.md#usb-allocation) for selection priorities and reporting.

## Profiles

A profile is a named set of rules on HID usages. Its ID is a positive integer from a sequence
separate from device IDs, stays the same for the profile's life, and is never reused. Its name is a
label, not an identity; names need not be unique.

Each rule requires an input usage, its application collection, and an effect. A profile has at
most one rule for each input usage and collection together:

- A remap makes an on/off input, such as a key, button, media key, or power key, hold a fixed set
  of outputs instead, such as a key with modifiers, a sleep key, or nothing at all.
- A scale multiplies a relative value, such as pointer motion or a wheel, by a ratio; a negative
  ratio inverts it.
- An axis rule routes an absolute Game Pad axis to `destination` in the same Game Pad collection.
  Its `invert` flag reverses the normalized range. It has no deadzones, curves, or sensitivity.

An input without a rule passes through unchanged, so a new profile changes nothing.

Each device has its own layers: an ordered list of profiles, empty for a new device. Each profile
applies to the input as the profile before it left it, so a profile that remaps C to B followed by
one that remaps B to A makes C produce A. Scales multiply, and a disabled input reaches no later
profile. An empty list passes everything through.

`Profile.roles` summarizes what a profile's rules change, so a client can show it as a keyboard,
mouse, media, system control, or gamepad profile, or several, without reading its rules. Each rule
adds one role, from its explicit input collection; outputs do not count. A rule matching no row
adds none.

| Rule input | Collection | Role |
| --- | --- | --- |
| Keyboard/Keypad usages (`07:xx`) | Keyboard (`01:06`) | Keyboard |
| Button usages (`09:xx`), Generic Desktop X, Y, or Wheel (`01:30`, `01:31`, `01:38`), or Consumer AC Pan (`0c:238`) | Mouse (`01:02`) | Mouse |
| Other Consumer usages (`0c:xx`) | Consumer Control (`0c:01`) | Consumer control |
| Other Generic Desktop usages (`01:xx`) | System Control (`01:80`) | System control |
| Supported Game Pad controls and absolute axes | Game Pad (`01:05`) | Gamepad |

A `profile` event carries a profile's record when it is created or copied or its roles change, and
`profile_removed` its ID when it is deleted. `ListProfileRules` returns all rules in one response,
ordered by input usage page, input usage, collection usage page, then collection usage.

## Devices

A saved device has a positive integer ID, such as `77`, that stays the same across scans,
address changes the Bluetooth stack resolves, reconnects, sessions, and restarts, and across
renewing its bond by pairing it again. An unpair removes it, and an ID is never reused. A
peripheral paired over both Classic and BLE has two records. Names are labels, not identities.

- `name` is the name the device reports when it has reported one, otherwise the name seen at
  pairing. A missing or blank report never replaces a known name.
- `kinds` lists what the device is, such as both a keyboard and a mouse. It comes from the device's
  saved roles when known, where a keyboard role gives keyboard, a mouse role mouse, a gamepad role
  gamepad, and other roles alone other; otherwise from information the device reports on the
  current connection.
- `state` is `connected` only once security, HID report setup, and input forwarding are all ready,
  not merely when a radio link exists.
- `inactive` says why the Dongle will not use the device, in this order: its transport is not
  supported by this firmware, its transport is disabled on the Dongle, it is blocked, the device is
  disabled, or the stack's bond table is full.
- `errors` is the complete set of connection, unpair, and profile problems as full `Error` messages,
  ordered and deduplicated by complete error. Outcomes describe the failed connection attempt,
  unpair, or profile load. An unresolved unpair reports `UNKNOWN`; related errors describe restoring
  the native bond. At most one connection or unpair failure is present; a new one replaces it.
  A successful connection, disabling the device, or restarting the Dongle clears it. Integration and
  setting failures stay on their own records.
- `security` describes the live link while connected; each property is missing when the stack cannot
  report it, which does not mean false. Values describe the negotiated link and key, never the
  requested policy; for example, Secure Connections Just Works reports `encrypted`,
  `secure_connections`, and a 16-byte key but not `authenticated`.
- `roles` lists the kinds of input the HID descriptor produces: keyboard, mouse, consumer controls
  such as media and volume keys, system controls such as power and sleep, and gamepads. They are
  saved when the descriptor is read, so they are known while the device is disconnected.
- `profiles` is the device's saved layers, empty for a new device.
- `ERROR_CODE_PROFILE_MEMORY_EXCEEDED` or `ERROR_CODE_PROFILE_LOAD_FAILED` in `errors` means a
  connected device has no loaded profiles. Its input passes through unchanged. These errors clear
  when profiles load or the device disconnects. An empty `errors` list clears all device problems.

A `device` event carries the whole record whenever anything in it changes, including battery and
other information; `device_removed` follows an unpair or a lost record. `ListDevices` returns every
saved device in one response in ascending ID order. A read failure is an `error` entry containing
`RecordError { id, error }`. Its `STORAGE_FAILED` error has reason `STORAGE` and outcome `NO_EFFECT`,
which means no record was returned, not that saved data is unchanged. The listing continues.
Clients retain an already known device as last-known data; this does not confirm its connection or
preferences. Fresh records and events supersede it, and actual absence or removal clears it.
Profile read-error placeholders do not require preserving a cached profile. An undecodable record
is removed as lost and omitted, as described under [Lost records](commands.md#lost-records).

`Device.output_preferences` holds saved output choices as `role` and `mode`. Only Gamepad is
configurable, with Disabled, HID, and XInput choices. A missing gamepad choice means Disabled.
Keyboard and mouse behavior does not depend on this list. Saved choices survive renewing an
existing bond. Unpairing deletes them.

During first-connection setup, a newly paired device with validated gamepad controls automatically
selects HID if the firmware supports it and the saved HID pool has room. Disconnected, disabled,
blocked, and nonresident saved HID choices count toward its four-slot limit. Otherwise setup settles
on Disabled. Explicit choices take precedence. Setup saves the choice before applying it and retries
a failed save on a later connection. Completed setup never automatically enables output later.
Gamepad selection does not finish pending HID++ detection. See the
[storage rules](../storage-format.md#devices) for setup persistence.

`Device.outputs` reports output status independently of Bluetooth state and `inactive`.
Each `OutputStatus.status` selects either a `state` or a full `error`. Producers always set one;
`state = UNKNOWN` explicitly reports unknown. Consumers treat an unset or unknown status variant
as Unknown. The state variants are:

| State | Meaning |
| --- | --- |
| Unknown | The output state is unknown. |
| Disabled | The selected mode is Disabled. |
| Disconnected | The selected backend is supported and the device has no live connection. |
| Pending | Connection setup or required USB enumeration is incomplete. |
| Active | A supported translation owns an enumerated slot. |

Only Active carries `slot`, a zero-based temporary slot within its role and mode pool. It is not a
Windows XInput user index. An unavailable selected backend reports `error` with code `UNSUPPORTED`
and reason `OUTPUT_BACKEND`; a connected layout without supported gamepad controls uses the same
code with reason `INPUT_LAYOUT`. Outcomes describe output setup. Connection setup, unavailable
storage, and USB setup report Pending; Pending alone is not an error. USB capacity errors belong
to `Status.errors`. Disabled or blocked devices have no assignment. Active requires a supported
layout, an assigned slot, and a ready USB backend.

USB allocation considers saved roles, output preferences, and profile outputs, together with live
connections. Each gamepad mode has up to four independently enumerated slots where supported.
Configured disconnected slots have lower priority than connected-device outputs. Unconfigured
extra slots use spare resources and yield when a higher-priority interface needs them. Saving a
preference is allowed even when the corresponding interface cannot fit. See
[USB allocation](hid-forwarding.md#usb-allocation).

Runtime assignments use the lowest free slot in the selected mode's actual enumerated pool.
Surviving assignments keep their slots when other devices connect or disconnect. Closing a link,
changing its mode, replacing its layout, or losing storage readiness releases its controls. A mode
change can move an assignment without rebuilding USB when the actual allocation stays the same.

The USB owner supplies `outputs::Enumeration` through `Application::set_output_enumeration`, with
an actual `Plan`, a generation that changes when endpoints are rebuilt, and USB readiness. No
capacity is assumed before that call. `Application::gamepad_outputs` reconciles connections and
returns fixed arrays of four HID and four XInput `SlotSnapshot` values. Each has an optional
`LinkId` occupant, a nonwrapping assignment generation, and normalized `hid::Gamepad` controls.
An empty slot has neutral controls. Assignment generations change when a slot is assigned again,
including when the same link receives a replacement layout. They are separate from enumeration
generations.

USB class workers own report encoding and transmission. Before sending queued controls, they
compare the current occupant and assignment generation, discard stale work, and send neutral
controls before a replacement occupant's controls. Reading snapshots consumes nothing, so workers
can resend current controls after reset or resume. The application caches controls when reports
arrive and when a slot is assigned. Reconciliation checks the board's runtime connection slots and
eight output slots; surviving assignments do not walk HID fields on each priority poll. The decoder
permits at most sixteen reports and ninety-six fields per map. No USB queues or per-report
allocations are needed by the runtime pools.

## Integrations

`Device.integrations` lists each integration the device has a saved preference for or is detected
with on the current link. `enabled` is the saved preference. `detected` is present when the device
speaks the integration on the current link, with the protocol version it reported. The `status`
oneof contains a state or full `Error`; an unset or unknown variant means Off. The states are:

| State | Meaning |
| --- | --- |
| Off | Turned off with `SetDevice`. |
| Disconnected | Turned on, and the device is not connected. |
| Starting | Connected; detecting the integration, reading its settings, and applying saved values. |
| Active | Up: its settings have been read and saved values are applied. |

An integration that cannot start reports `error`. Missing required protocol versions or reports
use `PROTOCOL_UNSUPPORTED`. Outcomes describe integration setup on the current connection.

For HID++, a device qualifies when its HID descriptor has usable HID++ reports. Detection reads the
HID++ version and the device's settings even while HID++ is off; turning it on starts special-key
translation, where the device supports it, and the setting apply, and turning it off resets the
temporary reporting changes translation made. Special-key
translation is never reported on its own: with HID++ on, controls the Dongle can translate are
translated, and the rest behave as the device sends them. Ordinary input never waits for HID++.

## Information

`Device.information` contains typed identity, battery, keyboard, power, backlight, pointer-sensor,
wheel, and thumbwheel facts. Missing scalars are unknown, never false or zero. Nothing here is saved
to flash. An event replaces the whole information object, clearing absent fields. Information
belongs to the current connection and clears on disconnect.

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
accept writes for, on its firmware revision, is reported in the corresponding information field.
A saved preference for that field remains a setting with an `UNSUPPORTED` error.

## Settings

`SettingsObject` is a complete domain owned by an integration. Its group is Keyboard, Power,
Backlight, Pointer Sensor, Wheel, or Thumbwheel. Each configurable field has a typed wrapper.
`value` is the last actual reading, never an echo of `saved`. It is current while the owning
integration is Active, otherwise possibly stale on the current connection, and absent before the
first read or after disconnect. `saved` exists only for an explicit preference stored on the
Dongle; `status` is present exactly when `saved` is present.

Integer limits are a range or a complete discrete set. A range accepts `min + n * step` through
`max`, with a zero step meaning one. A discrete set has at most six distinct values. Neither limit
means any integer is accepted. Enum choices contain each writable known member at most once and
exclude Unknown. An empty choice list grants no writable value, and an actual reading may be
outside the choices. Saved fields retain their full limits and choices while disconnected.

| Status | Meaning |
| --- | --- |
| Unknown | No confirmation that the saved preference took effect. |
| Pending | Saved, and waiting until the device can confirm it. |
| Applied | The device reports the saved value. |
| Changed on device | The device reports another value; the next apply writes the saved one back. |

`SettingStatus.result` contains a state from this table or a full `Error`. An unsupported saved
setting uses code `UNSUPPORTED`; other apply failures preserve their code and details. Outcomes
describe applying that field to the device, independently of saving its preference on the Dongle.
A write that may have taken effect but cannot be confirmed uses outcome `UNKNOWN`. An unset or
unknown status variant provides no confirmation that the preference took effect.

Discovery, reads, notifications, and applies never create a saved preference; only `SetSettings`
does. Unpairing deletes the device's saved settings. A field appears only when discovered as
configurable on this connection or when it has a saved preference. If its capability disappears,
a saved field remains with an `UNSUPPORTED` error. Disconnect removes unsaved fields and clears
readings on the rest. The integration state determines availability and freshness.

`SettingsRef` identifies an entire domain by integration, group, and optional `index`. Pointer
Sensor requires a zero-based physical sensor index, stable across connections to the same device.
At most two sensors are supported; other groups omit `index`. `ListSettings` sorts by integration,
group, then index, and returns every object in one response.

Each `settings_changed.changed` object replaces the whole object with that identity. Omitted
fields remove prior capabilities, a present wrapper without `value` clears the reading, and absent
`saved` clears the preference. An object with no fields is removed instead. `removed` contains
whole-object references. Objects not named in either list stay unchanged. Each identity appears
at most once in an event and never in both lists.

Settings responses and change events carry whole objects, including complete limits and saved
metadata, without a transport-derived object or message size cap. Device constraints still apply,
including at most six Pointer Sensor DPI choices. Events may split between whole objects or
references, and changes across separate events are not atomic.

`SettingsUpdate` chooses one domain and changes only its present fields, including nested delay
fields. Each field must select either a typed value or the empty `SettingForget` marker; an empty
update is invalid. Forget removes the saved preference while leaving the actual device value
unchanged. Changes apply in order, with the last change to each field winning. The complete request
must fit the 1024-byte request limit. Validation saves nothing if any field is invalid. See
[settings commands](commands.md#settings) for persistence and readback behavior.
