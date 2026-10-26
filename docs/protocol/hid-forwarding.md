# HID forwarding and limits

[Protocol index](README.md)

The adapter compiles each Bluetooth HID report map independently. It forwards
keyboard arrays and bitmaps, report IDs, global Push/Pop, signed and unsigned
relative key/button events, absolute and relative pointer axes, 16 mouse buttons,
both wheels, and Consumer-page keys, switches, and linear controls. Relative
selector items containing ordinary keys or buttons produce press/release events;
selectors for numeric pointer or Consumer axes require an application-defined
conversion and are not forwarded.

Each map is limited to 2,048 descriptor bytes, 512 report bytes, 16 report IDs,
96 fields, and 128 compressed usage spans. The shared forwarder has one source
slot per compiled runtime connection slot, eight simultaneously held Consumer keys, and 64 queued
transitions. Runtime slots cover both normal transport allowances and the shared pairing reserve;
see [Bluetooth capacity](limits-and-errors.md#bluetooth-capacity).
Keyboard state covers usages 1 through 255. Held Consumer controls cover usages
1 through 65,535. Exceeding an active input limit
returns an error rather than silently dropping keys.

Identical ordered usage-span lists share immutable map storage. Usage order,
selector offsets, and repeated-last-usage semantics remain intact.

Pointer motion is limited to 1,048,576 counts per sample and queue entry and is
split into signed 16-bit USB reports. Relative Consumer report 4 contains 17 signed
32-bit USB fields in the usage order below. Except for Volume `0x00e0`, relative
Consumer values use a signed 40-bit queue accumulator, preserving the sum of every
32-bit field that fits in a Bluetooth report. The 68-byte Consumer payload can
span two USB endpoint packets. Completion advances the queue only after the entire
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

Relative Volume `0x00e0` is limited to `-1..1` per input and USB report. Each
nonzero contribution is reduced to its direction before aggregation. Aggregation
discards only the excess at either bound, so later contributions in the opposite
direction still apply. Out-of-range Volume values submitted to the forwarder are
reduced to one step. Excess values do not become a backlog of repeated steps. This
bounds host volume changes and avoids prolonged volume input from one extreme
value. Volume occupies the sixth 32-bit field at payload byte offset 20, excluding
the report ID, with a USB logical range of `-1..1`.

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

## USB interfaces and boot keyboard

The adapter presents one composite USB device. Its serial port uses two interfaces. A shared
HID interface carries keyboard, mouse, Consumer, system and radio reports when configured devices
need it. Each HID gamepad slot uses a separate interface and IN endpoint with a 22-byte input
report. XInput uses separate receiver channels.

The desktop HID interface advertises the boot keyboard subclass and protocol. In normal report
protocol, it sends the full keyboard bitmap and the other desktop reports. When the Host selects
boot protocol, it sends only the standard eight-byte keyboard report, without a report ID. The
report contains modifiers and up to six other keys. More held keys produce ErrorRollOver in all
six key positions until the held keys fit again; modifiers remain reported. The underlying
bitmap remains intact. Switching protocols reconciles current input, and USB reset restores
report protocol. Lock LEDs and host-requested idle reporting work in both modes. Boot mouse
protocol is not provided.

VIA or Vial requests one interface with an IN and an OUT endpoint while enabled. Saved enablement
is separate from active USB exposure. Connected-device requirements can take priority over an
editor, and whether an editor application is open does not change its priority.

## USB control requests

Cordial validates standard endpoint requests before Embassy dispatches them to the USB bus.
Validation uses the full endpoint address, including direction, and rejects reserved address bits
and nonzero upper bytes. Endpoint requests require an assigned USB device address. Nonzero
endpoints must also be allocated and active in the current configuration and alternate setting.
`GET_STATUS` and endpoint-halt requests check their direction, value and length; isochronous
endpoints reject halt requests. `SET_ADDRESS`, `SET_CONFIGURATION` and `SET_INTERFACE` also check
their setup fields and required USB state. Invalid requests stall the control transfer; later valid
requests can proceed.

On RP2040, RP2350 and ESP32-S3, Cordial-owned USB wrappers preserve queued transfers
when the Host sets or clears HALT on non-control endpoints without reconfiguring them.
A halted endpoint pauses submission and rearming.
`CLEAR_FEATURE(ENDPOINT_HALT)` resets the next packet's data sequence to
DATA0, even if the endpoint was not halted. This applies independently to IN and
OUT endpoints, preserving queued payloads and completed OUT packets awaiting a read.
An idle endpoint also starts its next transfer at DATA0. Recovery does not flush
the endpoint or replay a completed packet.
ESP32-S3 asserts Global OUT NAK before it disables and halts an active OUT receive.
Clearing HALT re-primes that receive, while a completed packet remains buffered for
its pending read.

`SET_CONFIGURATION` and `SET_INTERFACE` clear HALT and initialize DATA0 on the
selected bulk and interrupt endpoints, including when the selection is unchanged.
On ESP32-S3, the wrapper stops active transfers, resets DATA0 and discards completed,
unread OUT packets from before reconfiguration. New OUT traffic receives NAK until
the receive buffer is empty and a reader starts a fresh receive. Endpoints without
a reader stay at NAK without blocking other interfaces. Cancellation, HALT and USB
reset preserve this discard policy. Disabled endpoints remain inactive, and
`SET_INTERFACE` leaves other interfaces unchanged. Pending endpoint
operations report cancellation even after immediate re-enabling. An interrupted
HID report is retried from its first packet without consuming undelivered input.
Desktop idle refreshes and gamepad transmissions resume when their endpoint is
enabled, without waiting for a separate USB status change. A disabled endpoint
waits for re-enabling before retrying.

All Cordial platforms use full-speed USB. Requests for `DeviceQualifier` or
`OtherSpeedConfiguration` descriptors stall.

On a configured VIA or Vial Raw HID interface, `GET_REPORT` for Input report ID 0 returns a
32-byte snapshot of the last successfully completed interrupt IN reply. A shorter requested
length returns that prefix; a longer request returns only 32 bytes. The snapshot starts at zero
and returns zeros after reset, configuration selection, USB disable or topology rebuild
until another reply completes. Pending or failed interrupt
writes do not replace it. Reading the snapshot consumes no queued reply and executes no editor
command. A reply cancelled by `SET_INTERFACE` is retried in queue order when its endpoint
is enabled, without executing the command again. Replies from an earlier USB generation or a
retargeted interface are discarded.

Raw HID accepts 32-byte commands through interrupt OUT or `SET_REPORT` for Output report ID 0.
It rejects other report types or IDs, zero-length `GET_REPORT`, malformed report requests,
`GET_IDLE`, `SET_IDLE`, `GET_PROTOCOL` and `SET_PROTOCOL`. These restrictions apply to Raw HID;
the desktop boot keyboard retains its protocol switching and idle reporting.

## USB remote wake

RP2040, RP2350 and ESP32-S3 support standard USB remote wake through Cordial-owned
USB wrappers. No custom host driver or Cordial application is required. The adapter
can request wake only while USB is configured, suspended and armed by the Host
through `SET_FEATURE(DEVICE_REMOTE_WAKEUP)`. The Host can revoke permission through
`CLEAR_FEATURE(DEVICE_REMOTE_WAKEUP)`. Host settings, hub support and platform sleep
policy determine whether USB wake is armed and whether it can wake the computer
from a particular sleep state. Support does not guarantee wake from every OS sleep
state.

Accepted new key or button presses and pointer motion can request wake, including
scrolling and changes to an already observed absolute pointer position. This
includes supported Consumer, system and radio controls and native gamepad digital
controls, even without a USB gamepad slot. The first report for each input report
ID establishes its held-control baseline. Status and battery reports, setup traffic,
rejected input, releases alone, unchanged held controls, resynchronization and
gamepad axis jitter do not request wake.

The adapter waits at least 5 ms after detecting suspend before starting a nominal
10 ms resume pulse. It waits for fresh host frames after the pulse before marking
input ready. Reset, USB disable and topology rebuild cancel stale wake state and
clear host permission. Disabling USB also deasserts any active resume signal.
After resume, currently held controls resynchronize under the suspend policy above.
A short tap fully released during wake can wake the Host without delivering the
tap as input; old motion and taps are not replayed.

### Board requirements

On RP2040 boards, endpoint recovery relies on the USB ABORT behavior of B2 or later silicon.

## USB allocation

One allocator selects interfaces for startup, configuration changes, device
connections and profile changes. It accounts for compiled interface and endpoint limits,
descriptor buffers, handlers and platform restrictions. Serial always fits and remains available.
HID gamepads keep separate interfaces, as does each XInput channel.

Lower numeric priorities win. When several conditions apply to a slot, it uses the lowest value.
Equal priorities keep stable slot order.

| Priority | Candidate |
| --- | --- |
| `0x00` | Serial, including both CDC interfaces |
| `0x41` | Shared keyboard/mouse interface required by a connected device |
| `0x42` | HID gamepad slot selected by a connected device |
| `0x43` | XInput channel selected by a connected device |
| `0x44` | Enabled editor whose selected profile is used by a connected device |
| `0x81` | Shared keyboard/mouse interface required only by disconnected configured devices |
| `0x82` | Configured HID slot zero, only when no HID users are connected |
| `0x83` | Configured XInput channel zero, only when no XInput users are connected |
| `0xC2` | Further configured HID slots with no connected user |
| `0xC3` | Further configured XInput channels with no connected user |
| `0xC4` | Enabled editor with no connected device using its selected profile |
| `0xE2` | Unconfigured extra HID slots, up to the compiled maximum |
| `0xE3` | Unconfigured extra XInput channels, up to the compiled maximum |
| `0xFF` | Disabled editor, excluded from candidates |

Configured users are saved devices whose roles, output preferences or profile outputs need the
interface, regardless of whether those devices are enabled or connected. A keyboard/mouse
interface with no configured users is absent. Each gamepad mode has
at most four slots where supported. Spare slots yield to higher priorities. A connected device can
preempt a lower-priority editor or an unused slot.

Connected users occupy the first slots in each mode. Slot zero uses the connected priority when
occupied; further configured disconnected slots keep their reserved priority of `0xC2` or `0xC3`.

The allocator computes the resulting plan before detach. Capacity does not reject valid
preferences: the adapter saves them and exposes what fits. Malformed values,
unsupported choices and profile conflicts still fail validation. Startup also selects a fitting
plan when saved preferences request more than the hardware can expose. Later reconfiguration
failures do not roll back persisted preferences.

USB rebuilds only when the actual allocation changes. A different priority or an omitted candidate
alone does not trigger another rebuild. This prevents repeated reconnects for an interface that
cannot fit. Bluetooth connections and current held input survive the rebuild; boot/report protocol
switching still reconciles held input and completes in-flight reports.

`Status.errors` independently reports `ERROR_CODE_USB_REQUIRED_CAPACITY_EXCEEDED` when any omitted
candidate has priority below `0xC0`, and `ERROR_CODE_USB_RESERVED_CAPACITY_EXCEEDED` when any omitted
candidate is in `0xC0..0xDF`. Both can be present together. Clients show the required error
prominently and mark tray attention; the reserved error has a subtle indicator. Omitted extras in
`0xE0..0xFE` produce neither error. Disabled interfaces at `0xFF` are not candidates.

Under the [session ending rules](transport.md#intentional-session-endings), only an
already-dispatched command may finish; queued requests are discarded. An atomic command in
progress completes before the transmission drain starts. Any active frame, its response and the
unsolicited `SessionEnding` event drain before detach, subject to a bounded transmission deadline
that host traffic cannot extend. Autonomous allocation changes send the event even when no command
is pending.
An active listing stops after its encoded piece in progress and ends with `Response.error`
using `ERROR_CODE_BUSY`, so producing the remaining entries cannot delay `SessionEnding`.
Detach closes the session, and clients read fresh status after reconnecting.

## Gamepad output preferences

Gamepad output has a separate saved choice per paired device. First-connection setup automatically
selects HID for a newly paired device with validated gamepad controls when the firmware supports HID
and its saved pool has room. Otherwise it settles on Disabled. Saved and explicit choices win;
completed setup never enables output retroactively. The choice takes effect after saving succeeds,
and a failed save retries on a later connection. Gamepad selection and HID++ detection settle
independently; both must finish before setup completes.

The generic output API defines HID and XInput choices, independent slot pools and runtime states.
Pico W, Pico 2 W, Pimoroni Pico Plus 2 W and Waveshare RP2350B-Plus-W provide both USB backends.
ESP32 leaves gamepad output
disabled. These preferences do not alter keyboard, mouse, Consumer or
system input forwarding, profile usage ranges, Bluetooth connection limits, or VIA/Vial behavior.
VIA/Vial coexistence work is tracked in [issue 1](https://github.com/ryanpetris/cordial/issues/1).

## Standard gamepad input

The existing HID compiler recognizes Generic Desktop Joystick and Game Pad application
collections, usages `0x0004` and `0x0005` on page `0x0001`. Its fixed input controls are:

- Button-page usages 1 through 32, as absolute variables or selectors. Each numbered button keeps
  its usage number. Numbered buttons do not imply face-button, shoulder-button or menu semantics.
- Absolute variable X, Y, Z, Rx, Ry and Rz, usages `0x0030` through `0x0035` on page `0x0001`.
  Each axis keeps its usage identity and scales its logical range linearly to 0 through 65,535,
  rounded to the nearest count. Signed, unsigned and nonzero logical minima work identically.
- Absolute variable Brake and Accelerator, usages `0x00c5` and `0x00c4` on Simulation page
  `0x0002`, use the same normalization. They retain their usage identities and independent
  validity. Zero is released and 65,535 is full actuation.
- An absolute variable Hat Switch, usage `0x0039` on page `0x0001`, with four or eight consecutive
  logical positions. The logical minimum is north; positions advance clockwise. Four-way hats
  use directions 0, 2, 4 and 6; eight-way hats use 0 through 7. Any out-of-range value releases
  the hat, including when the descriptor omits Null State.
- Explicit D-pad Up, Down, Right and Left, usages `0x0090` through `0x0093`, and Start and Select,
  usages `0x003d` and `0x003e`, on page `0x0001`. These absolute controls remain separate from
  numbered buttons and the hat.
- Consumer controls inside the Game Pad application and nested System Main Menu, usage `0x0085`
  on page `0x0001`. The snapshot retains the first eight distinct active Consumer usages in
  descriptor, report and service order. These controls are inputs to profile mapping.

`hid::Gamepad` exposes `buttons`, `consumers`, `menu`, `axes`, `axes_valid`, `axes_declared`,
`layout`, `hat`, `hat_valid`, `dpad`, `start` and `select`.
Axis order is X, Y, Z, Rx, Ry, Rz, Brake, Accelerator.
A clear bit in `axes_valid` means the axis is absent,
not yet reported, null or has a constant logical range. Its stored zero is not a trigger reading.
`hat_valid` distinguishes an observed neutral hat from an absent or unreported hat. `dpad` bits
0 through 3 mean Up, Down, Right and Left. Buttons and explicit controls default to released.
`axes_declared` and the candidate `layout` come from the compiled descriptor before input arrives.
Clearing input, receiving null samples or changing report arrival order preserves them.
Axis declarations combine across services. `Link::gamepad` requires a single gamepad service and
the layout and identity checks described below before selecting a builtin mapping.

`Map::decode` keeps accepted gamepad payloads in the service's existing state buffer. `Map::gamepad`
and `Link::gamepad` reconstruct current controls without allocating. Split report IDs and HID
services retain independent state. Buttons and explicit controls combine by union. When reports or
services repeat an axis or hat, the first valid report in descriptor order, then service admission
order, supplies that value. State never combines different Device connections. The normalized
snapshot occupies 48 bytes. Each gamepad report adds its exact payload length plus one received byte
to the existing state allocation, at most 8,208 bytes per map. Keyboard and mouse state allocations
do not grow.

Truncated or oversized retained reports and undeclared report IDs clear retained gamepad input.
Other decode failures also clear it, except keyboard rollover, which preserves gamepad controls in
every service. Omitted vendor input reports preserve all held controls. The existing
keyboard and mouse error behavior remains unchanged. A Link clears all its gamepad services after
other standard-input decode or forwarding errors and on disconnect. Null variable buttons and
explicit controls release; null axes lose validity. Non-null variable axes outside their logical
range clamp to an endpoint. Invalid array selections release without selecting another control.

Relative gamepad controls, numeric axis/hat selectors, buttons beyond usage 32, additional axes,
other hat ranges, vendor controls and Buffered Bytes fields have no gamepad interpretation. Gamepad
applications, including nested application collections, do not directly forward keys, mouse buttons,
pointer motion or Consumer controls to the host. Profile remaps can produce keyboard, mouse and
Consumer outputs from gamepad controls. Separate keyboard and mouse applications keep their normal
behavior. Profile rules identify gamepad inputs by their Game Pad application collection.
Absolute axis rules route and invert axes within that collection. No curves, deadzones, sensitivity,
rumble, proprietary initialization or console protocol is implemented.
Wholly vendor-defined gamepad report IDs are omitted from the compiled layouts; they do not consume the standard report count or payload
budget. A fixed 32-byte bitmap per map records omitted input IDs. Receiving one returns
`hid::Error::Ignored` from `Map::decode` and the Link preserves held gamepad, keyboard and mouse
controls without forwarding input or marking first input received. Only declared input IDs are
ignored; undeclared IDs and malformed retained reports keep their existing error behavior.
Output-only and Feature-only IDs do not count as input declarations. Vendor fields sharing a
retained report ID still contribute their declared offsets. The Xbox One S descriptor's single
trailing NUL is accepted after the final collection. Recognizing an input layout does not establish
support for an entire controller family.

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
no-change value. Selector arrays retain unrelated selections and reject an update
when there is no room for all required active indicators.
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

The following cases remain unavailable:

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

## USB gamepad outputs

On Pico W, Pico 2 W, Pimoroni Pico Plus 2 W and Waveshare RP2350B-Plus-W, each paired device
may select
Disabled, HID or XInput for its gamepad role. A missing selection means Disabled.
Saved devices contribute configured candidates even when disconnected or not resident in the
Bluetooth stack. Invalid bonds and duplicate peers follow the core's saved-record validation rules.
ESP32-S3 advertises no gamepad output modes.

The [shared allocator](#usb-allocation) selects the actual pools before initial enumeration and
whenever requirements change. A connection can use an already exposed vacant slot without a
rebuild, or preempt lower-priority interfaces when its required output does not fit. USB rebuilds
only when the selected interfaces change. A vacant HID slot reports neutral controls; a vacant
XInput channel reports absence.

The 22-byte HID gamepad report retains 32 numbered buttons, X/Y/Z/Rx/Ry/Rz axes,
the hat, explicit D-pad, Start and Select, and Simulation Brake/Accelerator.
Generic Desktop axes use signed 16-bit USB fields and center when unavailable.
Brake/Accelerator use unsigned `0..65535` fields and release at zero. These fields
share the existing gamepad interface. The HID encoder reconstructs all eight hat directions from
the mapped D-pad. Opposing directions cancel on each axis for the hat, while the explicit D-pad
bits retain every held direction. The raw input hat never bypasses profile mapping.

HID and XInput encode the same mapped HID state. XInput always reads Button usages
1/2/5/4/7/8/11/12/14/15/13 as A/B/X/Y/LB/RB/Back/Start/LS/RS/Guide, X/Y as the left
stick, Rx/Ry as the right stick, and Brake/Accelerator as the left/right triggers.
Explicit Start and Select also produce Start and Back; mapped D-pad directions produce XInput
D-pad bits, including opposing directions. Each missing or invalid stick axis centers independently;
missing or invalid triggers release. Triggers divide normalized `0..65535` values by 257 to produce
`0..255`. Sticks subtract 32,768 and invert Y with saturating negation. The XInput encoder has no
controller-specific rules, axis inference or deadzones.

Firmware builtin rules normalize recognized controller inputs before either USB encoder runs.
They apply only when the device has no assigned profiles. Any assignment disables the builtin,
including an empty profile, a keyboard-only profile or a profile that fails to load. Assigned
profiles receive the original controller inputs. Removing all assignments restores automatic
builtin selection. These rules are firmware definitions, not saved user profiles.

Builtin selection requires exact compiled input geometry: report IDs, payload lengths, field
offsets, usages, widths, logical ranges and flags. BLE also requires a matching PnP ID freshly read
for the current connection, including its vendor namespace. Classic uses bounded Xbox and DS4
descriptor signatures; Luna requires BLE identity confirmation. Multiple gamepad applications or
services prevent selection. The recognized layouts are:

- Luna Bluetooth LE: report `0x01`, 16-byte payload, 16-bit X/Y/Z/Rz sticks, 10-bit
  Brake/Accelerator triggers, a hat and sparse buttons. Rules route Z/Rz to Rx/Ry and map its
  face and shoulder buttons to the canonical usages. Consumer AC Back becomes Back, and AC Home
  in report `0x02` becomes Guide. The descriptor fixture credits
  [DJm00n's ControllersInfo Luna capture](https://github.com/DJm00n/ControllersInfo/blob/3013812fa478e09e70de7dde77b5e74919829177/lunacontroller/luna_bluetoothle_hid_report_descriptor.txt);
  [SDL's Luna decoder](https://github.com/libsdl-org/SDL/blob/release-3.2.0/src/joystick/hidapi/SDL_hidapi_luna.c)
  supplies the control meanings.
- Xbox Bluetooth Simulation: report `0x01`, 16-byte payload, 16-bit X/Y/Z/Rz sticks,
  10-bit Brake/Accelerator triggers, a hat and 15 buttons. Rules route Z/Rz to Rx/Ry and map
  source buttons 1/2/4/5 to canonical A/B/X/Y. Consumer AC Back and separate AC Home become
  Back and Guide where declared. Fixtures use the complete
  [Series descriptor](https://github.com/atar-axis/xpadneo/blob/v0.10.5/docs/descriptors/xbxs.md)
  and [One S Simulation descriptor](https://github.com/atar-axis/xpadneo/blob/v0.10.5/docs/descriptors/xb1s_linux.md).
  [SDL's Xbox decoder](https://github.com/libsdl-org/SDL/blob/release-2.30.9/src/joystick/hidapi/SDL_hidapi_xboxone.c#L843-L898)
  documents the button positions.
- Compact Xbox: report `0x01`, 15-byte payload, X/Y and Rx/Ry sticks, 10-bit Z/Rz triggers,
  a hat and ten buttons. Rules route Z/Rz to Brake/Accelerator and map source buttons
  1/2/3/4/5/6/7/8/9/10 to A/B/X/Y/LB/RB/Back/Start/LS/RS. System Main Menu in report
  `0x02` becomes Guide. The fixture uses the complete
  [compact One S descriptor](https://github.com/atar-axis/xpadneo/blob/v0.10.5/docs/descriptors/xb1s_windows.md).
  Incoming layouts are independent of the computer's operating system.
- DS4 basic Bluetooth: report `0x01`, nine-byte payload, 8-bit X/Y/Z/Rz sticks,
  hat and 14 buttons, six padding bits, then 8-bit Rx/Ry triggers, all axes `0..255`.
  Rules route Z/Rz to Rx/Ry and the original Rx/Ry to Brake/Accelerator simultaneously.
  Source buttons 2/3/1/4/5/6/9/10/11/12/13 map to
  A/B/X/Y/LB/RB/Back/Start/LS/RS/Guide. Digital L2/R2 buttons 7/8 and touchpad click
  are disabled by the builtin. Analog triggers remain available.
  The fixture uses the complete
  [hid-tools Bluetooth descriptor](https://github.com/bentiss/hid-tools/blob/0579ecdb4e041ce93c18ca45b3e8a2584f9ce952/hidtools/device/sony_gamepad.py#L716-L900).
  [Linux's DS4 decoder](https://github.com/torvalds/linux/blob/v6.12/drivers/hid/hid-playstation.c#L2288-L2300)
  supplies the button semantics.

Other Consumer controls, including Share and microphone, have no builtin output. Assigned custom
profiles can remap the original controls, including those disabled by a builtin. Unrecognized
layouts retain their HID usages; the fixed XInput conversion does not establish physical-control
semantics for them. Definitions in `rust/crates/cordial-core/src/gamepad_profiles.rs` and descriptor
fixtures in `rust/crates/cordial-core/tests/support/gamepad_descriptors.rs` credit their specific
sources. Absolute axis rules select a destination in the same Game Pad collection and optionally
invert its normalized range.

Each slot has current axis state, a 16-entry digital transition queue and an independent writer. The
priority loop publishes after every radio event, preserving button, hat, D-pad, Start and Select
transitions within a batch. Axes coalesce to their latest values. If a stalled host fills the queue,
the adapter discards its history, sends released digital controls and then restores current
controls. No input report allocates memory or waits for USB. Receiver OUT reads also run
independently. Status requests coalesce and the two ACK phases remain pending until the
corresponding presence or information transfer completes. A blocked gamepad endpoint does not stop
keyboard, mouse or serial traffic. Assignment epochs and occupants identify slot ownership. A writer
settles the previous transfer and sends neutral HID controls or XInput disconnect presence before
sending a replacement occupant's controls. USB reset and resume restart receiver presence and resend
current input. HID control reads use the current snapshot even while the interrupt endpoint is busy.

Pico transfer completion checks run every 125 microseconds for the first 8 milliseconds, then
every 8 milliseconds while the endpoint stays unread. A new transfer starts with fast checks.
This bounds wakeups for unopened gamepad endpoints while retaining physical completion ordering.
After a long stall, the next queued report can wait up to 8 milliseconds for completion detection.

The receiver channels share one contiguous `FF/5D/81` function with a Microsoft
OS 2.0 `XUSB20` compatible ID. Cordial keeps its USB identity and composite parent.
Microsoft documents OS 2.0 descriptor support in Windows 8.1 and later.
[Microsoft OS descriptors](https://learn.microsoft.com/en-us/windows-hardware/drivers/usbcon/microsoft-defined-usb-descriptors)
describe the binding mechanism. The receiver packet format follows
[the reference transport](https://github.com/9names/xinput-device/blob/main/xinput-device/src/xinput.rs)
and [Linux xpad](https://github.com/torvalds/linux/blob/master/drivers/input/joystick/xpad.c).

An active VIA or Vial Raw HID interface follows the gamepad functions. Disabling an active editor
rebuilds USB without its interface and endpoints. Vial's serial suffix follows its active exposure.
