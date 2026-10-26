# Current persistent format

Pico and ESP32-S3 use LittleFS through a local `littlefs2` 0.8.1 patch for runtime
block counts. Each file holds one protobuf message from
[`proto/storage/v2/storage.proto`](../proto/storage/v2/storage.proto). Application data and Bluetooth
bonds share the filesystem. ESP32-S3 has no NVS partition.

## Flash allocation

Startup detects physical flash. The firmware reservation is 1 MiB on 2 MiB chips
and 2 MiB on chips of at least 4 MiB. The linked image end never selects the
filesystem start. Board configuration's `minimum_flash_bytes` describes the image's
minimum flash capacity, not the storage capacity detected at startup.

Every erase block is 4096 bytes. Storage starts at the reservation boundary.
Its first sector is a guard outside LittleFS; the filesystem uses all remaining
flash. Pico's Embassy driver addresses at most 16 MiB and rejects larger chips.
ESP-IDF uses larger chips when its flash driver supports their addressing mode;
otherwise storage initialization fails rather than silently reducing capacity.

RP2350 requires at least 4 MiB flash. Its E10 workaround sector occupies
`0x1ff000..0x200000` inside the 2 MiB firmware reservation, leaving 2,093,056 bytes
for the linked image. The UF2 writes its dedicated marker page at `0x1fff00`,
absolute address `0x101fff00`. This is the only UF2 write allowed outside the
ordinary image area. Link and artifact checks include page padding and sector
erase bounds. The guard occupies `0x200000..0x201000`, and LittleFS ends at the
physical flash end without a tail reservation.

All boards try capacity discovery up to three times, with a 10 ms scheduler delay
between failed retryable attempts. No delay follows success or the final attempt.
Missing measurements use the board's `minimum_flash_bytes` to format fresh or
invalid filesystems, without authorizing filesystem growth. A successful
measurement below that minimum forbids formatting on all boards. Pico decodes
valid JEDEC capacities below the minimum as successful measurements, not missing
responses. Read-only discovery can still retain an existing filesystem with valid
geometry and application roots. With a below-minimum measurement, missing or
invalid filesystems and invalid roots return a bounds error without writes.

ESP32-S3 images require at least 4 MiB and reserve 2 MiB for bootloader, partition
table, PHY data and application. Unsupported discovery, no response and timeouts
permit fallback after a successful raw flash read. An output populated by a failed
probe is discarded, including failures in the SDK's operation-end hook.
Uninitialized drivers and raw I/O errors stop storage initialization.
The fixed on-flash partition table contains:

| Name | Type | Subtype | Offset | Size |
| --- | --- | --- | ---: | ---: |
| `phy_init` | data | `phy` | `0xf000` | `0x1000` |
| `factory` | app | `factory` | `0x10000` | `0x1f0000` |

The bootloader starts at `0x0` and the partition table at `0x8000`.
After detection, firmware registers `cordial_layout`, subtype `0x40`, at
`0x200000` for one sector and `cordial_app`, subtype `0x83`, over the remaining
flash from `0x201000`. Registration checks overlap with existing partitions and
does not rewrite the on-flash table. Registration preserves the initialized
driver's addressable range when a later measurement is smaller or unavailable.
A successful larger measurement extends that range. The image header's minimum
capacity does not cap storage. ESP-IDF 6.1 identifies the chip before publishing
the default driver, then replaces its size with the image minimum. Startup decodes
that cached ID using the selected SDK driver's density encoding to retain access
to an expanded filesystem even when all new probes fail. Unknown encodings do not
establish a larger extent. The cached ID supplies an I/O bound, not a growth
measurement or format limit. Capability checks use the SDK's operation protection
because a driver can read SFDP while checking its addressing support.

## Startup recovery

Startup classifies the committed `/format.pb` before application recovery, ordinary
record cleanup, topology, identity or radio initialization. The operating format
is `2`, reported independently of the firmware release. A genuinely empty filesystem may provision
normally. A missing root with any committed data, an unknown or newer format, or
malformed or ambiguous migration input blocks startup. None of these cases authorizes
formatting or erasing saved data. Native LittleFS consistency recovery runs before
reading entries, and startup deletes internal `/.file-write-<16 lowercase hex digits>` files before classification,
even on blocked storage. These are the only exceptions to preserving blocked storage.
Driver I/O errors stop initialization without a format retry loop.

Format `1` migrates to `2`, preserving identity, bonds, preferences, profiles and
allocated IDs. Firmware classifies the source and runs eligible on-device
migrations internally, initially `1 -> 2`. Hosts only request bootstrap status or
close the connection while firmware migrates. A known older format that has no
on-device path or cannot fit within the on-device resource budget needs host
assistance to preserve saved data.

`Status.information.storage_format` is optional and reports the committed root's
format when it can be determined. Missing, malformed or unreadable roots leave it
absent and report `Status.errors`. It is not proof that the dataset is valid or
ready. Optional `target_storage_format` reports the firmware's operating target,
currently `2`, independently of its release version. Firmware always sets it,
including when storage is blocked or unreadable. After a compatible permanent hello and bootstrap
status, clients check presence and exact target support before other commands. An absent or zero
target prevents migration and normal commands. The current format is informational and never
controls migration, source selection, reconnection or success. The migration-required error authorizes
host assistance and the downloaded `/format.pb` selects the source. Without that
marker, fresh clients report readiness and errors without file access.

Firmware selects normal, assisted or blocked operation at boot. Assisted mode
reports `ready = false` and `STORAGE_MIGRATION_REQUIRED` with reason `STORAGE`, outcome
`NO_EFFECT` and no related errors. This exact marker authorizes migration only
when no other status errors are present. Mode and marker stay latched through
intermediate and final root uploads until a hardware reset. Root telemetry failures
report `STORAGE_FAILED`, which blocks host conversion regardless of outcome.
Normal operation never reports the marker. Blocked mode allows `GetStatus` and `Restart`. General
`Restart` is available in every mode and acknowledges before resetting the MCU,
then reruns ordinary startup as a power cycle would. It has no target or strict
dataset validator.

A missing root can begin initialization only on a genuinely empty filesystem.
Essential identity or sequence failures in older or mixed storage leave saved data
intact. Once storage is positively identified as current format, existing recovery
still resets corrupt essential roots when formatting is permitted. An unknown or
newer essential record format never authorizes that reset. Current startup checks
record versions without requiring every payload to pass migration validation.
Conflicting version headers or insufficient memory to identify a record's format
block startup without resetting storage.
Recognizable v1 records, including a Sequence whose first field resembles a v2
header, block current startup before essential-root recovery can reset data.
Corrupt nonessential current records stay on storage for ordinary loaders to handle;
inactive children receive ordinary best-effort cleanup without host assistance.
Migration validates the complete dataset strictly. Restart uses ordinary startup
recovery, including its current-format repair and cleanup rules.

The guard's 32-byte layout hash describes the backend and record layout,
independently of capacity. It is repaired only after filesystem validation and
growth succeed. A stale, missing or torn guard cannot override a valid filesystem.
A compatible filesystem at the same start grows only when available space after
reservations exceeds its mounted block count. Equal, smaller and unavailable
measurements leave its geometry unchanged. This policy is common to all boards
and does not depend on previous fallback use. Moving its start requires a reset.
Recovery only writes the current storage range; it never erases the firmware or
E10 sector.

Pico first mounts candidate filesystems without writing, using LittleFS's CRC
validation and persisted geometry. RP2350 inspects only the filesystem after the
2 MiB reservation. RP2040 checks that location first, then the 1 MiB reservation
only when the current capacity policy permits it. A reliable measurement of at
least 4 MiB requires the 2 MiB reservation even if valid older metadata remains at
1 MiB. A valid current filesystem at 2 MiB wins over stale metadata at 1 MiB when
the measurement is smaller or missing.

The persisted filesystem end must fit checked byte arithmetic and Pico's 16 MiB
address window. On unchanged hardware, valid geometry retains the prior I/O
extent. Reads and writes are bounded to that extent or a larger measured capacity
before native consistency recovery, validation of application roots, growth,
cleanup or guard repair can write. Rejected candidate probes never run consistency
recovery. Later I/O remounts repeat it within the accepted bounds before reading entries.
With no valid filesystem geometry, formatting uses only measured capacity or the
board minimum, never the exploratory address window. With valid geometry, a
format mismatch enters the migration classification above. Only positively identified
current-format corruption permits the existing essential-root recovery. The checked extent remains
valid on later boots with missing measurements, even when the filesystem extends
beyond the board minimum. A successful below-minimum measurement forbids
formatting. Actual flash I/O failures never trigger format.

ESP32-S3 applies the same preserving root classification within its addressable
extent. A later successful measurement can grow the filesystem again.

The selected mount stays open through root validation and growth. With a matching
guard, startup mounts a current 2 MiB layout once. An RP2040 filesystem at 1 MiB
needs the preceding failed candidate mount at 2 MiB when that location fits the
address window. Growth runs on the mounted filesystem. A stale guard requires an
unmount for direct guard access and a remount for cleanup; guard access does not
borrow the underlying driver while LittleFS owns it.

Moving the Pico storage start from 1 MiB to 2 MiB on chips of at least 4 MiB is a
breaking storage change and resets saved devices and preferences once. Later
images can grow within their reserved area without resetting storage. A release
containing this change needs a new minor version before 1.0 or a new major version
from 1.0. Firmware downgrades are not supported.

Firmware artifact schema `2` records the minimum chip capacity, stable firmware
reservation and `detected-tail` storage policy instead of claiming fixed runtime
geometry. Current inspection and flashing tools reject schema `1` artifacts.

## Files

| Path | Message | Contents |
| --- | --- | --- |
| `/format.pb` | `Format` | `format: 2` and `initialized`, recording whether roots have been committed |
| `/identity.pb` | `Identity` | Bluetooth address, IR, ER and derived IRK |
| `/adapter.pb` | `Adapter` | Adapter preferences; without the file, use Linux, the image's default name, BLE alone and no configuration interface |
| `/sequence.pb` | `Sequence` | The next device and profile IDs |
| `/devices/<id>/device.pb` | `Device` | `policy` and complete portable `bond` in one message; this file commits the device's existence |
| `/devices/<id>/settings.pb` | `Settings` | Explicitly saved physical-device preferences, with their integration |
| `/devices/<id>/layout.pb` | `Layout` | The device's HID layout: `maps` and, for BLE, `reports` |
| `/profiles/<id>/profile.pb` | `Profile` | Profile name and roles summary; this file commits the profile's existence |
| `/profiles/<id>/rules.pb` | `Rules` | The profile's rules |
| `/.file-upload` | Raw bytes | Temporary file used by host convention for chunked replacement; accessible through file commands and excluded from the migration dataset |
| `/.file-write-<random>` | Raw bytes | Internal atomic-creation temporary file; `<random>` is 16 lowercase hexadecimal digits. Hidden from file listings and deleted before migration. |

IDs are positive decimal integers without padding or prefixes. Devices and profiles have separate
sequences; `/devices/1/device.pb` and `/profiles/1/profile.pb` can both exist.

## File format versions

All nine file messages have a required `uint32 format = 1` field with the same
value, `2`. The field's name, number and type are permanent, so readers can
extract the version independently before interpreting the payload with the
current schema. Protobuf fields may appear in any order; `format` need not be
the first encoded field. A missing, zero, malformed, duplicate or unsupported
`format` makes the record invalid. An empty file holds no record and reads as
missing.

The remaining outer fields have these numbers:

| Message | Payload fields |
| --- | --- |
| `Format` | `initialized = 2` |
| `Identity` | `address = 2`, `ir = 3`, `er = 4`, `irk = 5` |
| `Adapter` | `name = 2`, `host_platform = 3`, `transports = 4`, `configuration_interfaces = 5` |
| `Sequence` | `next_device = 2`, `next_profile = 3` |
| `Device` | `policy = 2`, `bond = 3` |
| `Settings` | `preferences = 2` |
| `Layout` | `maps = 2`, `reports = 3`, `database_hash = 4` |
| `Profile` | `name = 2`, `roles = 3` |
| `Rules` | `rules = 2` |

Nested messages have no file-format field. Their field numbers and meanings
remain as defined in the schema, including `Rule.input = 1` and
`Rule.collection = 2`.

The version applies to the entire storage format, not to individual file types.
A version change updates all nine messages together. Compatible additions whose
zero value or absence preserves the existing meaning do not require a version
bump. Migration edges explicitly define their source and destination formats;
adding a format requires its schema and conversion support. Firmware downgrades
are not supported. Breaking storage changes require a minor release increment
before `1.0`, or a major increment from `1.0`.

The schema's comments document each message, its fields, and firmware validation that requires
relationships between records. Fields follow the kinds of the [compatibility
rules](protocol/compatibility.md#fields): the firmware checks each Required field, reads a missing
plain field as its zero value, and gives `optional` and message fields presence. Later firmware adds
fields whose zero value or absence keeps the earlier meaning, so records gain fields without a new
format version. The firmware ignores unknown fields; it leaves out entries of repeated fields
whose transport, integration, configuration interface or role is unknown or unspecified, and reads
an unknown host platform as Linux. Otherwise a record whose Required field is missing or out of
range is undecodable.

Enumerated values are protobuf enums: platforms, transports, integrations, roles, setting scopes,
report types and configuration interfaces. A setting is saved by its name. HID usages are numbers,
with the usage page in the high 16 bits and the usage ID in the low 16. Keys, addresses and report
maps are bytes.

Writers always set `format`, so every saved record is nonempty. This does not
replace payload requirements: identity keys, both next IDs of the sequence, a
device's policy and bond, at least one saved setting, the report maps of a layout,
a profile's name and at least one rule remain required. Adapter writers still
include an entry for every transport. Forgetting the last setting or rule deletes
its file rather than saving a message containing only `format`.

## Preserving migration

Versioned schemas retain format `1` in
[`proto/storage/v1/storage.proto`](../proto/storage/v1/storage.proto) and format
`2` in [`proto/storage/v2/storage.proto`](../proto/storage/v2/storage.proto).
Firmware compiles only its operating schema. Host tools retain historical
schemas. The archived v1 schema is frozen and pinned by a SHA256 golden check.
The v1 root reliably identifies format `1`. Its other eight file messages have
no format field; v2 shifts their outer tags by one and adds `format = 1` with
value `2`. Nested messages keep their tags and meanings.

The committed root selects the dataset epoch. During interrupted migration,
recognition uses both complete outer record grammars, not a blind decode with
the root's schema. A v1 Sequence with `next_device = 2` can look like a v2 header.
Conversely, `08 02 10 05 18 07` is a completed v2 Sequence with IDs `5` and `7`;
a loose v1 decoder would read `2` and `5` and silently ignore tag `3`. The frozen
v1 outer grammar rejects unknown outer tags and duplicate singular fields as
ambiguous. Well-defined unknown fields inside nested payloads remain intact.
Malformed or ambiguous records stop the migration before writes.

On-device startup validates the source once, replaces files with readback, validates
the target and commits the root last. Replacements retain the inventory cursor;
deletions restart it because directory indices shift. Memory holds one file and its
conversion, plus the current directory path stack. Directory enumeration reopens
and skips entries, so large directories still require quadratic metadata reads.
Many obsolete files also cause repeated scans.

After a compatible permanent hello and bootstrap status, the host verifies exact support for
`Status.information.target_storage_format` before other commands, then
requires the exact `STORAGE_MIGRATION_REQUIRED`/`STORAGE`/`NO_EFFECT` marker with no related
errors and no other status errors. The informational current format may be absent,
matching or different; none of these values authorizes or blocks migration.
The host recursively
inventories files with `ListFiles`, downloads every dataset file into RAM with
bounded `ReadFile` calls, determines the source from
`/format.pb` and validates mixed files. It then chooses a supported directed
forward graph path to the target and completes the conversion in RAM before
writing anything. Edges may skip numbers; format numbers need not be contiguous.
Each edge declares an immediate checkpoint or permission to defer it, safe
write/delete order, and recognition and resume rules for mixed files. The first
edge is `1 -> 2`, for both on-device and assisted migration.

Each directory listing is one complete framed response, optionally produced
incrementally. The recursive dataset inventory includes every committed file,
including `/format.pb`, and excludes `/.file-upload`. Before downloading or
allocating, a host checks at most 16,384 files and a checked total of at most
64 MiB of input, independent of adapter capacity. Conversion budgets input,
output and working allocations together within 256 MiB. These host safety
limits do not change serial payload limits. Unsupported sizes stop before
writes. No host filesystem data, backups or disk spill are allowed.

Every read response must match the requested path and offset exactly. The expected
size is the inventory size for download and the uploaded size for readback. After
an uncertain replacement, accept only the old or proposed new state. The old state
is absence for a new path. A present file must match the complete old or new
content. Equal sizes do not select a candidate. Size stays constant throughout a file read.
Its data length must equal
`min(request.length, size - offset)`. Offset must not exceed size, zero bytes
are allowed only at EOF, and short positive chunks are rejected. Clients reject
mismatch, overflow or excess data before retaining or converting bytes.

File access uses ordinary `ListFiles`, `ReadFile`, `WriteFile`, `MoveFile` and
`DeleteFile` commands. Debug firmware enables them during normal operation;
production firmware enables them only in assisted mode, which remains latched
until hardware reset even after the root declares the target. They never run
concurrently with an on-device migration or on blocked storage. Each command opens
and closes its files. The adapter keeps no upload session or prepare handshake.

`WriteFile` at offset zero creates or truncates a file. A positive offset
overwrites or appends in an existing file without truncating the remaining
contents and cannot exceed its current size. Each request is atomic and flushes
before acknowledgement; a series of requests is not atomic together. A file
that fits within 512 bytes can be replaced directly at offset zero.

For every new file, the storage layer writes to a randomly named internal
temporary file, then atomically moves it to the requested path. This applies to
all callers, including normal record saves, provisioning and migration. Before migration,
startup deletes leftover files matching the internal temporary naming pattern
without recovering their contents, even when classification blocks startup.
`ListFiles` excludes these internal files.
This happens inside the adapter and requires no extra protocol commands.

For a larger replacement, the host writes `/.file-upload` at offset zero,
appends the remaining chunks, reads back the complete temporary file, and uses
`MoveFile` to replace its destination atomically. It then reads back and compares
the destination. The temporary path is a host convention, accessible through all
ordinary file commands and excluded only from the migration dataset. There is
no persisted migration journal. After interruption, the host checks the
destination before restarting or discarding a temporary file. Startup removes
an abandoned temporary file only for current storage, an eligible on-device
migration or assisted mode. Blocked startup preserves the host temporary
file; the internal creation files follow their separate cleanup rule above.
The temporary file alone counts as an empty filesystem.

The host reads back and compares each replacement before proceeding. It deletes
obsolete files only once replacements are durable and verified, in the edge's
safe order. `DeleteFile` succeeds for a missing file and forbids `/` and
`/format.pb`. Cleanup, including the host temporary file, finishes before each
checkpoint. The host uploads `/format.pb` last, then reads back and compares the
entire root. Intermediate checkpoints keep file access enabled for the next edge.

After verifying the final root, the host sends `Restart`. Root uploads alone do
not change assisted mode. Restart acknowledges before resetting the MCU and runs
ordinary startup recovery without a separate migration completion validator.
The migration worker reconnects by stable `Status.id`, repeats the permanent hello and checks exact
target support again. This worker reconnects independently of normal host sessions. The worker requires the same target, no migration-required marker and
`ready = true` for success. Normal startup may also report the warnings `STORAGE_FULL`,
`PROFILE_MEMORY_LOW`, `USB_REQUIRED_CAPACITY_EXCEEDED` and
`USB_RESERVED_CAPACITY_EXCEEDED`, each without related errors. Any other error
stops the worker. Transport reconnection continues until cancellation, with old
handles released before reopening.

If the final root acknowledgement is lost and the marker persists, the active
worker reads back the durable root against its known previous and proposed contents.
An already committed final root needs no further file writes before `Restart`.
If a physical reset has already restored readiness without the marker, status
verification suffices. During active reconnection or restart, no marker with
`ready = false` and nothing but those warnings permits only further status checks.
Other errors stop the worker. If a restart acknowledgement is lost and the marker
persists, the worker retries `Restart`.

A fresh client seeing the marker downloads and validates the complete dataset.
If `/format.pb` already declares the target, validation succeeds without conversion
or file writes and the host sends `Restart` to exit assisted mode. Keeping the
marker until reset lets a fresh host finish after the previous host committed the
final root and disconnected before reset. Informational current-format presence
or equality never changes this behavior.

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
preferred-enabled flags, `integrations`, `roles`, `profiles` and `output_preferences`. A new
device's policy also sets `setup_pending` until its first-connection setup completes; a policy
without it has completed setup. Connection state, effective enablement, discovery, battery
readings and transient errors are not persisted.

- `integrations` lists each integration the device has a saved preference for, as `integration`
  and `enabled`. The implemented integration is HID++.
- `roles` lists the input roles the device's HID descriptor produces: keyboard, mouse, consumer
  control, system control and gamepad. It is rewritten when a connection reads a descriptor with
  different roles, once input pauses (see [when files are written](#when-files-are-written)), and
  is empty until the first descriptor is read.
- `output_preferences` lists saved output choices as `role` and `mode`, with at most one entry per
  role. Only gamepad is configurable. Its modes are Disabled, HID and XInput; a missing entry means
  Disabled. Writers retain a settled Disabled choice while `setup_pending` is true so reconnects
  and restarts preserve it while other setup steps finish. Otherwise they omit Disabled entries.
  Unknown roles or modes are ignored, and duplicate known roles make the record undecodable.
  Missing entries do not change keyboard or mouse behavior.
  Preferences survive renewing an existing bond. Unpairing deletes them. Each gamepad mode's
  configured USB candidates include paired records selecting it, including disabled, blocked and
  nonresident devices and devices whose transport is disabled. Connection state changes allocation
  priority; the [USB allocator](protocol/hid-forwarding.md#usb-allocation) selects what fits.
- `profiles` is the device's layers: the profile IDs it applies, in order. A new device has none,
  and an empty list passes everything through.

First-connection setup chooses HID when the validated descriptor has supported gamepad controls,
the firmware supports HID output and fewer than four saved devices select HID. Otherwise it settles
on Disabled. A saved or explicit choice wins. Setup does not run again after completion, including
when capacity becomes available or firmware gains HID support. The choice takes effect after its
policy save succeeds; a failed save leaves setup pending for a later connection to retry.
`setup_pending` clears only after both gamepad selection and HID++ detection or an explicit HID++
choice settle.

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

Typed serial settings map to these persisted names. Storage names, numeric values and metadata
are independent of the serial domain and field names.

| Serial field | Persisted `metadata.key` |
| --- | --- |
| `keyboard.function_row` | `fn.row_default` |
| `keyboard.platform` | `keyboard.platform` |
| `power.auto_off` | `power.auto_off` |
| `backlight.enabled` | `backlight.enabled` |
| `backlight.mode` | `backlight.mode` |
| `backlight.brightness` | `backlight.level` |
| `backlight.delay.hands_away` | `backlight.delay.hands_out` |
| `backlight.delay.hands_nearby` | `backlight.delay.hands_in` |
| `backlight.delay.external_power` | `backlight.delay.powered` |
| `backlight.startup_effect` | `backlight.power_on` |
| `backlight.dial_effect` | `backlight.crown` |
| `backlight.power_save` | `backlight.power_save` |
| `backlight.effect` | `backlight.effect` |
| Pointer Sensor `dpi`, index 0 or 1 | `pointer.dpi.0` or `pointer.dpi.1` |
| `wheel.mode` | `wheel.mode` |
| `wheel.free_spin_threshold` | `wheel.threshold` |
| `wheel.invert` | `wheel.invert` |
| `thumbwheel.invert` | `thumbwheel.invert` |

Stored wheel mode 1 means free spin and 2 means stepped. The serial enum names are
`WHEEL_MODE_FREE_SPIN` and `WHEEL_MODE_STEPPED`.

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

`rules.pb` contains `format` at field 1 and repeated `rules` at field 2; an absent
file means no rules. Each nested `Rule` requires an `input` usage at field 1 and
its application `collection` at field 2. Both are packed `uint32` usages, with the
usage page in the high 16 bits and the usage ID in the low 16. Missing or zero input or collection
makes the record undecodable; no collection is inferred. The required `effect` is one of `remap`
at field 3, `scale` at field 4 or `axis` at field 5:

- `remap` holds a list of `outputs`, each a `usage` with the `collection` of the report that carries
  it. An empty list disables the input.
- `scale` holds a `numerator` and a `denominator`.
- `axis` holds a packed `destination` usage at field 1 and an `invert` boolean at field 2. It
  routes an absolute Game Pad axis within the input's Game Pad collection; inversion reverses the
  normalized axis range. It has no deadzones, curves or sensitivity settings.

The list is sorted by input usage, then collection, with at most one rule for each pair, so a
loaded profile can be searched without sorting; a file that is not is undecodable. Packed usage
order is usage page followed by usage ID. Each rule is its own entry of the
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
`ERROR_CODE_STORAGE_FULL` is present in `Status.errors`, so a client can free space. While the last
try of one ended with an unknown outcome, storage is not ready, as when a command's write ends so.
A later try that fails ends an unknown outcome only once the file is read back: holding the new rules, it counts as
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
interrupted save leaves either the old or the new contents, including the format
and payload together. Creating a file commits it empty before its contents are
written, so an interrupted creation can leave an empty file. Every record is a
nonempty message, and an empty file holds no record: it reads as missing and is not listed as a key.
Equal bytes skip the write; old contents of the same length that cannot be read are written over,
but a file that cannot be looked up fails the save. A failed write of the contents leaves the file
errored, so its close commits none of them, and it is a definite failure even when the old contents
cannot be read back. Other errors are resolved by remounting and checking the file; an empty file
left by a failed creation is removed when possible, with its record directory when that is empty. An
outcome that cannot be read back returns `Unknown` so application state is not published as a
confirmed save.

The initial sequence, with `next_device: 1` and `next_profile: 1`, is committed before the
`initialized` marker. A missing sequence after initialization triggers startup recovery. Allocating an ID
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
never reused. A driver I/O failure keeps storage unavailable. Native forget failure blocks the
session. An address mismatch prevents Bluetooth startup. BTstack reads cached roots, never a filesystem from inside its native root callback.

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
  its keys. The advertised `DEVICES.limit` bounds resident entries per transport, and `LAYERS.limit`
  bounds their layers;
- for each connected device, its policy, HID forwarding state, readings and errors, and the
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

For change events, the serial session tracks which settings the client may hold only
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

A device or profile listing walks the record directory in ascending ID order and reads records one
at a time. A directory whose record file is missing is left out when its record is read. Settings
listings read the device's policy and, while disconnected, its saved preferences. Profile rule
listings read metadata and walk the rules file incrementally, or use the loaded rules. Features
come from memory, and directory listings walk entries in bytewise name order. Listings iterate
live state without collecting the complete list. Corrupt nonresident device records and corrupt
profile metadata stay on storage while a response or pending cleanup events block reporting. The
adapter remembers a rescan for each record kind, then rechecks one record at a time and drains its
removal or reference-change events before continuing cleanup. Read failures leave records intact
and retry in a later pass. A paused profile cleanup retains one bounded device cursor separately
from bond sync and resident admission, so those operations can progress while its events wait.
Closing the session discards its events; cleanup continues without them.

The adapter serializes bounded batches as ordinary list responses, concatenates their protobuf
bytes and feeds one incremental COBS encoder. Each listing is one logical response in one frame.
Requests wait until that response finishes, and changes during it remain pending for subsequent
events. File reads use a 512-byte working buffer and return the requested range in one response.
An unbounded debug read returns the whole file. See
[list streaming](protocol/compatibility.md#listings).

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
  per record, and a read, change and save of one record happens within one step. List production
  alternates with turns for deferred saves, editor packets and background work, even when every
  listed record is missing or corrupt. While list output is blocked, each secondary step can
  advance that same work, including pairing, connection hydration and saved settings jobs. The
  response reserves the serial output until its delimiter, so events wait. Other commands finish
  before another request or background step runs. Requests go before new background work, and
  events and background work take turns. A dirty record that is due is written before the next
  request or editor packet is read. A step that wrote flash is followed by a rest at least as long as the
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

## File access

`ListFiles`, `ReadFile`, `WriteFile`, `MoveFile` and `DeleteFile` provide file
access in debug firmware and during required host assistance in production.
See [file commands](protocol/development.md#file-access). Downloads may contain
Bluetooth secrets. The CLI keeps payloads out of event logs and uses private
temporary files on Unix for explicit downloads. It saves a downloaded record
as JSON; see [downloaded records](protocol/clients.md#downloaded-records).
Assisted migration keeps its entire dataset in RAM instead.
