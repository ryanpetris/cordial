# Serial API

Cordial's USB CDC port exposes a protobuf control API. A client sends one `Request` and receives one `Response` in FIFO order; the adapter also emits unsolicited `Event` messages whenever state changes. There are no request IDs, subscriptions, or heartbeats. Every session begins with a permanent handshake identifying the adapter's operational protocol revision.

Separate USB interfaces carry standard keyboard/mouse HID input, enabled HID or XInput gamepads, and an enabled VIA/Vial configuration interface. Only one host process owns the serial port at a time. The adapter operates independently without host software once configured; the host is used only for configuration, diagnostics, and status reporting.

## Protocol Documents

- [USB discovery, framing, and sessions](transport.md) -- VID/PID discovery, COBS framing, handshake, sessions, and `SessionEnding`
- [Compatibility rules](compatibility.md) -- schema evolution, field kinds, streamed listings, and `check-protocol`
- [Commands](commands.md) -- adapter, discovery/pairing, device, settings, and profile commands
- [Records](records.md) -- `Status`, capacities, profiles, devices, outputs, integrations, information, and settings
- [Limits and errors](limits-and-errors.md) -- protocol/resource limits, Bluetooth board capacities, structured errors, and pre-checks
- [CLI, TUI, and desktop behavior](clients.md) -- client state model, commands, TUI/desktop behavior, and migration IPC
- [Development and file commands](development.md) -- debug commands, filesystem access, and `Restart`
- [HID forwarding and limits](hid-forwarding.md) -- report compiler, USB control/wake/allocation, lock LEDs, and gamepad outputs

## Schema Definitions

- [`proto/handshake.proto`](../../proto/handshake.proto) -- permanent opening handshake (`ClientHello`, `AdapterHello`).
- [`proto/serial/v1/cordial.proto`](../../proto/serial/v1/cordial.proto) -- operational serial revision `1` messages (current revision pinned in [`proto/serial/current`](../../proto/serial/current)).
- [`proto/storage/v2/storage.proto`](../../proto/storage/v2/storage.proto) -- on-board storage records (historical v1 retained in `proto/storage/v1/`; see [On-board storage format](../storage-format.md)).
- [`proto/migration.proto`](../../proto/migration.proto) -- length-prefixed subprocess IPC for host-assisted storage migration.

`just check-protocol` validates all schemas with `buf lint` and checks compatibility against released tags (see [Compatibility rules](compatibility.md)).

## Libraries

The Rust workspace separates protocol and transport layers for reuse:

- `cordial-protocol`: `no_std` protobuf message types, handshake helpers, and COBS framing shared between firmware and host tools.
- `cordial-client`: USB port discovery, handshake, FIFO request/response handling, and ordering helpers (`cordial_client::order`), without UI text or application state.
- Host builds enable `historical-storage` and `migration` features so the desktop app, TUI, and CLI share the coordinator and migration worker in-process. Firmware compiles only the current storage schema.

All examples in these documents use synthetic identifiers and values.
