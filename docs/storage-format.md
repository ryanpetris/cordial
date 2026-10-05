# Current persistent format

The application uses upstream LittleFS through `littlefs2` 0.8.1 on Pico and
ESP32-S3. The layout identity is `littlefs-json-1`. There is no migration or
automatic reformat on a read failure.
ESP-IDF retains its platform NVS partition; application data and Bluetooth
bonds use the common filesystem.

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
includes bootloader, partition table, platform NVS and PHY data.

Provisioning verifies the entire guard and filesystem range is erased before
its first write. It claims the guard before formatting. A missing guard over
nonblank storage, a mismatch, an interrupted format or a mount failure leaves
storage unavailable. None of these failures triggers an automatic erase.

## Files

| Path | Contents |
| --- | --- |
| `/format.json` | `format:1` and `initialized`, recording whether roots have been committed |
| `/identity.json` | Bluetooth address, IR, ER and derived IRK as hex strings |
| `/adapter.json` | Adapter preferences; without the file, use Linux, the image's default name, BLE alone and no configuration interface |
| `/sequence.json` | Last allocated device and profile IDs |
| `/devices/<id>/device.json` | `policy` and complete portable `bond` in one document; this file commits the device's existence |
| `/devices/<id>/settings.json` | Array of explicitly saved physical-device preferences, with their integration |
| `/devices/<id>/layout.json` | The device's HID layout: `maps` and, for BLE, `reports` |
| `/profiles/<id>/profile.json` | Profile name and roles summary; this file commits the profile's existence |
| `/profiles/<id>/rules.json` | The profile's rules |

IDs are positive decimal integers without padding or prefixes. Devices and profiles have separate
sequences; `/devices/1/device.json` and `/profiles/1/profile.json` can both exist.

[The JSON Schema](storage.schema.json) documents each file, its fields, and firmware validation
that requires relationships between records.

Enumerated values are stored by name, never by number: platforms, transports, integrations, roles
and configuration interfaces. HID usages are stored as numbers.

## Adapter

The adapter name override is a trimmed string of 1..64 UTF-8 bytes without control characters. A
null or absent `name` uses the board default baked into the image. `transports` lists the enabled
transports by name (`"classic"`, `"ble"`); an absent field means BLE alone, and an empty list
disables both.

`configuration_interfaces` lists the saved preferences of each configuration interface that has any,
as `interface` (`"via"` or `"vial"`), `enabled` and an optional numeric `profile` ID. An interface
without an entry is disabled with no profile, and writers omit such entries. An enabled interface
has a profile. Every adapter preference update rewrites the file once and commits before runtime
state changes.

## Devices

A device policy contains its ID, peer address/type/transport, name, trusted, blocked and
preferred-enabled flags, `integrations`, `roles` and `profiles`. A new device's policy also
contains `setup_pending: true` until its first-connection setup completes; the field is omitted
afterwards, and a policy without it has completed setup. Connection state, effective enablement,
discovery, battery readings and transient errors are not persisted.

- `integrations` lists each integration the device has a saved preference for, as `kind` and
  `enabled`. The implemented kind is `hidpp`.
- `roles` lists the input roles the device's HID descriptor produces: `keyboard`, `mouse`,
  `consumer_control` and `system_control`. It is rewritten when a connection reads a descriptor with
  different roles, and is empty until the first descriptor is read.
- `profiles` is the device's layers: the profile IDs it applies, in order. A new device has none.
  Absent means an empty list, which passes everything through, and writers omit it then.

The bond holds the canonical native-independent identity and security keys.
Classic stores link key and key type. BLE stores local and peer security
fields, including LTK, IRK, CSRK, RAND, EDIV, flags, key size and signing counter.
Cryptographic byte arrays are hex strings. The binary canonical bond
encoding is used only at native Bluetooth adapter boundaries.

Each physical-device preference contains `integration`, `metadata` and a numeric `value`.
Metadata contains the named setting key, feature ID, feature revision, scope, choices and optional
min/max/step range, which is enough to list the setting while the device is disconnected. One
settings file is rewritten for a setting change. Forgetting the last setting deletes the file.

The HID layout file holds each HID service's raw report map as a hex string in
`maps`. A BLE layout also lists its report characteristics in `reports`: owning
service index, report type, report ID, value handle, properties and the
optional CCCD handle. When the device exposes a GATT Database Hash, `hash`
holds it as a hex string. The file is written when a connection discovers the
device, including at pairing, and when a connected device's HID layout changes,
but not while storage is not ready. It is written only when free space covers
the 32 KiB maintenance reserve, the 16 KiB reserved for pairing another device
and the file itself, and it is removed again when writing it leaves less than
both reserves. A layout that cannot be saved removes the saved file instead.
Later connections supply it to the Bluetooth backend so input starts without
rediscovery. The file is optional: a missing, undecodable or unusable
file means the next connection discovers the device. An undecodable or
unusable file is removed when it is read, and the file is removed when a
connection ends because the device's HID layout could not be used. Re-pairing
removes it, and deleting the device removes it with the preferences. Layout
files are read by device ID and are not part of record enumeration; mounting
removes them with the directories of deleted devices.

## Profiles

`profile.json` contains `name` and `roles`, the roles the profile's rules change, worked out when
the rules are saved. The directory supplies the profile's ID.

`rules.json` contains `rules`; an absent file means no rules. Usages are stored compactly as
`[usage_page, usage]` pairs. Each rule has an `input` pair and either `remap` or `scale`:

- `remap` is a list of outputs, each `[usage_page, usage, collection_page, collection_usage]` with
  the collection of the report that carries it. An empty list disables the input.
- `scale` is `[numerator, denominator]`.

The array is sorted by input, with at most one rule for each input, so a loaded profile can be
searched without sorting. Forgetting the last rule deletes the file.

Profile commands read and write these files directly. A rules change rewrites `rules.json` once,
and `profile.json` too when the roles change. Saving `rules.json` commits the change; when
`profile.json` cannot be rewritten after it, the saved roles are corrected in the background,
retrying with the connection backoff until the Dongle restarts. Copying writes the new profile's
`rules.json` before its `profile.json`.

Deleting a profile, and finding whether one is referenced, reads `adapter.json` and every device
policy. Startup lists the profile directories without reading them, and while reading every device
policy removes references to profiles that do not exist from device layers and configuration
interfaces; deleting a lost profile removes its references the same way. A reference startup cannot
remove, because the write fails, is left out of what the Dongle uses and removed again at the next
startup.

Files are compact JSON. There is no 512-byte document ceiling and no configured number of saved
devices, profiles or saved settings. Setting choices use the u16 domain. Native
connection/bond-table and wire-frame bounds still apply. Allocation and storage exhaustion are
resource errors.

## Commit and recovery

File replacement writes and closes a sibling `.tmp`, then renames it over the
destination. Equal bytes skip the write. Errors are resolved by remounting and
checking the authoritative destination. An unreadable outcome returns `Unknown`
so application state is not published as a confirmed save.

The initial sequence object, with `device: 0` and `profile: 0`, is committed before the
`initialized` marker. A missing sequence after initialization is a storage fault. Each counter is
committed before its next device or profile is published. IDs are never reused, including after
deletion; failed pairing may leave gaps.
Provisional pairing exists only in RAM. Re-pairing finds the saved device by
reading device policies, retains its ID, policy and preference files, then
replaces policy and bond together and removes the saved layout. Separate stored
bond IDs and pending-pairing records do not exist.

Unpair first closes admission and forgets the active native entry. Removing `device.json` commits
deletion. Preference and layout cleanup follows and is retried on startup. Without `device.json`, a
directory is inactive and can be reclaimed. Profiles follow the same pattern: removing
`profile.json` commits deletion, and `rules.json` cleanup follows; a lost profile's references are
removed before its `profile.json` is. A failed publication with a known outcome cleans up at once;
an unknown outcome leaves the files for recovery. Startup removes interrupted temporary files and
record directories without their commit file. This cleanup is best effort: its first failure ends
it and storage opens anyway, because the next replacement truncates a stale temporary file, a
directory without its commit file holds no record, and IDs are never reused. Only a mount failure
keeps storage from opening. Native forget failure blocks the session. Missing
roots after initialization, corrupt roots or an address mismatch prevent Bluetooth startup. BTstack
reads cached roots, never a filesystem from inside its native root callback.

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
take far fewer reads for about the same number of bytes. Typed JSON buffers grow with the document,
with fallible buffer reservation.

RAM holds only what accepting connections and translating input need:

- the identity and adapter preferences;
- for each enabled device on an enabled transport that the Bluetooth stack has room for, a
  reconnection entry: ID, peer address, address type and transport, trust and HID++ preferences, its
  profile layers, disconnect pause, backoff and last connection error. The native bond table holds
  its keys. This is bounded by `max_enabled` per transport, and its layers by `max_layers`;
- for each connected device, its policy, HID forwarding state, readings and warnings, and the
  profiles it uses;
- each profile in use by a connected device or a configuration interface, once, shared by ID. A
  loaded profile holds its rules sorted for lookup, so its size follows what is saved rather than
  the input range. Loaded profiles together stay within the board's compiled-in profile memory
  budget.

Nothing is resident for a disabled device, for a disconnected device beyond its reconnection entry,
or for a profile no connection or configuration interface uses. Device and profile listings,
`GetDevice`, `GetProfile`, settings lists and every profile command read the files they need for
each request. Saved device preferences are read when an integration applies them, when a reading is
compared with them, and when a client lists them.

For change events, the serial session tracks which settings and warnings the client may hold only
for devices with a connection, from the session's listings and events. A device's entry stays until
the events that follow the end of its connection have been written, so listing disabled or
disconnected devices keeps nothing. A settings event for a device that is not tracked reports each
changed setting it has and removes each changed setting it no longer has. A device, settings,
profile or rules event whose record read fails stays pending and backs off on its own with the
connection backoff before it reads again; only its own success ends the backoff. Other events, and
events that need no read, such as those of a connection that holds its policy, go ahead
meanwhile. An event whose record is gone is dropped, since the record's removal event follows.

Startup reads each device policy once, keeps a reconnection entry and loads the bond for each
enabled device on an enabled transport that the stack has room for, in ascending ID order, and
keeps nothing for the rest. Enabling a transport or a device reads the policies it needs again.
Preference, layout and profile files are not read at startup; only the profile directory names are
listed.

When a connection starts, the Dongle reads the device's HID layout file and the `rules.json` of each
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
when its last connection or configuration interface lets it go. A configuration interface releases
its profile after five seconds without a packet, when its profile changes or when it is disabled.

A device or profile listing page takes one pass over the record directory, selecting the lowest IDs
above `after`, then reads those records one per step. The pass lists record directories without
opening them; a directory whose record file is missing is left out when its record is read, so a
page can list fewer records than its size while more follow. A page of a device's settings reads the
device's policy and, while it is disconnected, its saved preferences, one per step; a page of a
profile's rules reads its metadata and then its rules file, or uses the loaded rules. Warnings and
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
  writes about one record, and a step that writes may count free space again: a command or
  background task that touches many records takes one step per record, and a read, change and save
  of one record happens within one step. A command in progress runs until it responds, with no other
  request or background work in between; background work in progress waits while commands run,
  requests go before new background work, and events and background work take turns.

Input therefore waits for at most one step of the secondary loop at a time. A
command whose session ends while it runs finishes, and its response is discarded with the rest of
that session's output. LittleFS stays mounted between operations, because a mount reads every
metadata pair. A failed write, or a read error other than a missing, too-long or wrong-type path,
drops the mount so the next operation mounts again from flash. Free space comes from a traversal of
every metadata pair and file; the result is reused until the next write attempt. The interface
provides no asynchronous-latency guarantee.

## Development access

See [development filesystem access](protocol/development.md#development-filesystem-access) for streamed
`storage.list` and `storage.read`. Downloads may contain Bluetooth secrets.
The CLI keeps payloads out of event logs, uses private temporary files on Unix,
and renames only after a successful terminal response with matching length.
Production firmware omits the handlers and transfer state.
