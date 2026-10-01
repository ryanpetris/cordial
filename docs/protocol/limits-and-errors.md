# Limits and errors

[Protocol index](README.md)

## Limits

| Limit | Value |
| --- | --- |
| Request frame | 1024 bytes before frame encoding |
| Response and event frames | No limit |
| Scan candidates | 32 per scan |
| Live connections | 4, shared across transports, one kept free for pairing |
| Enabled devices | `TransportSupport.max_enabled` per transport |
| Saved devices | Bounded by flash; `storage.full` reports when another cannot fit |
| Text information values | 64 UTF-8 bytes |
| Adapter name | 1..64 UTF-8 bytes |

HID report-map limits are in [HID forwarding](hid-forwarding.md).

## Errors

`Error` carries an `ErrorCode`, the capacity that ran out for `ERROR_CODE_NO_CAPACITY`, and
`outcome_unknown` for a save that may or may not have taken effect. The same codes describe a
failed connection (`Device.error`), integration (`Integration.status`), setting (`Setting.status`)
and pairing (`failed`). The comments on `ErrorCode` in
[`proto/cordial.proto`](../../proto/cordial.proto) say what each code means; the host supplies the
words a user sees.

A failed save that the Dongle cannot resolve sets `outcome_unknown` and leaves storage not ready
until restart; a client shows that the save status is unknown rather than assuming either value.
A full flash keeps the previous value. A lost response leaves the outcome of a command uncertain;
after reconnecting, a client reads the current state before retrying, and never retries pairing or
unpairing automatically.

## Client pre-checks

A client does not send a request it can tell will be refused, and does not offer the action. The
Dongle checks every request regardless; when a client's view was out of date and the Dongle
refuses, the client shows the error and does nothing else, since events bring its view up to date.

| Refusal | Client checks |
| --- | --- |
| Enabling: `NO_CAPACITY`, `CAPACITY_REASON_ENABLED` | Enabled devices of that transport against its `max_enabled` |
| Pairing with no room: `NO_CAPACITY`, `CAPACITY_REASON_STORAGE` | `storage.full` in `Status.info` |
| A second pairing: `BUSY` | The latest pairing step is not `done` or `failed` |
| Answering a prompt: `NO_PROMPT` | The latest pairing step is `enter_code` or `confirm_code` |
| Connecting: `DISABLED`, `BLOCKED` | `Device.enabled`, `Device.blocked`, `Device.inactive` |
| Scanning with no transports: `BAD_ARGS` | The request itself |
| A transport the firmware lacks: `UNSUPPORTED` | `Status.transports` |
| Commands that need Bluetooth or storage: `NOT_READY` | `Status.ready` |
| Commands that need a connection: `NOT_CONNECTED` | `Device.state` |
| A setting value out of range or too long: `BAD_ARGS` | The setting type's limits |
| An adapter name too long or with control characters: `BAD_ARGS` | The name itself |
