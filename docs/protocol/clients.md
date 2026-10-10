# CLI, TUI and desktop behavior

[Protocol index](README.md)

The CLI, TUI and desktop application are built on the client libraries and add no commands of
their own. None of them runs a background service; the Dongle keeps forwarding input, reconnecting
devices and applying saved settings after they exit.

## Client state

All three keep their view of the Dongle the same way:

- Listing: a client reads a list one page at a time, passing the key of the last entry it received
  as the next request's `after`, until `end` when it needs the whole list. It never assumes a page
  size. See [Listings](compatibility.md#listings).
- Events: `device`, `adapter` and `profile` events replace the record they carry.
  `settings_changed`, `warnings_changed` and `profile_rules_changed` are applied to what the client
  has listed: settings are upserted by integration and key and rules by input, warnings are kept as
  a set, and removals of entries the client does not hold are ignored. A client lists again from
  the start when it needs a fresh view, such as in a new session or after it lost track of a list.
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

## CLI

`cordial --port PORT` opens the shell; `cordial --port PORT COMMAND ...` runs one command, and piped
input runs as a script. `cordial tui` opens the TUI. Without `--port`, the CLI uses the only attached
adapter and fails when there are several. Controller selection uses USB ports, not the host's
Bluetooth controllers.

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
| `device unpair DEV` | Delete the device's bond and settings. |
| `device refresh DEV` | Re-read a connected device's information and settings. |
| `warning list DEV` | The device's HID warnings. |
| `setting list DEV` / `setting get DEV KEY` | The device's settings, or one in detail. |
| `setting set DEV KEY VALUE` / `setting forget DEV KEY` | Save or forget a setting. |
| `feature list DEV` | Development firmware: the device's integration features. |
| `file list PATH` / `file get [--raw] PATH DEST` | Development firmware: browse or download adapter files. A saved record is converted to JSON unless `--raw` is given; see [downloaded records](#downloaded-records). |
| `help` / `help COMMAND` / `quit` / `exit` | Local help and clean shutdown. |

`DEV` is a device ID shown in output, or an unambiguous name; completion offers both. A number is
always taken as an ID, so a device whose name is a number is addressed by its ID. `CANDIDATE` is a
candidate ID shown in scan output, or an unambiguous name, with the same rule for numbers. Candidate
and device IDs are separate sequences; each command takes only one of them. The CLI never picks
silently between duplicate names. Setting keys are the catalog keys, such as `keyboard.fn_row` or
`pointer.sensor.0.dpi`.

- The shell keeps history and completion, and prints changes as they arrive without disturbing
  partly typed input, prefixed `[NEW]`, `[CHG]` or `[DEL]` with the device ID. It keeps accepting
  commands while a scan, pairing or connection runs.
- A pairing prompt asks for the passkey or PIN, or a yes/no comparison, directly. While a prompt is
  open, `/COMMAND` runs a normal command; `pair accept` and `pair reject` answer it explicitly.
  Saving the bond and the later connection result are reported separately.
- Ctrl-C, Ctrl-D, end of input, SIGINT and SIGTERM quit the shell, which closes the port: a
  running scan stops and an unsaved pairing is cancelled.
- After a configuration interface change that reconnects USB, the shell waits up to 15
  seconds for the same adapter, found by the adapter ID at the start of its USB serial number, and
  reopen it on whichever port it returns to. Only an adapter that doesn't return in time is reported
  as lost.
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
  15 seconds; its devices leave the list at once. The same adapter returning, found by the adapter
  ID that begins its USB serial number, keeps its page and tab. Disconnect, from the adapter's page or menu,
  closes its session; the adapter stays listed while plugged in and is opened again only by
  Connect.
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
  a failed save keeps them. Saving a change that reconnects USB asks for confirmation first.
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
  The web version lists the ports the user has granted by vendor and product ID. Ports that fail
  stay hidden. An unexpectedly disconnected adapter keeps its page and last reported settings for 15
  seconds while awaiting reconnection. Its devices leave the list immediately and configuration
  controls are disabled. Reconnecting the same adapter restores its selected tab; otherwise the
  adapter leaves the list when the wait expires. A session that ends after running normally is
  reopened once if its port is still present.
- Sessions: after `GetStatus` it reads every page of devices and each device's settings and
  warnings, then follows events, and lists the devices again when the adapter becomes ready.
- Disconnect: the user can disconnect an adapter from its context menu. The application closes the
  port and does not reopen it, recognizing it by the first 16 characters of its USB serial number,
  until the user chooses Connect or restarts the application. The web version recognizes it by its
  port instead.
- Pairing: Add Device scans the transports the firmware supports and has enabled while its dialog is
  open, stops the scan before pairing, answers prompts in the dialog, and follows the new device
  until it connects or fails. Closing the dialog, or hiding the window, stops a scan or cancels an
  unsaved pairing.
- Devices open on the Details tab, with the connection switches and the device's own facts. Settings
  follows when the device has settings to show, then Profiles when the adapter supports profiles,
  with the device's layers (an ordered list of profiles, empty for a new device), then Diagnostics:
  the device's warnings with the HID field or report each applies to, the HID++ protocol and status,
  link security and identifiers, with Refresh. Changes on Details, Settings and Profiles stay staged
  across tabs until that tab's Save sends its own changes in one request; Connect, Disconnect and
  Forget act at once. Warnings never appear as banners.
- Profile memory: the application notifies once when `memory_used` reaches 85% of `memory_budget`,
  and once when a connected device's `profile_error` shows its profiles were not loaded, and resets
  each after the condition clears. The device's Diagnostics tab shows a `profile_error`.
- Adapters open on Details, with Settings, Profiles when the adapter supports them, and Diagnostics.
  Profiles lists the profiles one page at a time, each with an icon for each role it changes, and
  the configuration interfaces, each with its own switch and profile. The profile picker starts at
  the first page and pages on its own. Changes on Settings and
  Profiles stay staged, across tabs and pages, until Save sends them together in one request; a
  failed save keeps them. Saving a change that reconnects USB asks for confirmation first. Creating,
  copying and deleting profiles take effect at once.
- Battery: a device's battery is low when a known `battery.level` is at or below the user's
  threshold (20% by default) and `battery.charging` is not true; at most 5% is critical. It notifies
  once on entering low and once on entering critical, and resets only after a reading above the
  threshold or while charging.
- Tray: a StatusNotifierItem icon is shown while at least one adapter is connected (or always, by
  preference). Its outline means no device is connected and filled means at least one is; a badge
  marks low battery or an adapter needing attention. Its menu lists devices, grouped by adapter when
  several are connected.

## Profile commands

The desktop application and TUI manage profiles, layers and configuration interfaces, and never read
or change a profile's rules. Remaps of keys, media and system controls, and mouse buttons are edited
with VIA or Vial.

These commands require `Status.profile_support`. A profile argument is its ID or a unique name; a
name is found by reading the profile pages. A number is always taken as an ID, so a profile whose
name is a number is addressed by its ID. LAYERS is one or more profile arguments in the order they
apply, quoted as needed, or `none` for an empty list; a profile named `none` is addressed by its ID.

| Command | Behavior |
| --- | --- |
| `profile list [--after ID]` | List every profile with its roles; with `--after`, list one page of profiles after that ID and the ID to pass as `--after` for the next page. |
| `profile show PROFILE` | Show a profile's name and roles. |
| `profile create NAME` | Create an empty profile. |
| `profile copy PROFILE NAME` | Copy on the adapter. |
| `profile delete PROFILE` | Delete an unused profile. |
| `adapter set interface INTERFACE on\|off [PROFILE]` | Enable or disable a configuration interface such as `via` or `vial`, optionally selecting its profile in the same request. |
| `adapter set interface INTERFACE profile PROFILE` | Select the profile a configuration interface edits. |
| `adapter reset interface INTERFACE profile` | Clear a disabled interface's profile. |
| `device set DEV profiles LAYERS` | Set a device's layers. |

### Advanced: profile rules

These commands read and change a profile's raw HID rules. They are meant for testing and for
people who understand HID usages; they do not prevent rules that make a keyboard or mouse hard to
use, and a mistake is undone only by forgetting the rule or deleting the profile.

| Command | Behavior |
| --- | --- |
| `profile rule list PROFILE` | Show every rule. |
| `profile rule remap PROFILE INPUT OUTPUTS` | Remap an on/off input to the outputs, held together. |
| `profile rule scale PROFILE INPUT N/D` | Multiply a value input by N/D; a negative N inverts it. |
| `profile rule forget PROFILE INPUT` | Forget a rule so the input passes through unchanged. |

A USAGE is `PAGE:USAGE` in hexadecimal, such as `07:39` for Caps Lock or `01:38` for the wheel. An
INPUT is a USAGE. OUTPUTS is `disabled` or a comma-separated list of USAGEs, each optionally
followed by `@PAGE:USAGE` naming the report's collection.

See [Input profiles](../input-profiles.md) for external editors and hardware checks.
