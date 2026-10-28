# Serial API

The Dongle's USB CDC port carries a protobuf API. A client sends one request and gets one response
back, in order; the Dongle also sends an event whenever something changes. There are no request
IDs, no subscriptions, and no heartbeats. Clients start every session with the permanent handshake
to identify the operational serial revision. Separate USB interfaces carry keyboard and mouse
input, enabled HID or XInput gamepad outputs, and enabled HID configuration traffic.

One host process owns the port at a time. The Dongle stores its bonds, connection policy, settings,
and profiles itself and works without any host software once set up; the host only configures it and
reports what it says, such as battery levels.

## Documents

- [USB discovery, framing, and sessions](transport.md)
- [Compatibility rules](compatibility.md)
- [Commands](commands.md)
- [Records: status, profiles, devices, integrations, information, and settings](records.md)
- [Limits and errors](limits-and-errors.md)
- [CLI, TUI, and desktop behavior](clients.md)
- [Development and file commands](development.md)
- [HID decoding, forwarding limits, and suspend behavior](hid-forwarding.md)

## Definitions

- [`proto/serial/v1/cordial.proto`](../../proto/serial/v1/cordial.proto) defines the revision `1` operational
  messages. Its comments say what each field means, which fields are required, and what a missing
  field means.
- [`proto/handshake.proto`](../../proto/handshake.proto) defines the permanent opening messages.
- [`proto/storage/v2/storage.proto`](../../proto/storage/v2/storage.proto) defines the operating saved
  records independently of the serial representation. Historical schemas remain under
  `proto/storage/v1/`. See [storage format](../storage-format.md).
- [`proto/migration.proto`](../../proto/migration.proto) defines framed migration-worker subprocess IPC.
  Its length prefix and bounds are separate from the serial API's COBS frames.

`buf lint` checks all schemas. `just check-protocol` checks the permanent handshake and retained
serial revisions against published schemas. Storage and migration IPC use the newest earlier
release in the same application series. See [Compatibility rules](compatibility.md).

## Libraries

The protocol and the connection are libraries that the apps build on, so other projects can use
them too:

`cordial-protocol` provides messages and framing. It performs no I/O, is `no_std`,
and is shared with the firmware. `cordial-client` provides port discovery and
connections, without user-facing text or state beyond the open connection.
Firmware compiles only the current v2 storage schema. Host clients also retain v1
for preserving migration. The Rust `historical-storage` and `migration` features
provide historical records and subprocess IPC to host builds. The native desktop,
CLI, and TUI share the Rust host coordinator and migration worker in-process.
All examples in
these documents use synthetic identifiers and values.
