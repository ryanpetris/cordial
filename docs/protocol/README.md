# Serial API

The Dongle's USB CDC port carries a protobuf API. A client sends one request and gets one response
back, in order; the Dongle also sends an event whenever something changes. There are no request
IDs, no subscriptions and no heartbeats. Clients start every session with the permanent handshake
to identify the operational serial revision. The separate USB HID interfaces carry the
keyboard and mouse, and the enabled configuration interfaces.

One host process owns the port at a time. The Dongle stores its bonds, connection policy, settings
and profiles itself and works without any host software once set up; the host only configures it and
reports what it says, such as battery levels.

## Documents

- [USB discovery, framing and sessions](transport.md)
- [Compatibility rules](compatibility.md)
- [Commands](commands.md)
- [Records: status, profiles, devices, integrations, information and settings](records.md)
- [Limits and errors](limits-and-errors.md)
- [CLI, TUI and desktop behavior](clients.md)
- [Development and file commands](development.md)
- [HID decoding, forwarding limits and suspend behavior](hid-forwarding.md)

## Definitions

- [`proto/serial/v1/cordial.proto`](../../proto/serial/v1/cordial.proto) defines the revision `1` operational
  messages. Its comments say what each field means, which fields are required, and what a missing
  field means.
- [`proto/handshake.proto`](../../proto/handshake.proto) defines the permanent opening messages.
- [`proto/storage/v2/storage.proto`](../../proto/storage/v2/storage.proto) defines the operating saved
  records independently of the serial representation. Historical schemas remain under
  `proto/storage/v1/`. See [storage format](../storage-format.md).
- [`proto/migration.proto`](../../proto/migration.proto) defines framed desktop/CLI subprocess IPC.
  Its length prefix and bounds are separate from the serial API's COBS frames.

`buf lint` checks all schemas. `make check-protocol` checks the permanent handshake and retained
serial revisions against published schemas. Storage and migration IPC use the newest earlier
release in the same application series. See [Compatibility rules](compatibility.md).

## Libraries

The protocol and the connection are libraries that the apps build on, so other projects can use
them too:

| | Rust | TypeScript |
| --- | --- | --- |
| Messages and framing | `cordial-protocol` | `@cordial/protocol` |
| Port discovery and connection | `cordial-client` | `@cordial/client` (`/node`, `/web`) |

The protocol libraries do no I/O; the Rust one is `no_std` and shared with the firmware.
Firmware compiles only the current v2 storage schema. Host clients also retain v1
for preserving migration; the Rust `historical-storage` and `migration` features
provide historical records and subprocess IPC to host builds. The client
libraries carry no user-facing text and keep no state beyond the open connection. All examples in
these documents use synthetic identifiers and values.
