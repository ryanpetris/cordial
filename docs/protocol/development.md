# Development commands and diagnostics

[Protocol index](README.md)

## Development authentication diagnostics

Development ESP-NimBLE firmware includes `authentication_failure` in
`adapter.status` after a native authentication failure. The object contains
`backend: "nimble"`, a numeric `attempt` scoped to the current boot, `stage`,
`status`, `encrypted` and `bonded`. Stages are `initiate`, `encryption`,
`security_state`, `prompt`, `inject` and `reply`. `status` retains the NimBLE
status for API failures; a zero `security_state` status means lookup succeeded
but the encryption or bonding requirement failed. Locally invalid prompts or
replies use NimBLE's invalid-argument/unsupported codes.

The snapshot survives USB sessions and pairing cleanup. A new explicit pairing
attempt or reboot clears it. It contains no peer addresses, passkeys or bond
keys and is never written to flash. Production firmware omits the field.
Absence means no retained diagnostic; it does not establish pairing success.
The most recent failure replaces the snapshot. Inspect it with
`cordial --json adapter status` before another pairing attempt.

## Development bootloader entry

`adapter.bootloader.enter` exists only in development firmware. Compile out its handler in production, omit the `debug` capability, and return the normal `unknown_command` error if a client sends it to a production image. No runtime setting, serial baud/DTR sequence, or alternative USB reset hook may enable remote BOOTSEL entry in production. Physical chip recovery is separate from this development command. The result mode is `bootsel` for Pico and `download` for ESP32-S3. ESP entry disconnects Embassy USB, returns the internal PHY to USB Serial/JTAG and requests ROM download mode.

Accept the command only when no other ordinary request or persistent-storage write is in progress; otherwise return `busy`. Monitoring and the idle heartbeat mechanism do not count as pending operations. Stop or finish scanning, pairing, connection, and policy operations before requesting bootloader entry.

```json
{"v":1,"id":30,"cmd":"adapter.bootloader.enter","args":{}}
{"v":1,"type":"response","id":30,"ok":true,"done":true,"result":{"rebooting":true,"mode":"bootsel"}}
```

Once accepted, stop accepting further commands, queue the terminal acknowledgement, stop input forwarding, and make a bounded attempt to flush the response and release held USB input before rebooting into the selected ROM programming mode within one second. The operation is not cancellable. It ends the control session and temporarily removes the USB keyboard, mouse, and serial interfaces. Preserve saved bonds and policy; this command does not erase or install firmware.

The host expects the adapter to reappear in USB programming mode and stops heartbeat/control traffic after acknowledgement or disconnection. A missing acknowledgement does not prove the reboot failed; inspect the resulting USB mode before retrying. Use UF2 transfer or `picotool` for Pico, or `esptool` for ESP32-S3, for the subsequent flash. Reopen a fresh control session after the new development firmware boots.

The CLI exposes `adapter.bootloader.enter` only when supported by the selected firmware, and the TUI provides a clickable "Enter bootloader" action with a clear indication that HID input will stop. A client checks the `debug` capability before invoking it. Flashing tools must independently verify the new artifact's profile and hardware compatibility before writing it.

## Development filesystem access

`storage.list` and `storage.read` take `{"path":"/absolute/path"}`. Paths are
ASCII, at most 255 bytes, with no `.` or `..` components or control bytes. The
path identifies any file within the mounted application filesystem, including
identity and bond files. There is no raw-flash access or write command.

One request produces multiple responses with its original request ID:

- `storage.list`: each nonterminal result is `{"name":"device.json","type":"file","size":800}`. Types are `file` or `directory`. The terminal result is `{"count":1}`. Listing is nonrecursive.
- `storage.read`: each nonterminal result is `{"offset":0,"data":"BASE64"}` with up to 512 decoded bytes. Offsets count exact file bytes. The terminal result is `{"bytes":800}`.

Empty files and directories return only the terminal result. File size and
entry count are not capped by the chunk buffer. A successful terminal response
is required; cancellation, disconnect, timeout or errors leave an incomplete
transfer. The global filesystem generation is checked between responses.
Concurrent mutation ends the transfer with `storage_changed`; clients discard
partial output. Slow readers do not hold a filesystem handle or delay writes.
Normal output backpressure and session teardown apply, and `request.cancel` accepts
these requests. Heartbeat expiry aborts active file transfers.

Only development profiles advertise and implement these commands. Production
returns `unknown_command` for a well-formed request, before pending-operation
admission. Shared argument validation precedes profile gating, so malformed
arguments return `invalid_args`. File
access works when the filesystem is available and Bluetooth is unavailable.
Mount failure returns `storage_failed`; it never triggers a format.

## Development GATT write diagnostics

On development XIAO/NimBLE builds, `adapter.status.gatt_writes` reports up to
four recent per-token report or Protocol Mode writes. Each has a native `token`,
`request`, ATT `handle`, `response` and queue `accepted` flags and monotonic
`queued_ms`, `started_ms`, `completed_ms` timestamps. Missing timestamps are
null. `status` is the raw NimBLE completion status, zero for success and null
until completion. Queued means submission was attempted; started means the
native task reached its write handler. A failed queue submission has
`accepted:false`. Early native rejection can complete without starting. These
RAM snapshots contain no HID payloads or bond keys. A new write on the same
token replaces its snapshot; new links evict rejected submissions first, then
completed writes, queued writes, then started incomplete writes, oldest first
within each group. Production and other backends omit this diagnostic. CLI/TUI
deserialization uses the same protocol type.

BLE output reports prefer the HID Data Output procedure, GATT Write Without
Response, when the report characteristic advertises it and the payload fits.
Otherwise they use an advertised acknowledged write path, or reject the write
if no supported procedure can carry it. Feature reports retain their existing
write selection. HID++ replies and setting readback determine application
success; native Write Command completion alone does not claim a setting applied.
