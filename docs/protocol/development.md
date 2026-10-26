# Development and file commands

[Protocol index](README.md)

Development firmware, built with the `debug` or `development` profile, reports
`Status.information.development_build = true`. It accepts `ListFeatures` and
`EnterBootloader`; production firmware omits these handlers and answers with
`ERROR_CODE_UNKNOWN_COMMAND`.

| Command | Result | Behavior |
| --- | --- | --- |
| `ListFeatures` | `FeatureList` | The device's complete integration feature tables, by integration and then index. For HID++, each entry has the feature index, ID, version and flags, and whether the Dongle supports it; hidden and engineering features are listed as unsupported. |
| `EnterBootloader` | none | See [Commands](commands.md#adapter). |

After `EnterBootloader`, the host expects the Dongle to reappear in USB programming mode: UF2 or
`picotool` for Pico, `esptool` for ESP32-S3. A missing response does not prove the reboot failed;
check the USB mode before retrying. Flashing tools verify the image's profile and hardware before
writing it.

## File access

Debug firmware enables file access during normal operation. Production firmware
enables all five file commands in assisted mode, selected at boot and latched
until hardware reset. Writing an intermediate or final root does not change that
mode. File commands never run concurrently with an on-device migration or on
blocked storage. Blocked mode permits `GetStatus` and `Restart`.

After a compatible permanent hello and bootstrap status, clients check exact
`Status.information.target_storage_format` support before file access or other commands.
Only `STORAGE_MIGRATION_REQUIRED` with reason `STORAGE`, outcome `NO_EFFECT`, no related
errors and no other status errors authorizes assisted migration. The marker stays
present until hardware reset, including after intermediate and final root uploads.
The informational `storage_format` does not control migration, source selection,
reconnection or success. The source comes from the downloaded `/format.pb`.
Without the marker, a fresh client reports readiness and errors without file access.
Root telemetry failures report `STORAGE_FAILED` and block migration.
There is no prepare handshake or
upload session; each command opens and closes its files. See
[preserving migration](../storage-format.md#preserving-migration).

| Command | Result | Behavior |
| --- | --- | --- |
| `ListFiles` | `FileList` | All entries in one directory, ordered by name compared bytewise. The host recurses explicitly. |
| `ReadFile` | `FileData` | Bytes starting at `offset`, with an optional bounded `length` of 1..512. Assisted production access requires a length. An omitted length is valid only at offset zero in debug firmware and returns the entire file without truncation. |
| `WriteFile` | none | Writes at most 512 bytes, atomically per request and durable before acknowledgement. Offset zero creates or truncates; a positive offset overwrites or appends in an existing file without truncating its remaining contents. Offset cannot exceed its current size. Empty data at offset zero creates or truncates an empty file. |
| `MoveFile` | none | Atomically moves a file over a destination file. After interruption, either source and previous destination remain, or the destination contains the complete source and the source is absent. Directories are unsupported; identical paths have no effect. |
| `DeleteFile` | none | Deletes one file. A missing file succeeds. `/` and `/format.pb` are forbidden. |

Paths are canonical absolute UTF-8 paths of at most 192 bytes, with no empty, `.` or `..`
components, control characters or trailing slash. `/` is valid for `ListFiles`. Paths can name
identity and bond files. There is no raw flash access. The complete encoded request, including its
envelope, must still fit 1024 protobuf bytes.

Internal `/.file-write-<16 lowercase hex digits>` files used for atomic creation are
hidden from `ListFiles` and deleted before startup classification, including when storage
is blocked. This cleanup is an explicit exception to preserving blocked storage.
The host's `/.file-upload` path remains an ordinary file.

`FileData.path` and `offset` must match the request. Its `size` is the full file size and stays
constant throughout a download or readback. Offset may equal but never exceed size. Bounded reads
return exactly `min(length, size - offset)` bytes, with empty data valid only at EOF. Clients check
these values and arithmetic bounds before retaining bytes. Short positive reads are invalid.

To replace a file in chunks, the host writes and appends to `/.file-upload`, reads back the entire
temporary file, moves it over the destination, and reads back the destination. This temporary path
is a host convention, accessible through ordinary file commands and excluded from the migration
dataset. Several writes together have no atomicity guarantee. Each mutation responds with empty
success after its effects are durable. After an uncertain response or disconnect, the host uses
`ListFiles` and `ReadFile` to verify complete old or proposed new contents before deciding whether
to repeat a mutation. Equal sizes require byte comparison.

A missing path returns `ERROR_CODE_NOT_FOUND`, except for idempotent deletion. Storage failures
carry code `STORAGE_FAILED` and reason `STORAGE`, with the full outcome evidence. A read failure's
outcome concerns returned results, not whether saved data changed. A `ListFiles` continuation
failure replaces the partial list with `Response.error`, never successful partial contents.
File access works while Bluetooth is unavailable and follows the shared
[error semantics](limits-and-errors.md#errors).

## Restart

`Restart` is available in production and debug firmware, in normal, assisted and
blocked modes. It is refused with `ERROR_CODE_BUSY` while a pairing or setup link is
open. Otherwise it drains an empty success response before resetting the MCU and
reruns ordinary startup recovery as a power cycle would. It neither writes the
root nor performs a target check or strict dataset validation.

A migration worker validates and converts files in RAM, verifies replacements,
and removes obsolete and temporary files before uploading each checkpoint root
last. It reads back the final root, sends `Restart` and reconnects by stable
adapter ID. Every reconnect checks exact target support and requires the same
target. Success requires the marker to be absent and `ready = true`, with no errors
other than the normal warnings `STORAGE_FULL`, `PROFILE_MEMORY_LOW`,
`USB_REQUIRED_CAPACITY_EXCEEDED` and `USB_RESERVED_CAPACITY_EXCEEDED`, each without
related errors.
When a final root acknowledgement is lost and the marker persists, the active
worker checks the durable root against its RAM snapshot before restarting. It
does not rewrite a root whose proposed contents are already durable. If a physical
reset has already restored normal readiness without the marker, status suffices.
An active worker seeing no marker, `ready = false` and nothing but those warnings
waits with status checks only. A persistent marker after a lost restart acknowledgement
permits retrying `Restart`. Other errors stop the worker.

A fresh host seeing the marker downloads and validates the complete dataset even
if the informational current format matches or is absent. If the downloaded root
is already the target, it performs no conversion or file writes and sends `Restart`
to exit assisted mode. The marker lets a fresh host finish after the previous host
committed the final root but disconnected before reset.
