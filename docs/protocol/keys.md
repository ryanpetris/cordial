# Information and settings keys

[Protocol index](README.md)

[`proto/keys.toml`](../../proto/keys.toml) lists every key that can appear in `Status.info`,
`Device.info` and settings, with its type, unit and enum values. Both libraries generate key
constants from it. A key, once added, never changes type or meaning. A key or enum value no longer
sent stays in the file as `retired` or in `retired_values`, so it is never reused.
`tools/check_keys.py --against <revision>` fails if the catalog breaks these rules or loses a key,
enum value or list compared with that revision.

## Naming

1. Lowercase ASCII letters, digits, `.` and `_`, at most 64 bytes.
2. `.` separates levels from the general to the specific: `battery.level`, `vendor.id`,
   `backlight.delay.hands_out`. Every key has at least two levels; the first names the part of the
   device or the subject.
3. `_` joins the words of one level and never stands in for a level.
4. No key is a prefix of another: a level is either a group of keys or a value, never both.
5. A numeric level indexes a part of the device that repeats, such as a lighting zone, a sensor or a
   button, and comes straight after the level naming the part: `pointer.sensor.1.dpi`. It is the
   device's own number for the part where it has one, so saved values stay attached across
   connections; otherwise it counts from 0 in the order the device reports them. Things with their
   own meaning get named keys instead of indexes.
6. Units are recorded in the catalog, never in the name.
7. A bool key names what is true: `enabled`, `charging`, `invert`.

The check enforces rules 1 to 4.

## Templates and rows

A key for a repeated part is listed once as a template with `{n}` in place of the index, such as
`pointer.sensor.{n}.dpi`; the libraries' lookup matches a concrete key to its template and index. A
repeated part can have an information key that says what it is, and a client labels the part from
it, falling back to its index.

The catalog can also list rows: sets of keys a client shows together with one Save. Rows are
presentation only and never appear on the wire.
