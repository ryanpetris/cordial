# USB discovery, framing and sessions

[Protocol index](README.md)

## Discovery

Automatic discovery selects USB devices by VID/PID `1209:c0d1` alone, read from the descriptors
before the serial port is opened. Manufacturer, product and interface strings are not discovery
keys. An explicitly selected port bypasses this filter.

CDC occupies interfaces 0 and 1; input HID occupies interface 2. Each enabled configuration
interface adds one interface after them, in `ConfigurationInterface` order; the configuration
descriptor holds no interface for a disabled one. VIA and Vial use Raw HID with usage page `0xff60`,
usage `0x61` and 32-byte reports. Endpoint numbers are not discovery keys either. The adapter starts
USB with its saved interfaces, so it enumerates once at power-up.

The adapter ID is 16 uppercase hexadecimal digits, and the USB serial number always starts with it.
While Vial is enabled, the serial is the ID followed by `-vial:f64c2b3c`, which Vial looks for to
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
- A client frame is at most 1024 bytes before COBS encoding. A longer frame is discarded through
  its delimiter and answered with `ERROR_CODE_TOO_LONG`.
- Dongle frames have no length limit. The Dongle writes long frames incrementally and never
  interleaves two frames.
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
  way once the change is applied.

Every command returns promptly. A command responds once it is accepted and any saved value is
written; Bluetooth work that takes longer, such as connecting, pairing, scanning or disconnecting,
reports its progress through events.

## Events

An event can arrive between any two frames, including between a request and its response; a
`Message` holding a `Response` always answers the oldest unanswered request.

Most events carry the complete current state of one thing: the adapter, a device, a profile, a scan
candidate, or the pairing. A client replaces what it had with the latest one.

The lists that belong to a device, its settings and its warnings, are listed in pages and change
through change events instead: `settings_changed` and `warnings_changed` carry only the entries
that changed, appeared or went away. A changed setting is carried whole, with its current state. A
client applies these events to what it has listed, and lists again from the start when it needs a
fresh view, such as in a new session or after losing track. Applying a change is idempotent: a
client upserts settings by integration and key, treats warnings as a set, and ignores the removal of
an entry it does not hold. A change event can name an entry the client has not listed yet; one
that a later page also holds is simply replaced. There are no revisions. A profile's rules have no
change event; a client lists them again when it needs a fresh view.

The Dongle keeps one pending slot per thing. When a thing changes again before its event is
written, the event that goes out carries the newer state, and a change event covers every change
since the previous one, so events are never lost under output pressure; intermediate states can be
skipped. Events are written only when no response is waiting, and a client that stops reading holds
back only the serial port, never HID forwarding.

## Sessions

- Opening the port (DTR rising) starts a session. The Dongle discards any partial input and unsent
  output from the previous session and writes a `0x00` delimiter.
- A client also writes a `0x00` before its first request, so a partial frame left by an earlier
  client cannot merge with it, and ignores everything up to the first `Response` after its first
  request.
- Closing the port (DTR falling), USB reset or USB disconnection ends the session, including the USB
  reconnect that follows a configuration interface change. Ending a session stops a running scan and
  cancels a pairing that has not saved its bond; a new session starts with no events waiting and no
  scan candidates. Saved devices, connections, automatic reconnection and HID forwarding carry on
  without a client.

The CLI cycles DTR low then high when it opens a port. The desktop application opens the port from
closed, which raises DTR, and its web version lowers DTR, waits 60 ms and raises it, because Web
Serial changes DTR without a break request. All of them lower DTR when they close the port.
