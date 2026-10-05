# HID forwarding and limits

[Protocol index](README.md)

The adapter compiles each Bluetooth HID report map independently. It forwards
keyboard arrays and bitmaps, report IDs, global Push/Pop, signed and unsigned
relative key/button events, absolute and relative pointer axes, 16 mouse buttons,
both wheels, and Consumer-page keys, switches, and linear controls. Relative
selector items containing ordinary keys or buttons produce press/release events;
selectors for numeric pointer or Consumer axes require an application-defined
conversion and produce a field warning.

Each map is limited to 2,048 descriptor bytes, 512 report bytes, 16 report IDs,
96 fields, and 128 compressed usage spans. The shared forwarder has four source
slots, eight simultaneously held Consumer keys, and 64 queued transitions.
Keyboard state covers usages 1 through 255. Held Consumer controls cover usages
1 through 65,535. Exceeding an active input limit
returns an error rather than silently dropping keys.

Identical ordered usage-span lists share immutable map storage. Usage order,
selector offsets, and repeated-last-usage semantics remain intact.

Pointer motion is limited to 1,048,576 counts per sample and queue entry and is
split into signed 16-bit USB reports. Relative Consumer values use signed 32-bit
USB fields and a signed 40-bit queue accumulator, preserving the sum of every
32-bit field that fits in a Bluetooth report. The Consumer report can span two
USB endpoint packets. Completion advances the queue only after the entire
report is submitted. Absolute values use 16-bit USB fields, with an explicit
null value for unchanged axes. Coalescing retains the latest value for each axis
without crossing source, key/button, pulse, or switch transitions.

Supported Consumer linear usages are `0x0071`, `0x007b`, `0x0086`, `0x00bd`,
`0x00bf`, `0x00e0`, `0x00e1`, `0x00e3`, `0x00e4`, `0x0101`, `0x0103`,
`0x0105`, `0x0109`, `0x0170`, `0x022f`, `0x0235`, and `0x0238`. Volume
changes are forwarded as HID values. They do not require synthetic media-key
presses. Standard Consumer on/off controls use separate toggle and explicit
signed-state reports, with neutral rearming for edge controls. Contact Edited,
Contact Added, and Contact Record Active use these reports too.

Repeated relative on/off controls for the same usage compose in field order.
One-shot controls retain repeated edges as separate press/release transitions.
Duplicate slots selecting the same control within one array represent one
selection. A pulse burst must fit the shared 64-transition queue; admission
returns input overflow before enqueueing a burst that cannot fit.

USB suspend or lack of configuration switches input to current-state tracking.
On host return, the adapter publishes currently held keys and buttons and the
latest absolute values. It discards old motion and taps. At most one report
already submitted to the USB controller can arrive before the current held
state. Endpoint congestion retains transitions in order; a configured host that
never polls HID can still exhaust the documented queue.

Truncated reports are rejected. Bounded trailing padding is ignored. Empty or
out-of-range array entries allow other keys to be released. A rejected report
does not change relative edge or held-key state. Valid releases remain recorded
when a cross-report Consumer-key union temporarily exceeds its capacity.

## Lock indicators

Num Lock, Caps Lock, Scroll Lock, Compose, and Kana indicators work through
absolute bitmaps and selectors, signed on/off commands, and unsigned toggles.
The adapter supports Output and writable Feature reports, Usage Selected/In Use
collections, Multi Mode selectors, Color selectors and bitmaps, and RGB channels
with optional intensity. Ordinal collections keep indicator instances distinct.
These controls concern lock indicators; keyboard backlighting uses its own
vendor features.

A fresh report read preserves unrelated nonvolatile fields. Relative companions
use neutral values. Absolute volatile fields use an encodable out-of-range
no-change value. Selector arrays retain unrelated selections and report a
capacity warning when there is no room for all required active indicators.
Color and brightness snapshots cover the complete indicator tuple across report
IDs and Output/Feature types. Turning a lock off preserves a color through its
mode or intensity control where available; turning it on restores a visible
saved tuple or selects a visible default.

Relative RGB and intensity values use a unique absolute Input/Feature
counterpart in the same explicit indicator group. The adapter prefers an
absolute writer where one is available and otherwise chooses one relative writer
per channel. It converts physical ranges, compatible units, and decimal unit
exponents using exact rational arithmetic. Deltas are split into representable
steps and recorded only after a successful write. Nonzero writes with feedback
arriving before acknowledgement require a fresh read before another delta.
GET_REPORT on a relative field never establishes absolute state.

The following cases remain unavailable and produce specific diagnostics:

- Buffered Bytes fields whose content is defined by the application.
- Relative numeric pointer or Consumer selector formats without a standard
  numeric interpretation.
- Relative indicator selectors outside the supported on/off-control forms.
- Unsigned toggle or relative numeric indicators without unique usable absolute
  feedback or known submitted state.
- Nonlinear relative numeric conversions without a device-defined conversion
  curve, incompatible or undefined units, and conversions exceeding exact
  arithmetic capacity.
- Ranges without a usable neutral value, dark value, visible state, or exactly
  reachable target; selector arrays without enough slots; incomplete RGB groups;
  and mode groups without a usable on/off selection.
- Required report reads/writes that the peripheral rejects or that exceed the
  negotiated Bluetooth report capacity.

HID delimiters and reserved long items remain unsupported descriptor syntax.
Malformed descriptors, descriptor limits, or exhausted admission memory fail
profile setup explicitly. BLE profiles retain separate maps for up to three HID
services. Native service and characteristic limits also apply.

## Warnings

Each limitation above is reported as a `DeviceWarning`, listed by `ListWarnings`, and added or
removed by a `warnings_changed` event when it appears or clears. A warning identifies its code, the
HID service, the report type and optional report ID, the field's bit offset and its usage. Input and
indicator capability limitations are separate from retryable indicator read and write failures; a
successful indicator update clears its failures. A reconnect replaces the earlier connection's
warnings.

## Output reports

BLE output reports prefer the HID Data Output procedure, GATT Write Without Response, when the
report characteristic advertises it and the payload fits. Otherwise they use an advertised
acknowledged write, or reject the write when no supported procedure can carry it. Feature reports
keep their existing write selection. HID++ replies and setting readback decide whether a setting was
applied; a completed write alone never does.

The report interpretation follows
[USB HID 1.11](https://www.usb.org/sites/default/files/hid1_11.pdf) and the
[USB HID Usage Tables](https://usb.org/sites/default/files/hut1_7.pdf), including
LED collections, Ordinal instances, and Consumer controls.
