# Limits and errors

[Protocol index](README.md)

## Limits

| Limit | Value |
| --- | --- |
| Complete host `Request` | 1024 protobuf bytes, including the command envelope and all entries, before COBS encoding |
| Adapter response and event frames | No total protocol size cap |
| Pointer Sensor DPI choices | At most six per sensor, including saved metadata |
| Scan candidates | 32 per scan |
| Normal connections | `CONNECTIONS.limit` in each `TransportSupport.resources` entry, independently per transport |
| Pairing connections | One additional connection overall, outside the normal transport allowances |
| Enabled devices | `DEVICES.limit` in each `TransportSupport.resources` entry |
| Saved devices | Bounded by flash; `ERROR_CODE_STORAGE_FULL` in `Status.errors` reports when another cannot fit |
| Saved profiles | Bounded by flash |
| Loaded profiles | Each profile in a connected device's layers, plus each enabled configuration interface's profile while a Host editor uses it; shared by ID, within the memory budget |
| Profiles per layer list | `LAYERS.limit` in `ProfileSupport.resources`; loaded layers also share the profile memory budget |
| Lists | One complete response frame, streamed with bounded adapter memory |
| Rule inputs | `ProfileSupport.ranges` of kind `KIND_REMAP_INPUT`, `KIND_SCALE_INPUT` or `KIND_AXIS_INPUT`, matching both usage and collection |
| Remap outputs | `ProfileSupport.ranges` of kind `KIND_REMAP_OUTPUT`, and at most `OUTPUTS.limit` in `ProfileSupport.resources` per remap and per held input |
| Absolute axis outputs | `ProfileSupport.ranges` of kind `KIND_AXIS_OUTPUT`, in the input's Game Pad collection |
| Loaded profile memory | `MEMORY.limit` in `ProfileSupport.resources`, in bytes, shared by every loaded profile; a device whose layers do not fit loads none of them |
| Profile name | 1..64 UTF-8 bytes |
| Text information values | 64 UTF-8 bytes |
| Adapter name | 1..64 UTF-8 bytes |

HID report-map limits are in [HID forwarding](hid-forwarding.md).

### Bluetooth capacity

Each transport reports the same normal allowance in `DEVICES.limit` and `CONNECTIONS.limit`.
The allowance governs enabled-device admission and normal connection admission. Classic and BLE
are independent; enabling Classic does not reduce BLE capacity. Both counts exclude the pairing
connection. Normal connection count includes connecting and closing links until their slots are
freed. Saved enabled-device count includes devices on a disabled transport and can exceed the
allowance when saved preferences exceed the firmware's capacity.

The board configuration generates the Rust and C pool sizes through
`rust/tools/bluetooth_config.py`, using `CORDIAL_CONFIG`. The configured allowances are:

| Board preset | Normal Classic | Normal BLE | Runtime slots, including pairing | Native Classic bonds | Native BLE bonds |
| --- | --- | --- | --- | --- | --- |
| `pico_w` | 2 | 8 | 11 | 3 | 9 |
| `pico2_w` | 6 | 14 | 21 | 7 | 15 |
| `waveshare_rp2350b_plus_w` | 6 | 14 | 21 | 7 | 15 |
| `xiao_esp32s3` | 0 | 8 | 9 | 0 | 9 |

Each supported transport has one additional native bond entry for pairing, but only one pairing
connection can use the shared runtime reserve at a time. ESP32-S3 also configures 10 controller
activities for its links and scanning. BLE requests a 7.5 ms connection interval.

Simultaneous mixed 6 Classic plus 14 BLE connections and full-load latency remain unverified on
hardware.

## Errors

`Error` carries an `ErrorCode`, the capacity that ran out for `ERROR_CODE_NO_CAPACITY`, and
`outcome_unknown` for a save that may or may not have taken effect. The same codes describe a
failed connection or profile load (`Device.errors`), integration (`Integration.status`),
setting (`SettingStatus`) and pairing (`failed`). `Status.errors` carries adapter problems. These scopes remain separate; an
integration or setting error does not become a device error. Each adapter and device list is a
complete replacement, without duplicates and in numeric order. The comments on `ErrorCode` in
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
| A device change that needs a bond table entry: `NO_CAPACITY`, `CAPACITY_REASON_ENABLED` | For a device whose `Device.inactive` is set, a change that leaves it enabled and unblocked on a transport the firmware supports and has enabled: the devices of that transport without `Device.inactive` against its advertised `DEVICES.limit` |
| Pairing with no room: `NO_CAPACITY`, `CAPACITY_REASON_STORAGE` | `ERROR_CODE_STORAGE_FULL` in `Status.errors` |
| A second pairing: `BUSY` | The latest pairing step is not `done` or `failed` |
| Answering a prompt: `NO_PROMPT` | The latest pairing step is `enter_code` or `confirm_code` |
| Connecting: `DISABLED`, `BLOCKED` | `Device.enabled`, `Device.blocked`, `Device.inactive` |
| Scanning with no transports: `BAD_ARGS` | The request itself |
| Scanning only transports the firmware lacks or has disabled, or using one otherwise: `UNSUPPORTED` | `Status.transports` and `TransportSupport.enabled` |
| Commands that need Bluetooth or storage: `NOT_READY` | `Status.ready` |
| Commands that need a connection: `NOT_CONNECTED` | `Device.state` |
| A setting value out of range: `BAD_ARGS` | The field's advertised range or choices |
| Profile commands on a board without profile support: `UNKNOWN_COMMAND` | `Status.profile_support` |
| Device layers on a board without profile support: `UNSUPPORTED` | `Status.profile_support` |
| A missing rule input or collection, an input or output out of range, too many outputs, or a zero scale: `BAD_ARGS` | `ProfileSupport.ranges`, matching kind, usage and collection, and `OUTPUTS.limit` in `ProfileSupport.resources` |
| A profile larger than the memory budget: `NO_CAPACITY`, `CAPACITY_REASON_PROFILE_MEMORY` | None; the size of a saved profile is the Dongle's |
| Too many layers: `BAD_ARGS` | `LAYERS.limit` in `ProfileSupport.resources` |
| An enabled configuration interface without a profile: `BAD_ARGS` | The resulting `enabled` and `profile` of each interface |
| Enabling conflicting configuration interfaces: `UNSUPPORTED` | `ConfigurationInterfaceSupport.conflicts` |
| Deleting a profile that is still used: `IN_USE` | Every configuration interface's `profile` and every saved device's `profiles` |
| A profile name empty, too long or with control characters: `BAD_ARGS` | The name itself |
| An adapter name too long or with control characters: `BAD_ARGS` | The name itself |

USB capacity is reported after allocation, independently of command errors. A missing required
interface sets `ERROR_CODE_USB_REQUIRED_CAPACITY_EXCEEDED`; a missing configured reserve sets
`ERROR_CODE_USB_RESERVED_CAPACITY_EXCEEDED`. Both are reported and displayed when applicable.
Neither prevents saving valid USB preferences. Clients do not disable Save based on USB resource
counts. Required capacity uses a prominent warning and tray attention; reserved capacity uses a
subtle indicator without a large Overview banner. See
[allocation priorities](hid-forwarding.md#usb-allocation).

`ERROR_CODE_PROFILE_MEMORY_LOW` means loaded profiles use at least 85% of a nonzero memory budget.
`ERROR_CODE_PROFILE_MEMORY_EXCEEDED` and `ERROR_CODE_PROFILE_LOAD_FAILED` belong to a connected
device whose layers are not loaded. These do not replace ordinary inactive or output states.
An immediate `ConnectDevice` refusal uses response `Error`; a failure after acceptance arrives
in `Event.device` with a disconnected state and a connection code in `Device.errors`.

For contextual resource units and independent optional count and limit fields, see
[resource capacities](records.md#resource-capacities).
