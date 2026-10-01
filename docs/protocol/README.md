# Serial API

The Dongle's USB CDC port carries a protobuf API. A client sends one request and gets one response
back, in order; the Dongle also sends an event whenever something changes. There are no request
IDs, no subscriptions, no heartbeats and no revisions. The separate USB HID interfaces carry the
keyboard and mouse.

One host process owns the port at a time. The Dongle stores its bonds, connection policy and
settings itself and works without any host software once set up; the host only configures it and
reports what it says, such as battery levels.

## Documents

- [USB discovery, framing and sessions](transport.md)
- [Compatibility rules](compatibility.md)
- [Commands](commands.md)
- [Records: status, devices, integrations, information, settings and warnings](records.md)
- [Information and settings keys](keys.md)
- [Limits and errors](limits-and-errors.md)
- [CLI, TUI and desktop behavior](clients.md)
- [Development commands](development.md)
- [HID decoding, forwarding limits and suspend behavior](hid-forwarding.md)

## Definitions

- [`proto/cordial.proto`](../../proto/cordial.proto) defines every message. Its comments say what
  each field means, which fields are required, and what a missing field means.
- [`proto/keys.toml`](../../proto/keys.toml) lists every information and setting key, with its type,
  unit and enum values.

Both are checked: `tools/check_keys.py` validates the key catalog and compares it with an earlier
version, and `buf breaking` compares the schema with the last release (see
[Compatibility rules](compatibility.md)).

## Libraries

The protocol and the connection are libraries that the apps build on, so other projects can use
them too:

| | Rust | TypeScript |
| --- | --- | --- |
| Messages, framing, key constants | `cordial-protocol` | `@cordial/protocol` |
| Port discovery and connection | `cordial-client` | `@cordial/client` (`/node`, `/web`) |

The protocol libraries do no I/O; the Rust one is `no_std` and shared with the firmware. The client
libraries carry no user-facing text and keep no state beyond the open connection. All examples in
these documents use synthetic identifiers and values.
