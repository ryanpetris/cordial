# Compatibility rules

[Protocol index](README.md)

The permanent handshake in [`proto/handshake.proto`](../../proto/handshake.proto) identifies the
adapter's operational serial revision. Clients complete it before sending ordinary requests. The
current revision is `1`, selected by [`proto/serial/current`](../../proto/serial/current). Its
schema lives in [`proto/serial/v1/cordial.proto`](../../proto/serial/v1/cordial.proto), with package
`cordial.serial.v1`. The shared libraries provide hello encoders and bounded decoders.

The handshake stays independent of operational revisions. `ClientHello` is an empty query that
carries no host revision. `AdapterHello` has exactly one nonzero `uint32 protocol_version` at tag
`1`. Its protobuf field is `optional` so decoders preserve presence, but its presence is required
by the contract. Unknown optional fields
are skipped. `AdapterHello` uses tags `2`, `3` and `4` for optional `firmware_version`,
`serial_number` and `name`. It may omit any or all of them. A valid hello with a different revision
still decodes so the host can report the mismatch.

Compatibility is the host's decision. Firmware always speaks its one native revision, answers the
hello whenever asked and accepts ordinary requests without it, so it has no compatibility gate and
never knows the host's revision. A host sends ordinary requests only after a valid `AdapterHello`
names a revision it implements.

Current applications use the current operational schema. A matching native revision connects
directly. A newer native revision is refused and requires an application upgrade. An older native
revision requires an implemented converter to the current schema. Revision `1` has no earlier
nonzero revision and no historical conversion. Firmware without the permanent hello is
unsupported; clients never infer a revision from a firmware release number or an ordinary response.

The native GTK desktop, CLI and TUI perform compatibility selection inside the Rust process.
Optional hello metadata is not a compatibility requirement. See [client implementations](clients.md).

Breaking operational changes get an explicitly approved new revision in `proto/serial/vN/`.
Retain earlier revision directories as schema history. Default builds generate only the current
revision and its imports, the handshake and existing storage records. Historical revisions
remain source schemas without generated message exports. The operational revision is independent of application
release versions and storage versions. The breaking transition to revision `1` requires the next
release to increment the minor version. Breaking protocol changes increment the release minor
version while the application major version is `0`, or the major version from `1` onward. The
development release template remains `0.0.0`.

### Operational schema identity

Rust exposes `OPERATIONAL_SCHEMA_SHA256`, a lowercase hexadecimal SHA-256 digest of
the normalized protobuf `FileDescriptorSet` for the current operational schema and all its imports.
The handshake and storage are not included. Normalization clears each file's `name` and
`source_code_info`, recursively replaces dependency filenames with the SHA-256 digest of each
normalized dependency descriptor, sorts files by lexicographic order of their encoded descriptor
bytes, then encodes and hashes the descriptor set.
All other descriptor fields, declaration order and dependency order remain significant. Comments
and incidental source paths do not affect the digest; operational fields and imported definitions
do. The Rust build generates the digest with its protobuf descriptor encoder. A dependency
fixture and the current schema's checked-in digest verify normalization. Cargo rejects
operational descriptors with custom option extensions because its typed descriptor conversion
would omit them. Such schemas require lossless normalization support before use.

The digest identifies exact schema content. It is not another compatibility version and is not a
handshake field. Compatible additive schema changes may change the digest without changing the
operational revision.

## Fields

Proto3 already behaves the way this API needs: a scalar field at its zero value is not sent, a
missing field reads as zero, unknown fields, `oneof` variants and enum values do not fail decoding,
and `optional` gives a field explicit presence. The schema comments mark each field as one of:

| Kind | Proto | When missing |
| --- | --- | --- |
| Required | Plain field marked `Required` in its comment; code checks it. | The record is invalid. A client ignores that record; the Dongle refuses a request with `ERROR_CODE_BAD_ARGS`, or `ERROR_CODE_NOT_FOUND` for a missing ID. |
| Defaulted | Plain field. | Read as the zero value, which is chosen to be the right default. |
| Optional | `optional` scalar, or a message field. | There is no value: unknown, not reported, or not applicable. |

A value that is meaningful as zero or false but can also be unknown, such as a battery level or a
security property, is Optional. Only identifiers and fields without which a record cannot be
understood are Required.

## Enums and oneofs

- The zero value of every enum is its safe default. Where none exists it is `_UNSPECIFIED` (for a
  required field) or `_UNKNOWN` (for a value that can come from newer firmware).
- A client reads an enum value it does not know as the zero value, and skips unknown values in
  repeated enum fields. An unknown `ErrorCode` is a generic failure; unknown `ErrorReason` and
  `ErrorOutcome` values provide no further detail. An unknown `InactiveReason` still means inactive.
- A `oneof` that holds a variant newer than the client reads as unset, with a documented meaning:
  an unknown pairing step means pairing is still in progress, an unknown integration status means
  off, an unknown setting status provides no confirmation, and an unknown output status means
  Unknown. An unknown settings group means the client skips the object, and an unknown listing
  entry means the client skips the entry. Output producers always set a state or error; explicit
  `OUTPUT_STATE_UNKNOWN` reports an unknown state.
- `Error` carries code, reason, outcome and related errors in every failure scope. Conversions
  preserve the complete error recursively, including unknown numeric details when retained by the
  decoder. A code alone never establishes an outcome. Missing outcome evidence uses `UNSPECIFIED`,
  even when the code commonly describes a refusal; `NO_EFFECT` requires evidence of no effects or
  results in the documented scope. See [Errors](limits-and-errors.md#errors) for outcome meanings
  and complete-error ordering and deduplication.

## Changing the schema

1. A field's number and type never change, and a field never changes meaning; a different meaning
   gets a new field.
2. A removed field, enum value or `oneof` variant is tombstoned with `reserved`, number and name,
   so neither is ever reused.
3. A new field's zero value, or its absence for an Optional field, means exactly what firmware did
   before the field existed.
4. A Required field is never removed.
5. New commands are new `Request.command` variants; old firmware answers them with
   `ERROR_CODE_UNKNOWN_COMMAND`. New events are new `Event.kind` variants and new result types new
   `Response.result` variants; old clients ignore them.
6. Old firmware ignores request fields it does not know, so a new request field must be safe to
   ignore. If ignoring it would do the wrong thing, the behavior gets a new command.
7. A command that needs more in its result than its record carries gets a new field on `Response`
   beside the `result` oneof.
8. Nothing is named after one member of an enum. Data that varies by transport, integration or any
   other kind is a repeated message with the kind as a field, like `TransportSupport` and
   `Integration`.

Additive operations and writable fields require a positive advertised capability before a client
uses them. An absent or unknown capability means unsupported. A matching protocol revision or a
successful response to an unrecognized field does not prove that firmware supports the operation.

`make check-protocol` runs `buf lint` on every schema and uses `WIRE_JSON` for compatibility
checks. Stable comparisons also check compiled descriptors to preserve released message and enum
names within their packages, including nested definitions. Definitions can move between source
files in the same package. The permanent handshake and operational revisions present in the current
tree compare against every local release tag, regardless of application version or branch ancestry, except the
release identified by `CORDIAL_VERSION`. This protects field assignments shared across release
branches. A revision released by an ancestor must remain present even when another revision becomes
current. A revision released only on another branch need not be added, but its contract is checked
if that revision is present. `PROTOCOL_STABLE_BASE` overrides these comparisons with one Git
revision. The comparison builds released schemas with their imports and protects their dependencies
too.

Storage and migration IPC compare against the newest earlier release in the same application
series, selected by `PROTOCOL_BASE`. Before application version `1.0`, a new minor version starts a
series; from `1.0`, only a new major version does. The first release of a series has no storage or
migration IPC baseline, but still checks permanent serial contracts. Baseline selection uses ancestor tags
reachable before `HEAD`; a development build selects the newest of these releases. Selection uses
existing local tags without fetching. An earlier release's unversioned
serial schema is not compared with revision `1`.

Buf's standard lint exceptions include `ENUM_ZERO_VALUE_SUFFIX` for meaningful zero defaults,
`PACKAGE_VERSION_SUFFIX` for permanent handshake and storage packages, and directory rules for the
schema layout. Rules 3 to 8 and the rules below are review rules.

## Set commands

A command that updates an existing object is a partial update: a client sends only the fields it
wants to change, and every missing field keeps its current value.

- Every updatable field on a set command is `optional` or a message, never a plain scalar, so
  `false`, `0` or `""` cannot overwrite a stored value by accident.
- Fields that only make sense together are grouped in one message, replaced or kept as a whole.
- Because a missing field means unchanged, clearing a value needs an explicit form, such as `name:
  ""` restoring the default adapter name, or a `forget` entry in a list of changes, such as
  `BoolSettingUpdate.forget`.
- A set command responds with no result once the change is applied, and the changed object's
  event announces it when anything changed. A success means the Dongle holds what was sent, in the
  saved form the command defines, so a client sends a newer field only when `Status` shows the
  firmware supports it, and a field that older firmware could not ignore safely gets a new command
  (rule 6).
- A new updatable property of an existing object is a new `optional` field on that object's set
  command, not a new command.
- A repeated list of updates, changes or items to forget applies in order. A later entry for the
  same transport, integration, interface, setting or rule replaces an earlier one, and repeating an
  entry is never refused. The command still saves in one storage write.

## Integrations

Settings, features and device-protocol state are not tied to HID++ on the wire. Each belongs to an
integration named by `IntegrationKind`; HID++ is the only kind so far. A new integration is a new
enum value; it reports itself as another `Device.integrations` entry, its readings in typed
`DeviceInformation` fields, its settings as `SettingsObject` records with its kind, and its
feature table as `Feature` records with a new `detail` variant. Settings domains describe device
behavior, independently of the integration providing it.

- A client keeps the number of a kind it does not know and passes it back unchanged.
- Integration failures use the shared `Error` message, including `PROTOCOL_UNSUPPORTED` for
  missing required protocol versions or reports.
- The Dongle decodes everything it reads from a device into typed fields before sending it. Raw
  report bytes, protocol responses and hex dumps never reach the wire, and a value the Dongle
  cannot decode is not sent. Decoded identifiers and positions, such as a report ID, a bit offset
  or a usage number, are typed fields, not raw data.

## Profiles

Profiles are not tied to keyboards, mice or any other kind of device. A profile is a set of rules
keyed by HID usage and application collection, and the Dongle applies whichever of them match the
input a device produces. A new kind of input or output, such as a joystick axis or a gamepad report,
is a new usage range in `ProfileSupport`, never a new field, message or command. HID usages and
collections are typed fields, decoded by the Dongle.

Each rule and rule reference requires both input usage and collection. Capability ranges in
`ProfileSupport.ranges` include a `UsageRange.Kind` and collection for inputs and outputs.
`KIND_REMAP_INPUT`, `KIND_SCALE_INPUT` and `KIND_AXIS_INPUT` describe on/off, relative and absolute
axis rules; `KIND_REMAP_OUTPUT` and `KIND_AXIS_OUTPUT` describe their destinations. Clients match
kind, collection and usage together. Unknown kinds and `KIND_UNSPECIFIED` grant no capability.
A usage and collection may appear separately under several kinds.

Configuration interfaces follow the transport pattern. Each is a `ConfigurationInterface` value
that reports itself as a `ConfigurationInterfaceSupport` entry with its preferences and the
interfaces it conflicts with, and is changed through a `ConfigurationInterfaceUpdate`. Any number
can be enabled at once; firmware that cannot run two together lists each in the other's
`conflicts`.

- A client keeps an interface or collection it does not know and passes it back unchanged, and
  skips a role it does not know.

## Identifiers

Saved records and scan candidates are identified by positive `uint32` IDs, never strings. Each
kind of record has its own IDs, and a saved record's ID is never reused. Where a field refers to an
optional record, `0` means none.

## Listings

Every `List` command returns the complete list in one logical protobuf `Message` and one
continuous COBS frame. There are no cursors or followup requests. The final frame delimiter marks
completion. An empty list explicitly selects its response variant.

| Command | Result | Order |
| --- | --- | --- |
| `ListDevices` | `DeviceList.entries` | Ascending ID |
| `ListProfiles` | `ProfileList.entries` | Ascending ID |
| `ListProfileRules` | `ProfileRules.rules` | Input usage page, input usage, collection usage page, then collection usage |
| `ListSettings` | `DeviceSettings.objects` | Integration, group, then component index |
| `ListFeatures` | `FeatureList.features` | Integration, then index |
| `ListFiles` | `FileList.entries` | Name compared bytewise |

The Dongle iterates live state rather than taking a snapshot. It serializes bounded batches as
ordinary `Message` values containing the same `Response` list variant and concatenates their
protobuf bytes before incremental COBS encoding. Protobuf merges repeated occurrences of the same
nested message, appending repeated entries. Identity fields such as `DeviceSettings.device` and
`ProfileRules.profile` stay constant throughout the response. Batch boundaries have no wire
meaning and need not align with COBS blocks, USB packets or host reads.

A failure after list data has been emitted appends an ordinary `Message` containing
`Response.error` to the same COBS frame. The response oneof replaces the list with that error.
Clients decode and apply only completed frames, so they never apply partial success. A disconnect
discards an incomplete frame. Adapter-to-host messages have no total protocol size cap. The Rust
client's local [receive limit](transport.md#rust-host-receive-limit) applies to the complete received
frame; an oversized listing is discarded in full.

Events never interleave within a response frame. The Dongle preserves changes that occur during
the listing for subsequent events. A client replaces its list with the completed response, then
applies those events in receive order. Profile rules have no change event, so clients list them
again when they need a fresh view.

`ListDevices` and `ListProfiles` hold one entry per saved record: the record, or an `error` entry
containing `RecordError { id, error }` when its flash read fails. The error uses `STORAGE_FAILED`,
reason `STORAGE` and outcome `NO_EFFECT`: no record was returned. It makes no claim about saved
data. This entry does not fail the whole response; later entries follow. A failure to continue the
listing instead returns `Response.error` and discards the partial list. An undecodable record is
removed as lost and is not listed. A client skips entry variants it does not understand and
continues reading the same frame.

Clients retain a previously known device after its record read fails as last-known data, without
treating that data as confirmation of its connection or preferences. Fresh records and events
supersede it; a completed listing that actually omits the ID, or a removal event, removes it.
Profile read-error placeholders do not require preserving previously cached profile data.

## Information and settings

`Status.information` and `Device.information` contain typed read-only facts. Each scalar has
independent presence. Missing means unknown, including for false and zero. A device event replaces
the whole information object, so absent fields clear prior readings. Device information belongs to
the current connection and clears on disconnect.

Settings use complete domain objects owned by an integration. The domain is selected by a
`SettingsObject.group` oneof. Every configurable field has a typed wrapper with independent
`value` and `saved` presence, optional per-field `status`, and its accepted range or choices where
applicable. Unknown enum readings provide no confirmation of a writable choice. Clients preserve
unknown integration kinds but skip unknown groups.

`SettingsUpdate` is partial, down to each nested field. Every present field chooses a typed value
or an empty `SettingForget` marker. Missing leaves the preference unchanged; `forget` removes only
the saved preference and never resets the actual device value. False and zero remain explicit
writes. See [settings records](records.md#settings) for metadata and event replacement rules.

The wire protocol imposes no object or message size limit on settings responses and change events.
Ranges, choices and saved metadata remain complete. Device and resource constraints still apply,
such as at most six Pointer Sensor DPI choices. For ordinary traffic, the 1024-byte limit applies
to the complete host `Request`, including its command envelope and all entries, before COBS
encoding. An `AdapterHello` has a separate 1024-byte decoded limit, and a `ClientHello` is always
empty.
