# Serial API

The Dongle's USB CDC port carries a protobuf API. A client sends one request and gets one response
back, in order; the Dongle also sends an event whenever something changes. There are no request
IDs, no subscriptions, no heartbeats and no revisions. The separate USB HID interfaces carry the
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
- [Development commands](development.md)
- [HID decoding, forwarding limits and suspend behavior](hid-forwarding.md)

## Definitions

- [`proto/cordial.proto`](../../proto/cordial.proto) defines every message. Its comments say what
  each field means, which fields are required, and what a missing field means.
- [`proto/storage.proto`](../../proto/storage.proto) defines the saved records independently of
  the serial representation. See [storage format](../storage-format.md).

`buf lint` checks both schemas, and `buf breaking` compares them with the newest earlier release
in the same series. See [Compatibility rules](compatibility.md).

## Libraries

The protocol and the connection are libraries that the apps build on, so other projects can use
them too:

| | Rust | TypeScript |
| --- | --- | --- |
| Messages and framing | `cordial-protocol` | `@cordial/protocol` |
| Port discovery and connection | `cordial-client` | `@cordial/client` (`/node`, `/web`) |

The protocol libraries do no I/O; the Rust one is `no_std` and shared with the firmware. The client
libraries carry no user-facing text and keep no state beyond the open connection. All examples in
these documents use synthetic identifiers and values.
