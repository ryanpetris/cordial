# USB Discovery, Framing, and Sessions

[Protocol index](README.md)

## Discovery

Automatic discovery matches USB devices by VID/PID `1209:c0d1` from USB descriptors before opening any serial port. Manufacturer, product, interface strings, and endpoint numbers are not discovery keys. Passing an explicit `--port` bypasses VID/PID filtering.

- **USB interfaces**: CDC occupies interfaces 0 and 1. The [USB allocator](hid-forwarding.md#usb-allocation) adds the shared keyboard/mouse HID interface when configured devices need it, one interface per exposed HID gamepad or XInput channel, and an enabled VIA/Vial Raw HID interface (usage page `0xFF60`, usage `0x61`, 32-byte reports) when resources allow.
- **Adapter ID and USB serial**: The adapter ID is 16 uppercase hex digits derived from hardware identity (flash unique ID on Pico W, chip ID on RP2350, little-endian bits `0..63` of `OPTIONAL_UNIQUE_ID` eFuse on ESP32-S3). The USB serial number begins with this ID, appending `-vial:f64c2b3c` whenever Vial is actively exposed. Clients read the authoritative ID from `GetStatus` after the handshake; before opening a port (such as recognizing a disconnected adapter), they compare the first 16 characters of the USB serial case-insensitively. The USB product string is always the firmware's default adapter name.
- **Serial line settings**: Conventional `115200 8-N-1` works; baud rate and line coding do not affect transfer speed and never reset the adapter.

## Framing

- Every frame is protobuf-encoded, COBS-encoded, and terminated by a single `0x00` delimiter byte. Receivers resynchronize on the next `0x00`. Empty delimiters (consecutive `0x00` bytes) are ignored.
- An empty decoded client frame (`0x01 0x00` on the wire) is a `ClientHello`. Every nonempty client frame is a `Request` (which always sets a command, so it encodes to >= 1 byte). Adapter frames are either `AdapterHello` (answering `ClientHello`) or `Message` (holding a `Response` or `Event`).
- Complete client `Request` payloads must fit in **1024 protobuf bytes** before COBS encoding. Oversized frames are discarded through their delimiter and answered with `ERROR_CODE_TOO_LONG`.
- Ordinary adapter frames have no protocol size limit; long responses (such as listings) stream incrementally without interleaving frames.
- USB provides packet-level CRCs, so frames carry no extra checksum.

### Rust Host Receive Limit

`cordial-client` (used by the desktop app, TUI, and CLI) enforces a local **32 MiB** (`33,554,432` bytes) decoded protobuf limit per complete received `Message` (at most `33,686,537` COBS bytes excluding delimiter). This bound also applies while awaiting `AdapterHello` (the hello itself is capped at 1024 decoded bytes).

If a frame exceeds the encoded limit, bytes are discarded until the next delimiter without growing the buffer, and the error is reported at the delimiter. An oversized or invalid frame fails the handshake before any request is sent, or closes an active session with a protocol error (failing all pending requests to prevent FIFO misalignment). Clients never apply oversized frames partially.

## Permanent Handshake

Clients must complete the permanent handshake before sending ordinary requests:

1. Host writes `0x00` followed by an empty `ClientHello` (`0x01 0x00`).
2. Adapter replies with `AdapterHello` carrying required nonzero `protocol_version` (tag `1`) and optional `firmware_version`, `serial_number`, and `name`.
3. Host sends ordinary requests only if it implements the reported revision.

Handshake semantics:

- Firmware answers `ClientHello` in FIFO order like any other query (without reading storage or checking host revisions) and accepts ordinary requests even without a prior hello; compatibility enforcement belongs solely to clients.
- While awaiting `AdapterHello`, clients silently drop frames matching the `Message` envelope (length-delimited fields `1` or `2`, such as stale responses or events) and fail on any other non-hello frame. An absolute deadline bounds the handshake (incoming frames do not extend it).
- `AdapterHello` is limited to `MAX_HANDSHAKE_BYTES` (`1024` decoded bytes). Strict pre-validation rejects missing, zero, duplicate, overflowing, or wrong-wire `protocol_version` fields and malformed protobuf structure (varint keys <= 5 bytes, group nesting <= 100), while skipping unknown optional fields.
- Rust exports `handshake::{ClientHello, AdapterHello, HandshakeError, decode_client_hello, decode_adapter_hello, encode_client_hello, encode_adapter_hello, CURRENT_PROTOCOL_VERSION, MAX_HANDSHAKE_BYTES}` and crate-root `OPERATIONAL_SCHEMA_SHA256`.

## Requests, Responses, and Events

- **Strict FIFO**: The adapter answers every `Request` with exactly one `Response` (and every `ClientHello` with one `AdapterHello`) in receive order, reading the next request only after the previous response finishes transmitting.
- **Parse/validation errors**: Undecodable requests return `ERROR_CODE_BAD_REQUEST`; nonempty requests with an unset/unknown command return `ERROR_CODE_UNKNOWN_COMMAND`; invalid arguments return `ERROR_CODE_BAD_ARGS`.
- **Empty success**: A `Response` with no `result` confirms success with nothing to return (used by `Set*` commands once saved durably).
- **Prompt replies**: Commands reply once accepted and persisted; long Bluetooth operations report progress via events.
- **Inactivity timeout**: Host request timeouts measure incoming-byte inactivity so streaming responses can continue while making progress; stalled requests close the session.
- **Events**: Events may arrive between any two frames (including before `AdapterHello` or between a request and its response), never interleaved mid-frame. Most events carry complete replacement state for one object; `settings_changed` carries complete changed `SettingsObject` entries and removed references. Profile rules have no change event.
- **Coalescing and backpressure**: The adapter keeps one pending event slot per entity so rapid updates coalesce to the latest state. Events wait behind pending responses, and a stalled serial reader never blocks HID forwarding.

## Sessions

- Raising DTR opens a session: the adapter discards leftover partial input/output and writes a `0x00` delimiter. Rust clients toggle DTR low then high on open and lower DTR on close.
- Clients send `0x00` + `ClientHello` on every new session (including USB reconnects).
- Lowering DTR, USB reset, or USB disconnect ends the session, discards incomplete frames, stops any active scan, and cancels any unsaved pairing. Saved devices, Bluetooth links, automatic reconnection, and HID forwarding continue unaffected.

### Intentional Session Endings

Before intentionally disconnecting USB or resetting while a serial session is open, firmware sends `Event.session_ending` with a structured `reason` (`REBOOT`, `CONFIGURATION_CHANGE`, `DEVICE_CONNECTION`, `DEVICE_DISCONNECTION`, `BOOTLOADER`, or `UNKNOWN`). With no open session, reconfiguration or reset proceeds immediately.

Drain behavior once an ending is committed:

1. Firmware stops accepting new requests and discards queued unexecuted requests.
2. Any already-dispatched atomic command finishes before the drain timer starts. An active listing or whole-file read aborts after its current piece by appending `Response.error(ERROR_CODE_BUSY)` (zero-filling remaining whole-file payload bytes) so it cannot starve `SessionEnding`.
3. Firmware transmits any in-progress frame/response, then any pending terminal pairing result (`PairingDone` or pairing error), and finally `SessionEnding` as the last frame, discarding other coalesced change events.
4. Firmware waits until the USB transport completes transmitting the `SessionEnding` delimiter (bounded by a short backend drain deadline that host traffic cannot extend), then resets or detaches.

Client contract:

- Only a received `SessionEnding` authorizes ordinary client reconnect waiting (for all reasons, including `BOOTLOADER` and unknown future reasons). A disconnect without it removes the adapter immediately (no fallback heuristics).
- `SessionEnding` is never a command acknowledgement. A lost command response leaves an unknown outcome until verified after reconnecting; durable saves are never rolled back on reconfiguration.
- See [Intentional disconnects](clients.md#intentional-disconnects) for the 15-second client retention window and [Storage migration](clients.md#storage-migration) for the migration worker's separate reconnect contract.
- When the private CLI subprocess proxy (`cordial proxy`) is used, it exits if the physical adapter disconnects or if the parent stops reading pending output for 3 seconds (idle sessions stay open), and the caller reconnects directly with a fresh handshake.
