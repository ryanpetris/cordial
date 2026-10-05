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
| `file list PATH` / `file get PATH DEST` | Development firmware: browse or download adapter files. |
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
- After a configuration interface change that reconnects USB, the shell and TUI wait up to 15
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
  response and event as one line of protobuf JSON with the schema's field names.

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

## TUI

The TUI uses Ratatui and Crossterm and the same commands as the CLI. It connects automatically when
it finds exactly one adapter, or to `--port`; otherwise it shows a chooser, and returns to it when
the adapter goes away.

- Every action works with the mouse: selection, discovery, pairing and its prompts,
  connect/disconnect, enable, trust and block, settings, help and quit. Keyboard input is needed
  only for text such as a passkey, and every control also has a keyboard equivalent. Mouse movement
  and Tab share one highlight; hover alone never activates anything, and a click in a dialog never
  reaches the controls underneath it.
- The view is built from the first full listing and then from events, and is marked unavailable when
  the session ends. After reconnecting it shows the current state and retries nothing.
- The Adapters pane below the device list shows the connected adapter with Disconnect, Choose
  Adapter, Refresh and, on development firmware, Files and the bootloader action.
  Selecting the adapter shows its page in place of a device's details: the name, with a rename
  dialog that keeps the draft on failure, the Linux, Windows and macOS platform choices, an On and
  Off choice for each transport the firmware supports, and the use of each transport's
  enabled-device places. On an adapter with profiles, its page leads to its Profiles view, which
  shows the profile memory in use, the configuration interfaces, and one page of profiles at a time,
  each with labels for its roles.
- A saved device shows its Logitech Features switch even while disconnected. On an adapter with
  profiles, a device that reports a layer list has a Profiles view that orders, adds and removes
  its layers. Its Diagnostics dialog lists its warnings, why its profiles aren't loaded, HID++
  protocol and status, link security and identifiers, and refreshes a connected device's
  information. New warnings are not written to the activity log.
- Settings are edited in place; Save sends every change in one request, and apply results arrive as
  they happen.
- Choices that change the adapter or a saved device stay staged, marked as changed, until Save:
  adapter settings and configuration interface choices go out together in one adapter request; a
  device's enable, trust, block and Logitech Features in one device request from its details, and
  its layers in another from its Profiles view. Ctrl-S presses the Save shown on screen; plain `s`
  never saves. Discard drops the staged changes the shown Save would send, and a failed save keeps
  them. Saving a change that reconnects USB, such as enabling a configuration interface, asks for
  confirmation first. The profile chooser pages on its own, apart from the Profiles view. Connect,
  disconnect, rename, removal and creating, copying and deleting profiles act at once.
- Development firmware adds the bootloader action and Files, which browses adapter directories and
  downloads a file to a host destination, asking before replacing one. A download is written to a
  temporary file and moved into place only when complete, with a no-replace rename, so a failure
  leaves the destination unchanged. Files works while Bluetooth is unavailable.
- Resizing keeps form input and recomputes click targets. Text labels accompany every color. `tui`
  is refused with `--json` or without an interactive terminal. Ctrl-C and SIGINT quit from anywhere,
  stop a scan, cancel an unsaved pairing, close the port and restore the terminal; quitting never
  disconnects saved devices.

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
- Devices open on the Details tab, with the connection switches, the device's layers (an ordered
  list of profiles, empty for a new device) and the device's own facts. Those changes stay staged
  until Save sends them in one request, like the Settings tab's; Connect, Disconnect and Forget act
  at once. Settings follows when the device has settings to show, then Diagnostics: the device's
  warnings with the HID field or report each applies to, the HID++ protocol and status, link
  security and identifiers, with Refresh. Warnings never appear as banners.
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
