# USB control protocol, version 1

These documents define the current USB control contract. All examples use synthetic device identifiers and values.

The USB CDC interface carries management commands and live notifications alongside the separate USB HID interfaces. One host process owns the CDC port. Multiple requests and notifications share that connection; independent host processes do not open the port concurrently. The Rust host executable provides a command shell, one-shot CLI commands, and a full-screen TUI through the same protocol client. References to the CLI's session, heartbeat, monitoring, and cleanup responsibilities apply equally to the TUI. The desktop application in `desktop/` is a separate host process with the same responsibilities; while it runs it keeps one session open to every adapter it has confirmed. There are no system services or background brokers, and host processes do not coordinate: quit the desktop application, or disconnect that adapter in it, before using the CLI or TUI with an adapter. The dongle stores its own bonds and connection policy and operates without any host tool after setup.

## Documents

- [USB discovery, framing, sessions, heartbeat and exit](transport.md)
- [Message envelopes and request IDs](messages.md)
- [Identity, device records and revisions](device-state.md)
- [Command reference](commands.md)
- [Notifications, snapshots and multiplexing](monitoring.md)
- [Resource limits and errors](limits-and-errors.md)
- [CLI, TUI and desktop behavior](clients.md)
- [Development commands and diagnostics](development.md)
- [HID decoding, forwarding limits and suspend behavior](hid-forwarding.md)

## Machine-readable schemas

[Schema documentation](../../schema/README.md) covers the checked-in Draft 2020-12 wire
schema and generated command catalog. The shared Serde types also derive Schemars
schemas behind a host-only feature. Regenerate with `(cd rust && cargo run -p cordial-schema)`;
check without writing with `(cd rust && cargo run -p cordial-schema -- --check)`.

Use the initiating command's response definition and the response's `done` phase.
Responses do not repeat the command name, so the union of all response types cannot
check correlation. Schemas describe individual messages. The protocol documentation and the
client enforce ordering, capability requirements, heartbeat deadlines,
cancellation, revisions and mutation detection. Codec tests enforce the 4096-byte
frame limit, duplicate keys and byte limits that JSON Schema cannot express.
