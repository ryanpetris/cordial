# Compatibility rules

[Protocol index](README.md)

The schema only grows. Old clients keep working with new firmware, and new clients with old
firmware, because every message follows the rules below. There is no protocol version number.

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
  repeated enum fields. An unknown `ErrorCode` is a generic failure; an unknown `InactiveReason`
  still means inactive.
- A `oneof` that holds a variant newer than the client reads as unset, with a documented meaning:
  an unknown pairing step means pairing is still in progress, an unknown integration status means
  off, an unknown setting type means the client skips the setting, and an unknown listing entry
  means the client skips the entry.
- `ErrorCode` and `WarningCode` are wire enums, separate from the firmware's internal error types.
  The firmware maps an internal error to the wire code that describes it to a user, or to a
  warning when the device keeps working.

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

`buf breaking` runs against the newest earlier release in the same series with the `WIRE_JSON`
rule set and fails on anything that breaks rules 1 and 2, including renamed fields, which
`--json` output exposes. Before 1.0, a new minor version starts a series and may break
compatibility; from 1.0, only a new major version may. A development build compares against the
newest release. `buf lint` uses
its standard rules except `ENUM_ZERO_VALUE_SUFFIX`, because zero values carry the default meaning,
and `PACKAGE_VERSION_SUFFIX`, because the package has no version. Rules 3 to 8 and the rules below
are review rules.

## Set commands

A command that updates an existing object is a partial update: a client sends only the fields it
wants to change, and every missing field keeps its current value.

- Every updatable field on a set command is `optional` or a message, never a plain scalar, so
  `false`, `0` or `""` cannot overwrite a stored value by accident.
- Fields that only make sense together are grouped in one message, replaced or kept as a whole.
- Because a missing field means unchanged, clearing a value needs an explicit form, such as `name:
  ""` restoring the default adapter name, or a `forget` entry in a list of changes, such as
  `SettingChange.forget`.
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
enum value; it reports itself as another `Device.integrations` entry, its readings as `Info`
records, its settings as `Setting` records with its kind, and its feature table as `Feature`
records with a new `detail` variant. Details only one integration has go in fields 20 and up of
`Integration` and `Setting`, or in a `Feature.detail` variant; the shared fields carry everything a
client needs to show, enable and edit them.

- A client keeps the number of a kind it does not know and passes it back unchanged.
- Integration failures use the shared error codes.
- The Dongle decodes everything it reads from a device into typed fields before sending it. Raw
  report bytes, protocol responses and hex dumps never reach the wire, and a value the Dongle
  cannot decode is not sent. Decoded identifiers and positions, such as a report ID, a bit offset
  or a usage number, are typed fields, not raw data.

## Profiles

Profiles are not tied to keyboards, mice or any other kind of device. A profile is a set of rules
keyed by HID usage, and the Dongle applies whichever of them match the input a device produces. A
new kind of input or output, such as a joystick axis or a gamepad report, is a new usage range in
`ProfileSupport`, never a new field, message or command. HID usages and collections are typed
fields, decoded by the Dongle, as for warnings.

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

Every `List` command reads its list in pages, the same way:

- The request carries `after`, the key of the last entry the client received. A missing `after`,
  `0` or `""` starts from the beginning.
- The reply holds the entries that follow `after` in the listing's order, and `end` is set when
  nothing follows the last of them. A reply holds at least one entry unless `end` is set.
- The Dongle chooses how many entries a page holds, and can choose differently for every page. A
  client never assumes a page size, and reads until `end` when it needs the whole list.
- The listing reflects the Dongle's state as each page is read. Entries that change between pages
  are reported by events, so a client that applies events while it pages ends with the current list.

| Command | `after` | Order |
| --- | --- | --- |
| `ListDevices` | Device ID | Ascending ID |
| `ListProfiles` | Profile ID | Ascending ID |
| `ListProfileRules` | The rule's input `Usage` | Usage page, then usage |
| `ListSettings` | `SettingRef`: integration and key | Integration, then key compared bytewise |
| `ListWarnings` | The last `DeviceWarning` itself | Service, report type, report ID, bit offset, usage page, usage, then code; a missing field orders before any value |
| `ListFeatures` | `FeatureRef`: integration and index | Integration, then index |
| `ListFiles` | Entry name | Name compared bytewise |

A listing of saved records bounded only by flash, `ListDevices` and `ListProfiles`, holds one entry
per record: the record, or its ID as `unreadable` when its saved record could not be read from
flash, so the listing continues past it. An undecodable record is removed as lost and is not
listed.

A client skips a `DeviceListEntry` or `ProfileListEntry` holding a variant newer than the client,
and reads the next page after the last entry it could read. A page that does not end the listing
and holds no entry the client can read leaves it nothing to continue after, so the client reports
the listing as failed.

## Information and settings

Everything about a device beyond its record is a keyed value in one of two lists. `Device.info`,
and `Status.info` for the Dongle itself, hold what is only reported: identity, battery, current
state. Settings hold values the Dongle can change on the device and save; every setting is
writable. Both use string keys from the [key catalog](keys.md) and the shared `Value` type, so a new
piece of information or a new setting is a new key, not a schema change.

- A key means the same thing in every list. A value that can be changed on one device but only read
  on another uses one key: a setting where it can be written, information where it cannot.
- Keys name what they describe, never the protocol they came from, so a client that knows a key
  can show it whichever integration provides it.
- A client shows only keys it knows; labels and formatting live in the client. An unknown enum
  value is shown as unknown, never guessed.
