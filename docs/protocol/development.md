# Development and File Commands

[Protocol index](README.md)

## Development-Only Commands

Firmware built with the `debug` or `development` profile sets `Status.information.development_build = true` and enables `ListFeatures`, `EnterBootloader`, and normal-mode file access. Production firmware omits `ListFeatures` and `EnterBootloader` (returning `ERROR_CODE_UNKNOWN_COMMAND`).

| Command | Result | Behavior |
| --- | --- | --- |
| `ListFeatures` | `FeatureList` | Complete integration feature table sorted by integration then index. For HID++, includes index, ID, version, flags, and support flag (hidden/engineering features marked unsupported). |
| `EnterBootloader` | none | Drains response and `SessionEnding(BOOTLOADER)`, releases held input, and reboots into ROM programming mode (UF2/`picotool` on Pico, `esptool` on ESP32-S3). Refused with `ERROR_CODE_BUSY` during pairing/setup. |

A missing response to `EnterBootloader` does not prove the reboot failed; check USB mode before retrying.

## File Access

The five file commands operate on LittleFS in **debug firmware during normal operation** and in **production firmware during assisted migration mode** (latched at boot until hardware reset). They never run during on-device migration or on blocked storage, and they remain usable during radio faults once `target_storage_format` is verified. Each command opens and closes files independently (no upload sessions or prepare handshakes).

| Command | Result | Behavior |
| --- | --- | --- |
| `ListFiles` | `FileList` | All entries in one directory in bytewise name order (host recurses explicitly). Hides internal `/.file-write-<hex16>` files; exposes `/.file-upload`. |
| `ReadFile` | `FileData` | Reads from `offset` with optional `length` (`1..512`, required in assisted production mode). Omitting `length` at `offset = 0` in debug firmware streams the entire file. |
| `WriteFile` | none | Writes `<= 512` data bytes atomically and durably per request. `offset = 0` creates/truncates (empty data creates an empty file); positive `offset` overwrites/appends up to current file size without truncating trailing bytes. |
| `MoveFile` | none | Atomically replaces a destination file with a source file (directories unsupported; identical paths are no-ops). |
| `DeleteFile` | none | Deletes one file (idempotent on missing files; `/` and `/format.pb` are forbidden). |

### Paths, Read Bounds, and Chunked Replacement

- **Paths**: Canonical absolute UTF-8 paths (`<= 192` bytes) with no empty, `.`, or `..` segments, control characters, or trailing slash (`/` is valid for `ListFiles`). Complete requests still fit 1024 protobuf bytes. There is no raw flash access.
- **Read validation**: `FileData.path` and `offset` must match the request; `size` stays constant across reads; `offset <= size`; bounded reads return exactly `min(length, size - offset)` bytes (empty only at EOF; short positive reads are invalid).
- **Chunked upload convention**: Files `<= 512` bytes replace directly at offset `0`. Larger files write sequentially to `/.file-upload`, read back the full staging file, move it atomically over the destination via `MoveFile`, and verify the destination by readback. After an uncertain response or disconnect, verify complete byte contents via `ListFiles`/`ReadFile` before retrying (equal file size alone is insufficient).
- **Errors**: Missing paths return `ERROR_CODE_NOT_FOUND` (except `DeleteFile`). Storage faults report `STORAGE_FAILED` with reason `STORAGE` and full outcome evidence. Mid-listing failures replace the partial list with `Response.error`.

## Restart

`Restart` is available in both production and debug firmware across normal, assisted, and blocked modes (refused with `ERROR_CODE_BUSY` during pairing/setup). It sends an empty response, drains `SessionEnding(REBOOT)`, and resets the MCU to rerun standard startup classification and recovery without validating or modifying storage.

For the full host-assisted migration and restart verification state machine, see [Preserving migration](../storage-format.md#preserving-migration).
