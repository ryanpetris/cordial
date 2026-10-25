# Commands

[Protocol index](README.md)

Each command is one `Request.command` variant; its message in
[`proto/cordial.proto`](../../proto/cordial.proto) documents its fields. This page describes what
each command does. Commands that need Bluetooth or storage return `ERROR_CODE_NOT_READY` until both
have started.

Every `List` command returns one page at a time and continues after the key of the last entry the
client received; see [Listings](compatibility.md#listings) for the keys, orders and page rules.
Commands that change saved preferences respond with no result once the change is applied, and the
changed object's event follows when anything changed; a change to what the Dongle already holds
sends no event. A success means the Dongle now holds the values sent, in the saved form the
command defines: `SetAdapter` trims the name, and `SetProfileRules` saves each rule in its
[normalized form](#profiles). A client applies the same normalization to the values it sent instead
of querying again. `SetAdapter` with `name: ""` is the exception: only the `adapter` event that
follows a change reports the default name.

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
| `SetAdapter` | none | Partial update of the name, host platform, enabled transports and configuration interfaces, saved together. A name is 1..64 UTF-8 bytes without control characters, trimmed; `""` restores the firmware default. Transport updates apply in order; one for a transport the firmware does not support returns `ERROR_CODE_UNSUPPORTED` and changes nothing. Any transport may be disabled, including all of them. An unchanged value writes nothing. A platform change reconfigures special-key translation on every connected device with HID++ on and re-applies its saved settings. Disabling a transport closes its links, fails a pairing over it that has not saved its bond with `ERROR_CODE_UNSUPPORTED`, drops it from a running scan (ending the scan when no transport is left), stops reconnecting its devices and declines their connections, and makes its saved devices inactive with `INACTIVE_REASON_TRANSPORT_DISABLED`. Enabling it makes them eligible again: after the response, the Dongle reads their saved records in the background, and a `device` event reports each device that becomes resident. Configuration interface changes are described under [Profiles](#profiles). |
| `EnterBootloader` | none | Development firmware only. Refused with `ERROR_CODE_BUSY` while a pairing or setup link is open. After responding, the Dongle stops reading requests, releases held input, and reboots into ROM programming mode (BOOTSEL on Pico, download mode on ESP32-S3) within about 250 ms. Saved data is untouched. |

An `adapter` event follows any change to the name, platform, enabled transports, configuration
interfaces, readiness or adapter information.

## Discovery and pairing

| Command | Result | Behavior |
| --- | --- | --- |
| `StartScan` | none | Scans the listed transports for 1..60 seconds (10 when zero). Listed transports the firmware does not support or has disabled are left out; when none is left, the scan returns `ERROR_CODE_UNSUPPORTED`. Starting a new scan discards earlier candidates, except one a pairing already captured. |
| `StopScan` | none | Ends the scan early; candidates already found stay usable. |
| `StartPairing` | none | Starts pairing a candidate. Only one pairing runs at a time. |
| `AcceptPrompt` | none | Answers the open prompt, with the passkey or PIN when the step is `EnterCode`. |
| `RejectPrompt` | none | Rejects the open prompt; pairing fails with `ERROR_CODE_REJECTED`. |
| `CancelPairing` | none | Cancels a pairing that has not saved its bond. |

Candidates arrive as `scan_found` events, again whenever a candidate's name, kinds or signal
changes, and the scan ends with a `scan_done` event. At most 32 candidates are kept; past that,
`scan_done` reports `truncated`. Candidate IDs stay valid until the next scan starts or the session
ends. Results are candidates, not a promise of HID support. An enabled saved BLE device advertising
its identity alone is not a fresh candidate.

`StartPairing` refuses a blocked saved device, a transport the firmware cannot pair or has
disabled, a full flash (`ERROR_CODE_NO_CAPACITY` with `CAPACITY_REASON_STORAGE`, also reported as
`ERROR_CODE_STORAGE_FULL` in `Status.errors`) and a full connection table
(`CAPACITY_REASON_CONNECTIONS`). A saved device is recognized from the candidate's address when the address is its identity or the stack can resolve
it; a disabled device advertising a private address is recognized only once pairing reveals its
identity, and a blocked one then fails with `ERROR_CODE_BLOCKED` without changing its bond. It
closes the selected device's own link and any setup link first, and never an unrelated working
device.

Progress arrives as `pairing` events: `connecting`, then `enter_code`, `confirm_code` or
`show_code` when the method needs the user, then `done` with the saved device ID or `failed` with
an error code. Just Works pairing goes straight from `connecting` to `done`. A prompt expires after
30 seconds and the whole pairing after 120.

The bond is committed only once pairing completes. Failure, cancellation, the session ending or a
restart before that leaves every existing bond unchanged. Pairing a device that is already saved
renews its bond in place, keeping its ID, name and preferences, including whether it is enabled; a
disabled device connects once it is enabled.

A newly paired device is saved trusted and unblocked, with no integration enabled; its first
connection turns on each integration it detects. It is saved enabled when the number enabled for
its transport is below the advertised `DEVICES.limit`, and disabled otherwise. An enabled device then connects on
its own; there is no separate connect step.

## Devices

| Command | Result | Behavior |
| --- | --- | --- |
| `ListDevices` | `DeviceList` | One page of saved devices in ascending ID order. A device whose record cannot be read is listed by ID as `unreadable` and the listing continues past it. |
| `GetDevice` | `Device` | One saved device. |
| `SetDevice` | none | Partial update of `enabled`, `trusted`, `blocked`, integration preferences, output preferences and profile layers, saved in one write. Integration and output updates apply in order. Turning `enabled` off or `blocked` on closes the device's link. |
| `ConnectDevice` | `Device` | Clears a disconnect pause and starts connecting, or joins an attempt already running. Refused for a disabled or blocked device and while a pairing runs. |
| `DisconnectDevice` | `Device` | Closes the link and pauses automatic reconnection until `ConnectDevice` or a restart. |
| `UnpairDevice` | none | Pauses and disconnects the device, then deletes its bond and settings once the link is gone; `device_removed` follows. |
| `RefreshDevice` | none | Re-reads device information and settings from a connected device. |

`device` and `settings_changed` events report changes. Immediate `ConnectDevice` refusals return
response `Error`. After acceptance, connection progress and failure arrive in `Event.device`, in
`Device.state` and `Device.errors`; a connection attempt times out after 30
seconds and records `ERROR_CODE_TIMEOUT` without pausing automatic reconnection.

### Enabled, trusted and blocked

Enabled is the saved preference that lets a device connect. Eligible devices are loaded into the
Bluetooth stack's bond table to reconnect, recognize incoming connections and encrypt their links;
a disabled device stays saved but cannot connect. Each transport reports its allowance as
`DEVICES.limit` in `TransportSupport.resources`, with one additional native bond entry kept free
for pairing. The table holds every enabled, unblocked
device whose transport the firmware supports and has enabled. A `SetDevice` that changes anything
about a device without an entry, and leaves it enabled and unblocked on such a transport, is refused
with `ERROR_CODE_NO_CAPACITY` and `CAPACITY_REASON_ENABLED` while that transport's table is full. A
`SetDevice` that changes nothing succeeds without the check, and a device that has an entry keeps
it, including a device turned off or blocked whose link is still closing. If firmware with a
smaller table starts
with more devices enabled, nothing is disabled or deleted: the lowest device IDs are loaded and the
rest report `INACTIVE_REASON_CAPACITY` until the user disables enough of them.

The same allowance is reported as `CONNECTIONS.limit` and governs normal connection admission for
that transport. Classic and BLE have independent allowances; enabling Classic does not reduce BLE
capacity. Connecting and closing links occupy normal slots until freed. The Dongle reserves one
additional pairing connection overall, outside both normal allowances. An eligible device can
connect while its transport has room, regardless of the other transport's normal connection count.

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

### Output preferences

`SetDevice.output_preferences` changes only the listed roles. Entries apply in order, with the last
entry for a role winning, and the complete policy is saved once before runtime state changes.
Only Gamepad is configurable. Disabled clears its saved enabled choice and works without backend
support. HID and XInput require that role and mode in `Status.output_support`; otherwise the command
returns `ERROR_CODE_UNSUPPORTED`. Unknown numeric roles or modes and an unspecified role return
`ERROR_CODE_BAD_ARGS`. Other known roles return `ERROR_CODE_UNSUPPORTED`. Invalid updates and
failed saves leave the previous policy and desired pools unchanged. An empty list changes nothing.
Clients gate these writes on advertised support because older firmware ignores unknown fields.

A missing Gamepad preference means Disabled. Saving HID or XInput contributes configured candidates
for that mode even while the device is disconnected, disabled or blocked. The `SLOTS.limit` in
`OutputModeSupport.resources` limits USB allocation, not how many paired devices may save the
choice. Valid preferences save even when their requested interfaces cannot fit. Pairing, unpairing,
connection and profile changes all use the same allocator; USB reconnects only when its actual
allocation changes. Connected-device requirements can preempt lower-priority interfaces.
See [live output states and pool rules](records.md#devices) and
[USB gamepad behavior](hid-forwarding.md#usb-gamepad-outputs). Client controls,
CLI commands and human-readable slot numbering are described in
[Gamepad outputs](clients.md#gamepad-outputs).

## Settings

| Command | Result | Behavior |
| --- | --- | --- |
| `ListSettings` | `DeviceSettings` | One page of complete typed settings domains, across integrations: configurable fields discovered on this connection plus every saved preference. Sends nothing to the device. |
| `SetSettings` | none | Saves and forgets values in one storage write. Each change either saves a value or, with `forget`, removes the saved value. A `settings_changed` event follows. |

A valid value is always saved, whatever the device's current state: disconnected, busy, its
integration off, or in a mode where the value does not apply. It stays pending until an apply writes
and confirms it. Changes are typed partial domain updates, so missing nested fields stay unchanged.
Each present field selects a value or an empty `SettingForget` marker. Changes apply in order,
so a later change to the same field replaces an earlier one. If any
change in a request is invalid, nothing is saved. A forgotten setting is never written to the device
again; the device keeps whatever value it has.

### Applying saved settings

The Dongle's saved settings are the truth for the device, and an apply syncs them to it. One routine
does every apply, for one integration: it reads the device's current values, skips those already
equal to the saved ones, writes the differences one setter at a time (fields that share a setter
go out together), and reads back to confirm. Only saved settings are written.

The routine runs when an integration comes up (on connect, or when it is turned on), when the
adapter platform changes, and after every `SetSettings` for each integration in the request, once
the command has responded. A failure sets the affected settings' status to an error code in a
`settings_changed` event, keeps their saved values, and is tried again at the next apply. Different
setters and integrations are separate device writes, so one failing does not undo the others. A
write the device does not answer is reported as `ERROR_CODE_TIMEOUT`; it may or may not have taken
effect, and the next apply settles it by reading first.

A setting that only applies while another has a particular value is still saved: a configured
backlight level saved while the mode is automatic waits as pending and is written once the mode
becomes permanent manual. If the user changes a saved setting on the device itself, the setting
shows `SETTING_STATE_CHANGED_ON_DEVICE` until the next apply writes the saved value back.

See [HID++](../hidpp.md) for the device operations behind each HID++ setting.

## Profiles

| Command | Result | Behavior |
| --- | --- | --- |
| `ListProfiles` | `ProfileList` | One page of saved profiles in ascending ID order. A profile whose record cannot be read is listed by ID as `unreadable` and the listing continues past it. |
| `GetProfile` | `Profile` | One saved profile. |
| `CreateProfile` | `ProfileCreated` | Saves a new, empty profile and returns its ID; a `profile` event follows. |
| `CopyProfile` | `ProfileCreated` | Saves a new profile with the source's rules and returns its ID; a `profile` event follows. |
| `DeleteProfile` | none | Deletes an unused profile with its rules; `profile_removed` follows. |
| `ListProfileRules` | `ProfileRules` | One page of the profile's rules, ordered by input usage page, input usage, collection usage page, then collection usage. `after` is the last rule's `ProfileRuleRef`, containing both `input` and `collection`. |
| `SetProfileRules` | none | Saves and forgets rules in one storage write. Each change either saves a rule or, with `forget`, removes the rule for an input and collection. |

A new profile is empty and passes every input through unchanged. Nothing is filled in on the
client's behalf. Every rule and `ProfileRuleRef` requires both an `input` usage and its application
`collection`; the collection is never inferred from the usage. Each capability range includes its
collection, so the same usage in Mouse and Game Pad collections identifies different inputs.
A rule is refused with `ERROR_CODE_BAD_ARGS`, and nothing is saved, when its input and collection
are outside `ProfileSupport.ranges` of kind `KIND_REMAP_INPUT` for a remap, `KIND_SCALE_INPUT`
for a scale or `KIND_AXIS_INPUT` for an axis rule, when a remap has an output outside ranges of
kind `KIND_REMAP_OUTPUT` or more outputs than `OUTPUTS.limit` in `ProfileSupport.resources`, or
when a scale has a zero numerator or denominator. An axis rule's `destination` must be within ranges of kind
`KIND_AXIS_OUTPUT` in the input's Game Pad collection. Unknown and unspecified kinds grant no
capability. Axis rules apply only to absolute Game Pad axes; `invert` reverses the normalized
axis range. They add no deadzones, curves or sensitivity settings. A save that would make the
profile larger than `MEMORY.limit` in `ProfileSupport.resources` returns `ERROR_CODE_NO_CAPACITY` with
`CAPACITY_REASON_PROFILE_MEMORY`, since it could never be loaded. The Dongle applies every rule it accepts to every device that uses the profile, whatever
kind of device it is. The number of saved profiles is limited only by flash; creating or copying one
that does not fit returns `ERROR_CODE_NO_CAPACITY` with `CAPACITY_REASON_STORAGE`.

A profile has at most one rule for each input and collection together. Changes in one
`SetProfileRules` apply in order, so a later save or forget for the same input and collection
replaces an earlier one; forgetting a rule that does not exist is accepted. Rules stay sparse. A
rule is saved and reported in its normalized form:

- A missing output collection is resolved to the only report that carries the usage.
- A remap keeps each output, a usage with its collection, once, sorted by usage page, usage, then
  collection usage page and usage.
- A scale's ratio is reduced to lowest terms.

A normalized rule that remaps its input to only itself in the same collection, scales it by a
ratio equal to 1, or maps an axis to itself without inversion changes nothing. Saving it forgets
the rule for that input and collection, through the serial API and through a configuration
interface alike. An input keeps the application collection of its report. Game Pad inputs use
Game Pad (`01:05`), independently of Mouse (`01:02`), Keyboard (`01:06`), Consumer Control
(`0c:01`) and System Control (`01:80`) inputs with the same usage.

These limits apply to saves. A saved profile that a later firmware's limits no longer allow is kept
as saved. A rule whose input or output the firmware does not support has no effect. Only the first
profiles up to `LAYERS.limit` in `ProfileSupport.resources` apply. A save that would leave the
profile or layer list over the limits is refused.

`SetDevice.profiles` sets a device's layers, up to the `LAYERS.limit` in `ProfileSupport.resources`,
in the order they apply, or an empty list to pass everything through. A newly paired device has no
layers; each device's layers are set on their own.

`SetAdapter.configuration_interfaces` changes the preferences of each configuration interface it
lists, such as VIA or Vial, applying the updates in order. Each interface has its own enabled flag
and profile, independent of any device's layers; 0 clears the profile. Every interface starts
disabled. An enabled interface needs a profile in the resulting configuration, and several
interfaces can be enabled at once unless `conflicts` lists one of the others; a resulting
configuration that enables conflicting interfaces returns `ERROR_CODE_UNSUPPORTED` and changes
nothing. A disabled interface keeps its profile. Saved `enabled` requests an interface; `active`
reports whether it fits in the actual USB allocation. Valid preferences save even when capacity
omits the interface. USB reconnects only when the actual allocation changes. The plan is computed
before durable save/apply and detach, and later failures do not roll back persisted preferences.
The response normally finishes transmitting before detach, subject to the short response-drain
deadline described under [sessions](transport.md#sessions). Bluetooth connections and held input
carry on. Clients reopen the serial session and read fresh status without a reconnect confirmation
dialog.

`DeleteProfile` refuses with `ERROR_CODE_IN_USE` while the profile is the profile of any
configuration interface, enabled or not, or in any saved device's layers, whether or not the device
is enabled.

### Using profiles

When a device's connection starts, the Dongle takes the device's layers from its resident
reconnection entry and loads every profile in them before the device's first input is forwarded. A
profile already loaded for another connected device or a configuration interface is shared and costs
nothing more. Loading profiles waits only for their flash reads, never for HID++ setup, a client or
USB. Commands, events and background storage work read or write about one record at a time, and a
step that writes may count free space again. Each step waits until radio events have been handled
and pending input has been sent to a host that is reading it, so loading a starting connection's
profiles waits for at most one such step already in progress. A command that
reads many records, such as a listing or a reference check, therefore answers after reading them
one at a time.

Loaded profiles share the `MEMORY.limit` in `ProfileSupport.resources`, set by the board
configuration. A board configured without profile support, such as the Pico W, reports no
`ProfileSupport` and no configuration interfaces, answers profile commands with
`ERROR_CODE_UNKNOWN_COMMAND`, and refuses `SetDevice.profiles` with `ERROR_CODE_UNSUPPORTED`.
`MEMORY.count` reports loaded bytes. A device
loads all of its layers or none: when the profiles it would add do not fit in what is left of the
budget, or one of them cannot be read or decoded, none is loaded, its input passes through
unchanged, and `Device.errors` includes `ERROR_CODE_PROFILE_MEMORY_EXCEEDED` or
`ERROR_CODE_PROFILE_LOAD_FAILED`. The connection itself carries on. When a connected
device's layers change, the profiles it no longer uses count as released before the new ones are
checked. The Dongle tries again when memory is released, when the device's layers change, or when a
profile in its layers is edited, and after a failed read it also retries with the connection
backoff, from two seconds to five minutes. Saving layers never depends on loading the profiles.

An edit that would grow a loaded profile beyond what is left of the budget is refused with
`ERROR_CODE_NO_CAPACITY` and `CAPACITY_REASON_PROFILE_MEMORY`, through the serial API and through a
configuration interface alike, so a device never loses its loaded profiles because of an edit. A
configuration interface loads its profile when an editor first uses it; when it does not fit, the
editor's requests fail and no device's profiles are unloaded to make room.

Each input goes through the layers in order, one rule lookup per layer. A remap's outputs are each
looked up in the next layer by their usage, and their results are held together in order, each
output's results taking its place, each output at most once. After each layer, only the first
outputs up to the advertised `OUTPUTS.limit` are kept. A remap applies only while the device reports the input as a
held control; other forms of the same usage keep their usual behavior. A remap output never reaches a scale rule, because no usage is both a remap output and a
scale input. Scales from every layer multiply into one ratio, kept in lowest terms and applied once
with one fractional remainder per connection and input, which resets when the ratio changes; a
result too large for the USB report is limited to the largest value it carries, and a ratio too
large to represent is limited the same way.

Changing a connected device's layers or a profile's rules takes effect from the device's next input,
before the command responds. A held input keeps the outputs it was pressed with until it is
released, and an output that several held inputs produce stays held until the last of them is
released. A profile is released once no connected device or configuration interface uses it.

See [Input profiles](../input-profiles.md) for external editors.

### Lost profiles

A profile whose `profile.pb` turns out to be missing or undecodable when the Dongle reads it is
deleted with its rules, and `profile_removed` follows. Its references are removed first, so no
device applies its rules once deletion begins. An undecodable rules file is removed, leaving the
profile empty. Neither cleanup runs between a connection starting and its first forwarded input;
the connection loads no profiles, its `errors` includes `ERROR_CODE_PROFILE_LOAD_FAILED`, and the
cleanup follows.

A saved reference to a profile that no longer exists is skipped until it is changed. Startup checks
which profile directories exist without reading them. Before USB starts, it clears and disables a
configuration interface whose profile does not exist, so the adapter still enumerates once. While
reading device policies, it removes such references from device layers. Deleting a lost profile
removes its references the same way, and USB reconnects if the actual allocation changes.
`device` and `adapter` events follow. A listing leaves out a lost record it removes; an `unreadable`
entry names only a record whose flash read failed. As for devices, a failed read is never proof that
data is missing and deletes nothing.

## Development

See [Development commands](development.md).
