# Records

[Protocol index](README.md)

Field schemas are defined in [`proto/serial/v1/cordial.proto`](../../proto/serial/v1/cordial.proto). This document describes record semantics, state transitions, and invariants.

## Status

`Status` reports the adapter's persistent ID (16 uppercase hex digits, matching the start of its USB serial), readiness, saved host platform (default Linux), supported transports (default BLE on, Classic off), output backends, profile support, configuration interfaces, resource capacities, and current errors.

- **`Status.information`**: Independently optional `firmware_version`, `board_name`, `development_build` (`false` in production), `storage_format` (committed `/format.pb` version, omitted if unreadable; informational only), and `target_storage_format` (always set by firmware, currently `2`).
- **`Status.ready`**: `true` only when both Bluetooth and storage are usable. False alone does not identify which subsystem is unavailable.
- **`Status.errors`**: Complete ordered, deduplicated set of active adapter `Error` messages (replaced in full by every `adapter` event). Radio faults use reason `RADIO` (or `NOT_READY`); storage faults use reason `STORAGE`. An unresolved save (`UNKNOWN` outcome) keeps storage unready across radio recovery until resolved or restarted; `STORAGE_FULL` does not make storage unready.
- **Migration gate**: After the handshake, clients verify exact support for `target_storage_format` before sending any other command. Only `STORAGE_MIGRATION_REQUIRED` (`STORAGE`/`NO_EFFECT`, no related or other errors) authorizes host-assisted migration; the marker stays latched until hardware reset. See [Preserving migration](../storage-format.md#preserving-migration).

### Resource Capacities

Each `ResourceCapacity` carries a `ResourceType` and independently optional `count` and `limit` (absent means unknown, never zero or unlimited; explicit `0` stays `0`). Each container emits at most one entry per type.

| Container | Type | Count | Limit |
| --- | --- | --- | --- |
| `Status.resources` | `INTERFACES` | USB interfaces currently exposed | Compiled USB interface maximum |
| `Status.resources` | `STORAGE` | Best-effort mounted LittleFS bytes in use (includes overhead, excludes firmware/guard/E10) | Total bytes in mounted LittleFS |
| `TransportSupport.resources` | `DEVICES` | Saved enabled devices for this transport (including while transport is disabled; excludes pairing) | Enabled-device allowance (`= CONNECTIONS.limit`) |
| `TransportSupport.resources` | `CONNECTIONS` | Occupied normal slots including connecting/closing links (excludes pairing) | Normal connection allowance (`= DEVICES.limit`) |
| `OutputModeSupport.resources` | `SLOTS` | Enumerated USB pool size for this role/mode (omitted until known; `0`/`0` for Disabled) | Compiled maximum (<= 4 per gamepad mode) |
| `ProfileSupport.resources` | `MEMORY` | Loaded profile bytes in RAM (shared by ID) | Board `profile_memory_budget` in bytes |
| `ProfileSupport.resources` | `OUTPUTS` | Omitted | Maximum outputs per remap / held input |
| `ProfileSupport.resources` | `LAYERS` | Omitted | Maximum layers per device |

Notes:

- `STORAGE.count` is `mounted_size - free_space` without subtracting admission reserves; if free-space measurement fails, `count` is omitted while `limit` remains reported. `GetStatus` refreshes storage counts; best-effort recounts after writes may emit `adapter` events.
- `Status.profile_support` and configuration interfaces are omitted on boards without profile support (such as Pico W). Each `ConfigurationInterfaceSupport` distinguishes saved `enabled` from live USB `active`, keeps its selected `profile` while disabled/omitted, and lists `conflicts`.
- `Status.output_support` lists supported output modes; `INTERFACES.count < INTERFACES.limit` does not guarantee other USB resources (such as endpoints or descriptor bytes) are available. See [USB allocation](hid-forwarding.md#usb-allocation).

## Profiles

A profile has an immutable positive `uint32` ID (independent of device IDs, never reused), a non-unique `name`, and derived `roles`.

`Profile.roles` summarizes the input collections modified by the profile's rules (outputs never add roles):

| Rule Input | Collection | Derived Role |
| --- | --- | --- |
| Keyboard/Keypad (`07:xx`) | Keyboard (`01:06`) | Keyboard |
| Buttons (`09:xx`), X/Y/Wheel (`01:30`, `01:31`, `01:38`), AC Pan (`0C:238`) | Mouse (`01:02`) | Mouse |
| Other Consumer usages (`0C:xx`) | Consumer Control (`0C:01`) | Consumer control |
| Other Generic Desktop usages (`01:xx`) | System Control (`01:80`) | System control |
| Supported Game Pad controls and axes | Game Pad (`01:05`) | Gamepad |

`profile` events report creation, copies, and role changes; `profile_removed` reports deletion.

## Devices

Each saved device has a permanent positive `uint32` ID (preserved across reconnects, resolved address changes, reboots, and re-pairing; never reused after unpair). A dual-transport peripheral paired over both Classic and BLE has two separate device records.

Key fields:

- `name`: device-reported name if available, else pairing name (blank reports never erase a known name).
- `kinds`: derived from saved `roles` when known (keyboard, mouse, gamepad, or other), otherwise from live connection info.
- `state`: `connected` only after security, HID report setup, and input forwarding are all ready.
- `inactive`: first applicable reason why the adapter will not connect it: transport unsupported, transport disabled, blocked, disabled, or bond table full.
- `errors`: complete ordered, deduplicated set of active connection, unpair, and profile-load errors. At most one connection/unpair error is kept (cleared on connect, disable, or restart). `PROFILE_MEMORY_EXCEEDED` and `PROFILE_LOAD_FAILED` clear when profiles load or the device disconnects.
- `security`: negotiated live link properties (`encrypted`, `authenticated`, `secure_connections`, key size); missing fields mean unreported, not `false`.
- `roles`: HID input roles saved when the descriptor is read (available while disconnected).
- `profiles`: ordered layer IDs (empty by default).
- `output_preferences`: saved choices per role (Gamepad: `Disabled`, `HID`, `XInput`; missing means `Disabled`; first-connection setup auto-selects `HID` when supported and < 4 saved HID devices exist).

`device` events replace the full record; `device_removed` reports unpair or lost-record cleanup.

### Output Status and Slot Assignment

`Device.outputs` reports live status independently of Bluetooth state. Each `OutputStatus.status` holds either a `state` or a full `error` (unset/unknown decodes as `Unknown`):

| State | Meaning |
| --- | --- |
| `Unknown` | Output state is unknown. |
| `Disabled` | Selected mode is Disabled. |
| `Disconnected` | Backend supported, device not connected. |
| `Pending` | Connection, storage, or USB setup is incomplete (not an error). |
| `Active` | Translation owns a live enumerated slot (`slot` is set, 0-based within its pool; not a Windows XInput user index). |

Backend unavailability reports `error(UNSUPPORTED, OUTPUT_BACKEND)`; an unsupported controller descriptor reports `error(UNSUPPORTED, INPUT_LAYOUT)`.

Runtime slots assign the lowest free slot in the active pool and remain stable while other devices connect or disconnect. Firmware tracks each slot's occupant (`LinkId`) and nonwrapping assignment generation separately from USB enumeration generations, so USB workers discard stale queued reports and emit neutral HID controls (or XInput disconnect presence) before switching occupants without per-report allocations.

## Integrations

`Device.integrations` lists each integration with a saved preference or live detection (`IntegrationKind`, currently HID++). `enabled` is the saved preference; `detected` carries the reported protocol version on the active link; `status` holds a state or full `Error` (unset/unknown decodes as `Off`):

| State | Meaning |
| --- | --- |
| `Off` | Disabled via `SetDevice`. |
| `Disconnected` | Enabled, device not connected. |
| `Starting` | Connected; probing, reading settings, and applying saved preferences. |
| `Active` | Settings read and saved values applied. |

Missing required HID++ versions or reports return `PROTOCOL_UNSUPPORTED`. HID++ read-only discovery runs even while off; enabling HID++ starts special-key translation and settings application, while disabling resets temporary reporting. Ordinary HID input never waits for HID++.

## Information

`Device.information` holds non-persisted live facts (identity, battery, keyboard, power, backlight, pointer sensor, wheel, thumbwheel) cleared on disconnect or omitted from replacement events.

Battery source precedence (no cross-source fallback on missing/failed reads):

| Connection | Battery Source |
| --- | --- |
| BLE or Classic with usable HID++ reports and HID++ on | HID++ |
| Other BLE (including HID++ off) | GATT Battery Service |
| Other Classic (including HID++ off) | Standard HID battery reports |

Coarse battery levels map to `100` (full), `75` (high/good), `50` (medium), `20` (low), `5` (critical), `0` (empty). When multiple batteries exist, the main battery wins; otherwise the lowest known level (with its charging flag) is reported. For other fields, valid HID++ readings take precedence over BLE fallbacks. Text fields are capped at 64 bytes without control characters. Read-only fields on a given firmware revision appear in `Device.information`; any saved preference for an unwritable field stays in `Settings` with an `UNSUPPORTED` error.

## Settings

`SettingsObject` represents one complete settings domain (`Keyboard`, `Power`, `Backlight`, `Pointer Sensor`, `Wheel`, `Thumbwheel`) owned by an integration, identified by `SettingsRef(integration, group, optional index)`. Pointer Sensor uses a stable 0-based `index` (up to 2 sensors); other groups omit `index`.

Each configurable field wrapper carries:

- `value`: last actual device reading (never echoes `saved`; fresh while integration is `Active`, cleared on disconnect).
- `saved`: explicitly saved adapter preference (created only by `SetSettings`).
- `status`: present iff `saved` is present, carrying a state or full apply `Error`:

| State | Meaning |
| --- | --- |
| `Unknown` | No confirmation whether the saved preference took effect. |
| `Pending` | Saved and waiting for device confirmation. |
| `Applied` | Device reports the saved value. |
| `Changed on device` | Device reports a different value; next apply writes the saved value back. |

- **Bounds**: Integer fields specify either a `(min, max, step)` range (`step = 0` means `1`) or a discrete list of up to 6 values (unconstrained if neither is present). Enum choices list writable known members without `Unknown`; an empty list is unwritable. Saved fields retain bounds while disconnected.
- **Event updates**: `settings_changed.changed` replaces whole `SettingsObject` records by `SettingsRef` (an empty object is removed); `settings_changed.removed` lists removed `SettingsRef` identities. Each identity appears at most once per event. Unsaved fields disappear on disconnect; saved fields remain without `value`.
