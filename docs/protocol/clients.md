# CLI and TUI behavior

[Protocol index](README.md)

The shell, one-shot CLI and TUI share one client, capability discovery and serial
session. `cordial --port PORT` opens the shell; `cordial --port PORT COMMAND ...`
runs one command. `cordial tui` opens the TUI and enumerates adapters. It connects
automatically only when its initial enumeration finds exactly one adapter.
An explicit `--port` takes precedence. Subsequent failures or disconnects return
to the chooser. The host has no BlueZ or background-service dependency.

CLI commands use the wire namespaces as space-separated words. Storage uses
`ls` and `get` to distinguish browsing/downloading from raw wire operations.

| CLI command | Behavior |
| --- | --- |
| `adapter list` / `adapter select PORT` | Enumerate or switch USB adapters locally. |
| `adapter status` / `adapter capabilities` | Inspect status or capability enum values. |
| `adapter name set NAME` | Save the adapter name. Quote names containing spaces. |
| `adapter name reset` | Clear the saved name and use the image default. |
| `adapter platform set linux\|windows\|mac` | Save adapter platform preference. |
| `adapter bootloader enter` | Enter programming mode when `debug` is advertised. |
| `discovery scan on\|le\|bredr` / `discovery scan off` | Start discovery or cancel the session's scan. |
| `device list [Saved\|Paired\|Enabled\|Connected\|Trusted]` | Show saved records and discovered candidates, or filter records. |
| `device get DEV` | Inspect a saved device or cached discovery candidate. |
| `pairing start CANDIDATE` | Pair a Nearby candidate, then connect if enabled. |
| `pairing reply ID PROMPT accept [VALUE]` / `pairing reply ID PROMPT reject` | Answer authentication. |
| `device connect DEV` / `device disconnect DEV` | Connect or disconnect a saved device. |
| `device enabled set DEV on\|off` | Change preferred Bluetooth enablement. |
| `device trusted set DEV on\|off` | Change unattended-connection permission. |
| `device blocked set DEV on\|off` | Deny or allow connections. |
| `device hidpp set DEV on\|off` | Change the saved HID++ preference. |
| `device unpair DEV` | Remove a saved bond or hide a cached Nearby row. |
| `hidpp feature list DEV` | List discovered features. |
| `hidpp setting list DEV` / `hidpp setting get DEV KEY` | Inspect settings. |
| `hidpp setting set DEV KEY VALUE` / `hidpp setting forget DEV KEY` | Save or forget a preference. |
| `hidpp setting refresh DEV` / `hidpp setting apply DEV` | Read values or apply preferences. |
| `session monitor set on\|off` | Toggle live notifications for this session. |
| `request cancel ID` | Cancel a pending operation. |
| `storage ls PATH` | Stream one directory listing. |
| `storage get PATH DEST` | Download to a new host file, publishing it only after a validated completion. |
| `help` / `help COMMAND` / `quit` / `exit` | Local help and clean shutdown. |

The TUI offers Files when `storage_management` is advertised. It shows the current
path, directory entries, sizes and listing progress. Enter or click opens a
directory; Parent returns one level. Download asks for a host destination and
explicit confirmation before replacing a file. Transfers show progress and can
be cancelled. Errors, cancellation and disconnects remove temporary downloads
and leave an existing destination unchanged. Lists and reads stream over the wire;
file bytes are never accumulated in host memory. Directory rows use host memory,
with no new arbitrary item limit. New files use native atomic no-replace rename
on Linux, macOS and Windows. Unix falls back to a hard link only when the native
operation is unsupported; if neither is available, the download fails safely.
A stalled consumer can exhaust the bounded transfer queue. That transfer fails
and is cancelled while the serial reader continues handling control responses
and heartbeats. Firmware stream chunks leave two output slots for control traffic.

Files remain available while Bluetooth is unavailable. Device readiness and
filesystem readiness are separate from the capabilities that control menus.

`DEV` is an opaque candidate/device ID shown in output, with completion for those IDs and unambiguous names. Do not silently select between duplicate names, transports, or devices. Use IDs for scriptable selection and BLE privacy; raw Bluetooth addresses are not required as public identifiers. Controller selection uses USB ports, not the host's Bluetooth controllers. Filter names such as `Paired` and `Connected` are case-insensitive.

This is the HID-management subset of the reference workflow. Pairing authentication is handled directly by the CLI without requiring `agent` or `default-agent` setup. The command set covers discovery, bonding, device information, connection policy, and live state changes; general Bluetooth profile and radio-debugging menus are outside the proxy's scope.

## Interaction and scripting

- Provide command history, command/device completion, and asynchronous status lines that preserve partially typed input. Prefix changes clearly, for example `[NEW]`, `[CHG]`, and `[DEL]`, and include the device ID.
- Show the request ID for commands still pending. Keep accepting commands while scan, pair, or connect runs. An interactive `pairing start` reports bond creation separately from the connection result; failure to connect must not be presented as failure to save the bond.
- Present authentication as direct passkey/PIN entry or a yes/no comparison prompt. While that prompt is active, `/COMMAND` runs a normal command without treating it as an authentication answer. Provide `pairing reply REQUEST_ID PROMPT_ID accept [VALUE]` and `pairing reply REQUEST_ID PROMPT_ID reject` as explicit equivalents for scripting or selecting a prompt. Heartbeats and event dispatch continue throughout.
- Ctrl-C clears partial input or rejects the active authentication prompt; when waiting for a foreground cancellable operation, it cancels that operation. An independently running scan remains active until `discovery scan off`, cancellation by ID, or exit. At an idle prompt, Ctrl-C leaves the CLI open. Ctrl-D/EOF follows orderly exit.
- A one-shot `pairing start` performs discovery and selection by Nearby name within that invocation because candidate IDs are session-local. Every Nearby entry offers Pair; discovery never associates it with a saved device. Saved entries have no pairing action. One-shot pairing stays attached for authentication and the subsequent connection result.
- A one-shot `discovery scan on|le|bredr` collects results for `--timeout SECONDS` (10 by default), then stops and exits; interactive scans continue until stopped. One-shot commands with other pending work wait for their terminal result, honor an optional overall `--timeout`, and return a nonzero exit status on failure. Timeout/interrupt cleanup does not roll back a bond already saved.
- Accept commands on standard input for scripting. Dispatch dependent commands sequentially after each command's result, except `discovery scan on|le|bredr`, which returns after dispatch and continues in the session. While pairing awaits authentication, consume the next input line as its answer or explicit `pairing reply`; do not wait for pairing's terminal response before reading that answer. Missing or invalid answers fail clearly. Plain device names are accepted only when unambiguous. EOF follows orderly exit and does not leave discovery running.
- Default CLI output is human-readable for the interactive shell, one-shot commands and piped commands. Commands format their own results and failures once; successful internal startup, heartbeat and refresh responses remain silent. Background discovery completion and device/pairing/setting notifications remain visible. Never fall back to displaying a raw JSON payload. Provide explicit `--json` output that preserves response/event envelopes and IDs, including internal responses. Human output uses safe terminal rendering, and JSON escapes control characters.
- Reading and dispatching serial messages, renewing client presence, and redrawing notifications continue while the user types or considers a prompt. All of this work runs inside the foreground CLI process, with no separate service.

Example interactive workflow:

```text
$ cordial --port PORT
[cordial]# adapter status
[cordial]# discovery scan on
[NEW] c_12 Example keyboard (BLE)
[cordial]# pairing start c_12
Enter passkey for c_12: 123456
[NEW] d_7 Example keyboard: Paired yes, Trusted yes
[CHG] d_7 Connected: yes
[cordial]# discovery scan off
[cordial]# device list Paired
d_7 Example keyboard
[cordial]# device get d_7
[cordial]# quit
```

The dongle continues forwarding that keyboard's input and reconnecting it after the CLI exits. CLI exit, heartbeat loss, and monitoring state are independent of the saved HID setup.

## Full-screen TUI

The TUI uses Ratatui and Crossterm and the same version 1 commands as the CLI. It does not add wire commands or start a second process to control the dongle.

Every TUI action must be usable with the mouse once the application is open. Keyboard input may be required for text, such as a pairing code, but selecting controls, submitting input, confirming, cancelling, and quitting must not require keyboard shortcuts.

- Show the selected adapter and its availability, a device list, selected-device details, and a bounded event history. Distinguish candidates from saved bonds and show connected, disabled, inactive, unsupported-transport, invalid-record, trusted, and blocked state with text labels. Adapter settings show saved, paired, preferred and effectively enabled counts, each enabled-capacity constraint, and per-transport pairing availability with its advisory estimate.
- In adapter settings, show the device-reported name and provide a rename dialog. Save through `adapter.name.set`, preserving the draft on failure.
- In adapter settings, provide clickable Linux/Windows/Mac platform choices, usable with no selected or paired device. For saved devices, show HID++ enabled preference, runtime status, and any error, with clickable on/off even while disconnected. Do not expose a per-device platform setting. A saved preference and active normalization are distinct states.
- Provide visible clickable controls for adapter/device selection, discovery, pair/remove, connect/disconnect, enable/disable, trust/untrust, block/unblock, monitoring, help, and quit. When status reports pairing unavailable for a candidate's transport, the TUI withholds Pair and shows the reason; firmware still enforces admission. Keep keyboard equivalents. Use the same semantics as the command shell, including pairing followed by connection. Keep selected rows identified by device/candidate ID as updates arrive.
- For development firmware, include the clickable [bootloader action](development.md#development-bootloader-entry). Production firmware must not expose that action or accept its wire command.
- Support wheel scrolling and clickable scrolling controls for device lists, details, and event history. Any tabs, filters, menus, or dialogs must be navigable and dismissible by mouse. Actions must not depend on hover, double-clicks, or undisclosed keyboard shortcuts.
- Mouse movement and Tab share a single control highlight. Hovering a visible control replaces the highlighted control; moving off controls clears it. Hover alone never activates an action. Click or Enter activates the highlighted control through the existing input rules.
- Show passkey/PIN entry, displayed codes, and numeric confirmation in an authentication dialog with clickable input focus, submit/accept, reject, and cancel controls as appropriate. Opening a dialog must not pause serial dispatch, heartbeats, or updates to other devices. Expired prompts must no longer accept answers. A click in a dialog must not activate controls underneath it.
- Enable monitoring and build the initial view using the [snapshot procedure](monitoring.md#monitoring-and-multiplexing). Apply notifications as they arrive, refresh after loss, and mark cached state as unavailable when the serial session ends. Redisplay current state after reconnecting rather than retrying mutations automatically.
- Handle resizing and preserve partially entered form data. Recompute click targets from the displayed layout after scrolling, resizing, and live list updates. Provide visible help and text labels so color is not the only indication of state. Keep UI rendering separate from structured CLI output; reject `tui` with `--json` or without an interactive terminal.
- Enable Crossterm mouse reporting and handle movement, clicks, releases, and wheel events for every applicable control.
- Ctrl+C and OS SIGINT quit the entire TUI, including from a dialog or active operation. Perform the same bounded monitor/scan/pair cleanup as Quit, close the serial port, and restore terminal modes and mouse reporting. A dialog's Cancel control only cancels or rejects that interaction; quitting the TUI does not disconnect saved peripherals. A crashed TUI is covered by the same heartbeat expiry.

Exact TUI layout, key bindings, supported host release targets, and dependency versions remain implementation choices. The wire protocol, command semantics, and [single-owner interactive model](README.md) are the version 1 design.

## Desktop application

The desktop application uses the same version 1 commands and session rules as
the CLI. It adds no wire commands and needs no firmware support beyond this
protocol.

- Discovery: it lists serial ports once at startup, then again only after USB
  hotplug events (with short retries while udev finishes setting up a new
  node) or an explicit refresh. Ports must match the
  [USB discovery descriptors](transport.md). A matching port is only a candidate; an
  adapter is shown after discovery reports protocol `1` and the client receives
  capabilities and a status with an `adapter_id` on the new session. The port's
  USB serial number must equal that `adapter_id`. The web version can read neither
  the serial number nor the manufacturer string: it lists the ports the user has
  granted by USB vendor and product ID and relies on the handshake alone. Ports that fail are logged and
  stay hidden. A removed adapter, including one whose port fails,
  disappears with its devices without an error. A session that ends after
  running normally is reopened once if its port is still present.
- Sessions: after the handshake it starts heartbeats, waits for readiness,
  enables monitoring, builds the device view by the [snapshot procedure](monitoring.md#monitoring-and-multiplexing),
  reads `device.info` for each saved device, and follows events. A heartbeat
  reporting `monitor:false` (for example after suspend) re-enables monitoring
  and resynchronizes. Settings lists are read only for the device shown in the
  window; a `busy` read is retried and other failures are shown. Status is
  refreshed shortly after device events and mutations, and every status must
  keep the session's identity and pass the consistency checks the CLI applies.
- Disconnect: the user can disconnect an adapter from its context menu. The
  application closes that session in the [orderly shutdown procedure](transport.md#client-heartbeat-and-exit) and does
  not open the port again, recognizing it by USB serial number, until the user
  chooses Connect or restarts the application. The web version recognizes it by
  its port instead; after the adapter is plugged in again, it opens the new port
  once to identify it and then closes it.
- Discovery and pairing: Add Device runs `discovery.scan` with `duration_ms:0`
  while its dialog is open, using both transports only when both are
  advertised. It cancels the scan before `pairing.start`, answers prompts in
  the dialog, and follows an effectively enabled result with `device.connect`,
  like the CLI. Closing the dialog, or hiding the window, cancels a scan or
  uncommitted pairing; a new scan starts only after the previous one ends.
- Battery: a device's battery is low when a fresh `battery_percent` is at or
  below the user's threshold (default 20%) and `battery_charging` is not
  `true`; unknown values never count. At most 5% is critical. It notifies
  once on entering low and once on entering critical; the alert resets only
  after a reading above the threshold or while charging.
- Tray: a StatusNotifierItem icon is shown while at least one adapter is
  connected (or always, by preference). Its outline means no device is
  connected; filled means at least one is; a badge marks low battery or an
  adapter needing attention (not ready, or a device needing pairing again).
  Its menu lists devices, grouped by adapter when several are connected.
