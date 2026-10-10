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
`storage.full`) and a full connection table (`CAPACITY_REASON_CONNECTIONS`). A saved device is
recognized from the candidate's address when the address is its identity or the stack can resolve
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
connection turns on each integration it detects. It is saved enabled when fewer than `max_enabled`
devices of its transport are enabled, and disabled otherwise. An enabled device then connects on
its own; there is no separate connect step.

## Devices

| Command | Result | Behavior |
| --- | --- | --- |
| `ListDevices` | `DeviceList` | One page of saved devices in ascending ID order. A device whose record cannot be read is listed by ID as `unreadable` and the listing continues past it. |
| `GetDevice` | `Device` | One saved device. |
| `SetDevice` | none | Partial update of `enabled`, `trusted`, `blocked`, integration preferences and profile layers, saved in one write. Integration updates apply in order. Turning `enabled` off or `blocked` on closes the device's link. |
| `ConnectDevice` | `Device` | Clears a disconnect pause and starts connecting, or joins an attempt already running. Refused for a disabled or blocked device and while a pairing runs. |
| `DisconnectDevice` | `Device` | Closes the link and pauses automatic reconnection until `ConnectDevice` or a restart. |
| `UnpairDevice` | none | Pauses and disconnects the device, then deletes its bond and settings once the link is gone; `device_removed` follows. |
| `RefreshDevice` | none | Re-reads device information and settings from a connected device. |
| `ListWarnings` | `DeviceWarnings` | One page of the device's current HID warnings. |

`device`, `settings_changed` and `warnings_changed` events report every change. Connection progress
and failure show up in `Device.state` and `Device.error`; a connection attempt times out after 30
seconds and records `ERROR_CODE_TIMEOUT` without pausing automatic reconnection.

### Enabled, trusted and blocked

Enabled means the device is loaded into the Bluetooth stack's bond table, which lets it reconnect,
be recognized when it connects in, and encrypt its link; a disabled device stays saved but cannot
connect. The table has a fixed size per transport, reported as `TransportSupport.max_enabled` (the
stack's bond table less one entry kept free for pairing). The table holds every enabled, unblocked
device whose transport the firmware supports and has enabled. A `SetDevice` that changes anything
about a device without an entry, and leaves it enabled and unblocked on such a transport, is refused
with `ERROR_CODE_NO_CAPACITY` and `CAPACITY_REASON_ENABLED` while that transport's table is full. A
`SetDevice` that changes nothing succeeds without the check, and a device that has an entry keeps
it, including a device turned off or blocked whose link is still closing. If firmware with a
smaller table starts
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
| `ListSettings` | `DeviceSettings` | One page of every setting the Dongle knows for the device, across integrations: those read on the current connection plus every saved value. Sends nothing to the device. |
| `SetSettings` | none | Saves and forgets values in one storage write. Each change either saves a value or, with `forget`, removes the saved value. A `settings_changed` event follows. |

A valid value is always saved, whatever the device's current state: disconnected, busy, its
integration off, or in a mode where the value does not apply. It stays pending until an apply writes
it. Changes apply in order, so a later change to the same setting replaces an earlier one. If any
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
| `ListProfileRules` | `ProfileRules` | One page of the profile's rules, in ascending usage page and usage order of their inputs. |
| `SetProfileRules` | none | Saves and forgets rules in one storage write. Each change either saves a rule or, with `forget`, removes the rule for an input. A `profile_rules_changed` event follows when any rule changed. |

A new profile is empty and passes every input through unchanged. Nothing is filled in on the
client's behalf. A rule is refused with `ERROR_CODE_BAD_ARGS`, and nothing is saved, when its input
is outside `ProfileSupport.remap_inputs` for a remap or `scale_inputs` for a scale, when a remap
has an output outside `remap_outputs` or more than `max_remap_outputs` outputs, or when a scale has
a zero numerator or denominator. A save that would make the profile larger than `memory_budget`
returns `ERROR_CODE_NO_CAPACITY` with `CAPACITY_REASON_PROFILE_MEMORY`, since it could never be
loaded. The Dongle applies every rule it accepts to every device that uses the profile, whatever
kind of device it is. The number of saved profiles is limited only by flash; creating or copying one
that does not fit returns `ERROR_CODE_NO_CAPACITY` with `CAPACITY_REASON_STORAGE`.

A profile has at most one rule for each input. Changes in one `SetProfileRules` apply in order, so a
later save or forget for the same input replaces an earlier one; forgetting a rule that does not
exist is accepted. Rules stay sparse. A rule is saved and reported in its normalized form:

- A missing output collection is resolved to the only report that carries the usage.
- A remap keeps each output, a usage with its collection, once, sorted by usage page, usage, then
  collection usage page and usage.
- A scale's ratio is reduced to lowest terms.

A normalized rule that remaps its input to only itself, in the collection the input arrives in, or
scales it by a ratio equal to 1, changes nothing, so saving it forgets the input's rule, through the
serial API and through a configuration interface alike. An input arrives in the Keyboard collection
(`01:06`) from the Keyboard page, in the Mouse collection (`01:02`) from the Button page and as AC
Pan (`0c:238`) or a Generic Desktop X, Y or Wheel value, in the Consumer Control collection
(`0c:01`) from the rest of the Consumer page, and in the System Control collection (`01:80`) from
the rest of the Generic Desktop page.

These limits apply to saves. A saved profile that a later firmware's limits no longer allow is kept
as saved: a rule whose input or output the firmware does not support has no effect, only the first
`max_layers` profiles of a longer saved layer list apply, and a save that would leave the profile or
list over the limits is refused.

`SetDevice.profiles` sets a device's layers: up to `max_layers` profiles in the order they apply,
or an empty list to pass everything through. A newly paired device has no layers; each device's
layers are set on their own.

`SetAdapter.configuration_interfaces` changes the preferences of each configuration interface it
lists, such as VIA or Vial, applying the updates in order. Each interface has its own enabled flag
and profile, independent of any device's layers; 0 clears the profile. Every interface starts
disabled. An enabled interface needs a profile in the resulting configuration, and several
interfaces can be enabled at once unless `conflicts` lists one of the others; a resulting
configuration that enables conflicting interfaces returns `ERROR_CODE_UNSUPPORTED` and changes
nothing. A disabled interface keeps its profile. Enabling or disabling an interface, or changing an
enabled interface's profile, reconnects USB after the response has been written; Bluetooth
connections and held input carry on.

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

Loaded profiles share `ProfileSupport.memory_budget`, set by the board configuration so that every
connected device can load two profiles that remap every key the VIA editor shows. A board configured
without profile support, such as the Pico W, reports no `ProfileSupport` and no configuration
interfaces, answers profile commands with `ERROR_CODE_UNKNOWN_COMMAND`, and refuses
`SetDevice.profiles` with `ERROR_CODE_UNSUPPORTED`. `memory_used` reports what is loaded. A device
loads all of its layers or none: when the profiles it would add do not fit in what is left of the
budget, or one of them cannot be read or decoded, none is loaded, its input passes through
unchanged, and `Device.profile_error` says why. The connection itself carries on. When a connected
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
`max_remap_outputs` outputs are kept. A remap applies only while the device reports the input as a
held control; other forms of the same usage keep their usual behavior. A remap output never reaches a scale rule, because no usage is both a remap output and a
scale input. Scales from every layer multiply into one ratio, kept in lowest terms and applied once
with one fractional remainder per connection and input, which resets when the ratio changes; a
result too large for the USB report is limited to the largest value it carries, and a ratio too
large to represent is limited the same way.

Changing a connected device's layers or a profile's rules takes effect from the device's next input,
before the command responds. A held input keeps the outputs it was pressed with until it is
released, and an output that several held inputs produce stays held until the last of them is
released. A profile is released once no connected device or configuration interface uses it.

`profile_rules_changed` events report every change, including those made through a configuration
interface.
See [Input profiles](../input-profiles.md) for external editors.

### Lost profiles

A profile whose `profile.json` turns out to be missing or undecodable when the Dongle reads it is
deleted with its rules, and `profile_removed` follows. Its references are removed first, so no
device applies its rules once deletion begins. An undecodable rules file is removed, leaving the
profile empty, and a `profile_rules_changed` event follows. Neither cleanup runs between a
connection starting and its first forwarded input; the connection loads no profiles, its
`profile_error` is `ERROR_CODE_STORAGE_FAILED`, and the cleanup follows.

A saved reference to a profile that no longer exists is skipped until it is changed. Startup checks
which profile directories exist without reading them. Before USB starts, it clears and disables a
configuration interface whose profile does not exist, so the adapter still enumerates once. While
reading device policies, it removes such references from device layers. Deleting a lost profile
removes its references the same way, and disabling an enabled interface then reconnects USB.
`device` and `adapter` events follow. A listing leaves out a lost record it removes; an `unreadable`
entry names only a record whose flash read failed. As for devices, a failed read is never proof that
data is missing and deletes nothing.

## Development

See [Development commands](development.md).
