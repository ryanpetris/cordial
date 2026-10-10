# Current persistent format

The application uses upstream LittleFS through `littlefs2` 0.8.1 on Pico and
ESP32-S3. The layout identity is `littlefs-2`. Each file holds one protobuf
message from [`proto/storage.proto`](../proto/storage.proto). There is no
migration or automatic reformat on a read failure, and storage of another layout
identity, such as an earlier release's `littlefs-json-1`, stays unopened until
the board is re-flashed with its flash erased, as [building](building.md#firmware)
describes.
Application data and Bluetooth bonds use the common filesystem. The ESP32-S3
partition table has no NVS partition.

## Flash allocation

Every erase block is 4096 bytes. Board configuration specifies `firmware_bytes`;
storage occupies the remaining flash after platform reservations. Firmware
linking and artifact inspection reject overlap.

| Board | Flash | Firmware reservation | LittleFS blocks | LittleFS bytes |
| --- | ---: | ---: | ---: | ---: |
| Pico W | 2 MiB | 1 MiB | 255 | 1,044,480 |
| Pico 2 W | 4 MiB | 1 MiB | 766 | 3,137,536 |
| Waveshare RP2350B-Plus-W | 16 MiB | 1 MiB | 3,838 | 15,720,448 |
| XIAO ESP32-S3 | 8 MiB | 2 MiB | 1,535 | 6,287,360 |

The first storage sector contains a 32-byte layout hash outside LittleFS.
RP2350 also reserves its final flash sector for the ROM E10 marker. ESP uses
`cordial_layout` subtype `0x40` and `cordial_app` subtype `0x83`. Its firmware reservation
includes bootloader, partition table, PHY data and the application image. Its partition table is:

| Name | Type | Subtype | Offset | Size |
| --- | --- | --- | ---: | ---: |
| `phy_init` | data | `phy` | `0xf000` | `0x1000` |
| `factory` | app | `factory` | `0x10000` | firmware reservation minus `0x10000` |
| `cordial_layout` | data | `0x40` | firmware reservation | `0x1000` |
| `cordial_app` | data | `0x83` | firmware reservation plus `0x1000` | remaining flash |

The bootloader starts at `0x0` and the partition table at `0x8000`. The range `0x9000..0xf000` is
unallocated. Storage occupies the end of flash and the application partition starts on a 64 KiB
boundary, so that range cannot extend either.

Provisioning verifies the entire guard and filesystem range is erased before
its first write. It claims the guard before formatting. A missing guard over
nonblank storage, a mismatch, an interrupted format or a mount failure leaves
storage unavailable. None of these failures triggers an automatic erase.

## Files

| Path | Message | Contents |
| --- | --- | --- |
| `/format.pb` | `Format` | `format: 1` and `initialized`, recording whether roots have been committed |
| `/identity.pb` | `Identity` | Bluetooth address, IR, ER and derived IRK |
| `/adapter.pb` | `Adapter` | Adapter preferences; without the file, use Linux, the image's default name, BLE alone and no configuration interface |
| `/sequence.pb` | `Sequence` | The next device and profile IDs |
| `/devices/<id>/device.pb` | `Device` | `policy` and complete portable `bond` in one message; this file commits the device's existence |
| `/devices/<id>/settings.pb` | `Settings` | Explicitly saved physical-device preferences, with their integration |
| `/devices/<id>/layout.pb` | `Layout` | The device's HID layout: `maps` and, for BLE, `reports` |
| `/profiles/<id>/profile.pb` | `Profile` | Profile name and roles summary; this file commits the profile's existence |
| `/profiles/<id>/rules.pb` | `Rules` | The profile's rules |

IDs are positive decimal integers without padding or prefixes. Devices and profiles have separate
sequences; `/devices/1/device.pb` and `/profiles/1/profile.pb` can both exist.

The schema's comments document each message, its fields, and firmware validation that requires
relationships between records. Fields follow the kinds of the [compatibility
rules](protocol/compatibility.md#fields): the firmware checks each Required field, reads a missing
plain field as its zero value, and gives `optional` and message fields presence. Later firmware adds
fields whose zero value or absence keeps the earlier meaning, so records gain fields without a new
layout identity. The firmware ignores unknown fields; it leaves out entries of repeated fields
whose transport, integration, configuration interface or role is unknown or unspecified, and reads
an unknown host platform as Linux. Otherwise a record whose Required field is missing or out of
range is undecodable.

Enumerated values are protobuf enums: platforms, transports, integrations, roles, setting scopes,
report types and configuration interfaces. A setting is saved by its name. HID usages are numbers,
with the usage page in the high 16 bits and the usage ID in the low 16. Keys, addresses and report
maps are bytes.

Every record has a field writers always set, so no record encodes as an empty file: `format` in
the format record, the keys of the identity, an entry for every transport in the adapter
preferences, both next IDs of the sequence, the policy and bond of a device, at least one setting,
the report maps of a layout, a profile's name and at least one rule.

## Adapter

The adapter name override is a trimmed string of 1..64 UTF-8 bytes without control characters. An
absent `name` uses the board default baked into the image. `transports` holds an entry for each
transport, as `transport` and `enabled`; a transport without an entry uses its default, BLE enabled
and Classic disabled, and writers list every transport.

`configuration_interfaces` lists the saved preferences of each configuration interface that has any,
as `interface`, `enabled` and an optional `profile` ID. An interface without an entry is disabled
with no profile, and writers omit such entries. An enabled interface has a profile. Every adapter
preference update rewrites the file once and commits before runtime state changes.

## Devices

A device policy contains its ID, peer address/type/transport, name, trusted, blocked and
preferred-enabled flags, `integrations`, `roles` and `profiles`. A new device's policy also sets
`setup_pending` until its first-connection setup completes; a policy without it has completed setup.
Connection state, effective enablement, discovery, battery readings and transient errors are not
persisted.

- `integrations` lists each integration the device has a saved preference for, as `integration`
  and `enabled`. The implemented integration is HID++.
- `roles` lists the input roles the device's HID descriptor produces: keyboard, mouse, consumer
  control and system control. It is rewritten when a connection reads a descriptor with
  different roles, once input pauses (see [when files are written](#when-files-are-written)), and
  is empty until the first descriptor is read.
- `profiles` is the device's layers: the profile IDs it applies, in order. A new device has none,
  and an empty list passes everything through.

The bond holds the canonical native-independent identity and security keys: its owner, which is
the device ID, its identity, which equals the policy's peer, and the keys of the identity's
transport. Classic stores link key and key type. BLE stores local and peer security fields,
including LTK, IRK, CSRK, RAND, EDIV, flags, key size and signing counter. Keys are bytes fields of
their exact length. A saved bond is complete. The binary canonical bond encoding is used only at
native Bluetooth adapter boundaries.

Each physical-device preference contains `integration`, `metadata` and a numeric `value`.
Metadata contains the setting's name as `key`, such as `backlight.level`, feature ID, feature
revision, scope, choices and optional min/max/step range, which is enough to list the setting while
the device is disconnected. One settings file is rewritten for a setting change. Forgetting the
last setting deletes the file.

The HID layout file holds each HID service's raw report map as bytes in `maps`. A BLE layout also
lists its report characteristics in `reports`: owning service index, report type, report ID, value
handle, properties and the CCCD handle, zero when the characteristic has none. When the device
exposes a GATT Database Hash, `database_hash` holds its 16 bytes. Saving a layout writes the report
maps straight from the layout into the file's buffer. The file is written when a connection
discovers the device, including at pairing, and when a connected device's HID layout changes, once
input pauses, but not while storage is not ready. It is written only when free space covers the
32 KiB maintenance reserve, the 16 KiB reserved for pairing another device and the file itself, and
it is removed again when writing it leaves less than both reserves. A layout that cannot be saved
removes the saved file instead. Later connections supply it to the Bluetooth backend so input starts
without rediscovery. The file is optional: a missing, undecodable or unusable file means the next
connection discovers the device. An undecodable or unusable file is removed when it is read, and the
file is removed when a connection ends because the device's HID layout could not be used. Re-pairing
removes it, and deleting the device removes it with the preferences. Layout files are read by device
ID and are not part of record enumeration; mounting removes them with the directories of deleted
devices.

## Profiles

`profile.pb` contains `name` and `roles`, the roles the profile's rules change, worked out when
the rules are saved. The directory supplies the profile's ID.

`rules.pb` contains `rules`; an absent file means no rules. Each rule has an `input` usage and
either `remap` or `scale`:

- `remap` holds a list of `outputs`, each a `usage` with the `collection` of the report that carries
  it. An empty list disables the input.
- `scale` holds a `numerator` and a `denominator`.

The list is sorted by input, with at most one rule for each input, so a loaded profile can be
searched without sorting; a file that is not is undecodable. Each rule is its own entry of the
repeated field, so a reader can decode or skip the rules one at a time, and loading fills the
lookup table entry by entry without holding the decoded list. Saving encodes each rule in turn into
a buffer of exactly the file's length. Forgetting the last rule deletes the file.

Profile commands read and write these files directly. A rules change rewrites `rules.pb` once, and
`profile.pb` too when the roles change. Saving `rules.pb` commits the change; when `profile.pb`
cannot be rewritten after it, the saved roles are corrected once input pauses, retrying with the
connection backoff. Until then, and while an editor's edits wait to be written, profile listings
report the roles of the rules in use. A configuration editor's edits change the loaded rules and are
written later, as described in [when files are written](#when-files-are-written). A rules change,
from a command or an editor, merges the changes into the loaded table, or into the saved table when
the profile is not loaded, building the new table at its exact size without copying the old one.
Copying writes the new profile's `rules.pb` before its `profile.pb`.

Deleting a profile, and finding whether one is referenced, reads `adapter.pb` and every device
policy. Startup lists the profile directories without reading them, and while reading every device
policy removes references to profiles that do not exist from device layers and configuration
interfaces; deleting a lost profile removes its references the same way. A reference startup cannot
remove, because the write fails, is left out of what the Dongle uses and removed again at the next
startup.

Files are protobuf encodings without framing, encoded into a buffer of exactly the record's length.
There is no 512-byte record ceiling and no configured number of saved
devices, profiles or saved settings. Setting choices use the u16 domain. Native
connection/bond-table and wire-frame bounds still apply. Allocation and storage exhaustion are
resource errors.

## When files are written

Commands write the files they change before they respond, so a successful response means the
change is on flash: adapter preferences, device policies and settings, profile creation, copying,
deletion and `SetProfileRules`, and unpairing. A pairing saves its bond before the device is
admitted.

Other files are written once keyboard and mouse input pauses. The Dongle keeps a set of dirty
records whose newest contents are in RAM: a profile's `rules.pb` changed by a configuration editor,
a profile's roles summary that follows those rules or that a command could not save, and the roles a
connected device's descriptor reported. A dirty record is written once no input has reached the
input forwarder for 250 ms, and never later than 2 s after it first became dirty, even while input
continues; further changes before then are merged into that one write. Before that limit, it also
waits while a connection waits for its first input. A record whose write failed waits for its
backoff, and its 2 s limit then counts from the end of the backoff; while it waits, other records
keep their own timing. A profile's `rules.pb` is written before its roles summary. Apart from the
writes of everything waiting described below, the secondary loop writes one record per step,
resting after each step that wrote.

Discovered layouts, first-connection setup and the removal of lost records also wait for input to
pause, at most 2 s from when they first wait, but never run while a connection waits for its first
input.

A configuration editor's edits change the loaded rules at once and are acknowledged before they
reach flash. The editor hands them to storage as one change 500 ms after its last edit packet, and
at least every 2 s while edit packets keep arriving. They are dirty from the first edit, so an edit
reaches flash within about 2 s of being made. A loaded table with edits not yet written stays loaded
until they are, and every user of the profile, including a device that connects meanwhile, uses
the edited rules.

Everything waiting is written at once, in one step and whatever input is doing: before USB
enumerates again, before the bootloader is entered and when a configuration editor's profile is
released, including when the editor moves to another profile. Dirty records, but
not layouts, are also written before free space is counted to admit a new profile, a pairing or a
new setting, so admission counts them; a layout never takes the room admission keeps. Edits are lost
if power is removed before they reach flash.

A dirty `rules.pb` that cannot be written stays dirty and is retried with its own backoff, starting
at 2 s and doubling to 10 s. While the last try of a dirty `rules.pb` found the filesystem full,
`storage.full` is set, so a client can free space, and while the last try of one ended with an
unknown outcome, storage is not ready, as when a command's write ends so. A later try that fails
ends an unknown outcome only once the file is read back: holding the new rules, it counts as
written, and holding other contents, the failure is definite. A try that fails before reaching the
file, or whose file cannot be read, leaves the outcome unknown. Loading storage again, after the
radio restarts, resolves none of this, so storage stays not ready until the file's write is
resolved. Edits there is no memory to list are written straight from the editor's table on the same
timing, with their own backoff, and when everything waiting is written; an editor whose edits cannot
be written or listed refuses to move to another profile until they are. Any other flash failure, and
running out of memory for the file's buffer, is retried without being reported and does not make
storage not ready, so the profile can still be changed, overwritten or deleted; a command that
writes the profile's rules reports its own failure, and overwriting or deleting the profile ends the
retries. Other dirty records are retried with their own backoff without being reported, as when they
are saved with a command. A command whose change matches edits not yet written writes them before it
responds.

## Commit and recovery

A file is saved in place: it is opened truncated, written and closed. LittleFS keeps an existing
file's old contents until the close commits the new contents in one metadata commit, so an
interrupted save leaves either the old or the new contents. Creating a file commits it empty before
its contents are written, so an interrupted creation can leave an empty file. Every record is a
nonempty message, and an empty file holds no record: it reads as missing and is not listed as a key.
Equal bytes skip the write; old contents of the same length that cannot be read are written over,
but a file that cannot be looked up fails the save. A failed write of the contents leaves the file
errored, so its close commits none of them, and it is a definite failure even when the old contents
cannot be read back. Other errors are resolved by remounting and checking the file; an empty file
left by a failed creation is removed when possible, with its record directory when that is empty. An
outcome that cannot be read back returns `Unknown` so application state is not published as a
confirmed save.

The initial sequence, with `next_device: 1` and `next_profile: 1`, is committed before the
`initialized` marker. A missing sequence after initialization is a storage fault. Allocating an ID
commits the sequence with the following ID before the device or profile with the allocated ID is
published, and once every 32-bit ID has been used, allocation reports a full store. IDs are never reused, including after
deletion; failed pairing may leave gaps.
Provisional pairing exists only in RAM. Re-pairing finds the saved device by
reading device policies, retains its ID, policy and preference files, then
replaces policy and bond together and removes the saved layout. Separate stored
bond IDs and pending-pairing records do not exist.

Unpair first closes admission and forgets the active native entry. Removing `device.pb` commits
deletion. Preference and layout cleanup follows and is retried on startup. Without `device.pb`, a
directory is inactive and can be reclaimed. Profiles follow the same pattern: removing
`profile.pb` commits deletion, and `rules.pb` cleanup follows; a lost profile's references are
removed before its `profile.pb` is. A failed publication with a known outcome cleans up at once;
an unknown outcome leaves the files for recovery. Startup removes record directories without their
commit file, or with an empty one. This cleanup is best effort: its first failure ends it and
storage opens anyway, because a directory without its commit file holds no record, and IDs are
never reused. Only a mount failure keeps storage from opening. Native forget failure blocks the
session. Missing roots after initialization, corrupt roots or an address mismatch prevent Bluetooth
startup. BTstack reads cached roots, never a filesystem from inside its native root callback.

LittleFS supplies its normal power-loss recovery and wear levelling. Block
cycles are explicitly 500. Pairing admission leaves 32 KiB for maintenance and
budgets 16 KiB per additional device. Creating or copying a profile keeps the
same reserves. These are conservative admission estimates, not fixed record
limits or a guarantee that every arbitrary file update fits.

## RAM and execution

Read and program caches are 512 bytes each and lookahead is 128 bytes; they and the library
state stay allocated while the filesystem is mounted. An open file's 512-byte cache and a 512-byte
byte-comparison buffer live on the stack of the loop doing an operation, and file handles are not
retained between operations. LittleFS reads flash in units of at least 128 bytes: its directory and
metadata lookups ask for a few bytes at a time, and each flash read has a fixed cost, so whole units
take far fewer reads for about the same number of bytes. A record is read into one buffer of the
file's length and encoded into one buffer of its exact encoded length; both reserve memory
fallibly.

RAM holds only what accepting connections and translating input need:

- the identity and adapter preferences;
- for each enabled device on an enabled transport that the Bluetooth stack has room for, a
  reconnection entry: ID, peer address, address type and transport, trust and HID++ preferences, its
  profile layers, disconnect pause, backoff and last connection error. The native bond table holds
  its keys. This is bounded by `max_enabled` per transport, and its layers by `max_layers`;
- for each connected device, its policy, HID forwarding state, readings and warnings, and the
  profiles it uses;
- each profile in use by a connected device or a configuration interface, or with edits waiting to
  be written, once, shared by ID. A loaded profile holds its rules sorted for lookup, so its size
  follows what is saved rather than the input range. Loaded profiles together stay within the
  board's compiled-in profile memory budget.

Nothing is resident for a disabled device, for a disconnected device beyond its reconnection entry,
or for a profile no connection or configuration interface uses and whose edits are written. Device
and profile listings, `GetDevice`, `GetProfile`, settings lists and every profile command read the
files they need for each request. Saved device preferences are read when an integration applies
them, when a reading is compared with them, and when a client lists them.

For change events, the serial session tracks which settings and warnings the client may hold only
for devices with a connection, from the session's listings and events. A device's entry stays until
the events that follow the end of its connection have been written, so listing disabled or
disconnected devices keeps nothing. A settings event for a device that is not tracked reports each
changed setting it has and removes each changed setting it no longer has. A device, settings or
profile event whose record read fails stays pending and backs off on its own with the connection
backoff before it reads again; only its own success ends the backoff. Other events, and events that
need no read, such as those of a connection that holds its policy, go ahead meanwhile. An event
whose record is gone is dropped, since the record's removal event follows.

Startup reads each device policy once, keeps a reconnection entry and loads the bond for each
enabled device on an enabled transport that the stack has room for, in ascending ID order, and
keeps nothing for the rest. Enabling a transport or a device reads the policies it needs again.
Preference, layout and profile files are not read at startup; only the profile directory names are
listed.

When a connection starts, the Dongle reads the device's HID layout file and the `rules.pb` of each
profile in the layers its reconnection entry holds, before the device's first input is forwarded. A
profile another connected device or configuration interface already uses is shared instead of read
again. The device's policy and saved preferences are read after its first input, or one second after
the link connects when the device sends no input before then, and its `device` and
`settings_changed` events, which read its policy, wait for the same. Only a report that reaches the
input forwarder counts as input: keyboard rollover reports, HID++ responses and vendor reports of
other services do not. Background work (reading policies and preferences, saving first-connection
setup and discovered layouts, finishing unpairs, filling the stack, lost-record cleanup, syncing the
stack's bonds, repairing profile roles and retrying failed storage work) waits until every
connection has forwarded input or waited one second for it. A pairing in progress does not wait.
Commands and other devices' events can read records meanwhile, one at a time as described below.
Failed background storage work is retried with the connection backoff and does not hold up other
background work. Everything loaded for a connection is released when it ends; a profile is released
when its last connection or configuration interface lets it go and its edits are written. A
configuration interface releases its profile after five seconds without a packet, when its profile
changes or when it is disabled, and writes its edits then.

A device or profile listing page takes one pass over the record directory, selecting the lowest IDs
above `after`, then reads those records one per step. The pass lists record directories without
opening them; a directory whose record file is missing is left out when its record is read, so a
page can list fewer records than its size while more follow. A page of a device's settings reads the
device's policy and, while it is disconnected, its saved preferences, one per step; a page of a
profile's rules reads its metadata and then its rules file from the start only as far as the page, a
part at a time, skipping earlier rules without decoding them, or uses the loaded rules. Warnings and
features come from memory. A directory page reads one entry per step and keeps the lowest names
after `after`. Each response and event is encoded into one frame buffer, held until the USB endpoint
has written it; requests are not read while a response waits. File reads use one 512-byte chunk and
return the whole file in one response.

Filesystem operations are synchronous. Two loops on one executor share the application, its storage
and the Bluetooth stack, one at a time:

- The priority loop does what input needs right now: it polls the radio and handles every radio
  event, including the Bluetooth stack's own bond reads and connection setup with its layout and
  profile reads. Loading every record when the radio starts is a radio event too. When a pairing's
  bond is ready, it only notes the bond. It translates input, hands reports to USB and sends
  keyboard indicator output to devices.
- The secondary loop does everything else: serial requests, events and output, configuration editor
  packets, and background storage work, including deciding when USB enumerates again. It finishes a
  pairing: it reads device policies one per step to find a re-paired device, then saves the bond and
  admits the link. Before each step it waits for a pass of the priority loop that left no radio
  event unhandled and no input report waiting for a host that is reading reports, or, while input
  keeps arriving, for a pass 5 ms after its last step ended, so continuous input cannot hold up
  serial requests, configuration editors and background work indefinitely. Each step reads or
  writes about one record: a command or background task that touches many records takes one step
  per record, and a read, change and save of one record happens within one step. A command in
  progress runs until it responds, with no other request or background work in between; background
  work in progress waits while commands run, requests go before new background work, and events
  and background work take turns. A dirty record that is due is written before the next request
  or editor packet is read. A step that wrote flash is followed by a rest at least as long as the
  step took, so input keeps at least half the time while writes follow one another.

Input therefore waits for at most one step of the secondary loop at a time. A
command whose session ends while it runs finishes, and its response is discarded with the rest of
that session's output. LittleFS stays mounted between operations, because a mount reads every
metadata pair. A failed write, or a read error other than a missing, too-long or wrong-type path,
drops the mount so the next operation mounts again from flash. Free space comes from a traversal of
every metadata pair and file; the result is reused until the next write attempt. A check that admits
new data, such as pairing, creating a profile, adding a setting or saving a layout, counts again
when a write came after the last count. Saving a layout also counts after its write, to remove it
when it took the room kept for pairing. Otherwise the storage-full status is counted again by a
background step after other work. The interface provides no asynchronous-latency guarantee.

## Development access

See [development filesystem access](protocol/development.md#development-filesystem-access) for streamed
`storage.list` and `storage.read`. Downloads may contain Bluetooth secrets.
The CLI keeps payloads out of event logs, uses private temporary files on Unix,
and renames only after a successful terminal response with matching length. The
CLI and TUI save a downloaded record as JSON; see
[downloaded records](protocol/clients.md#downloaded-records).
Production firmware omits the handlers and transfer state.
