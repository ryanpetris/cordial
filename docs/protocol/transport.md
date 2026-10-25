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
identity. A connected client takes the adapter's identity from `GetStatus` and never compares it
with USB metadata. A client that needs to recognize an adapter before opening its port, such as one
the user disconnected, uses the first 16 characters of the USB serial, compared without regard to
case. The product string is the firmware's default adapter name, independent of any name the user
saved.

Pico W uses its flash unique ID, RP2350 boards their chip ID, and ESP32-S3 bits 0..63 of its factory
`OPTIONAL_UNIQUE_ID` eFuse field read as a little-endian 64-bit integer. The USB identity is
available before the radio starts.

The conventional 115200 8-N-1 serial settings work; line coding does not change the transfer speed
or the protocol, and opening the port or changing the baud rate never reboots the Dongle.

## Framing

- Each message is protobuf-encoded, then COBS-encoded, then followed by one `0x00` byte. COBS
  output contains no zero byte, so a zero always ends a frame and a receiver that joins mid-stream
  resynchronizes at the next one.
- Client to Dongle, every frame is a `Request`. Dongle to client, every frame is a `Message` holding
  either a `Response` or an `Event`.
- Empty frames (two delimiters in a row) are ignored.
- The complete client `Request`, including its command envelope and all repeated entries, is at
  most 1024 protobuf bytes before COBS encoding. A longer frame is discarded through its delimiter
  and answered with `ERROR_CODE_TOO_LONG`.
- Dongle frames have no total protocol size limit, including typed settings and saved metadata.
  The Dongle writes long frames incrementally and never interleaves two frames. Lists use bounded
  batches inside one frame; see [Listings](compatibility.md#listings).
- USB checksums every packet, so frames carry no checksum of their own.

## Requests and responses

The Dongle answers every request frame with exactly one `Response`, in the order requests arrive.
It reads the next request only after the previous response has been written, so a client may send
requests back to back but never needs to match responses to requests.

- A frame that does not decode as a `Request` gets `ERROR_CODE_BAD_REQUEST`.
- A `Request` with no command set gets `ERROR_CODE_UNKNOWN_COMMAND`. That is also what firmware sees
  for a command added after it was built, because protobuf decodes an unknown `oneof` variant as an
  unset one.
- A command with a missing required field, a value of the wrong type or a value out of range gets
  `ERROR_CODE_BAD_ARGS`.
- A `Response` with no result set means success with nothing to return. Commands that change saved
  preferences, such as `SetAdapter`, `SetDevice`, `SetSettings` and `SetProfileRules`, respond this
  way once the change is durably saved.

Every command returns promptly. A command responds once it is accepted and any saved value is
written; Bluetooth work that takes longer, such as connecting, pairing, scanning or disconnecting,
reports its progress through events.

Host request timeouts measure inactivity between incoming bytes rather than a fixed response
deadline. A large response may take longer while it keeps progressing; a stalled request still
ends the session because losing a response would misalign every later request.

## Events

An event can arrive between any two frames, including between a request and its response; a
`Message` holding a `Response` always answers the oldest unanswered request.

Most events carry the complete current state of one thing: the adapter, a device, a profile, a scan
candidate, or the pairing. A client replaces what it had with the latest one.

Settings are listed as complete lists of whole domain objects. `settings_changed` carries complete
objects that changed or appeared, and references to those that went away. A client replaces objects
by integration, group and optional component index, and ignores removal of an entry it does not
hold. A completed listing replaces the list before later events apply. There are no revisions.
Profile rules have no change event; a client lists them again when it needs a fresh view, such as
in a new session.

The Dongle keeps one pending slot per thing. When a thing changes again before its event is
written, the event that goes out carries the newer state, and a change event covers every change
since the previous one, so events are never lost under output pressure; intermediate states can be
skipped. Events are written only when no response is waiting, and a client that stops reading holds
back only the serial port, never HID forwarding. List production shares secondary steps with
configuration editor packets, connection setup, settings jobs, pairing and deferred saves, even
when the host keeps reading or records yield no list entries. This work also runs while serial output
is blocked. Cleanup of corrupt saved records waits for its removal and reference-change events to
drain, retaining a bounded rescan cursor rather than every affected ID.

## Sessions

- Opening the port (DTR rising) starts a session. The Dongle discards any partial input and unsent
  output from the previous session and writes a `0x00` delimiter.
- A client also writes a `0x00` before its first request, so a partial frame left by an earlier
  client cannot merge with it, and ignores everything up to the first `Response` after its first
  request.
- Closing the port (DTR falling), USB reset or USB disconnection ends the session, including the USB
  reconnect that follows a USB allocation change. Clients discard incomplete received frames.
  Ending a session stops a running scan and cancels a pairing that has not saved its bond. A new
  session starts with no events waiting and no scan candidates. Saved devices, connections, automatic reconnection and HID forwarding carry on
  without a client.

The CLI cycles DTR low then high when it opens a port. The desktop application opens the port from
closed, which raises DTR, and its web version lowers DTR, waits 60 ms and raises it, because Web
Serial changes DTR without a break request. All of them lower DTR when they close the port.

A save that changes the actual USB allocation is durable before its response is sent. The Dongle
normally finishes transmitting that response before detaching. A short backend-defined deadline
bounds this response drain; continued host traffic cannot extend it. At detach the serial session
closes and its partial input and pending output are discarded. A stalled host cannot prevent USB
reconfiguration. There is no additional acknowledgement.

After USB reconnects, a client opens a new session and reads `GetStatus` and the records it needs.
It does not wait for an adapter event from the old session. A received success confirms the saved
preference even if its requested interface is absent from the new allocation. A lost response
leaves the outcome unknown until the client reads the saved state. Reconfiguration never rolls
back durable preferences.
