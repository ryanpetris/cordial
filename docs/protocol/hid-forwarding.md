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

## Gamepad output preferences

Gamepad output has a separate saved choice per paired device. First-connection setup automatically
selects HID for a newly paired device with validated gamepad controls when the firmware supports HID
and its saved pool has room. Otherwise it settles on Disabled. Saved and explicit choices win;
completed setup never enables output retroactively. The choice takes effect after saving succeeds,
and a failed save retries on a later connection. Gamepad selection and HID++ detection settle
independently; both must finish before setup completes.

The generic output API defines HID and XInput choices, independent desired pools and runtime states.
Pico W, Pico 2 W and Waveshare RP2350B-Plus-W provide both USB backends. ESP32 leaves gamepad output
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

## USB gamepad outputs

On Pico W, Pico 2 W and Waveshare RP2350B-Plus-W, each paired device may select
Disabled, HID or XInput for its gamepad role. A missing selection means Disabled.
Each USB pool contains the number of valid paired records selecting that mode,
capped independently at four. Saved devices count even when disconnected or not
resident in the Bluetooth stack. Invalid bonds and duplicate peers follow the
core's saved-record validation rules. ESP32-S3 advertises no gamepad output modes.

USB allocates these pools before the initial enumeration. Pairing, unpairing or a
saved output change rebuilds USB only when a pool count changes. The owner waits
for persistence and physical completion of the command response, cancels the USB
workers, disables the device, drops the allocation, and waits 250 ms before
reborrowing the peripheral. Bluetooth connection changes assign or release slots
without USB enumeration. A vacant HID slot reports neutral controls; a vacant
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
  supplies the button semantics. Enhanced Bluetooth report `0x11` and the USB
  64-byte report `0x01` are outside this mapping. No feature request switches modes.

Other Consumer controls, including Share and microphone, have no builtin output. Assigned custom
profiles can remap the original controls, including those disabled by a builtin. Unrecognized
layouts retain their HID usages; the fixed XInput conversion does not establish physical-control
semantics for them. Definitions in `rust/crates/cordial-core/src/gamepad_profiles.rs` and descriptor
fixtures in `rust/crates/cordial-core/tests/support/gamepad_descriptors.rs` credit their specific
sources. Core `gamepad_profiles` and USB `gamepad_tests` fixtures cover mapping and encoding.
Descriptor and driver evidence does not establish universal controller support. The manual Windows
results below cover only the tested setup. Absolute axis rules select a destination in the same
Game Pad collection and optionally invert its normalized range. Neither mode adds curves, deadzones
or sensitivity. Host LED and rumble commands are consumed without forwarding them to the device.

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

Manual Windows testing reports working XInput in MultiPadTester and HIDAPI enumeration with button
input. MultiPadTester's generic HID labels use buttons 1/2/3/4/5/6/7/8/9/10 for
A/B/X/Y/LB/RB/Back/Start/LS/RS. Cordial's Linux convention above therefore explains the observed
X-to-L1, L1-to-Back and R1-to-Start labels. Those labels do not establish a firmware mapping fault.
Browser XInput visibility and right-stick visualization remain unresolved. Full HID axis and trigger
behavior and multiple simultaneous channels have not been manually verified.

The existing Raw HID configuration slot remains allocated after the gamepad
functions. Its descriptor filtering, VIA/Vial behavior and serial suffix are
unchanged. Reclaiming that slot's resources is tracked separately in
[issue 1](https://github.com/ryanpetris/cordial/issues/1).
