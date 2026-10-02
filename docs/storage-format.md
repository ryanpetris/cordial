# Current persistent format

The application uses upstream LittleFS through `littlefs2` 0.8.1 on Pico and
ESP32-S3. The layout identity is `littlefs-json-1`. No alternative reader,
migration, duplicate-copy scheme or automatic reformat is maintained.
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
| `/adapter.json` | `host_platform`, optional custom `name` and enabled `transports`; without the file, use Linux, the image's default name and BLE alone |
| `/sequence.json` | Last allocated device ID as a JSON integer |
| `/devices/<16 lowercase hex digits>/device.json` | `policy` and complete portable `bond` in one document |
| `/devices/<id>/hidpp.json` | Array of all explicitly saved HID++ preferences |
| `/devices/<id>/layout.json` | The device's HID layout: `maps` and, for BLE, `reports` |

The adapter name override is a trimmed string of 1..64 UTF-8 bytes without control characters. A null or absent `name` uses the board default baked into the image. `adapter.name.set` with `name: null` clears the override. `transports` lists the enabled transports by name (`"classic"`, `"ble"`); an absent field means BLE alone, and an empty list disables both. Name, platform and transport updates preserve each other and commit before runtime state changes.

A device policy contains its ID, peer address/type/transport, name, trusted,
blocked, HID++ and preferred-enabled flags. A new device's policy also
contains `setup_pending: true` until its first-connection setup completes;
the field is omitted afterwards, and a policy without it has completed setup. Connection state, effective
enablement, discovery, battery readings and transient errors are not persisted.
The bond holds the canonical native-independent identity and security keys.
Classic stores link key and key type. BLE stores local and peer security
fields, including LTK, IRK, CSRK, RAND, EDIV, flags, key size and signing counter.
Cryptographic byte arrays are hex strings. The existing binary canonical bond
encoding remains only at native Bluetooth adapter boundaries.

Each HID++ preference contains `metadata` and a numeric `value`. Metadata
contains the named setting key, feature ID, feature revision, scope, choices
and optional min/max/step range. One settings file is rewritten for a setting
change. Forgetting the last setting deletes the file.

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

Files are compact JSON. There is no 512-byte document ceiling and no configured
number of saved devices. Setting counts follow the supported setting-key table;
choices use the u16 domain. Native connection/bond-table and wire-frame bounds
still apply. Allocation and storage exhaustion are resource errors.

## Commit and recovery

File replacement writes and closes a sibling `.tmp`, then renames it over the
destination. Equal bytes skip the write. Errors are resolved by remounting and
checking the authoritative destination. An unreadable outcome returns `Unknown`
so application state is not published as a confirmed save.

The initial zero sequence is committed before the `initialized` marker. A missing
sequence after initialization is a storage fault. The device ID sequence is committed before pairing can publish a new device.
IDs are never reused, including after deletion; failed pairing may leave gaps.
Provisional pairing exists only in RAM. Re-pairing retains the device ID, policy
and preference files, then replaces policy and bond together and removes the
saved layout. Separate stored bond IDs and pending-pairing records do not exist.

Unpair first closes admission and forgets the active native entry. Removing
`device.json` commits deletion. Preference and layout cleanup follows and is
retried on startup. Without `device.json`, a directory is inactive and can be
reclaimed.
Startup removes interrupted temporary files and orphan device directories.
Native forget failure blocks the session. Missing roots after initialization,
corrupt roots or an address mismatch prevent Bluetooth startup. BTstack reads
cached roots, never a filesystem from inside its native root callback.

LittleFS supplies its normal power-loss recovery and wear levelling. Block
cycles are explicitly 500. Pairing admission leaves 32 KiB for maintenance and
budgets 16 KiB per additional device. These are conservative admission estimates,
not fixed record limits or a guarantee that every arbitrary file update fits.

## RAM and execution

Read and program caches are 512 bytes each, lookahead is 128 bytes and an open
file cache is 512 bytes: 1,664 bytes before library state. Byte comparison uses
another 512-byte buffer. These allocations live on the owner stack during an
operation; filesystem handles are not retained across polls. Typed JSON buffers
grow with the document, with fallible buffer reservation. Policies and saved
preferences still consume heap proportional to saved devices; the filesystem
itself does not build a full file index in RAM.

Documents are loaded one at a time at startup. Inactive catalogs remain resident
to support offline settings inspection. HID layout files are not loaded at
startup; one is read when its device's connection starts and released once the
backend has accepted it. There is no configured saved-device
count, but RAM can be exhausted before flash.

Directory enumeration reopens and skips to an index, trading O(n²) enumeration
for fixed iterator state. Logical document enumeration selects the next ID
without collecting all keys. Each response and event is encoded into
one frame buffer, held until the USB endpoint has written it; requests are not
read while a response waits. File reads use one 512-byte chunk and return the
whole file in one response.

Filesystem operations are synchronous and serialized by the application owner.
They can stall execution while flash is busy. Remounting each operation avoids
self-referential handles and stale-cache commit checks, at additional read cost.
The interface provides no asynchronous-latency guarantee.

## Development access

See [development filesystem access](protocol/development.md#development-filesystem-access) for streamed
`storage.list` and `storage.read`. Downloads may contain Bluetooth secrets.
The CLI keeps payloads out of event logs, uses private temporary files on Unix,
and renames only after a successful terminal response with matching length.
Production firmware omits the handlers and transfer state.
