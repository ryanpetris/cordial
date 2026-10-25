# CLI, TUI and desktop behavior

[Protocol index](README.md)

The CLI, TUI and desktop application are built on the client libraries and add no commands of
their own. None of them runs a background service; the Dongle keeps forwarding input, reconnecting
devices and applying saved settings after they exit.

## Client state

All three keep their view of the Dongle the same way:

- Listing: a client sends one request for the complete list. It waits for the frame delimiter,
  decodes the merged protobuf response, and applies the complete collection or error in receive
  order. It discards incomplete frames on disconnect. See [Listings](compatibility.md#listings).
- Events: `device`, `adapter` and `profile` events replace the record they carry.
  `settings_changed` is applied to what the client has listed: whole domain objects are replaced by integration, group
  and optional component index, and removals of entries the client does not hold are ignored. Adapter and device error
  lists replace the previous lists with the complete reported sets. A client lists again from the start when it needs a fresh view, such
  as in a new session or after it lost track of a list.
- Successful changes: a response without an error means the Dongle now holds the values sent, in
  the saved form the command defines (see [Commands](commands.md)): a trimmed adapter name, and
  normalized rules, with a rule that changes nothing forgotten. The client applies the values it
  sent, normalized the same way, to its own state and does not query again to confirm them. A
  default adapter name restored with `""` arrives in the `adapter` event that follows.
  After `CreateProfile` or `CopyProfile` it records the returned ID with the name it sent; a new
  empty profile has no roles, and a copy has the source's roles. Events still update the client's
  state as they arrive.
- Failed changes: only after an error, including a storage failure whose outcome is unknown, may a
  client query again to learn the current state, and only when it needs that state.

Clients preserve full `Error` messages through decoding, library errors and state updates,
including reason, outcome and related errors recursively. A familiar code does not justify
discarding details or inferring an outcome. Missing details confirm neither an unchanged value
nor success and never authorize retry. Related cleanup failures do not replace a primary failure
or its outcome; a pairing error with `COMPLETE` confirms the bond save while still reporting the
later failure. See [Errors](limits-and-errors.md#errors).

A device listing's `RecordError` retains any previously known device as last-known data. It does
not confirm its connection or preferences. Fresh records and events replace that data; actual
absence from a completed list or a removal event removes it. A profile read-error placeholder does
not require retaining its cached record. A listing continuation failure instead rejects the
entire partial list and leaves the previous collection intact.

`ScanDone` reports retained candidate count and an optional full error. Scan errors leave those
candidates usable until a new scan or session end, subject to adapter readiness for pairing.
Clients distinguish a scan failure from an ordinary timeout and preserve candidate-loss details
related to a primary radio error.

## Client library listings

Rust `Connection` methods `list_devices()`, `list_profiles()`, `list_settings(device)`,
`list_profile_rules(profile)`, `list_features(device)` and `list_files(path)` return the complete
protobuf response messages. TypeScript uses `listDevices()`, `listProfiles()`, `listSettings(device)`,
`listProfileRules(profile)`, `listFeatures(device)` and `listFiles(path)` with the same result
shapes. The collections are `entries`, `objects`, `rules` or `features`, as defined by each response.
No method takes a cursor. The `all_*` and `listAll*` conveniences return the response collection
from a single request. Rust identity and ordering helpers live in `cordial_client::order`.

Both clients assemble responses on the host and preserve request FIFO and response/event ordering.
Request timeouts measure incoming-byte inactivity so a progressing response can outlast the timeout.
Every complete outgoing `Request` must fit 1024 protobuf bytes, including all entries and the
command envelope. `SetProfileRules` is atomic per request, with no automatic splitting or atomicity
across several requests.

The Rust client library sends the first request alone. Other callers wait until a `Response`
answers it, including `Response.error` or a response with no result. It then allows pipelining and
matches responses to requests in write order. If sending or waiting for the opening response fails,
including a timeout or connection close, waiting callers fail without writing their requests.

The Rust client library, CLI and TUI enforce a local 32 MiB decoded receive limit on each complete
protobuf `Message`, including nested envelopes and all repeated listing batches. The TypeScript
desktop and web clients do not apply this limit. See
[Rust host receive limit](transport.md#rust-host-receive-limit) for overflow and session behavior.

## CLI

`cordial --port PORT` opens the shell; `cordial --port PORT COMMAND ...` runs one command, and piped
input runs as a script. `cordial tui` opens the TUI. Without `--port`, the CLI uses the only attached
adapter and fails when there are several. Controller selection uses USB ports, not the host's
Bluetooth controllers.

Only one app can manage an adapter at a time. Quit the desktop app, or disconnect the adapter in it,
before using the CLI.

| CLI command | Behavior |
| --- | --- |
| `adapter list` / `adapter select PORT` | List attached adapters, or switch to one. |
| `adapter status` | Identity, readiness, platform, and each supported transport with whether it is enabled and its limits. |
| `adapter set name NAME` / `adapter reset name` | Save the adapter name, or restore the firmware default. Quote names containing spaces. |
| `adapter set platform linux\|windows\|mac` | Save the host platform. |
| `adapter set transport classic\|ble enabled on\|off` | Enable or disable a transport. Lists only the transports the firmware supports. |
| `adapter bootloader` | Development firmware: reboot into programming mode. |
| `scan start [classic\|ble] [SECONDS]` / `scan stop` | Discover devices on every enabled transport or one, for 10 seconds by default. |
| `pair start CANDIDATE` | Pair a discovered candidate. |
| `pair accept [VALUE]` / `pair reject` / `pair cancel` | Answer the pairing prompt, or cancel the pairing. |
| `device list` / `device get DEV` | Saved devices and candidates, or one device with its information. |
| `device connect DEV` / `device disconnect DEV` | Connect, or disconnect and pause reconnection. |
| `device set DEV enabled\|trusted\|blocked\|hidpp on\|off` | Change a saved device's policy or HID++ preference. |
| `device set DEV gamepad-output disabled\|hid\|xinput` | Save a paired device's Gamepad Output choice. |
| `device unpair DEV` | Delete the device's bond and settings. |
| `device refresh DEV` | Re-read a connected device's information and settings. |
| `setting list DEV` / `setting get DEV KEY` | The device's settings, or one in detail. |
| `setting set DEV KEY VALUE` / `setting forget DEV KEY` | Save or forget a setting. |
| `feature list DEV` | Development firmware: the device's integration features. |
| `file list PATH` / `file get [--raw] PATH DEST` | Development firmware: browse or download adapter files. A saved record is converted to JSON unless `--raw` is given; see [downloaded records](#downloaded-records). |
| `help` / `help COMMAND` / `quit` / `exit` | Local help and clean shutdown. |

`DEV` is a device ID shown in output, or an unambiguous name; completion offers both. A number is
always taken as an ID, so a device whose name is a number is addressed by its ID. `CANDIDATE` is a
candidate ID shown in scan output, or an unambiguous name, with the same rule for numbers. Candidate
and device IDs are separate sequences; each command takes only one of them. The CLI never picks
silently between duplicate names. Setting paths name typed fields, such as `keyboard.function_row` or
`pointer_sensor.0.dpi`; these paths are CLI labels, not serial fields.

- The shell keeps history and completion, and prints changes as they arrive without disturbing
  partly typed input, prefixed `[NEW]`, `[CHG]` or `[DEL]` with the device ID. It keeps accepting
  commands while a scan, pairing or connection runs.
- A pairing prompt asks for the passkey or PIN, or a yes/no comparison, directly. While a prompt is
  open, `/COMMAND` runs a normal command; `pair accept` and `pair reject` answer it explicitly.
  Saving the bond and the later connection result are reported separately.
- Ctrl-C, Ctrl-D, end of input, SIGINT and SIGTERM quit the shell, which closes the port: a
  running scan stops and an unsaved pairing is cancelled.
- When the open adapter's connection drops, as when a save or a device connecting or disconnecting
  changes the USB allocation, the shell waits up to 15 seconds for the same adapter, found by the
  adapter ID at the start of its USB serial number. It reopens the adapter on whichever port it
  returns to and reads its status and devices again. Only an adapter that doesn't return in time is
  reported as lost. Commands cut short by the drop are not sent again. Each reports its result once,
  even after the adapter is reopened, and a save cut off before the adapter answered reports that
  the adapter couldn't confirm whether the change was saved. `adapter select` or quitting ends the
  wait, and `adapter select` replaces a reopen already in progress.
- One-shot commands and scripts run each command after the previous one's result and exit nonzero
  on failure. `--timeout SECONDS` bounds a one-shot command and sets the length of a one-shot scan.
  A one-shot `pair start` finds the candidate by name within the same invocation, since candidate
  IDs last only for the session. A timeout never undoes a bond already saved.
- Before sending, the CLI refuses what the Dongle would refuse, using what it already knows (see
  [Client pre-checks](limits-and-errors.md#client-pre-checks)); when the Dongle refuses anyway, the
  error is shown and nothing is retried.
- Output is human-readable and rendered safely for the terminal. `--json` instead prints every
  response and event as one line of protobuf JSON with the schema's field names. File contents
  are never printed: the response of `file get` is not shown.

Example interactive workflow:

```text
$ cordial --port PORT
[cordial]# scan start
[NEW] 12 Example keyboard (BLE)
[cordial]# pair start 12
[NEW] 7 Example keyboard
[CHG] 7 Connected: yes
[cordial]# scan stop
[cordial]# device get 7
[cordial]# quit
```

### Downloaded records

The adapter sends a file's bytes as it saved them; it never converts them. Each saved record holds
one message from `proto/storage.proto`, named by its path as listed in the [storage
format](../storage-format.md#files). `file get` decodes a file at such a path and saves the message
as indented protobuf JSON with the schema's field names, the mapping `--json` uses, under the
destination name with `.json` in place of `.pb`. A file at any other path, an empty file, one that
doesn't decode, or one holding an enum value this version of Cordial has no name for is saved
unconverted under its original name, with `.pb` in place of a destination's `.json`, and the result
says why. `--raw` saves the bytes unconverted under the destination name as given.

## TUI

The TUI uses Ratatui and Crossterm and the same commands as the CLI. It follows the desktop
application's layout, words and staging rules in the terminal, and opens a session with every
attached adapter; `--port` limits it to that one port.

- Discovery: it lists the attached adapters every second, opens each new port that matches the
  [USB discovery descriptors](transport.md#discovery), and shows an adapter once its first
  `GetStatus` identifies it. A port that fails to open, or whose session ends within 10 seconds
  of opening, is tried again after a wait that doubles with each failure in a row, up to a minute;
  Refresh Adapters (F5) tries it at once. An adapter that goes away keeps its page, locked, for
  15 seconds; its devices leave the list at once. A shown device page also stays locked, with its
  tab, staged changes and save result, until the adapter lists its devices again. The same adapter
  returning is found by the adapter ID that begins its USB serial number. A device missing from
  the new list, or an expired hold, returns the page to Overview. Disconnect, from the adapter's
  page or menu, closes its session; the adapter stays listed while plugged in and is opened again
  only by Connect.
- Layout: a sidebar with Overview, the saved devices of every adapter in one list, the adapters,
  and Add Device; and the selected page with its tabs and bottom bar. The Overview counts
  connected and paired devices and connected adapters, and lists what needs attention, the
  connected devices with their batteries, and the adapters. Adapter and device pages have the same
  tabs and controls as the desktop application's, and the same pages and dialogs. Development
  firmware adds Files and Enter Bootloader to the adapter's Diagnostics tab. Files browses adapter
  directories and downloads a file to a host destination, asking before replacing one. A saved
  record is converted to JSON as `file get` converts it, and its destination is suggested with
  `.json` in place of `.pb`. A download is written to a temporary file and moved into place only
  when complete, with a no-replace rename, so a failure leaves the destination unchanged.
- Attention: while the TUI runs, a device whose battery is low (20% or less and not charging) or
  critical (5% or less), a connected device whose profiles weren't loaded, an adapter whose
  profile memory is 85% used, and an adapter that needs attention are listed under Needs Attention
  on the Overview, and the most pressing one is shown in red on the status line.
- Staging: changes stay staged, marked as changed, until Save, as in the desktop application;
  Ctrl-S presses the Save shown on screen. Discard drops the changes the shown Save would send, and
  a failed save keeps them. Valid USB preferences save without a reconnect confirmation dialog.
  Connect, disconnect, rename, forget and creating, copying and deleting profiles act at once.
- Input: every action works with the mouse, and every control has a keyboard equivalent. Tab and
  Shift-Tab move between controls, Up and Down choose in the sidebar or move between controls,
  Left and Right change the highlighted setting or switch tabs, and Enter or Space presses the
  highlighted control. Mouse movement and Tab share one highlight; hover alone never activates
  anything, and a click in a dialog never reaches the controls underneath it.
- Resizing keeps form input and recomputes click targets. Text labels accompany every color. `tui`
  is refused with `--json` or without an interactive terminal. Ctrl-C and SIGINT quit from
  anywhere, close every session, which stops a scan and cancels an unsaved pairing, and restore the
  terminal; quitting never disconnects saved devices.

## Desktop application

The desktop application uses the same commands and session rules as the CLI.

- Discovery: it lists serial ports once at startup, then again after USB hotplug events (with short
  retries while udev sets up a new node) or an explicit refresh. A port must match the [USB
  discovery descriptors](transport.md#discovery), and the first `GetStatus` identifies the adapter.
  The web version lists the ports the user has granted by vendor and product ID. Unidentified
  ports stay hidden. When an adapter has a readable USB serial number that matches its status ID,
  an unexpected connection loss marks the adapter unavailable and its devices Disconnected.
  The existing adapter and device entries, pages, selected tabs, staged changes and save results
  remain in place during a 15-second reconnect window.
  Configuration controls stay disabled until current data is confirmed. The application retries
  every 500 milliseconds throughout the window, including after an early connection loss or a
  temporary failure to reopen the port. Port enumeration continues while other ports are opening.
  It recognizes the same adapter by the adapter ID at the start of its USB serial number,
  even on a different port, confirms that ID in the handshake, and updates the existing state.
  A candidate without a matching readable USB serial number cannot resume that state.
  The entries and pages remain in place between the new session's handshake and device listing.
  A failed listing retains them; a
  completed listing removes devices that are absent. A per-record read error retains last-known
  device data as described above. Settings and profile lists remain visible while their reads are
  pending or fail, but their writes stay disabled until the relevant read succeeds. Failed reads
  can be retried. An adapter that returns with a radio or storage fault remains listed with its
  current status and disabled device actions, even beyond the window. Its existing device records
  remain until it becomes ready and lists them again. When the original window expires, the
  application removes an adapter whose transport or ready device listing has not recovered, and
  its devices, and leaves their pages. Without a usable USB serial number, a session loss or
  disappearance from the port list removes the adapter and its devices immediately, without a
  reconnect window or automatic retries. This includes Web Serial, which cannot read USB serial
  numbers. A later hotplug scan or explicit refresh creates a fresh connection, including on the
  same port, and does not restore the previous session's state.
- Sessions: after `GetStatus` it reads the complete device list and each device's settings, then
  follows events, and lists the devices again when the adapter becomes ready. When a device
  listing fails on a connected, ready adapter, the adapter's Diagnostics tab names the failure with
  Retry, which lists the devices again. If the devices haven't been listed since the adapter last
  became ready, it shows Needs Attention rather than Starting until Retry or another listing starts. A USB
  reconnect opens a fresh session and reads status instead of expecting an event from the old
  session.
  Bluetooth radio loss leaves paired devices listed.
- Disconnect: the user can disconnect an adapter from its context menu. The application closes the
  port and does not reopen it, recognizing it by the first 16 characters of its USB serial number,
  until the user chooses Connect or restarts the application. The web version recognizes it by its
  port instead. This intentional disconnect does not start the reconnect window.
- Pairing: Add Device scans the transports the firmware supports and has enabled while its dialog is
  open, stops the scan before pairing, answers prompts in the dialog, and follows the new device
  until it connects or fails. Closing the dialog, or hiding the window, stops a scan or cancels an
  unsaved pairing. A settled pairing result that confirms a saved device can continue following
  that device after reconnect. Candidates and active pairing prompts from the previous session
  cannot be used in the new session. An interrupted active pairing reports that the adapter could
  not confirm whether the device was paired; it is not replayed.
- Devices open on the Details tab, with the connection switches and the device's own facts. Settings
  follows when the device has settings to show, then Profiles when the adapter supports profiles,
  with the device's layers (an ordered list of profiles, empty for a new device), then Diagnostics:
  the device's errors, the HID++ protocol and status,
  link security and identifiers, with Refresh. Changes on Details, Settings and Profiles stay staged
  across tabs until that tab's Save sends its own changes in one request; Connect, Disconnect and
  Forget act at once. Device errors stay on Diagnostics.
- Profile memory: the application notifies once when `Status.errors` includes
  `ERROR_CODE_PROFILE_MEMORY_LOW`, and once when a connected device's `errors` includes
  `ERROR_CODE_PROFILE_MEMORY_EXCEEDED` or `ERROR_CODE_PROFILE_LOAD_FAILED`. Each notification
  resets after its condition clears. Device errors appear on Diagnostics.
- Adapters open on Details, with Settings, Profiles when the adapter supports them, and Diagnostics.
  Profiles lists every profile, each with an icon for each role it changes, and the configuration
  interfaces, each with its own switch and profile. The profile picker reads the complete list.
  Changes on Settings and Profiles stay staged across tabs until Save sends them together in one request; a
  failed save keeps them. Saving needs no reconnect confirmation dialog. Creating,
  copying and deleting profiles take effect at once.
- Battery: a device's battery is low when a known `battery.level` is at or below the user's
  threshold (20% by default) and `battery.charging` is not true; at most 5% is critical. It notifies
  once on entering low and once on entering critical, and resets only after a reading above the
  threshold or while charging.
- Tray: a StatusNotifierItem icon is shown while at least one adapter is connected (or always, by
  preference). Its outline means no device is connected and filled means at least one is; a badge
  marks low battery or an adapter needing attention. Its menu lists devices, grouped by adapter when
  several are connected.

## Gamepad outputs

Gamepad Output has the choices Disabled, HID and XInput. A missing saved gamepad
preference means Disabled. The desktop application and TUI show this control only
on a paired device's Settings tab. Its roles or kind must identify a gamepad and
the adapter must advertise an enabled mode, or the device must have a saved
gamepad choice other than Disabled. Unrelated keyboards and mice have no such
control unless they have a saved choice to clear. Changes follow the existing
Save and Discard behavior. The CLI saves at once:

```sh
cordial --port PORT device set DEV gamepad-output hid
```

HID and XInput use the same profiles and mapped HID state. XInput only converts canonical HID
buttons and axes through a fixed mapping. On connection, firmware automatically selects a builtin
controller mapping only for a device with no assigned profiles. Any assignment disables it,
including an empty profile or one with only keyboard or mouse rules. Assigned profiles receive
original controller inputs, not builtin-normalized inputs. Assign an empty profile for raw input
mapping, or remove all assignments to restore automatic builtin mapping.

Builtins are firmware-only mappings, never stored, listed or reported as user profiles.

Clients use `Status.output_support` to offer enabled modes, rather than checking
board names. When a backend is unavailable, clients offer no new enabled choices;
a saved choice can still be cleared with Disabled. `SLOTS.limit` in `OutputModeSupport.resources`
limits USB allocation, not the number of paired devices that may select a mode. Each change uses
`SetDevice.output_preferences` for the Gamepad role and preserves unrelated preferences.

The saved choice is separate from live output status. Clients use `Device.outputs` for the current
mode, state or full error, and optional slot. They never infer a slot from pairing order or treat
a saved choice as proof of active input. Human-readable slots start at 1; raw JSON and protocol
slots start at 0. A slot is temporary and is not a Windows XInput user index. Only Active carries a
slot. Unsupported backend and input layout errors retain their distinct reasons; Pending alone
is not an error. Technical output states and device errors belong on Diagnostics.
The CLI's `device get DEV` shows Gamepad Output and,
when reported for an enabled choice, Gamepad Status and Gamepad Slot.
`device list` uses `gamepad-output`, `gamepad-state` and `gamepad-slot` tokens.
`adapter status` shows each advertised mode's pool, such as `Gamepad Slots (HID): 2 of 4`,
or `at most 4` while its size is unknown. See [output states and
pools](records.md#devices) and [controller mapping limits](hid-forwarding.md#usb-gamepad-outputs).

The desktop application and TUI save valid output preferences without a reconnect confirmation
or a USB-capacity pre-check. Save sends other staged device settings before Gamepad Output,
since its save can change USB allocation. If those settings fail to save or their outcome is
unknown, Gamepad Output stays staged. A received success confirms the saved preference; active
USB exposure is reported separately. If the response is lost, clients report an unknown outcome
and may read the device after reconnecting. When the adapter has a usable USB serial number,
automatic reconnect tracking keeps the device page and its staged state in place while reading
fresh status in the new session. Neither output mode requires a running Host service or an
additional configuration interface.

Clients show both USB capacity errors when both appear in `Status.errors`. Required capacity has
a prominent warning and tray attention. Reserved capacity has a subtle indicator and no large
Overview banner. Resource displays use each reported optional value independently: count only,
limit only, both, or neither. Missing values are unknown and are never formatted as zero or
unlimited.

## Profile commands

The desktop application and TUI manage profiles, layers and configuration interfaces, and never read
or change a profile's rules. Remaps of keys, media and system controls, and mouse buttons are edited
with VIA or Vial. Their fixed matrix addresses Keyboard, Mouse, Consumer Control and System
Control input and collection pairs. Ordinary writes preserve unrelated rules, including Game Pad
rules. Keymap reset clears every rule in the selected profile, including rules outside the matrix.

The adapter derives each profile's roles from the input collections its rules change, including
Game Pad. A remap's output collection does not add a role. A Game Pad button remapped to a mouse
button therefore gives the profile the gamepad role.

These commands require `Status.profile_support`. A profile argument is its ID or a unique name; a
name is found by reading the profile list. A number is always taken as an ID, so a profile whose
name is a number is addressed by its ID. LAYERS is one or more profile arguments in the order they
apply, quoted as needed, or `none` for an empty list; a profile named `none` is addressed by its ID.

| Command | Behavior |
| --- | --- |
| `profile list` | List every profile with its roles. |
| `profile show PROFILE` | Show a profile's name and roles. |
| `profile create NAME` | Create an empty profile. |
| `profile copy PROFILE NAME` | Copy on the adapter. |
| `profile delete PROFILE` | Delete an unused profile. |
| `adapter set interface INTERFACE on\|off [PROFILE]` | Enable or disable a configuration interface such as `via` or `vial`, optionally selecting its profile in the same request. |
| `adapter set interface INTERFACE profile PROFILE` | Select the profile a configuration interface edits. |
| `adapter reset interface INTERFACE profile` | Clear a disabled interface's profile. |
| `device set DEV profiles LAYERS` | Set a device's layers. |

### Profile rules

These commands read and change a profile's HID rules. Each input is identified by both its usage
and its application collection. Saving or forgetting a rule affects only that pair.

| Command | Behavior |
| --- | --- |
| `profile rule list PROFILE` | Show every rule. |
| `profile rule remap PROFILE INPUT OUTPUTS` | Remap an on/off input to the outputs, held together. |
| `profile rule scale PROFILE INPUT N/D` | Multiply a relative value input by N/D; a negative N inverts it. |
| `profile rule axis PROFILE INPUT AXIS [invert]` | Route an absolute Game Pad axis to another axis in the same Game Pad collection, optionally reversing its normalized range. |
| `profile rule forget PROFILE INPUT` | Forget a rule so the input passes through unchanged. |

A USAGE is `PAGE:USAGE` in hexadecimal, such as `07:39` for Caps Lock or `01:38` for the wheel.
INPUT requires `USAGE@PAGE:USAGE`, with the part after `@` naming its application collection, such as
`07:39@01:06` for Keyboard Caps Lock, `09:01@01:02` for Mouse button 1 or `09:01@01:05` for Game
Pad button 1. The input collection is never inferred. AXIS is a USAGE in the input's Game
Pad collection. OUTPUTS is `disabled` or a comma-separated list of USAGEs, each optionally followed
by `@PAGE:USAGE` naming the output report's collection. An output collection may be omitted only
when exactly one advertised collection carries that usage. For example, button 1 needs an explicit
Mouse or Game Pad collection.

Clients match usage and collection against `ProfileSupport.ranges` with `kind` set to
`KIND_REMAP_INPUT`, `KIND_SCALE_INPUT` or `KIND_AXIS_INPUT` for the chosen effect, and to
`KIND_REMAP_OUTPUT` or `KIND_AXIS_OUTPUT` for its outputs. Unknown kinds and `KIND_UNSPECIFIED`
grant no capability. The same usage and collection may appear separately under several kinds. A rule that remaps an input only to itself in the same collection, scales it by 1, or
routes an axis to itself without inversion is forgotten instead of saved.

For a profile named `Controls`, remap Game Pad button 1 to Mouse button 1:

```sh
cordial --port PORT profile rule remap Controls 09:01@01:05 09:01@01:02
```

Invert the Game Pad Y axis:

```sh
cordial --port PORT profile rule axis Controls 01:31@01:05 01:31 invert
```

To swap X and Y, save two rules in the same profile:

```sh
cordial --port PORT profile rule axis Controls 01:30@01:05 01:31
cordial --port PORT profile rule axis Controls 01:31@01:05 01:30
```

Axis routes within one profile read the values entering that profile and apply simultaneously, so
the two swap rules retain both source values. Each route moves its source to the destination and
takes precedence over the destination's unchanged input. Later profiles see the resulting axes.
Axis rules add no deadzones, curves or sensitivity settings.

See [Input profiles](../input-profiles.md) for external editors.
