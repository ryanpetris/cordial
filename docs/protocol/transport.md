# USB discovery, framing and sessions

[Protocol index](README.md)

## Discovery

Automatic discovery selects USB devices by VID/PID `1209:c0d1` alone, read from the descriptors
before the serial port is opened. Manufacturer, product and interface strings are not discovery
keys. An explicitly selected port bypasses this filter.

CDC occupies interfaces 0 and 1. The allocator adds the shared keyboard/mouse HID interface when
configured devices require it, one interface per selected HID gamepad or XInput channel, and any
selected configuration interface. An enabled editor can be absent when higher-priority interfaces
use the available resources. See [USB allocation](hid-forwarding.md#usb-allocation). VIA and Vial use
Raw HID with usage page `0xff60`, usage `0x61` and 32-byte reports. Endpoint numbers are not discovery
keys either. The adapter calculates its allocation from saved preferences before starting USB.

The adapter ID is 16 uppercase hexadecimal digits, and the USB serial number always starts with it.
While Vial is active, the serial is the ID followed by `-vial:f64c2b3c`, which Vial looks for to
discover the adapter; otherwise it is the ID alone. Firmware derives both from the same hardware
identity. After a compatible handshake, a connected client takes the adapter's authoritative
identity from `GetStatus` and never compares it with USB metadata. Optional hello metadata does
not replace `GetStatus`. A client that needs to recognize an adapter before opening its port,
such as one the user disconnected, uses the first 16 characters of the USB serial, compared
without regard to case. The product string is the firmware's default adapter name, independent of
any name the user saved.

Pico W uses its flash unique ID, RP2350 boards their chip ID, and ESP32-S3 bits 0..63 of its factory
`OPTIONAL_UNIQUE_ID` eFuse field read as a little-endian 64-bit integer. The USB identity is
available before the radio starts.

The conventional 115200 8-N-1 serial settings work; line coding does not change the transfer speed
or the protocol, and opening the port or changing the baud rate never reboots the Dongle.

## Framing

- Each message is protobuf-encoded, then COBS-encoded, then followed by one `0x00` byte. COBS
  output contains no zero byte, so a zero always ends a frame and a receiver that joins mid-stream
  resynchronizes at the next one.
- A client frame that decodes to zero bytes, encoded as `0x01 0x00`, is a `ClientHello`. Every
  other client frame is a `Request`, and every `Request` a client sends sets a command, so it
  encodes to at least one byte. Adapter frames are either an `AdapterHello`, answering a
  `ClientHello`, or a `Message` holding a `Response` or an `Event`.
- A delimiter with nothing before it, such as the second of two in a row, ends no frame and is
  ignored.
- The complete client `Request`, including its command envelope and all repeated entries, is at
  most 1024 protobuf bytes before COBS encoding. A longer frame is discarded through its delimiter
  and answered with `ERROR_CODE_TOO_LONG`.
- Ordinary Dongle frames have no total protocol size limit, including typed settings and saved
  metadata. The Dongle writes long frames incrementally and never interleaves two frames. Lists
  use bounded batches inside one frame; see [Listings](compatibility.md#listings). Rust clients
  apply a local [receive limit](#rust-host-receive-limit).
- USB checksums every packet, so frames carry no checksum of their own.

### Rust host receive limit

The Rust client library, including the CLI and TUI, accepts at most 32 MiB, or 33,554,432 decoded
protobuf bytes, per received frame. This counts the complete `Message`, including nested envelopes
and every repeated listing batch. Its COBS receive buffer holds at most 33,686,537 encoded bytes,
excluding the delimiter. This limit also applies while the client waits for the hello, since
ordinary messages can arrive first; the hello itself has a 1024-byte decoded limit. The wire
protocol has no overall cap on ordinary adapter-to-host frames.

Once the encoded limit is exceeded, the decoder discards bytes without further buffer growth until
the next delimiter. It also rejects a frame whose decoded size exceeds the limit. The decoder
reports an oversized frame only at its delimiter. An invalid hello, or any frame over the receive
limit, fails the Rust handshake before any ordinary request is sent. After a compatible hello, an
invalid or oversized ordinary frame closes the session with a protocol error and fails all pending
requests to avoid misaligning later FIFO responses.

An endless stream of nonzero bytes therefore keeps frame buffering bounded but does not trigger
an immediate protocol-error close. During ordinary traffic, incoming bytes still reset the request
inactivity timer. The handshake has an absolute deadline that incoming bytes cannot extend.
Buffer allocation capacity and decoded objects can use additional memory; these byte limits
are not a bound on total process memory.

Complete valid supported lists and events within the limit keep their normal semantics. Clients
never apply an oversized frame partially. The cap allows generous room for current firmware
responses; revisit it when producers or storage limits grow.

## Permanent handshake

The hello tells a host which operational revision the adapter speaks. Clients require it before
ordinary traffic; the firmware does not.

1. The host writes a delimiter and an empty `ClientHello` frame.
2. The adapter answers with an `AdapterHello` holding its own operational revision. It may include
   `firmware_version`, `serial_number` and `name`.
3. The host sends ordinary requests only after it has a valid `AdapterHello` naming a revision it
   implements.

To the firmware, `ClientHello` is one more query. It accepts ordinary requests whether or not a
hello came first, answers a hello at any point in a session and never learns or compares a host
revision. A hello takes its turn with requests: the `AdapterHello` follows the responses to
earlier requests and goes out as one complete frame. `ClientHello` has no fields. Nonempty bytes
are never a `ClientHello`, so they cannot be confused with a `Request`.

Events can go out before the `AdapterHello`, and so can responses to requests an earlier client
left behind. While it waits for the hello, a host drops every frame with the `Message` envelope:
one or more length-delimited fields `1` or `2`. It checks only the envelope, because it can't
decode the payload before it knows the revision. Any other frame that is not a valid
`AdapterHello` fails the handshake. A frame that fits both, such as an `AdapterHello` without a
version that carries only `firmware_version`, is dropped and never accepted. The handshake has an
absolute deadline that incoming frames cannot extend. A host never sends an ordinary request to
learn the revision and never infers one from the shape of other frames.

Firmware constructs its hello from build and hardware metadata without reading storage. It
currently omits the name. This does not guarantee a response before initialization finishes;
startup work holding the application owner lock can delay serial servicing. After a compatible
handshake, `GetStatus` supplies the authoritative identity and saved adapter name. Only
`protocol_version` is required; missing firmware version, serial number or name never prevents a
compatible handshake.

An `AdapterHello` is at most `MAX_HANDSHAKE_BYTES`, currently `1024` decoded protobuf bytes, and a
`ClientHello` is always empty. Frames before the hello can be ordinary messages of any length, and
none longer than the hello limit is accepted as a hello. The Rust client keeps its ordinary
[receive limit](#rust-host-receive-limit) while it waits. The adapter hello decoders validate the complete bytes before generated decoding
and reject missing, zero, duplicate, overflowing, wrong-wire or truncated versions and malformed
protobuf structure. Field order does not matter. Nested fields do not count as the hello's version.
Optional future fields are skipped, including opaque length-delimited contents and structurally
valid groups. Field keys use at most five varint bytes. Groups may nest to depth `100`; a group at
depth `100` must be empty.

Rust exports `handshake::{ClientHello, AdapterHello, HandshakeError, decode_client_hello,
decode_adapter_hello, encode_client_hello, encode_adapter_hello, CURRENT_PROTOCOL_VERSION,
MAX_HANDSHAKE_BYTES}`. Decode helpers take unframed protobuf bytes and return `Result`; encode
helpers return validated protobuf bytes for `frame::encode_bytes`. `decode_client_hello` accepts
only empty bytes. Hello types and both constants are also exported at the crate root.
`OPERATIONAL_SCHEMA_SHA256` is a crate-root constant.

## Requests and responses

The Dongle answers every request frame with exactly one `Response`, and every `ClientHello` with one
`AdapterHello`, in the order they arrive. It reads the next request only after the previous response
has been written, so a client may send requests back to back but never needs to match responses to
requests.

- A frame that does not decode as a `Request` gets `ERROR_CODE_BAD_REQUEST`.
- A nonempty `Request` with no command set gets `ERROR_CODE_UNKNOWN_COMMAND`. That is also what
  firmware sees for a command added after it was built, because protobuf decodes an unknown `oneof`
  variant as an unset one. An empty frame is a `ClientHello`, not a `Request`.
- A command with a missing required field, a value of the wrong type or a value out of range gets
  `ERROR_CODE_BAD_ARGS`.
- A `Response` with no result set means success with nothing to return. Commands that change saved
  preferences, such as `SetAdapter`, `SetDevice`, `SetSettings` and `SetProfileRules`, respond this
  way once the change is durably saved.

A failed request returns a complete `Response.error`, including its reason, outcome and related
errors. Neither a code nor missing outcome details proves that nothing happened. A listing's
per-record `RecordError` is an entry, not a second response; a failure to continue the listing
replaces the partial list with `Response.error` in the same frame. See
[Listings](compatibility.md#listings) and [Errors](limits-and-errors.md#errors).

Every command returns promptly. A command responds once it is accepted and any saved value is
written; Bluetooth work that takes longer, such as connecting, pairing, scanning or disconnecting,
reports its progress through events.

Host request timeouts measure inactivity between incoming bytes rather than a fixed response
deadline. A large response may take longer while it keeps progressing; a stalled request still
ends the session because losing a response would misalign every later request.

## Events

An event can arrive between any two adapter frames, including between a request and its response
and before the `AdapterHello`. A `Message` holding a `Response` always answers the oldest
unanswered request.

Most events carry the complete current state of one thing: the adapter, a device, a profile, a scan
candidate, or the pairing. A client replaces what it had with the latest one.

Settings are listed as complete lists of whole domain objects. `settings_changed` carries complete
objects that changed or appeared, and references to those that went away. A client replaces objects
by integration, group and optional component index, and ignores removal of an entry it does not
hold. A completed listing replaces the list before later events apply. Records and events carry
no revision counters.
Profile rules have no change event; a client lists them again when it needs a fresh view, such as
in a new session.

Error payloads remain complete when events are coalesced. Each adapter or device error list
replaces the preceding set and uses complete-error ordering and deduplication. Adapter radio and
storage faults clear on their respective recovery; aggregate `ready = false` does not identify
which subsystem is ready. Radio recovery cannot clear an unresolved save.

The Dongle keeps one pending slot per thing. When a thing changes again before its event is
written, the event that goes out carries the newer state, and a change event covers every change
since the previous one; intermediate states can be skipped. An intentional session ending discards
pending change events while preserving a pending terminal pairing result. A drain timeout can
discard that result or `SessionEnding`, under the rules below.
Events are written only when no response is waiting, and a client that stops reading holds
back only the serial port, never HID forwarding. List production shares secondary steps with
configuration editor packets, connection setup, settings jobs, pairing and deferred saves, even
when the host keeps reading or records yield no list entries. This work also runs while serial output
is blocked. Cleanup of corrupt saved records waits for its removal and reference-change events to
drain, retaining a bounded rescan cursor rather than every affected ID.

## Sessions

- Opening the port (DTR rising) starts a session. The Dongle discards any partial input and unsent
  output from the previous session and writes a `0x00` delimiter.
- A client writes a `0x00` followed by `ClientHello`, so a partial frame left by an earlier
  client cannot merge with it. It validates `AdapterHello` before sending any ordinary request.
  Every new session repeats this exchange, including reconnects after USB reconfiguration.
- Closing the port (DTR falling), USB reset or USB disconnection ends the session, including the USB
  reconnect that follows a USB allocation change. Clients discard incomplete received frames.
  Ending a session stops a running scan and cancels a pairing that has not saved its bond. A new
  session starts with no events waiting and no scan candidates. Saved devices, connections, automatic reconnection and HID forwarding carry on
  without a client.

The Rust host clients cycle DTR low then high when opening a port and lower DTR
when they close it.

### Intentional session endings

With an open serial session, the Dongle sends `Event.session_ending` before intentionally
disconnecting USB or resetting. The controller populates its structured `reason` with the trigger:
`REBOOT`, `CONFIGURATION_CHANGE`, `DEVICE_CONNECTION`, `DEVICE_DISCONNECTION` or `BOOTLOADER`.
These describe why the session ends, rather than how USB disconnects. This includes autonomous
allocation changes when a Bluetooth device connects or disconnects. No pending request, user
action or request correlation is needed. Clients do not display the reason. The controller uses
`UNKNOWN` for an unclassified trigger. An unspecified or unknown future reason retains the event's
reconnect semantics.
With no active session, the Dongle reconfigures or restarts without an event or serial drain.

Once the ending is committed, the Dongle stops accepting or dispatching new requests. Only an
already-dispatched command may finish; queued or fully received but unexecuted requests are
discarded. An atomic command already in progress completes before the transmission drain starts;
streamed responses remain subject to the drain deadline. During the drain, the Dongle finishes any
frame already in progress and the dispatched command's response, then sends any pending terminal
pairing result before `SessionEnding` as the last frame, all subject to that deadline. A terminal
pairing result is `PairingDone` or a pairing error, including the bond-save outcome. It never
inserts an event into another frame.
If a listing is still being produced, the Dongle stops producing entries, finishes the encoded
piece in progress, appends `Response.error` with `ERROR_CODE_BUSY` through the failed-listing path,
and finishes that frame before sending `SessionEnding`. It does not finish the remaining listing.
A whole-file read also ends with `ERROR_CODE_BUSY`. Remaining bytes in its declared payload are
zero-filled to close the protobuf field; the error makes the entire read invalid.
Other pending coalesced change events are discarded; clients read fresh state in the new session.
Further requests, hellos or state changes cannot starve the final event or extend the deadline.
A save that changes the actual USB allocation is durable before its response is sent.

The Dongle waits for successful transport completion through the event's final `0x00` delimiter
before resetting or detaching. Merely encoding or queueing the frame, or submitting its last USB
packet, is not completion. A short backend-defined deadline bounds transmission, not the time
needed to finish an already-dispatched atomic command. If the host stalls or the transport fails,
the Dongle proceeds when the deadline expires or the session closes. At close it discards partial
input and pending output, including any unfinished frame. A timeout or cable removal is not a
successful drain, and even successful USB transmission cannot guarantee that the client decoded
the event.
There is no additional client acknowledgement.

After USB reconnects, a client opens a new session and reads `GetStatus` and the records it needs.
It does not wait for another event from the old session. In ordinary client sessions, only a
received `SessionEnding` authorizes reconnect waiting for that adapter and session. All reasons,
including `BOOTLOADER` and unspecified or unknown future reasons, have the same bounded wait.
A disconnect without the event removes the adapter immediately, even after a command that might
reconfigure or restart it. This deliberately gives ordinary sessions no reconnect hold with older
firmware that sends no event. There is no interaction, capability or version fallback. The storage
migration worker's separate reconnect-until-cancelled policy is unchanged.
See [client retention and deadlines](clients.md#intentional-disconnects).

A received success confirms the saved preference even if its requested interface is absent from
the new allocation. A lost response leaves the outcome unknown until the client reads the saved
state. Reconfiguration never rolls back durable preferences. `SessionEnding` is not a command result and never stands in for a
successful response. Clients never replay a write whose outcome is unknown.

A CLI proxy exits when its physical adapter disconnects and never reconnects itself. If the
application stops reading pending proxy output for three seconds, the helper also exits. Partial
writes reset this deadline; an idle session with no pending output stays open. If the helper
exits, the desktop handles it as a disconnected session. Each desktop retry starts with a
fresh direct hello exchange, so an adapter that now reports the current revision can reconnect
directly. See [client implementations](clients.md) for proxy selection and client-specific behavior.
