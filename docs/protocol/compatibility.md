# Compatibility Rules

[Protocol index](README.md)

## Protocol Revisions

The permanent handshake ([`proto/handshake.proto`](../../proto/handshake.proto)) identifies the adapter's operational serial revision independently of operational schemas, firmware release versions, and storage format versions. The current revision is `1`, selected by [`proto/serial/current`](../../proto/serial/current) and defined in [`proto/serial/v1/cordial.proto`](../../proto/serial/v1/cordial.proto) (`package cordial.serial.v1`).

- `ClientHello` is empty and carries no host revision.
- `AdapterHello` requires exactly one nonzero `protocol_version` at tag `1` (`optional uint32` for presence tracking) and may include optional `firmware_version` (2), `serial_number` (3), and `name` (4).
- Compatibility is decided strictly by the client. Firmware speaks only its native revision and never inspects host versions. Clients connect directly when revisions match, refuse newer revisions (requiring an app update), and require an implemented converter for older revisions (revision `1` has no older revision). Firmware lacking the permanent hello is unsupported.
- Breaking operational changes introduce an explicitly approved `proto/serial/vN/` directory while keeping earlier revisions as schema history. Breaking protocol or storage changes increment the release minor version while major is `0`, or major from `1.0` onward.

### Operational Schema Fingerprint

`cordial-protocol` exports `OPERATIONAL_SCHEMA_SHA256`, the lowercase hex SHA-256 of the normalized protobuf `FileDescriptorSet` for the current operational schema and its imports (excluding handshake and storage). Normalization strips filenames and `source_code_info`, replaces dependency filenames recursively with their normalized SHA-256 digests, and sorts files lexicographically by encoded descriptor bytes. Comments and file paths do not change the digest; operational definitions do. (Schemas using custom option extensions are rejected at build time because typed descriptor conversion would drop them.)

This digest identifies exact schema content; it is not a compatibility version or handshake field.

## Fields

Proto3 wire semantics align with the API: zero scalars are omitted, missing scalars decode as zero, unknown fields/variants/enums are ignored, and `optional` adds presence. Schema comments classify every field into one of three kinds:

| Kind | Protobuf Form | When Missing |
| --- | --- | --- |
| **Required** | Plain field documented `Required`; enforced in code. | Invalid record (client ignores it) or refused request (`ERROR_CODE_BAD_ARGS`, or `ERROR_CODE_NOT_FOUND` for missing IDs). |
| **Defaulted** | Plain field. | Zero value, chosen as the safe default. |
| **Optional** | `optional` scalar or message field. | Unknown, unreported, or not applicable. |

Values where zero or `false` is distinct from unknown (such as battery level or link security flags) are always Optional.

## Enums and Oneofs

- Every enum's zero value is its safe default, `_UNSPECIFIED` (required fields), or `_UNKNOWN` (values from newer firmware).
- Unknown enum values decode as zero (and are skipped in repeated enum fields). An unknown `ErrorCode` is a generic failure; unknown `ErrorReason`/`ErrorOutcome` add no detail; unknown `InactiveReason` still means inactive.
- Unknown `oneof` variants decode as unset with safe fallbacks: pairing still in progress, integration off, setting unconfirmed, output Unknown, unknown settings group or listing entry skipped.
- Structured `Error` records (`code`, `reason`, `outcome`, `related`) are preserved recursively across conversions. An error code alone never establishes an outcome (`UNSPECIFIED` when unknown; `NO_EFFECT` only with evidence). See [Errors](limits-and-errors.md#errors).

## Schema Evolution Rules

1. Never change a field's number, type, or meaning; add a new field for a new meaning.
2. Tombstone removed fields, enum values, and `oneof` variants with `reserved` (both number and name).
3. New fields' zero values (or absence for Optional fields) must match prior firmware behavior.
4. Never remove a Required field.
5. Add commands as new `Request.command` variants (older firmware returns `ERROR_CODE_UNKNOWN_COMMAND`), events as `Event.kind` variants, and results as `Response.result` variants (older clients ignore them).
6. New request fields must be safe for older firmware to ignore; otherwise add a new command.
7. Extra return data alongside a record goes in a new field on `Response` beside `result`.
8. Never name fields after individual enum members; use repeated messages keyed by enum (like `TransportSupport` and `Integration`).

Clients must gate additive operations and writable fields on advertised capabilities (`Status` fields), never on revision matching alone.

### Automated Protocol Checks

`just check-protocol` runs `buf lint` (`buf.yaml`) and `WIRE_JSON` breaking checks plus descriptor name preservation:

- **Permanent contracts** (`proto/handshake.proto` and all retained `proto/serial/vN/` revisions): compared against every local release tag (except `CORDIAL_VERSION`, overridable via `PROTOCOL_STABLE_BASE`). Revisions released by ancestor tags must remain present.
- **Storage and migration IPC**: compared against the newest earlier ancestor release in the same application series (`PROTOCOL_BASE`; one minor version before `1.0`, one major from `1.0`). The first release in a series skips only the storage/IPC baseline check.

## Set Commands

Every `Set*` command is a partial update:

- Updatable fields are `optional` or messages (never plain scalars) so zero/`false`/`""` cannot accidentally overwrite saved state.
- Interdependent fields are grouped in one message and updated together.
- Clearing a field uses an explicit representation (`name: ""` for default adapter name; `forget` markers for settings and rules).
- Repeated update lists apply in order (later entries for the same target win) and commit in a single flash write.
- Successful `Set*` responses return no payload; an event follows if state actually changed.

## Extensibility: Integrations, Profiles, and IDs

- **Integrations**: Wire records are keyed by `IntegrationKind` (currently HID++). Clients preserve unknown kinds unchanged. Firmware decodes all device values into typed fields; raw HID++ bytes never cross the serial wire.
- **Profiles**: Rules are keyed by `(usage, collection)` rather than device type. New input/output classes appear as `UsageRange` entries in `ProfileSupport.ranges` (`KIND_REMAP_INPUT`, `KIND_REMAP_OUTPUT`, `KIND_SCALE_INPUT`, `KIND_AXIS_INPUT`, `KIND_AXIS_OUTPUT`; unknown/unspecified kinds grant no capability). Configuration interfaces follow the `TransportSupport` pattern with explicit `conflicts` lists. Clients preserve unknown interfaces and collections unchanged.
- **Identifiers**: Saved records and scan candidates use positive `uint32` IDs (`0` means none in optional references). Saved IDs are never reused.

## Listings

Every `List*` command returns its complete collection in **one logical protobuf `Message` and one continuous COBS frame** with no cursors:

| Command | Result Field | Sort Order |
| --- | --- | --- |
| `ListDevices` | `DeviceList.entries` | Ascending ID |
| `ListProfiles` | `ProfileList.entries` | Ascending ID |
| `ListProfileRules` | `ProfileRules.rules` | Input usage page, input usage, collection page, collection usage |
| `ListSettings` | `DeviceSettings.objects` | Integration, group, component index |
| `ListFeatures` | `FeatureList.features` | Integration, index |
| `ListFiles` | `FileList.entries` | Bytewise name order |

Streaming mechanics:

- Firmware iterates live state in bounded batches, serializing ordinary `Message` chunks holding the same `Response` list variant and concatenating their protobuf bytes into one incremental COBS frame (protobuf merges repeated messages and appends repeated entries).
- If an error aborts the listing mid-frame, firmware appends a `Message` holding `Response.error` to the same frame, replacing the list in the `oneof`. Clients apply only complete frames, discarding incomplete frames on disconnect.
- Events never interleave inside a response frame; changes occurring during a listing emit afterward.
- In `ListDevices` and `ListProfiles`, a per-record flash read failure emits a `RecordError { id, error }` entry (`STORAGE_FAILED` / `STORAGE` / `NO_EFFECT`) and continues the listing (undecodable records are removed as lost). Clients retain last-known device data on `RecordError` without treating it as confirmed; profile read errors need not retain cached profiles.

## Information and Settings

- `Status.information` and `Device.information` carry typed read-only facts with independent optional presence (cleared on disconnect or absent from a replacement event).
- `SettingsObject` groups configurable fields by integration and domain (`SettingsObject.group`), each wrapper having independent `value`, `saved`, `status`, and accepted range/choices.
- `SettingsUpdate` is partial down to nested fields, choosing either a typed value or `SettingForget` per field.
- Adapter-to-host settings responses and events have no wire size cap; host `Request` messages remain bounded to 1024 protobuf bytes.
