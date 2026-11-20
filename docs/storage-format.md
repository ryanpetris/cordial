# On-Board Storage Format

Both RP2040/RP2350 and ESP32-S3 boards store all persistent state in a single LittleFS filesystem (using a patched `littlefs2` 0.8.1 supporting runtime block counts). Each record file holds one raw protobuf message from [`proto/storage/v2/storage.proto`](../proto/storage/v2/storage.proto) with no framing or length prefix. Application data and Bluetooth bonds share this filesystem; ESP32-S3 does not use an NVS partition.

Firmware downgrades are not supported.

## Flash Allocation

Board presets fix the firmware reservation at 1 MiB (`pico_w`) or 2 MiB (RP2350 and ESP32-S3). Neither the linked image size nor detected flash capacity moves the filesystem start. `minimum_flash_bytes` declares the image's minimum chip size and fallback capacity, not the actual detected storage size.

- **Sector layout**: Erase blocks are 4096 bytes. Storage begins at the reservation boundary with a 1-sector guard (holding a 32-byte layout hash independent of capacity), followed by LittleFS across the remaining flash.
- **RP2350 E10 workaround**: Requires >= 4 MiB flash. The E10 sector occupies `0x1FF000..0x200000` inside the 2 MiB firmware reservation (leaving 2,093,056 bytes for the image), with a UF2 marker page at `0x1FFF00` (`0x101FFF00`). The guard occupies `0x200000..0x201000`, and LittleFS runs to physical flash end.
- **Maximum addressable flash**: Pico's Embassy driver addresses up to 16 MiB (larger chips are rejected). ESP-IDF uses larger chips when its flash driver supports their addressing mode; otherwise initialization fails rather than silently truncating capacity.
- **Capacity discovery**: All boards attempt discovery up to 3 times (10 ms scheduler delay between failed retryable attempts). Missing measurements fall back to `minimum_flash_bytes` for fresh formatting without permitting growth. A successful measurement below `minimum_flash_bytes` forbids formatting on all boards (though an already-valid filesystem may still mount read-only within checked bounds).
- **Artifact schema**: Firmware artifact schema `2` records minimum chip capacity, stable firmware reservation, and `detected-tail` storage policy.

### ESP32-S3 Partition Layout

ESP32-S3 reserves 2 MiB for bootloader (`0x0`), partition table (`0x8000`), PHY data, and application:

| Name | Type | Subtype | Offset | Size |
| --- | --- | --- | ---: | ---: |
| `phy_init` | data | `phy` | `0xF000` | `0x1000` |
| `factory` | app | `factory` | `0x10000` | `0x1F0000` |

At startup, firmware registers in-memory partitions `cordial_layout` (subtype `0x40`, one sector at `0x200000`) and `cordial_app` (subtype `0x83`, remaining flash from `0x201000`) without modifying the on-flash partition table. Because ESP-IDF 6.1 caps the default driver size to the image header minimum after probing, startup decodes the cached flash ID density encoding to preserve access to previously expanded filesystems if runtime probes fail.

## Startup Classification and Recovery

At boot, firmware mounts LittleFS (preserving checked bounds on Pico's 16 MiB window), runs native LittleFS consistency recovery, deletes any leftover internal `/.file-write-<hex16>` temporary files, and classifies `/format.pb` before initializing topology, identity, or radio. The current operating format is `2`.

Startup selects one of three operating modes:

- **Normal**: Empty flash provisions cleanly; valid format `2` mounts directly (growing LittleFS if detected flash exceeds the mounted block count, then repairing a stale/missing guard). Eligible older formats (`1 -> 2`) migrate on-device automatically.
- **Assisted**: An older known format that cannot migrate on-device latches assisted mode until hardware reset, reporting `ready = false` with `STORAGE_MIGRATION_REQUIRED` (`reason = STORAGE`, `outcome = NO_EFFECT`, no related or other errors). Production firmware enables the five file commands only in this mode.
- **Blocked**: A missing `/format.pb` alongside committed data, an unknown/newer format, malformed or ambiguous records, or root telemetry failures block startup without erasing data. Only `GetStatus` and `Restart` are accepted.

Additional recovery rules:

- Essential-root corruption triggers automatic reset only after storage is positively identified as format `2` and formatting is permitted. Recognizable v1 records (even a v1 `Sequence` whose first field resembles a v2 tag) block startup rather than resetting data.
- Flash driver I/O errors halt storage initialization without entering a format loop.
- `Status.information.target_storage_format` is always reported (currently `2`), even on blocked storage; `Status.information.storage_format` is informational only and omitted when `/format.pb` is missing or unreadable.

## Files

| Path | Message | Contents |
| --- | --- | --- |
| `/format.pb` | `Format` | `format: 2` and `initialized` (roots committed) |
| `/identity.pb` | `Identity` | Bluetooth address, IR, ER, and derived IRK |
| `/adapter.pb` | `Adapter` | Adapter preferences (defaults when absent: Linux, board default name, BLE only, no editors) |
| `/sequence.pb` | `Sequence` | Next device and profile IDs |
| `/devices/<id>/device.pb` | `Device` | Policy and portable bond; commits the device's existence |
| `/devices/<id>/settings.pb` | `Settings` | Explicitly saved physical-device preferences with integration metadata |
| `/devices/<id>/layout.pb` | `Layout` | Cached HID report maps and (for BLE) report characteristics and GATT Database Hash |
| `/profiles/<id>/profile.pb` | `Profile` | Profile name and derived roles; commits the profile's existence |
| `/profiles/<id>/rules.pb` | `Rules` | Sorted profile rules |
| `/.file-upload` | Raw bytes | Host staging path for chunked uploads; accessible via file commands, excluded from migration datasets |
| `/.file-write-<hex16>` | Raw bytes | Internal atomic-creation temporary file; hidden from `ListFiles`, deleted at startup before classification |

Record IDs are positive unpadded decimal integers. Devices and profiles have independent ID sequences and never reuse IDs.

## File Format Versions

All nine record messages reserve field `1` permanently as a required `uint32 format` equal to the storage format (`2`). Readers verify `format` before decoding the remaining fields; missing, zero, duplicate, malformed, or unsupported `format` values make a record invalid. Empty files hold no record and read as missing.

Outer payload field numbers in format `2`:

| Message | Payload Fields |
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

Nested messages carry no `format` field and keep their existing field numbers.

- **Compatibility**: Fields follow the [compatibility field kinds](protocol/compatibility.md#fields). Additive fields whose zero value or absence preserves prior meaning do not bump the format version. Firmware ignores unknown fields, drops repeated entries with unknown enums (transport, integration, interface, role), and reads an unknown host platform as Linux. Breaking changes bump the format version across all nine messages together and require a minor release bump before `1.0` (or major from `1.0`).
- **Nonempty records**: Writers always set `format` alongside required payload fields (identity keys, sequence counters, device policy/bond, >= 1 saved setting, layout report maps, profile name, >= 1 rule). Forgetting the last setting or rule deletes its file.
- **No 512-byte record ceiling**: Records are encoded directly into exact-length protobuf buffers with no fixed device, profile, or setting count cap beyond flash, native tables, and wire-frame bounds.

## Preserving Migration

Historical schemas stay in [`proto/storage/v1/storage.proto`](../proto/storage/v1/storage.proto) (frozen and pinned by SHA-256) alongside the current v2 schema in [`proto/storage/v2/storage.proto`](../proto/storage/v2/storage.proto). Firmware compiles only its operating schema; host tools retain historical schemas. In format `1`, only `Format` had `format = 1`; format `2` adds `format = 1` to the other eight outer messages and shifts their outer tags by one while keeping nested messages unchanged.

During interrupted migrations, mixed files are classified using strict complete outer grammars (rejecting unknown outer tags and duplicate singular fields on v1 records so a v2 `Sequence` cannot mis-decode as v1, while preserving well-defined unknown nested fields). Any malformed or ambiguous record aborts migration before any write.

### On-Device Migration

Eligible edges (currently `1 -> 2`) run internally at startup: firmware validates the complete source dataset, rewrites each file with readback verification (converting one file at a time in RAM), validates the target dataset, and commits `/format.pb` last. Hosts may only read status or close the connection while on-device migration runs.

### Host-Assisted Migration

When firmware boots into assisted mode (`STORAGE_MIGRATION_REQUIRED` with `STORAGE`/`NO_EFFECT`, no related errors, and no other errors), a host client that supports the exact `target_storage_format` drives migration over the ordinary file commands (`ListFiles`, `ReadFile`, `WriteFile`, `MoveFile`, `DeleteFile`):

1. **Inventory and download**: Recursively lists all committed files (excluding `/.file-upload`) and downloads the dataset into RAM using bounded `ReadFile` calls. Safety caps enforced before download/allocation: <= 16,384 files, <= 64 MiB total input, and <= 256 MiB total RAM for input, output, and working memory (no host disk spill or backups).
2. **Validate and convert**: Identifies the source format from `/format.pb`, validates any mixed files, chooses a directed forward migration path to `target_storage_format`, and converts the entire dataset in RAM before writing anything. (If `/format.pb` already declares the target and validates cleanly, no file writes occur.)
3. **Upload with readback**: Replaces files <= 512 bytes atomically at offset `0`; stages larger files via `/.file-upload`, reads back the full staging file, moves it atomically with `MoveFile`, and reads back the destination. Deletes obsolete files only after replacements are verified, cleans up `/.file-upload`, writes `/format.pb` last, and verifies it by readback.
4. **Restart and verify**: Sends `Restart`, reconnects by stable `Status.id` (retrying until cancelled), repeats the handshake, verifies unchanged `target_storage_format`, confirms `ready = true` with `STORAGE_MIGRATION_REQUIRED` absent, and allows only standard warnings (`STORAGE_FULL`, `PROFILE_MEMORY_LOW`, `USB_REQUIRED_CAPACITY_EXCEEDED`, `USB_RESERVED_CAPACITY_EXCEEDED`, without related errors).

Every `ReadFile` response must match path, offset, constant file size, and exact byte count `min(request.length, size - offset)`. If an acknowledgement is lost mid-write or during the final root/restart step, the worker reads back durable state against its old and proposed contents before deciding whether to retry `Restart` or fail. Because assisted mode stays latched until hardware reset, a fresh client can validate an already-converted dataset and send `Restart` to finish an interrupted run.

## Record Details

### Adapter (`/adapter.pb`)

- `name`: optional trimmed override (`1..64` UTF-8 bytes, no control characters); absent uses the board default.
- `transports`: one `(transport, enabled)` entry per transport (defaults: BLE on, Classic off).
- `configuration_interfaces`: entries with `(interface, enabled, optional profile)`; omitted entries are disabled with no profile; enabled entries require a profile.

Every preference update rewrites `/adapter.pb` durably before updating runtime state.

### Devices (`/devices/<id>/`)

- **Policy** (`device.pb`): stores ID, peer identity (`address`, `random`, `transport`), name, `trusted`, `blocked`, `enabled`, `setup_pending`, `integrations`, `roles`, `profiles` (ordered layer IDs), and `output_preferences` (Gamepad: Disabled, HID, or XInput; writers retain an explicit Disabled entry while `setup_pending` is true and omit it afterward).
- **First-connection setup**: Automatically picks HID gamepad output if the validated descriptor has supported gamepad controls, firmware supports HID, and fewer than 4 saved devices select HID (otherwise Disabled), and probes HID++ integration. Explicit user choices win. `setup_pending` clears only once both choices settle and save.
- **Bond** (`device.pb`): canonical transport-independent bond (owner device ID, peer identity, Classic link key/type or BLE LTK/IRK/CSRK/RAND/EDIV/flags/key size/signing counter).
- **Settings** (`settings.pb`): explicitly saved preferences with `integration`, `metadata` (`key`, feature ID/revision, scope, choices, min/max/step range), and numeric `value`. Serial fields map to persisted `metadata.key` names:

| Serial Field | Persisted `metadata.key` |
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
| Pointer Sensor `dpi` (index 0 or 1) | `pointer.dpi.0` / `pointer.dpi.1` |
| `wheel.mode` | `wheel.mode` (`1` = free spin, `2` = stepped) |
| `wheel.free_spin_threshold` | `wheel.threshold` |
| `wheel.invert` | `wheel.invert` |
| `thumbwheel.invert` | `thumbwheel.invert` |

- **HID layout** (`layout.pb`): optional cache of raw report maps, BLE report characteristic handles/CCCDs, and 16-byte GATT Database Hash so subsequent connections start forwarding without rediscovery. Saved only when free space covers the 32 KiB maintenance reserve, 16 KiB pairing reserve, and the file itself; automatically deleted if corrupt, unusable, re-paired, or if writing encroaches on reserves.

### Profiles (`/profiles/<id>/`)

- `profile.pb`: `name` and derived `roles` (computed from rules' input collections).
- `rules.pb`: repeated `Rule` entries sorted strictly by `(input, collection)` packed `uint32` (`(page << 16) | usage`), with at most one rule per pair and no collection inference. Each rule's effect is `remap` (list of `(usage, collection)` outputs; empty disables), `scale` (`numerator`, `denominator`), or `axis` (`destination`, `invert`). An unsorted or invalid file is undecodable.
- `rules.pb` is streamed entry-by-entry when loading into RAM. Saving `rules.pb` commits rule changes; if updating `profile.pb` roles afterward fails, roles repair on a background retry. Copying writes `rules.pb` before `profile.pb`.
- Startup checks profile directory existence without loading rules and strips dangling profile references from device layers and disabled/invalid configuration interfaces.

## When Files Are Written

- **Synchronous (before command response)**: `SetAdapter`, `SetDevice`, `SetSettings`, `CreateProfile`, `CopyProfile`, `DeleteProfile`, `SetProfileRules`, `UnpairDevice`, and pairing bond commits all flush to flash before responding.
- **Deferred (on input pause)**: VIA/Vial rule edits, profile role repairs, discovered device roles, HID layouts, first-connection setup, and lost-record cleanup wait until input has been idle for 250 ms (capped at 2 s from first becoming dirty; skipped while any connection awaits its first input). VIA/Vial batches incoming packets 500 ms after the last packet (and at least every 2 s during bursts).
- **Forced flush**: All pending writes flush immediately before USB re-enumeration, bootloader entry, editor profile release/switch, and (for dirty records, excluding layouts) before free-space admission checks. Power loss before flush loses uncommitted editor edits.
- **Failed deferred writes**: Dirty `rules.pb` writes retry with exponential backoff (2 s to 10 s). Flash-full failures set `ERROR_CODE_STORAGE_FULL`; unknown write outcomes mark storage unready until readback resolves them. Other dirty records retry silently on backoff.

## Commit, Reserves, and Deletion

- **Atomic updates**: Existing files are overwritten in place via LittleFS's copy-on-write commit (unchanged bytes skip writing). New files are written to `/.file-write-<hex16>` and atomically moved to their final path. Unresolvable write errors remount and verify; if still unresolvable, they return `Unknown` rather than reporting success.
- **Sequences and pairing**: `/sequence.pb` initializes to `(1, 1)` before `Format.initialized` is committed and increments before publishing each new device or profile ID. Re-pairing updates the existing device's policy and bond in place (retaining ID and preferences) and removes its cached layout.
- **Unpair and delete**: Removing `device.pb` or `profile.pb` commits deletion; remaining files and directories without their commit file are cleaned up immediately or on a best-effort basis at startup.
- **Wear and admission**: LittleFS block cycles are set to `500`. Pairing and profile creation/copying require 32 KiB maintenance reserve plus 16 KiB per new entry (conservative admission estimates).

## RAM and Execution Model

LittleFS uses 512-byte read/program caches and a 128-byte lookahead buffer while mounted (minimum read granularity 128 bytes); temporary 512-byte file/compare buffers live on the stack during operations.

RAM retains only what active operation needs:

- Adapter identity and preferences.
- Resident reconnection entries and native bonds for enabled devices on enabled transports (up to `DEVICES.limit` per transport, loaded in ascending ID order).
- Connected devices' policies, HID forwarding state, readings, and errors.
- Loaded profiles currently referenced by connected devices, active editors, or unflushed edits (shared by ID within the board's `profile_memory_budget`).

Startup does not read settings, layouts, or profile rules. When a device connects, firmware reads its layout and profile rules first so input begins immediately, deferring policy/settings hydration and background work until first input (or 1 s).

Because LittleFS I/O is synchronous, firmware schedules work across two loops on one executor:

- **Priority loop**: handles radio polling/events, native bond reads, connection layout/profile loads, input translation, USB HID submission, and keyboard lock LEDs.
- **Secondary loop**: handles serial requests/events, VIA/Vial packets, pairing completion, and background storage work one record step at a time. It yields to the priority loop (guaranteed at least one step every 5 ms during continuous input) and rests after each flash write for at least the write's duration so input keeps at least half the CPU time.

Listings iterate live records incrementally into bounded batches fed to a single incremental COBS frame (see [Listings](protocol/compatibility.md#listings)). File access commands are detailed in [Development and file commands](protocol/development.md#file-access).
