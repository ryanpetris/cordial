# Limits and Errors

[Protocol index](README.md)

## Limits

| Limit | Value |
| --- | --- |
| Complete host `Request` | 1024 protobuf bytes (including envelope and all entries) before COBS encoding |
| Adapter response/event frames | No total protocol size cap |
| Rust host receive frame | 32 MiB (`33,554,432` decoded bytes; `33,686,537` COBS bytes excluding delimiter) |
| `AdapterHello` | 1024 decoded protobuf bytes (`ClientHello` is always empty) |
| Pointer Sensor DPI choices | <= 6 per sensor |
| Scan candidates | 32 per scan |
| Normal connections / enabled devices | `CONNECTIONS.limit` / `DEVICES.limit` per transport (independent per transport) |
| Pairing connection | 1 shared reserve overall, outside normal transport allowances |
| Saved devices and profiles | Bounded by flash (`ERROR_CODE_STORAGE_FULL` when full) |
| Loaded profiles | Shared by ID within `ProfileSupport.resources[MEMORY].limit` |
| Profile layers per device | `ProfileSupport.resources[LAYERS].limit` |
| Remap outputs | `ProfileSupport.resources[OUTPUTS].limit` per rule and per held input |
| Adapter name / profile name / text info | `1..64` UTF-8 bytes (`<= 64` for text info), no control characters |
| File path | Canonical absolute UTF-8, `<= 192` bytes |
| Bounded file read / file write | `1..512` bytes per read (required in assisted production mode); `<= 512` data bytes per write |
| Host migration input / RAM | `<= 16,384` files, `<= 64 MiB` total input, `<= 256 MiB` total RAM (no disk spill) |
| Migration subprocess IPC frame | `1..65,536` protobuf bytes after 4-byte LE length prefix |

HID report-map and forwarding limits are documented in [HID forwarding](hid-forwarding.md). Rust receive buffering is described in [Rust host receive limit](transport.md#rust-host-receive-limit).

### Bluetooth Capacity

Each transport reports the same normal allowance in `DEVICES.limit` and `CONNECTIONS.limit` (Classic and BLE are independent; both exclude the shared pairing connection). Connecting and closing links occupy normal slots until freed.

`rust/tools/bluetooth_config.rs` generates matching Rust and C pool sizes from `CORDIAL_CONFIG` and `rust/tools/bluetooth_limits.json` (defaulting to RP2350 allowances when `chip` is absent/unknown):

| Board Preset | Normal Classic | Normal BLE | Runtime Slots (incl. Pairing) | Native Classic Bonds | Native BLE Bonds |
| --- | ---: | ---: | ---: | ---: | ---: |
| `pico_w` | 2 | 8 | 11 | 3 | 9 |
| `pico2_w` | 6 | 14 | 21 | 7 | 15 |
| `pimoroni_pico_plus2_w` | 6 | 14 | 21 | 7 | 15 |
| `waveshare_rp2350b_plus_w` | 6 | 14 | 21 | 7 | 15 |
| `esp32s3` | 0 | 8 | 9 | 0 | 9 |

Each supported transport reserves one extra native bond entry for pairing, while at most one pairing connection runs at a time. ESP32-S3 configures 10 controller activities for links and scanning. BLE requests a 7.5 ms connection interval.

## Errors

Every failure across commands, adapter status, devices, record reads, scans, pairing, integrations, settings, and outputs uses the structured `Error { code, reason, outcome, related }` message (scopes remain separate; integration/setting errors never become device errors). Error codes are defined in [`proto/serial/v1/cordial.proto`](../../proto/serial/v1/cordial.proto).

### Error Reasons

| Reason | Context |
| --- | --- |
| `UNKNOWN` | No additional context. |
| `ENABLED_DEVICES` | Enabled-device admission (`DEVICES` allowance). |
| `CONNECTIONS` | Connection slot admission (`CONNECTIONS` allowance). |
| `SCAN_CANDIDATES` | Candidate retention or ID allocation. |
| `STORAGE` | Persistent storage read, write, or space allocation. |
| `PROFILE_MEMORY` | Individual profile size or shared loaded-profile RAM budget. |
| `RADIO` | Bluetooth radio initialization or operation. |
| `OUTPUT_BACKEND` | Selected USB output backend. |
| `INPUT_LAYOUT` | Device HID layout for requested translation. |

### Error Outcomes

| Outcome | Evidence |
| --- | --- |
| `UNSPECIFIED` | No outcome evidence supplied (also how unknown enum values decode). |
| `UNKNOWN` | Adapter explicitly cannot determine whether effects occurred. |
| `NO_EFFECT` | Confirmed that no effects or results occurred in the documented scope. |
| `PARTIAL` | Some effects/results confirmed; remaining effects are not confirmed absent. |
| `COMPLETE` | Intended effects/results confirmed despite a later failure (such as cleanup). |

Key error invariants:

- An error code alone never implies an outcome or authorizes retries; conversions preserve the entire `Error` tree recursively.
- `related` holds secondary failures in their own scope without replacing the primary failure or outcome.
- Error lists (including `related`) sort lexicographically by `(code, reason, outcome, related)` with shorter prefixes first, deduplicating only identical complete error trees.
- An unresolved flash write reports `STORAGE_FAILED` (`STORAGE`, `UNKNOWN`) and keeps storage unready across radio recovery until resolved or restarted. `STORAGE_FULL` does not make storage unready.
- After a lost response or reconnect, clients inspect current state rather than automatically retrying mutations, pairing, or unpairing.

### Condition Errors

- **USB capacity**: `USB_REQUIRED_CAPACITY_EXCEEDED` (omitted candidate with priority `< 0xC0`; prominent warning and tray badge) and `USB_RESERVED_CAPACITY_EXCEEDED` (omitted candidate in `0xC0..0xDF`; subtle indicator) are reported independently in `Status.errors` after allocation without rejecting valid saved preferences.
- **Profile memory**: `PROFILE_MEMORY_LOW` triggers at >= 85% of a nonzero budget; `PROFILE_MEMORY_EXCEEDED` and `PROFILE_LOAD_FAILED` appear in `Device.errors` when a connected device's layers fail to load.
- **Async device failures**: Immediate `ConnectDevice` refusals return `Response.error`; post-acceptance connection failures and post-disconnect unpair failures appear in `Device.errors`.

## Client Pre-Checks

Clients prevent predictable refusals before sending; firmware still validates every request and returns an error if client state was stale.

| Firmware Refusal | Client Pre-Check |
| --- | --- |
| `NO_CAPACITY(ENABLED_DEVICES)` | Nonresident device left enabled and unblocked on an enabled transport when active devices on that transport reach `DEVICES.limit` |
| `NO_CAPACITY(STORAGE)` on pairing | `ERROR_CODE_STORAGE_FULL` in `Status.errors` |
| `BUSY` on second pairing | Latest pairing step is not `done` or `error` |
| `NO_PROMPT` on prompt answer | Latest pairing step is `enter_code` or `confirm_code` |
| `DISABLED` / `BLOCKED` on connect | `Device.enabled`, `Device.blocked`, `Device.inactive` |
| `BAD_ARGS` on empty scan | Request lists at least one transport |
| `UNSUPPORTED` on transport | `Status.transports` and `TransportSupport.enabled` |
| `NOT_READY` | `Status.ready` |
| `NOT_CONNECTED` | `Device.state` |
| `BAD_ARGS` on setting value | Field's advertised range or choices |
| `UNKNOWN_COMMAND` / `UNSUPPORTED` on profiles | Presence of `Status.profile_support` |
| `BAD_ARGS` on profile rule | `ProfileSupport.ranges` (kind, usage, collection) and `OUTPUTS.limit` |
| `NO_CAPACITY(PROFILE_MEMORY)` | No client check (evaluated by firmware) |
| `BAD_ARGS` on layer count | `ProfileSupport.resources[LAYERS].limit` |
| `BAD_ARGS` on enabled interface without profile | Resulting `enabled` and `profile` per interface |
| `UNSUPPORTED` on conflicting interfaces | `ConfigurationInterfaceSupport.conflicts` |
| `IN_USE` on `DeleteProfile` | Every interface's `profile` and every device's `profiles` |
| `BAD_ARGS` on adapter or profile name | Nonempty (or `""` reset for adapter), `<= 64` UTF-8 bytes, no control chars |
