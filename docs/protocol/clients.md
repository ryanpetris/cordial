# CLI, TUI and desktop behavior

[Protocol index](README.md)

The CLI, TUI and desktop application are built on the client libraries and add no commands of
their own. None of them runs a background service; the Dongle keeps forwarding input, reconnecting
devices and applying saved settings after they exit.

## CLI

`cordial --port PORT` opens the shell; `cordial --port PORT COMMAND ...` runs one command, and piped
input runs as a script. `cordial tui` opens the TUI. Without `--port`, the CLI uses the only attached
adapter and fails when there are several. Controller selection uses USB ports, not the host's
Bluetooth controllers.

| CLI command | Behavior |
| --- | --- |
| `adapter list` / `adapter select PORT` | List attached adapters, or switch to one. |
| `adapter status` | Identity, readiness, platform and per-transport limits. |
| `adapter set name NAME` / `adapter reset name` | Save the adapter name, or restore the firmware default. Quote names containing spaces. |
| `adapter set platform linux\|windows\|mac` | Save the host platform. |
| `adapter bootloader` | Development firmware: reboot into programming mode. |
| `scan start [classic\|ble] [SECONDS]` / `scan stop` | Discover devices on both transports or one, for 10 seconds by default. |
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

`DEV` is a device or candidate ID shown in output, or an unambiguous name; completion offers both.
The CLI never picks silently between duplicate names. Setting keys are the catalog keys, such as
`keyboard.fn_row` or `pointer.sensor.0.dpi`.

- The shell keeps history and completion, and prints changes as they arrive without disturbing
  partly typed input, prefixed `[NEW]`, `[CHG]` or `[DEL]` with the device ID. It keeps accepting
  commands while a scan, pairing or connection runs.
- A pairing prompt asks for the passkey or PIN, or a yes/no comparison, directly. While a prompt is
  open, `/COMMAND` runs a normal command; `pair accept` and `pair reject` answer it explicitly.
  Saving the bond and the later connection result are reported separately.
- Ctrl-C, Ctrl-D, end of input, SIGINT and SIGTERM quit the shell, which closes the port: a
  running scan stops and an unsaved pairing is cancelled.
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
[NEW] c_12 Example keyboard (BLE)
[cordial]# pair start c_12
[NEW] d_7 Example keyboard
[CHG] d_7 Connected: yes
[cordial]# scan stop
[cordial]# device get d_7
[cordial]# quit
```

## TUI

The TUI uses Ratatui and Crossterm and the same commands as the CLI. It connects automatically when
it finds exactly one adapter, or to `--port`; otherwise it shows a chooser, and returns to it when
the adapter goes away.

- Every action works with the mouse: selection, discovery, pairing and its prompts,
  connect/disconnect, enable, trust and block, settings, help and quit. Keyboard input is needed
  only for text such as a passkey, and every control also has a keyboard equivalent. Mouse
  movement and Tab share one highlight; hover alone never activates anything, and a click in a
  dialog never reaches the controls underneath it.
- The view is built from the first full listing and then from events, and is marked unavailable
  when the session ends. After reconnecting it shows the current state and retries nothing.
- Adapter settings hold the name, with a rename dialog that keeps the draft on failure, and the
  Linux, Windows and macOS platform choices. A saved device shows its Logitech Features switch even
  while disconnected. Its Diagnostics dialog lists its warnings, HID++ protocol and status, link
  security and identifiers, and refreshes a connected device's information. New warnings are not
  written to the activity log.
- Settings are edited in place; Save sends every change in one request, and apply results arrive
  as they happen.
- Development firmware adds the bootloader action and Files, which browses adapter directories and
  downloads a file to a host destination, asking before replacing one. A download is written to a
  temporary file and moved into place only when complete, with a no-replace rename, so a failure
  leaves the destination unchanged. Files works while Bluetooth is unavailable.
- Resizing keeps form input and recomputes click targets. Text labels accompany every color. `tui`
  is refused with `--json` or without an interactive terminal. Ctrl-C and SIGINT quit from
  anywhere, stop a scan, cancel an unsaved pairing, close the port and restore the terminal;
  quitting never disconnects saved devices.

## Desktop application

The desktop application uses the same commands and session rules as the CLI.

- Discovery: it lists serial ports once at startup, then again after USB hotplug events (with short
  retries while udev sets up a new node) or an explicit refresh. A port must match the
  [USB discovery descriptors](transport.md#discovery), and its serial number must equal the adapter
  ID in the first `Status`. The web version can read neither the serial number nor the manufacturer
  string, so it lists the ports the user has granted by vendor and product ID and relies on
  `GetStatus` alone. Ports that fail stay hidden. A removed adapter disappears with its devices
  without an error, and a session that ends after running normally is reopened once if its port is
  still present.
- Sessions: after `GetStatus` it lists the devices and each device's settings and warnings, then
  follows events, and lists the devices again when the adapter becomes ready.
- Disconnect: the user can disconnect an adapter from its context menu. The application closes the
  port and does not reopen it, recognizing it by USB serial number, until the user chooses Connect
  or restarts the application. The web version recognizes it by its port instead.
- Pairing: Add Device scans both transports the firmware supports while its dialog is open, stops
  the scan before pairing, answers prompts in the dialog, and follows the new device until it
  connects or fails. Closing the dialog, or hiding the window, stops a scan or cancels an unsaved
  pairing.
- Devices open on the Details tab, with the connection switches and the device's own facts.
  Settings follows when the device has settings to show, then Diagnostics: the device's
  warnings with the HID field or report each applies to, the HID++ protocol and status, link
  security and identifiers, with Refresh. Warnings never appear as banners.
- Battery: a device's battery is low when a known `battery.level` is at or below the user's
  threshold (20% by default) and `battery.charging` is not true; at most 5% is critical. It notifies
  once on entering low and once on entering critical, and resets only after a reading above the
  threshold or while charging.
- Tray: a StatusNotifierItem icon is shown while at least one adapter is connected (or always, by
  preference). Its outline means no device is connected and filled means at least one is; a badge
  marks low battery or an adapter needing attention. Its menu lists devices, grouped by adapter
  when several are connected.
