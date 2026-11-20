# Commands

[Protocol index](README.md)

Each command is a `Request.command` variant defined in [`proto/serial/v1/cordial.proto`](../../proto/serial/v1/cordial.proto). Commands requiring an unavailable subsystem return `ERROR_CODE_NOT_READY` (pairing requires both Bluetooth and storage).

- `List*` commands return the full collection in one response frame; see [Listings](compatibility.md#listings).
- `Set*` commands respond with an empty `Response` once changes are durably saved and emit a change event only when state actually changed. Clients apply the command's normalized form locally (`SetAdapter` trims `name`; `SetProfileRules` normalizes rules) rather than re-querying. `SetAdapter` with `name: ""` is the sole exception: the following `adapter` event reports the firmware default name.
- Firmware refuses commands only when execution would fail or corrupt state (missing required fields, out-of-range values, missing IDs, concurrent pairing, insufficient capacity). Valid commands whose effect does not currently apply are still saved and applied when possible. Clients enforce predictable checks before sending; see [Client pre-checks](limits-and-errors.md#client-pre-checks).

## Adapter

| Command | Result | Behavior |
| --- | --- | --- |
| `GetStatus` | `Status` | Refreshes storage usage readings and returns current adapter status. |
| `SetAdapter` | none | Partial update of `name` (`1..64` UTF-8 bytes, trimmed, no control chars; `""` resets to board default), `host_platform`, `transports`, and `configuration_interfaces`, saved atomically. Unsupported transports return `ERROR_CODE_UNSUPPORTED` without changing anything; all transports may be disabled. Platform changes reconfigure HID++ translation and reapply settings on connected HID++ devices. Disabling a transport closes its links, fails unsaved pairings (`ERROR_CODE_UNSUPPORTED`), drops it from active scans, stops reconnection, and marks its devices `INACTIVE_REASON_TRANSPORT_DISABLED`. Enabling a transport loads eligible devices in the background and emits `device` events. |
| `Restart` | none | Available in normal, assisted, and blocked modes (refused with `ERROR_CODE_BUSY` during pairing/setup). Sends empty success, drains `SessionEnding(REBOOT)`, and resets the MCU. |
| `EnterBootloader` | none | Development firmware only (refused with `ERROR_CODE_BUSY` during pairing/setup). Sends empty success, drains `SessionEnding(BOOTLOADER)`, releases held input, and reboots into ROM bootloader (BOOTSEL / ESP download mode) with saved data intact. |

An `adapter` event follows any change to adapter preferences, readiness, information, or best-effort storage counts.

## File Access and Storage Migration

`ListFiles`, `ReadFile`, `WriteFile`, `MoveFile`, and `DeleteFile` operate on LittleFS in debug firmware and automatically during assisted migration in production firmware. After a compatible handshake, clients read `GetStatus` and verify exact `target_storage_format` support before sending any other command.

Only `STORAGE_MIGRATION_REQUIRED` (`reason = STORAGE`, `outcome = NO_EFFECT`, no related or other errors) authorizes host-assisted migration. See [File access](development.md#file-access) and [Preserving migration](../storage-format.md#preserving-migration) for the full protocol and verification rules.

## Discovery and Pairing

| Command | Result | Behavior |
| --- | --- | --- |
| `StartScan` | none | Scans listed transports for `1..60` s (`10` s when `0`). Unsupported/disabled transports are skipped; if none remain, returns `ERROR_CODE_UNSUPPORTED`. Discards earlier candidates except one captured by an active pairing. |
| `StopScan` | none | Stops scanning early while keeping discovered candidates usable. |
| `StartPairing` | none | Pairs one candidate (at most one pairing runs at a time). |
| `AcceptPrompt` | none | Answers the active prompt (including passkey/PIN for `EnterCode`). |
| `RejectPrompt` | none | Rejects the active prompt (`ERROR_CODE_REJECTED`). |
| `CancelPairing` | none | Cancels an in-progress pairing before bond commit. |

Scanning and pairing details:

- **Candidates**: Arrive via `scan_found` events (re-emitted when name, kinds, or signal updates) and finish with `scan_done`. Up to 32 candidates are kept; overflow reports `NO_CAPACITY(SCAN_CANDIDATES)` and ID exhaustion reports `INTERNAL(SCAN_CANDIDATES)` in `ScanDone.error` (with `PARTIAL` if some were kept, `NO_EFFECT` if none). Candidate IDs last until the next scan or session end. Enabled saved BLE devices advertising their identity are skipped as fresh candidates.
- **Admission**: `StartPairing` rejects blocked saved devices (`ERROR_CODE_BLOCKED`), unsupported/disabled transports, full flash (`NO_CAPACITY`/`STORAGE`), and full connection tables (`CONNECTIONS`). It closes only the target device's existing link or an existing setup link, never unrelated devices.
- **Progress**: Emits `pairing` events (`connecting`, optional `enter_code`/`confirm_code`/`show_code`, then `done` with saved device ID or `error`). Prompts time out after 30 s; total pairing times out after 120 s.
- **Bond commit**: Committed only on completion. Failures before commit leave bonds unchanged (`NO_EFFECT`); `UNKNOWN` means save durability could not be confirmed; `COMPLETE` confirms the bond saved despite a later failure (cleanup errors stay in `related`). Cancellation, session end, or restart never undoes a committed bond. Re-pairing an existing device renews its bond in place while keeping its ID, name, and preferences.
- **Initial state**: Newly paired devices start trusted, unblocked, with integrations off (probed on first connection), and enabled if the transport's enabled count is below `DEVICES.limit` (otherwise disabled). Enabled devices connect automatically.

## Devices

| Command | Result | Behavior |
| --- | --- | --- |
| `ListDevices` | `DeviceList` | All saved devices in ascending ID order. Read failures emit `RecordError` entries and continue. |
| `GetDevice` | `Device` | One saved device. |
| `SetDevice` | none | Partial atomic update of `enabled`, `trusted`, `blocked`, `integrations`, `output_preferences`, and `profiles`. Disabling or blocking closes any active link. |
| `ConnectDevice` | `Device` | Clears disconnect pause and starts/joins a connection attempt (refused if disabled, blocked, or pairing). |
| `DisconnectDevice` | `Device` | Closes the link and pauses automatic reconnection until `ConnectDevice` or restart. |
| `UnpairDevice` | none | Disconnects the device and deletes its bond and settings once closed; emits `device_removed`. |
| `RefreshDevice` | none | Re-reads device information and settings from a connected device. |

Immediate `ConnectDevice` refusals return `Response.error`; post-acceptance connection failures arrive in `Event.device` (connection attempts time out after 30 s with `ERROR_CODE_TIMEOUT` without pausing reconnection).

### Enabled, Trusted, Blocked, and Reconnection

- **Bond table and connection capacity**: Each transport has an independent allowance reported as both `DEVICES.limit` and `CONNECTIONS.limit`, plus one shared pairing reserve overall (see [Bluetooth capacity](limits-and-errors.md#bluetooth-capacity)). Modifying a nonresident device so it remains enabled and unblocked on an enabled transport returns `NO_CAPACITY(ENABLED_DEVICES)` when that transport's table is full. If saved enabled devices exceed a smaller firmware's table at boot, the lowest IDs load and the rest report `INACTIVE_REASON_CAPACITY`.
- **Automatic reconnection**: Requires an enabled, trusted, unblocked device without a disconnect pause. Disconnected BLE devices use the controller accept list (clean drops reconnect immediately; 3+ consecutive drops within 1 s back off 1 s to 5 s). Classic devices retry on a timer (starting 2 s after clean disconnect). Failures back off from 2 s to 5 min and record the error; `ConnectDevice` resets backoff to 2 s. BLE discovery runs alongside accept-list reconnection when supported, otherwise alternating in 1 s windows.
- **Trust and block**: Untrusting prevents future unattended connections without dropping an active link; blocking closes the link while keeping the bond and disconnect pause.

### Lost Records

If a device's `device.pb` is missing, undecodable, or holds a mismatched bond when read, firmware deletes the device record and settings and emits `device_removed`. Flash I/O errors return `ERROR_CODE_STORAGE_FAILED` and never delete data (at startup they keep `Status.ready = false`).

### Output Preferences

`SetDevice.output_preferences` updates only the listed roles in order (last wins). Only Gamepad is configurable (`Disabled`, `HID`, `XInput`; missing means `Disabled`). `Disabled` works even without backend support; `HID` and `XInput` require matching entries in `Status.output_support` (otherwise `ERROR_CODE_UNSUPPORTED`; unknown numeric roles/modes return `ERROR_CODE_BAD_ARGS`). Valid preferences always save even if USB slots cannot currently expose them; USB reconnects only when the active interface allocation changes. See [Devices](records.md#devices) and [USB allocation](hid-forwarding.md#usb-allocation).

## Settings

| Command | Result | Behavior |
| --- | --- | --- |
| `ListSettings` | `DeviceSettings` | Complete typed `SettingsObject` list across integrations (discovered fields + saved preferences). Sends nothing to the device. |
| `SetSettings` | none | Saves values or forgets saved preferences in one flash write; emits `settings_changed`. |

Valid values always save regardless of device state (disconnected, integration off, or dependent mode inactive) and stay pending until applied. Updates are partial down to nested fields (each present field sets a value or `SettingForget`); if any field is invalid, nothing saves. Forgetting a preference leaves the device's current setting alone and stops future writes.

### Applying Saved Settings

The adapter's saved settings are authoritative. One apply routine runs per integration on connect, integration enable, adapter platform change, and after `SetSettings`:

1. Reads current device values and skips fields that already match saved values.
2. Writes differing fields one setter at a time (merging companion fields that share a setter) and reads back to verify.
3. Failed writes record a full `Error` on the affected fields in `settings_changed` while keeping saved preferences intact for the next apply (unanswered writes report `TIMEOUT` with outcome `UNKNOWN`).
4. User changes made directly on the device report `SETTING_STATE_CHANGED_ON_DEVICE` until the next apply restores the saved value.

See [Logitech HID++ support](../hidpp.md) for device-level command layouts.

## Profiles

| Command | Result | Behavior |
| --- | --- | --- |
| `ListProfiles` | `ProfileList` | All saved profiles in ascending ID order (read failures emit `RecordError` entries). |
| `GetProfile` | `Profile` | One saved profile. |
| `CreateProfile` | `ProfileCreated` | Creates a new empty profile; emits `profile`. |
| `CopyProfile` | `ProfileCreated` | Copies a profile and its rules; emits `profile`. |
| `DeleteProfile` | none | Deletes an unused profile (refused with `ERROR_CODE_IN_USE` if referenced by any device layer or configuration interface); emits `profile_removed`. |
| `ListProfileRules` | `ProfileRules` | All rules sorted by `(input_page, input_usage, collection_page, collection_usage)`. |
| `SetProfileRules` | none | Atomic partial save/forget of rules in one flash write (complete request <= 1024 protobuf bytes). `cordial-client` sends one request without auto-splitting; callers divide larger change sets into sequential <= 1024-byte requests (not atomic across requests). |

### Rule Validation and Normalization

- Every rule requires explicit `input` usage and application `collection` matching `ProfileSupport.ranges` (`KIND_REMAP_INPUT`, `KIND_SCALE_INPUT`, or `KIND_AXIS_INPUT`). Remap outputs must match `KIND_REMAP_OUTPUT` (count <= `OUTPUTS.limit`); scale ratios require nonzero numerator and denominator; axis destinations must match `KIND_AXIS_OUTPUT` in the same Game Pad collection. Invalid rules return `ERROR_CODE_BAD_ARGS`.
- Rules normalize before saving: omitted output collections resolve when unique; remap outputs deduplicate and sort by `(usage, collection)`; scale ratios reduce to lowest terms; no-op rules (self-remap in same collection, scale `1`, non-inverted self-axis) delete the rule.
- Rules exceeding `MEMORY.limit` return `NO_CAPACITY(PROFILE_MEMORY)`; flash exhaustion returns `NO_CAPACITY(STORAGE)`.
- Boards without profile support (such as Pico W) omit `Status.profile_support`, answer profile commands with `ERROR_CODE_UNKNOWN_COMMAND`, and refuse `SetDevice.profiles` with `ERROR_CODE_UNSUPPORTED`.

### Configuration Interfaces and Runtime Loading

- `SetAdapter.configuration_interfaces` updates VIA/Vial preferences in order (`profile = 0` clears). Enabling an interface requires a profile and no enabled conflicting interface (`ERROR_CODE_UNSUPPORTED`). Saved `enabled` is independent of active USB exposure; USB reconnects only if the actual allocation changes.
- `SetDevice.profiles` assigns up to `LAYERS.limit` profile IDs in application order (empty list passes through).
- When a device connects, firmware loads all of its layer profiles into the shared `MEMORY.limit` RAM budget (sharing copies by ID) before forwarding its first input. Loading is all-or-nothing: if layers do not fit or fail to read, none load, input passes through unchanged, and `Device.errors` reports `PROFILE_MEMORY_EXCEEDED` or `PROFILE_LOAD_FAILED` (retrying when memory frees, layers/rules change, or on backoff).
- Live edits refuse to grow a loaded profile past the remaining budget so connected devices never lose loaded profiles. Edits and layer updates apply from the next input; already-held inputs keep their original outputs until released.
- Remaps chain through layers (capped at `OUTPUTS.limit` per layer), scales multiply into one reduced ratio with a single fractional remainder per input, and axis rules within one profile evaluate simultaneously against the axes entering that profile.

### Lost Profiles

A missing or undecodable `profile.pb` is deleted after stripping its references from devices and configuration interfaces (emitting `profile_removed`, `device`, and `adapter` events). An undecodable `rules.pb` is deleted, leaving the profile empty. Cleanup never delays a connecting device's first input; flash read errors never delete profiles.
