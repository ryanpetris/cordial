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
| Saved profiles | Bounded by flash |
| Loaded profiles | Each profile in a connected device's layers, plus each enabled configuration interface's profile while a Host editor uses it; shared by ID, within the memory budget |
| Profiles per layer list | `ProfileSupport.max_layers`, so that every enabled device's layers fit in memory |
| List pages | Chosen by the Dongle for every `List` command and every page; at least one entry unless `end` is set |
| Rule inputs | `ProfileSupport.remap_inputs` and `scale_inputs` |
| Remap outputs | `ProfileSupport.remap_outputs`, the keys, Consumer controls, mouse buttons and System Control buttons the Dongle's USB reports carry, and at most `max_remap_outputs` per remap and per held input |
| Loaded profile memory | `ProfileSupport.memory_budget` bytes, shared by every loaded profile; a device whose layers do not fit loads none of them |
| Profile name | 1..64 UTF-8 bytes |
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
| A device change that needs a bond table entry: `NO_CAPACITY`, `CAPACITY_REASON_ENABLED` | For a device whose `Device.inactive` is set, a change that leaves it enabled and unblocked on a transport the firmware supports and has enabled: the devices of that transport without `Device.inactive` against its `max_enabled` |
| Pairing with no room: `NO_CAPACITY`, `CAPACITY_REASON_STORAGE` | `storage.full` in `Status.info` |
| A second pairing: `BUSY` | The latest pairing step is not `done` or `failed` |
| Answering a prompt: `NO_PROMPT` | The latest pairing step is `enter_code` or `confirm_code` |
| Connecting: `DISABLED`, `BLOCKED` | `Device.enabled`, `Device.blocked`, `Device.inactive` |
| Scanning with no transports: `BAD_ARGS` | The request itself |
| Scanning only transports the firmware lacks or has disabled, or using one otherwise: `UNSUPPORTED` | `Status.transports` and `TransportSupport.enabled` |
| Commands that need Bluetooth or storage: `NOT_READY` | `Status.ready` |
| Commands that need a connection: `NOT_CONNECTED` | `Device.state` |
| A setting value out of range or too long: `BAD_ARGS` | The setting type's limits |
| Profile commands on a board without profile support: `UNKNOWN_COMMAND` | `Status.profile_support` |
| Device layers on a board without profile support: `UNSUPPORTED` | `Status.profile_support` |
| A rule input or output out of range, too many outputs, or a zero scale: `BAD_ARGS` | `ProfileSupport.remap_inputs`, `scale_inputs`, `remap_outputs` and `max_remap_outputs` |
| A profile larger than the memory budget: `NO_CAPACITY`, `CAPACITY_REASON_PROFILE_MEMORY` | None; the size of a saved profile is the Dongle's |
| Too many layers: `BAD_ARGS` | `ProfileSupport.max_layers` |
| An enabled configuration interface without a profile: `BAD_ARGS` | The resulting `enabled` and `profile` of each interface |
| Enabling conflicting configuration interfaces: `UNSUPPORTED` | `ConfigurationInterfaceSupport.conflicts` |
| Deleting a profile that is still used: `IN_USE` | Every configuration interface's `profile` and every saved device's `profiles` |
| A profile name empty, too long or with control characters: `BAD_ARGS` | The name itself |
| An adapter name too long or with control characters: `BAD_ARGS` | The name itself |
