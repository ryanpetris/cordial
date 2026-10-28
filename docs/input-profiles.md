# Input profiles

Cordial translates HID usages on the adapter, before combining input from connected devices. The
Host's language layout still interprets the resulting keyboard usages. A profile does not need the
physical switch layout of the connected keyboard, and using it requires no Host software after
configuration.

A profile is a named set of rules on HID usages, the numbers USB and Bluetooth devices use to
identify each key, button, axis, and media control. It is not tied to keyboards, mice, or any other
kind of device: the adapter applies whichever rules match the input a device produces.

- A remap rule makes an on/off input, such as a key, mouse button, media key, or Game Pad button,
  hold a fixed set of outputs instead, or disables it. Outputs can belong to different application
  collections, such as a Game Pad button holding a mouse button or a keyboard key with modifiers.
- A scale rule multiplies a relative value, such as pointer motion or a scroll wheel, by a ratio.
  A negative ratio inverts it.
- An axis rule routes an absolute Game Pad axis to a destination axis in the same Game Pad
  collection. Its `invert` flag reverses the normalized axis range.

A rule identifies its input by both its HID usage and its application collection. Mouse button 1,
`09:01` in Mouse collection `01:02`, and Game Pad button 1, `09:01` in Game Pad collection `01:05`,
are separate inputs. Every rule requires its input collection; the adapter never infers it from the
usage. A profile has at most one rule for each input usage and collection together.

A new profile is empty: every input passes through unchanged until a rule applies to it. The
adapter reports which usages and collections each rule type can name, which outputs it can produce,
and how much memory loaded profiles can use. It derives the profile's roles from its rules' input
collections, so clients can label it as keyboard, mouse, media, system control, or gamepad, or several.
Outputs do not add roles. A Game Pad button remapped to a Mouse output gives the profile the gamepad
role.

Remaps cover Game Pad buttons, Start, Select, and directional controls. Axis rules cover the advertised
absolute Game Pad axes. Clients match `ProfileSupport.ranges` by capability kind, collection, and
usage to determine what the adapter accepts. `UsageRange.Kind` distinguishes remap inputs and
outputs, scale inputs, and axis inputs and outputs. Unknown and unspecified kinds grant no
capability. The same usage and collection may appear separately under several kinds. A remap output may omit its collection only when exactly one advertised output
collection carries that usage. The adapter saves and reports the selected collection.

## Builtin controller mappings

On connection, firmware automatically selects a builtin controller mapping only when the device
has no assigned profiles. Any assigned profile disables the builtin, including an empty profile
or one with only keyboard or mouse rules. Assigning an empty profile gives raw input mapping;
removing all assignments restores automatic builtin mapping. Assigned profiles process the
device's original controller inputs, not builtin-normalized inputs.

HID and XInput share the same profiles and mapped HID state. XInput applies only a fixed conversion
of the canonical HID buttons and axes. Builtins exist only in firmware and are never stored,
listed, or reported as user profiles.

## Layers and editing

Each device has its own layers: an ordered list of profiles, empty when the device is first paired.
Each profile applies to the input as the profile before it left it: if the first remaps C to B and
the second remaps B to A, pressing C sends A. Separate keyboard and mouse profiles combine by
listing both. A remapped output reaches the next profile with its output usage and collection.
Saved profiles are limited only by flash.

Axis routes within one profile apply simultaneously to the values entering that profile. Two rules
routing X to Y and Y to X swap the axes without one route overwriting the other's source value. A
route moves its source value and whether that axis is present to the destination, taking precedence
over the destination's unchanged input. The next profile sees the resulting axes. Mapping an axis to
itself without inversion forgets its rule, as does remapping an input only to itself in the same
collection or scaling it by a ratio equal to 1.

A profile occupies memory only while a connected device or a configuration interface uses it. Loaded
profiles share a memory budget set by the board configuration and reported to clients. The Pico W
has no profile support, to keep its limited RAM for connections; the other presets support profiles.
A device whose profiles do not fit in what is left loads none of them and passes its input through
unchanged. Cordial notifies the user and also warns when the budget is nearly full. When a device
connects, the adapter reads the profiles in its layers from flash before forwarding its first input,
and shares a profile that another connected device already uses. Edits and layer changes apply to connected
devices from their next input; a held input keeps the outputs it was pressed with until it is
released.

Cordial's desktop app and TUI manage profiles and their layers but do not show or edit rules. Remaps
of keys, media and system controls, and mouse buttons are edited with VIA or Vial. The CLI edits all
supported rule types, including Game Pad controls and axes. Configuration interfaces expose one profile
each to an external editor over USB, each with its own switch and profile. The protocol allows
several interfaces to be enabled at once, and the adapter reports which ones it cannot run together:
VIA and Vial share a Raw HID usage, so this firmware lists them as conflicting. With every interface
disabled, no editor interface or its endpoints are allocated and every profile keeps translating input.
Saved enablement requests an editor interface; active exposure depends on
[USB allocation](protocol/hid-forwarding.md#usb-allocation). Valid preferences save even when the
interface cannot fit. USB reconnects only when the actual allocation changes. It computes that
allocation before startup and saves, then exposes what fits. After a reconnect, clients read fresh
status in a new serial session.
Selecting a profile for an interface does not apply it to any device. Cordial deletes a profile by
first removing it from each affected device's layers, keeping the other layers in order, then
turning off and clearing every configuration interface that selects it. The desktop app and TUI
list these changes in order for confirmation; the CLI uses the dependencies present when its
command starts. Deletion waits while unsaved layer or interface changes would use the profile. If
the listed changes differ when confirmed, Cordial asks for confirmation again.
If a dependency changes or an operation fails, deletion stops and completed changes remain.
The adapter's `DeleteProfile` command refuses a profile that is still in use.

For a profile that is in use, the desktop deletion dialog shows numbered steps and keeps the
confirmed plan in place while it runs. Completed steps get checkmarks, the active step gets a
spinner, and pending steps keep their numbers. If turning off an active interface reconnects the
adapter, Cordial waits for the same adapter and reads its current device and interface settings
before continuing. A newly assigned device or interface stops deletion. Failed and unconfirmed
steps have distinct markers while the remaining plan is unchanged. New steps keep their numbers;
the failure message remains visible. Retrying uses only the remaining current plan, and a changed
remaining plan needs confirmation after it is displayed. A connection lost before the final
deletion is confirmed leaves its outcome unknown.
Cordial does not automatically repeat it. After success, the heading names the deleted profile and
every step has a checkmark. The dialog stays open with only a full-width Close button until closed.

## External editors

- VIA uses protocol 9 over a 32-byte Raw HID interface. Load
  [`cordial-via.json`](../configs/keyboards/cordial-via.json) as a VIA v2 definition through the
  editor's Design tab. Remap uses the same VIA interface and definition, subject to its device
  registration/import workflow.
- Vial uses protocol 6. Active Vial exposure appends `-vial:f64c2b3c` to the adapter's USB serial, allowing
  Vial desktop to discover it and download its embedded definition automatically. The definition
  comes from [`cordial-vial.json`](../configs/keyboards/cordial-vial.json). Regenerate its
  compressed bytes with `python3 tools/generate_vial_definition.py` after editing that definition.

Both editors use the [Raw HID control report behavior](protocol/hid-forwarding.md#usb-control-requests).
Commands receive replies on interrupt IN; control reads return a snapshot without consuming a reply.

The virtual keyboard presents Keyboard/Keypad usages 0x04..0xa4, the eight modifiers 0xe0..0xe7,
seven media controls (Next Track, Previous Track, Stop, Play/Pause, Mute, Volume Up, and Volume
Down), the System Power Down, Sleep, and Wake Up controls, the other 16 Consumer controls QMK's
basic keycodes name (Media Select, Eject, Mail, Calculator, My Computer, the seven browser
controls, Fast Forward, Rewind, Brightness Up, and Brightness Down), and mouse buttons 1..16. The
familiar main block is followed by additional usage selectors. Its 14 by 16 matrix is stable and
identifies input usage and collection pairs, not Bluetooth keyboard switch locations. Keyboard
positions use Keyboard collection `01:06`, mouse buttons use Mouse `01:02`, media controls use
Consumer Control `0c:01`, and system controls use System Control `01:80`. Its last 13 positions select
nothing, read as disabled, and ignore writes. The editor shows the selected profile's own remap
rules, not the result of a device's layers. Rules for other inputs, including all Game Pad rules,
and scale or axis rules stay in the profile but are not exposed by this editor geometry. Ordinary
writes preserve unrelated rules. An input without a rule reads as its own usage, or as transparent
when the editor's keycode table cannot name that usage. An editor write that maps an input to its
own usage alone in the same collection, or to transparent, forgets the input's rule, as it does
through the serial API; any other write saves a remap rule. Resetting the keymap forgets every rule
in the selected profile, including Game Pad rules and rules outside the matrix.

Each editor uses QMK's keycode table for its protocol: VIA protocol 9 uses QMK's original
keycodes and Vial protocol 6 its current ones. Supported actions are disabled input, a single
keyboard usage, one of the System or Consumer controls above, a mouse button, and QMK's
representable left-side or right-side modifier combination with a keyboard key. VIA names mouse
buttons 1..5 and Vial buttons 1..8. Cordial's rules also support other consumer controls, mouse
buttons 9..16 as outputs, and other combinations of held outputs, including a modifier with a
control or mouse button; a rule the selected external editor cannot represent keeps working on
the adapter but cannot be read through that editor. Reading such a rule makes the editor's keymap
read fail: a buffer read containing it returns 0xff for the entire chunk. The rule stays saved
unchanged. Enabling Vial explicitly authorizes fixed remap edits, so the interface reports unlocked.

Each supported edit packet changes the loaded rules atomically and is acknowledged at once, before
it reaches flash: devices use the edit immediately, and the editor's edits are saved together once
it pauses, within about 2 s of each edit, and before USB enumerates again or the bootloader is
entered. See [when files are written](storage-format.md#when-files-are-written). An edit made
shortly before power is removed can be lost. An invalid action, or an edit the profile memory budget
cannot hold, returns VIA's unhandled response, 0xff, without publishing a new runtime rule. A save
that fails after the acknowledgement keeps the edit in use and is retried until it succeeds. While
its last try found the filesystem full, storage is reported full, and while its last try ended with
an unknown outcome, storage is not ready; editor packets are refused while storage is not ready. A
complete multi-packet import is not atomic. External editors may not present rejection clearly;
verify the result by reading it back. Copies include every saved rule and remain independent.

The USB consumer report holds at most eight distinct simultaneous consumer usages across all
connected devices, including remapped outputs. Exceeding that bound triggers the input-overflow
failure and disconnects the source; the adapter does not silently drop part of a chord.

Protocol references: [QMK VIA commands](https://github.com/qmk/qmk_firmware/blob/master/quantum/via.h),
[Vial VIA implementation](https://github.com/vial-kb/vial-qmk/blob/vial/quantum/via.c), and
[Vial extensions](https://github.com/vial-kb/vial-qmk/blob/vial/quantum/vial.c).

On Linux, the active local user needs access to the adapter's `hidraw` node for external editors.
The Arch and Debian packages install [`50-cordial.rules`](../configs/50-cordial.rules), which grants
the active seat user access to the adapter's serial port and HID nodes. Without the packages, copy
`50-cordial.rules` from the release archive to `/etc/udev/rules.d/`, reload udev rules, and
reconnect the adapter.

## Controller research credits

Controller descriptor research uses [ControllersInfo](https://github.com/DJm00n/ControllersInfo)
by [DJm00n](https://github.com/DJm00n) and its contributors. Their published HID descriptor captures
and controller documentation inform Cordial's gamepad mappings.
