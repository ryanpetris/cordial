# CLI, TUI, and desktop behavior

[Protocol index](README.md)

The CLI, TUI, and desktop application are built on the client libraries and add no commands of
their own. None of them runs a background service; the Dongle keeps forwarding input, reconnecting
devices, and applying saved settings after they exit.

## Client state

All three keep their view of the Dongle the same way:

- Listing: a client sends one request for the complete list. It waits for the frame delimiter,
  decodes the merged protobuf response, and applies the complete collection or error in receive
  order. It discards incomplete frames on disconnect. See [Listings](compatibility.md#listings).
- Events: `device`, `adapter`, and `profile` events replace the record they carry.
  `settings_changed` is applied to what the client has listed: whole domain objects are replaced by integration, group,
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

The Rust client preserves full `Error` messages in responses and events, including reason,
outcome, and related errors recursively. Response observers, state updates, and command callers
receive the original protocol messages in both debug and release builds. A diagnostic sink
installed with `Connection::set_diagnostics` before the handshake also receives wire responses
and events on the serial reader thread. The sink must return without waiting for output.

The application queues JSON protocol records and foreground diagnostics for standard error.
Delivery uses a 256-entry queue and drops excess entries when the queue is full. Terminal
interfaces defer terminal-bound output until they restore terminal ownership. Shutdown drains
accepted output with a two-second deadline. Closing a connection does not wait for output.

A completed save confirms saved preferences, not a Bluetooth connection or application of a
setting. Clients retain the saved values while displaying the error. Device and setting records
determine connection and application state. The primary outcome describes the requested
operation. Related failures retain their own scope, such as cleanup after pairing, and cannot
invalidate a confirmed bond save. Read errors and record errors retain their original details;
clients never construct missing records or identifiers. Unknown, partial, no-effect, missing,
and unrecognized outcomes never authorize an automatic retry.

Human-readable error feedback leads with the known outcome in the operation's scope: a saved
change, an applied change, pairing, a read, or pairing cleanup. A confirmed save
with a Bluetooth error reports both without claiming the device connected or applied the change.
Unknown and partial saves ask the user to check the current settings before trying again.
Specific causes and related failures remain in that feedback. See
[Errors](limits-and-errors.md#errors).

A device listing's `RecordError` retains any previously known device as last-known data. It does
not confirm its connection or preferences. Fresh records and events replace that data; actual
absence from a completed list or a removal event removes it. A profile read-error placeholder does
not require retaining its cached record. A listing continuation failure instead rejects the
entire partial list and leaves the previous collection intact.

`ScanDone` reports retained candidate count and an optional full error. Scan errors leave those
candidates usable until a new scan or session end, subject to adapter readiness for pairing.
Clients distinguish a scan failure from an ordinary timeout and preserve candidate-loss details
related to a primary radio error.

### Intentional disconnects

For ordinary client sessions, the
[intentional session ending](transport.md#intentional-session-endings) event alone authorizes
reconnect waiting. Clients handle `SessionEnding` at any time in an established session,
including while idle and when autonomous USB allocation changes have no associated request.
Its reason describes the controller's trigger and is not displayed. An unspecified or unknown
future reason still authorizes a bounded wait for the current known adapter. `BOOTLOADER` uses the same bounded wait.
Neither a sent command, a successful response, nor recent interaction substitutes for the event.
Without it, a lost adapter and its devices leave the view immediately, including with older
firmware that sends no event. There is no interaction, capability, or version fallback.

A received event arms only its receiving session and starts a 15-second absolute deadline.
Repeated events within that session and reconnect attempts do not extend it. An event in a new
established session starts that session's own deadline. A valid `AdapterHello` establishes a
replacement session but does not confirm identity. Reconnect waiting ends when `GetStatus` matches
the known adapter's ID; the original deadline still bounds retained entries and pages during
reload. A replacement session that fails without its own event never inherits the old session's
authorization, even before identity is confirmed; the adapter and its devices leave the view
immediately. Failed attempts to reopen a port before a replacement session is established may be
retried within the original deadline. Explicit disconnect, adapter selection, or quitting cancels
the wait. Timeout ends retention of old data and removes unconfirmed retained device records and
their pages. An adapter whose replacement connection and `GetStatus` have succeeded remains
listed, including when its current status reports a fault.
An adapter still awaiting reconnection or identification is removed. A client unable to identify
the same adapter does not retain it for reconnect.

The storage migration worker has its own reconnect-until-cancelled contract in
`proto/migration.proto`. The event gate and 15-second deadline govern ordinary client retention
only and do not limit that worker.

The event never completes a pending command or confirms a saved value. An interrupted write
keeps its unknown outcome until readback in a new session and is never replayed automatically.

## Client library listings

Rust `Connection` methods `list_devices()`, `list_profiles()`, `list_settings(device)`,
`list_profile_rules(profile)`, `list_features(device)`, and `list_files(path)` return the complete
protobuf response messages. The collections are `entries`, `objects`, `rules`, or `features`, as defined by each response.
No method takes a cursor. The `all_*` conveniences return the response collection
from a single request. Rust identity and ordering helpers live in `cordial_client::order`.

Clients assemble responses on the host and preserve request FIFO and response/event ordering.
Request timeouts measure incoming-byte inactivity so a progressing response can outlast the timeout.
Every complete outgoing `Request` must fit 1024 protobuf bytes, including all entries and the
command envelope. `SetProfileRules` is atomic per request, with no automatic splitting or atomicity
across several requests.

The Rust client library sends the first request alone. Other callers wait until a `Response`
answers it, including `Response.error` or a response with no result. It then allows pipelining and
matches responses to requests in write order. If sending or waiting for the opening response fails,
including a timeout or connection close, waiting callers fail without writing their requests.

The Rust client library, native desktop, CLI, and TUI enforce a local 32 MiB decoded receive limit on each complete
protobuf `Message`, including nested envelopes and all repeated listing batches. See
[Rust host receive limit](transport.md#rust-host-receive-limit) for overflow and session behavior.

## Protocol selection

Every client connection starts with the permanent hello. Clients send no ordinary requests until
a valid `AdapterHello` names a revision they implement. Firmware doesn't require the hello and
never learns the client's revision, so this check is the client's alone. A current adapter
connects directly. A newer revision requires an application upgrade. Firmware without the
permanent hello is unsupported.

The native GTK desktop, CLI, and TUI select compatibility inside the Rust process. An older
revision requires an implemented converter to the current schema. Revision `1` has no earlier
supported revision and no historical converter.

## Storage migration

After a compatible hello, clients read bootstrap status and check the exact
`Status.information.target_storage_format` before any other command. A missing,
zero, or unsupported target allows nothing else. The committed
`storage_format` is shown but never decides whether to migrate. Clients don't
infer migration success from readiness alone, an empty command response, or a
subprocess exit. While firmware runs an on-device migration, clients only
bootstrap, wait, or close.

Host assistance starts automatically only when `Status.errors` holds
`ERROR_CODE_STORAGE_MIGRATION_REQUIRED` with reason `STORAGE`, outcome `NO_EFFECT`, and no
related errors, and holds nothing else. Any other error, including a radio
error, blocks it: clients show the first such error and allow only status and
`Restart`. Without the marker, clients use the adapter as usual and never
migrate or restart it on their own.

The host downloads all dataset files into RAM, determines the source from
`/format.pb` and validated mixed files, and chooses a supported forward graph
path before writing anything. A dataset already in the target format needs no
conversion. The host verifies each uploaded file and writes the root last for
each checkpoint, then sends `Restart`. See
[preserving migration](../storage-format.md#preserving-migration) for readback
checks, limits, and temporary replacement files.

`Restart` is also an ordinary command. The CLI's `adapter restart` and the TUI's
Restart button on the adapter's Diagnostics tab send it whenever the target is
supported, without waiting for readiness or migration. The adapter refuses it with
`ERROR_CODE_BUSY` while a pairing or setup link is open.

The native desktop, CLI, and TUI run assisted conversion through their in-process Rust
coordinator. Before starting it, they suspend discovery and reconnects and close adapter sessions.
The migration worker reconnects indefinitely by stable `Status.id`. A port is only a
discovery hint and never authorizes using another adapter.
A response lost during a write requires readback after reconnect before retry.

[`proto/migration.proto`](../../proto/migration.proto) defines migration-worker subprocess IPC.
Each stdin `MigrationControl` and stdout `MigrationEvent` has a four-byte
unsigned little-endian length prefix followed by 1..65,536 protobuf bytes.
Receivers validate the length before allocation or decoding. Zero, oversized,
malformed, or truncated frames fail closed; pipe write boundaries have no
meaning. Stdout contains only framed events, and diagnostics use stderr. There
is no IPC version counter.

The parent sends exactly one start, naming the stable adapter ID and an optional
port hint. Events report progress, waiting for reconnect, committed root
checkpoints, failure, verified success, or explicit cancellation. An intermediate
checkpoint event reports a root upload and readback, without completing migration
or restarting the adapter. Success requires reconnecting to the same ID and
verifying the unchanged firmware target, `Status.ready = true`, and the absence of
`ERROR_CODE_STORAGE_MIGRATION_REQUIRED`.

Explicit cancellation stops file commands, closes the adapter, and exits. A partial
temporary file may remain for cleanup on resume.

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
| `migrate` / `migrate cancel` | Migrate the selected adapter's saved data, or cancel an active migration in the shell. |
| `cordial migrate --ipc` | Run the migration worker using framed protobuf on stdin/stdout. Exit 0 means verified success; cancellation and failure exit 1. |
| `adapter status` | Identity, readiness, platform, storage format and usage, and each supported transport with whether it is enabled and its limits. Device counts appear once the saved devices are listed. |
| `adapter set name NAME` / `adapter reset name` | Save the adapter name, or restore the firmware default. Quote names containing spaces. |
| `adapter set platform linux\|windows\|mac` | Save the host platform. |
| `adapter set transport classic\|ble enabled on\|off` | Enable or disable a transport. Lists only the transports the firmware supports. |
| `adapter restart` | Restart the adapter. Works while it is unready or needs migration. |
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
| `file put LOCAL_FILE PATH` | Development firmware: upload a local file's bytes, replacing the adapter file and verifying the result. |
| `file delete PATH` | Development firmware: delete an adapter file. Deleting a missing file succeeds. |
| `help` / `help COMMAND` / `quit` / `exit` | Local help and clean shutdown. |

`DEV` is a device ID shown in output, or an unambiguous name; completion offers both. A number is
always taken as an ID, so a device whose name is a number is addressed by its ID. `CANDIDATE` is a
candidate ID shown in scan output, or an unambiguous name, with the same rule for numbers. Candidate
and device IDs are separate sequences; each command takes only one of them. The CLI never picks
silently between duplicate names. Setting paths name typed fields, such as `keyboard.function_row` or
`pointer_sensor.0.dpi`; these paths are CLI labels, not serial fields.

- The shell keeps history and completion, and prints changes as they arrive without disturbing
  partly typed input, prefixed `[NEW]`, `[CHG]`, or `[DEL]` with the device ID. It keeps accepting
  commands while a scan, pairing, or connection runs.
- A pairing prompt asks for the passkey or PIN, or a yes/no comparison, directly. While a prompt is
  open, `/COMMAND` runs a normal command; `pair accept` and `pair reject` answer it explicitly.
  Saving the bond and the later connection result are reported separately.
- Ctrl-C, Ctrl-D, end of input, SIGINT, and SIGTERM quit the shell, which closes the port: a
  running scan stops and an unsaved pairing is cancelled.
- After a received `SessionEnding`, the shell waits within the
  [intentional-disconnect deadline](#intentional-disconnects) for the same adapter, found by the
  adapter ID at the start of its USB serial number. It reopens the adapter on whichever port it
  returns to and reads its status and devices again. Only an adapter that doesn't return in time is
  reported as lost. Without the event, a dropped adapter is reported as lost immediately.
  Commands cut short by the drop are not sent again. Each reports its result once,
  even after the adapter is reopened, and a save cut off before the adapter answered reports that
  the adapter couldn't confirm whether the change was saved. `adapter select` or quitting ends the
  wait, and `adapter select` replaces a reopen already in progress.
- One-shot commands and scripts run each command after the previous one's result and exit nonzero
  on failure. `--timeout SECONDS` bounds a one-shot command and sets the length of a one-shot scan.
  A one-shot `pair start` finds the candidate by name within the same invocation, since candidate
  IDs last only for the session. A timeout never undoes a bond already saved.
- Required assisted migration runs automatically before ordinary commands. The shell and scripts
  close their sessions before the migration worker starts. Migration reconnects have no deadline;
  time spent migrating does not consume a one-shot command's timeout. Ctrl-C cancels migration and
  waits for the worker to release the adapter.
- Before sending, the CLI refuses what the Dongle would refuse, using what it already knows (see
  [Client pre-checks](limits-and-errors.md#client-pre-checks)); when the Dongle refuses anyway, the
  error is shown and nothing is retried.
  An unready adapter without a validated target storage format accepts only status requests.
  Debug file access and bootloader commands remain available during a radio failure when the
  adapter reports its validated storage format.
- Output is human-readable and rendered safely for the terminal. `--json` also logs every original
  wire response and event to standard error as one line of protobuf JSON with the schema's field
  names. It includes background reads, migration traffic, and complete file contents. The stream
  retains original error trees. Events injected by handlers are not protocol records.
  Commands, piped scripts, the interactive shell, the TUI, and the desktop app support it. Human command results
  stay on standard output, and diagnostics keep their usual destination. Use
  `cordial --json tui 2>cordial.jsonl` or `cordial tui --json 2>cordial.jsonl` to keep the terminal
  usable while recording the stream. Desktop accepts `--json` before or after `desktop` too.
  Its stream belongs to the primary process's standard error. Ordinary desktop launches activate
  the running instance. A subsequent launch with `--json` exits with an error asking the user to
  quit Cordial first.

The applications queue standard-error diagnostics with a 256-entry limit. Logging is best-effort:
a full queue drops excess entries, and a failed writer stops logging. Neither changes a command's
result or interrupts an interface. The TUI and interactive shell defer output addressed to a
terminal until they restore terminal ownership; redirected standard error streams immediately.
Shutdown drains accepted output for at most two seconds, then discards any remaining diagnostics.
Redirect stderr to capture a long terminal session without the deferred queue's limit.

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
one message from `proto/storage/v2/storage.proto`, named by its path as listed in the [storage
format](../storage-format.md#files). `file get` decodes a file at such a path and saves the message
as indented protobuf JSON with the schema's field names, including `format`, the
mapping `--json` uses, under the destination name with `.json` in place of `.pb`.
It checks the file's format independently before decoding the current payload.
A file at any other path, an empty file, one with a missing, zero, malformed,
duplicate, or unsupported format, one that doesn't decode, or one holding an enum
value this version of Cordial has no name for is saved unconverted under its
original name, with `.pb` in place of a destination's `.json`, and the result
says why. `--raw` saves the bytes unconverted under the destination name as given.

### Uploaded files

`file put LOCAL_FILE PATH` uploads the local bytes without converting them. Files of at most
512 bytes use one atomic write. Larger files use consecutive writes to `/.file-upload`, a complete
readback, an atomic move over the destination, and a destination readback. Each upload replaces
the whole destination, including when the local file is empty. The command reports success only
after readback matches. An error does not trigger an automatic retry.

Large uploads stop if `/.file-upload` already exists or its absence cannot be confirmed. A failed
upload can leave this staging file behind. Use `file delete /.file-upload` to clear it before
trying another large upload.

`file delete PATH` deletes one file and succeeds if it is already absent. The adapter's
`/format.pb` file cannot be deleted.

## TUI

The TUI uses Ratatui and Crossterm and the same commands as the CLI. It follows the desktop
application's layout, words, and staging rules in the terminal, and opens a session with every
attached adapter; `--port` limits it to that one port.

Required assisted migration starts automatically. The TUI closes every adapter and suspends
discovery and reconnects until migration succeeds or the user cancels and the worker exits.
The adapter page provides Migrate and Cancel controls. A failed migration stays suspended;
cancelling prevents an immediate automatic retry.

- Discovery: it lists the attached adapters every second, opens each new port that matches the
  [USB discovery descriptors](transport.md#discovery), and shows an adapter once its first
  `GetStatus` identifies it. A port that fails to open, or whose session ends within 10 seconds
  of opening, is tried again after a wait that doubles with each failure in a row, up to a minute;
  Refresh Adapters (F5) tries it at once. After `SessionEnding`, an
  adapter that goes away keeps its page, locked, within the
  [intentional-disconnect deadline](#intentional-disconnects); its devices leave the list at once.
  A shown device page also stays locked, with its tab, staged changes, and save result, until the
  adapter lists its devices again within that deadline. Without the event, the adapter and its
  device pages disappear immediately. The same adapter returning is found by the adapter ID that
  begins its USB serial number. A device missing from
  the new list, or an expired hold, returns the page to Overview. Disconnect, from the adapter's
  page or menu, closes its session; the adapter stays listed while plugged in and is opened again
  only by Connect.
- Protocol refusals: an adapter on an unsupported revision stays listed under Needs Attention,
  with the connection error beside Connect. Reported firmware appears on Details and a reported
  serial number on Diagnostics. No configuration actions are available. Refresh or Connect retries
  the port. A replug also retries it when a port listing observed its absence. A successful
  connection uses the ID from `GetStatus`. The shell also stops its reconnect attempts when an
  adapter reports an unsupported revision and prints the refusal reason.
- Layout: a sidebar with Overview, the saved devices of every adapter in one list, the adapters,
  and Add Device; and the selected page with its tabs and bottom bar. The Overview counts
  connected and paired devices and connected adapters, and lists what needs attention, the
  connected devices with their batteries, and the adapters. Adapter and device pages have the same
  settings and actions as the desktop application's, with tabs for Details, Settings, Profiles,
  and Diagnostics. Development firmware adds Enter Bootloader to the adapter's Diagnostics tab.
  Adapter files are managed through the CLI commands `file list`, `file get`, `file put`, and
  `file delete`.
- Attention: while the TUI runs, a device whose battery is low (20% or less and not charging) or
  critical (5% or less), a connected device whose profiles weren't loaded, an adapter whose
  profile memory is 85% used, and an adapter that needs attention are listed under Needs Attention
  on the Overview, and the most pressing one is shown in red on the status line.
- Staging: changes stay staged, marked as changed, until Save;
  Ctrl-S presses the Save shown on screen. Discard drops the changes the shown Save would send, and
  a failed save keeps them. Valid USB preferences save without a reconnect confirmation dialog.
  Connect, disconnect, rename, and forget act at once, as do creating, copying, and deleting profiles.
- Input: every action works with the mouse, and every control has a keyboard equivalent. Tab and
  Shift-Tab move between controls, Up and Down choose in the sidebar or move between controls,
  Left and Right change the highlighted setting or switch tabs, and Enter or Space presses the
  highlighted control. Mouse movement and Tab share one highlight; hover alone never activates
  anything, and a click in a dialog never reaches the controls underneath it.
- Resizing keeps form input and recomputes click targets. Text labels accompany every color. `tui`
  requires interactive standard input and standard output; standard error can be redirected.
  Ctrl-C and SIGINT quit from anywhere and restore the terminal. Closing each session stops its
  scan and cancels an unsaved pairing. Quitting never disconnects saved devices.

## Desktop application

The native GTK4/libadwaita desktop runs as `cordial desktop`, with `--hidden` to start without
presenting a window. It uses the Rust controller, commands, and session rules shared with the TUI.
Protocol compatibility and storage migration run in-process.

- Discovery: it lists serial ports once at startup, then again after USB hotplug events (with short
  retries while udev sets up a new node) or an explicit refresh. A port must match the [USB
  discovery descriptors](transport.md#discovery) and complete the
  [handshake](transport.md#permanent-handshake) on the current protocol revision, and the first
  `GetStatus` identifies the adapter.
  Unidentified ports stay hidden. When an adapter has a readable USB serial number that matches its status ID,
  a received `SessionEnding` marks the adapter unavailable and its devices Disconnected.
  The existing adapter and device entries, pages, selected tabs, staged changes, and save results
  remain in place within the [intentional-disconnect deadline](#intentional-disconnects).
  Configuration controls stay disabled until current data is confirmed. The application retries
  every 500 milliseconds while awaiting a replacement session, including after a
  temporary failure to reopen the port. Port enumeration continues while other ports are opening.
  It recognizes the same adapter by the adapter ID at the start of its USB serial number,
  even on a different port, confirms that ID in `GetStatus` after a compatible hello, and updates
  the existing state.
  A candidate without a matching readable USB serial number cannot resume that state.
  The entries and pages remain in place between the new session's handshake and device listing,
  within the same absolute deadline. A failure of that session without its own event removes them
  immediately, even before identification. A failed listing on a live session retains them within
  the deadline; a completed listing removes devices that are absent. A per-record read error
  retains last-known device data as described above. Settings and profile lists remain visible while
  their reads are pending or fail, but their writes stay disabled until the relevant read succeeds. Failed reads
  can be retried. An adapter that returns with a radio or storage fault shows its current status
  and disabled device actions. Its retained device records remain only within the deadline.
  When the original window expires, the application removes unconfirmed retained device records
  and leaves their pages. It keeps an adapter with a live replacement connection and authoritative
  status, even if it is not ready or its device listing failed. An adapter whose transport or
  identification has not recovered is removed with its devices. Without the event or
  a usable USB serial number, a session loss or disappearance from the port list removes the
  adapter and its devices immediately, without a
  reconnect window or automatic retries. A later hotplug scan or explicit refresh creates a fresh connection, including on the
  same port, and does not restore the previous session's state.
- Protocol revisions: opening sends the delimiter and the empty `ClientHello`, waits for a valid
  `AdapterHello`, and finishes with `GetStatus` within 4 seconds in total. Input does not extend
  that deadline. Responses and events that arrive before the hello are dropped, and so is any
  frame longer than a hello can be. An adapter that
  sends any other frame before its hello, or no hello in time, stays hidden. An adapter whose hello
  names a newer revision is listed as refused with its name, firmware version, and serial number
  where the hello includes them, and receives no other request. An older revision requires an
  implemented converter. A refused adapter is opened again only on a refresh, a hotplug event, or
  Connect. Quitting closes every open port.
- Sessions: after a compatible hello, `GetStatus`, and the storage gate, it reads the complete
  device list and each device's settings, then
  follows events, and lists the devices again when the adapter becomes ready. When a device
  listing fails on a connected, ready adapter, the adapter's Diagnostics tab names the failure with
  Retry, which lists the devices again. If the devices haven't been listed since the adapter last
  became ready, it shows Needs Attention rather than Starting until Retry or another listing starts. A USB
  reconnect opens a fresh session and reads status instead of expecting an event from the old
  session.
  Bluetooth radio loss leaves paired devices listed.
- Disconnect: the user can disconnect an adapter from Adapter Menu on Details or its sidebar
  context menu. The application closes the port and does not reopen it, recognizing it by the
  first 16 characters of its USB serial number,
  until the user chooses Connect or restarts the application. This intentional disconnect does not
  start the reconnect window.
- Pairing: Add Device scans the transports the firmware supports and has enabled while its dialog is
  open, stops the scan before pairing, answers prompts in the dialog, and shows Paired as soon as
  the adapter confirms pairing, regardless of the device's connection state. Acknowledging a PIN
  answer alone does not confirm pairing. In the desktop app and TUI, Search Again starts a new
  scan once scanning stops, beside any scan error, and after a failed or cancelled pairing. Closing the
  dialog, or hiding the window, stops a scan or cancels an unsaved pairing. Cancel Pairing closes
  the dialog immediately and silently requests cancellation. Late pairing or cancellation results
  cannot reopen it or show errors; device updates still populate the sidebar.
  A received `SessionEnding` retains the pairing dialog with its adapter and
  device entries within the intentional-disconnect deadline. A matching `PairingDone` from the
  original session confirms the saved device even if the host processes the session loss first.
  The dialog follows that device on a replacement session only after matching the adapter's
  persistent ID. `SessionEnding` alone never confirms pairing success. Candidates and active
  pairing prompts from the previous session cannot be used in the new session, and the client
  never replays them. A loss without reconnect authorization, an expired reconnect deadline, or
  explicit adapter disconnect closes the dialog.
  In the native desktop, Add Device is a plus icon at the start of the selected adapter or device's
  Details header. Profiles and Diagnostics have no Add Device button. It uses that page's adapter
  and is disabled when the adapter cannot pair. The dialog has no adapter selector. A passive
  spinner in its header indicates scanning, without searching text or a status row. When scanning
  stops, Search Again appears beside any scan error. Candidate signal icons name their strength.
  The device picker and pairing use distinct native dialogs with independent sizes. Scanning uses
  a 520 by 560 content area; pairing uses a 420 by 372 content area, with the scan title hidden.
  Pairing keeps its size through progress, prompts, and results. Long content scrolls above the
  answers, which stay at the bottom.
  The device icon, step title, messages, and codes are centered 23 pixels above the center of the
  space above the answers, accounting for the header. Long device names are truncated.
  Answers fill the available width. Two answers share a row, with the primary answer last.
  Enter-code prompts offer Reject and Pair. An acknowledged rejection returns to the device
  picker without showing Pairing Failed. Code comparisons offer Codes Differ and Codes Match;
  Codes Differ retains the failure result.
  A displayed code offers Cancel Pairing. A Paired result stays open with Add Another and
  Dismiss. Add Another starts a new search; Dismiss opens the paired device's page.
  Failures and external cancellation offer Close and Search Again.
  Ctrl+N uses the selected page's adapter. Settings has no pairing target. Each adapter group in
  the tray has a submenu with Connect or Disconnect, Open, and Add Device. Add Device is enabled
  when that adapter is ready. It brings the window forward and pairs through that adapter.
- Native navigation: the sidebar starts with Overview and Settings, then each adapter followed by
  its saved devices. Separators divide adapter groups without heading rows. An Add Device row
  appears only when one ready adapter is available and no devices have been added. Connect and
  Refresh Adapters rows remain available when needed. Connected devices and adapters use normal
  colors; disconnected rows have subdued icons and names. A device keeps its last battery reading
  in the sidebar while disconnected or while its adapter is away, until the adapter reports none;
  readings don't expire with age. An adapter waiting for an announced restart is subdued without
  a spinner, as is an adapter waiting to return during migration. Connecting, starting, and active
  migration show a spinner. The battery is the only
  trailing icon. Overview, Settings, and action rows keep their normal colors. Sidebar battery
  icons expose the reading through their accessible names, while the device page shows the exact
  percentage and a full-width battery bar. Charging without a known percentage has a fact row without a bar.
  Adapter sidebar menus open with a secondary click, a long press, Menu, or Shift+F10 and offer
  Rename followed by Connect or Disconnect.
  Settings opens in the regular content pane from the sidebar, Ctrl+, or the tray. Its sidebar
  row stays selected, its five controls save immediately and follow external GSettings changes,
  and a rejected save restores the saved value. Low Battery Level accepts numbers from 1 through
  100, with a separate percent unit. In a narrow window, Back returns to the sidebar.
  Adapters and devices each have one Details page containing their settings
  and information, with no tab controls. The page title stays visible in its header. Profiles and
  Diagnostics open from link rows near the bottom as subpages with Back. The minimum window width
  is 480sp, and the sidebar collapses at 660sp. In a narrow window, Back first returns from a
  subpage to Details, then from Details to the sidebar. Overview lists adapters first without an
  Adapters heading, followed by Needs Attention and Connected when populated, without a separate
  count card. Page actions sit beside their content. Adapter Menu on Details offers Rename followed
  by Connect or Disconnect. The adapter's Details page has no Status row. Device Menu is a
  three-dot button in the Details and Diagnostics headers. It offers Refresh followed by Connect
  or Disconnect; Details omits Refresh
  when the device has no settings. Profiles has no Device Menu.
- Native search: the Search icon at the start of the sidebar header and Ctrl+F open a search
  entry. Search matches every typed word across a result's title and context, ignoring case.
  Results include Overview, Settings, adapters, devices, Profiles, and Diagnostics; individual app,
  adapter, and device settings; and displayed information such as Battery, Firmware, and Device ID.
  Device results name their device and include the adapter when device names repeat. Choosing a
  setting or information field opens its page, scrolls to its row, and focuses it.
  Advertised VIA and Vial settings open Profiles and focus their profile selectors.
  Profile names in each adapter's cached library and assigned device layers are searchable.
  Library results name their adapter; layer results name their device and include its adapter
  when device names repeat. Choosing a profile opens the corresponding Profiles subpage,
  scrolls to that exact library or layer row, and focuses it. Search uses the loaded profile list
  without reading profile rules. Information fields appear only when their rows are displayed;
  Serial Number requires a reported serial number. Diagnostic groups such as Security and
  Identifiers open Diagnostics and focus their first row. Search matches titles and contexts,
  rather than field values, message text, or menu commands.
  Result titles and dimmed contexts wrap beside a search result icon. No matches shows a compact
  search icon with "No Results Found" and "Try a different search", matching GNOME Settings.
  Enter opens the current query's first result and does nothing for an empty query or no matches.
  Down moves focus into the results. Escape clears the search and
  restores navigation. At narrow widths, use Back to show the sidebar before searching. Ctrl+F
  does not navigate away from the content page. Searching and choosing a result do not change settings.
- Native lifecycle: closing or hiding the window leaves configuration writes running. Explicit Quit
  commits the focused numeric field and waits for pending writes in a separate Cordial window
  labelled "Settings are being applied." If a write fails or its outcome is unknown, Continue
  Editing returns to the main window and Quit Anyway exits.
  An error that confirms the save completed does not block Quit or repeat the write. Its feedback
  stays beside the action. For an enable or unblock save, a matching healthy connected-device
  readback received after the save response clears radio feedback. Other completed-save notes
  clear on navigation. A newer failed change keeps its failure feedback.
- Devices open on Details, with connection switches, status, battery, and the device's own facts.
  The native page also contains the available settings in their catalog groups and Gamepad Output
  when supported. Its header shows the device's status without repeating its battery reading,
  and the content has no separate Status row. The Battery row shows its exact value and meter.
  Each setting's saved-state menu follows its control on the right with a symbolic icon, menu
  arrow, and tooltip, and an accessible name such as "Pointer Sensitivity: Saved".
  Marker states have no visible status words. Boolean settings use
  switches. An unknown boolean value offers Off and On buttons until a value is chosen.
  Choice settings and Gamepad Output show Unknown while their current value is unknown.
  Their menu buttons list only supported choices. Unknown is a display value, never an option.
  Numeric fields take numbers only, with units beside the field, and stay empty while the value
  is unknown. Automatic Free Spin and Automatic Power-Off show their numeric fields in sibling
  rows below their switches only while on. A continuous 3-pixel colored band on the left joins
  each pair. Bands follow build order through blue, purple, teal, orange, green, and pink, then
  repeat. Each numeric row reserves the saved-state menu's width so its controls align with the
  switch.
  Forget Device is at the end of the scrolling page. Profiles appears when the adapter supports
  profiles and shows the device's layers, an ordered list of profiles, empty for a new device.
  Drag a layer onto another row to move it to that position. Each row also has a drag handle and
  Remove. Each removal button has an accessible name such as "Remove Typing" while its tooltip
  remains "Remove". Profile menus in the adapter library have accessible names such as
  "Typing Profile Menu". A profile whose name is empty after display cleanup uses "Profile {id}".
  Diagnostics shows the device's errors, the HID++ protocol and status, link security, and
  identifiers, with Refresh. Details switches, settings, Gamepad Output, and adding, moving, or
  removing profile layers apply immediately. Numeric fields apply on a step, Enter, or leaving
  the field; typing and redraws do not commit an edit. Controls stay editable and show the chosen
  values while changes are being applied, without progress or success feedback. Only failures
  appear beside the action. Connect and Disconnect act at once; Forget asks for confirmation.
  Device errors stay on Diagnostics.
- Profile memory: the application notifies once when `Status.errors` includes
  `ERROR_CODE_PROFILE_MEMORY_LOW`, and once when a connected device's `errors` includes
  `ERROR_CODE_PROFILE_MEMORY_EXCEEDED` or `ERROR_CODE_PROFILE_LOAD_FAILED`. Each notification
  resets after its condition clears. Device errors appear on Diagnostics. The adapter's Profiles
  page shows the percentage In Use with a full-width meter under Profile Memory.
- Adapters open on Details. The native page includes Platform and a card for each supported
  transport, with its enable switch and reported Active Devices and Connections capacities.
  Settings precede Information, and subpage links follow the information. Rename is in Adapter Menu.
  Diagnostics has an Adapter Menu for Restart and the maintenance actions the firmware supports.
  When an adapter is disconnected or needs a storage migration, the centered status page offers
  Connect or the available migration actions.
  Capacity bars span the row below its name and usage. Storage and USB interface capacity also
  have bars when both count and limit are known. New Pairings appears only when storage is full,
  with a warning; pairing has no capacity bar. Details links to Diagnostics and, when supported,
  Profiles. Details shows storage usage from `RESOURCE_TYPE_STORAGE`, and Diagnostics shows the
  storage format, each only while the adapter reports it.
  Profiles lists every profile, each with an icon for each role it changes, and the configuration
  interfaces, each in its own card with an enable switch above a Profile selection row. The
  profile can be chosen while the interface is off, and None is offered only then. The switch
  is available once the interface has a profile and no conflicting interface is on. New Profile
  sits beside the list; a device's Profiles list has Add Profile beside it. The adapter library
  and Add Profile picker show the complete list with scrolling.
  A picker whose listing failed shows the error with Retry, which lists the profiles again and
  keeps the chosen profile. An empty picker shows No Profiles and New Profile. Creating a profile
  there reopens the picker with the new profile selected. Cancelling creation returns to the picker.
  Platform, transport, and configuration interface switches apply immediately. Selecting a
  configuration interface profile applies it immediately. Adding a device layer applies it when
  Choose is pressed. Controls stay editable while changes are being applied;
  only failures appear beside the action. Applying a change needs no reconnect confirmation
  dialog. Creating and copying profiles open name dialogs. Deleting a profile asks for confirmation
  and lists its changes in numbered order under the confirmation's introduction, with wrapped
  lines aligned to the change's text. The desktop keeps the confirmed steps in place while the
  deletion runs. A checkmark marks each confirmed change, a spinner marks the active step, and
  pending steps keep their numbers. It removes the profile from those
  devices while keeping their other layers in order, turns off and clears the listed interfaces,
  then deletes the profile. If turning off an active interface reconnects the adapter, Cordial
  shows its waiting state until the same adapter returns, then reads its current device and
  interface settings.
  Deletion continues only after the listed interfaces are off with no assigned profile and no
  device or interface uses the profile. Unsaved layer or interface changes that would use the
  profile block deletion. If the displayed plan changes before confirmation, Cordial asks again.
  Changes stop at the first failure or changed dependency. Completed changes remain marked, and
  failed or unconfirmed steps have distinct markers. Retrying applies only the remaining current
  plan. A changed remaining plan must be displayed and confirmed before it can run. A lost
  connection before the final deletion is confirmed reports an unknown outcome. Cordial does
  not automatically repeat an unconfirmed deletion. On success, the desktop heading names the
  deleted profile, all steps show checkmarks, and only a full-width Close button remains. The
  dialog stays open until closed.
- Battery: a device's battery is low when a known `battery.level` is at or below the user's
  threshold (20% by default) and `battery.charging` is not true. A low battery at 5% or less is
  critical, so neither alert occurs above the chosen threshold. It notifies
  once on entering low and once on entering critical, and resets only after a reading above the
  threshold or while charging.
- Tray: a StatusNotifierItem icon is shown while at least one adapter is connected (or always, by
  preference). Its outline means no device is connected and filled means at least one is; a badge
  marks low battery or an adapter needing attention. Its menu has a submenu for each adapter with
  Connect or Disconnect, Open, and Add Device, followed by that adapter's devices. Device rows show
  the name and battery reading without connection or error status text. Known disconnected adapters
  remain in the menu when the tray is visible, so their Connect action is available. The tooltip and
  menu update at once. The icon image changes only when it
  differs from the one shown, and applies immediately.

## Gamepad outputs

Gamepad Output has the choices Disabled, HID, and XInput. A missing saved gamepad
preference means Disabled. The native desktop shows this control on a paired device's
main Details page. The TUI shows it on the device's Settings tab.
Its roles or kind must identify a gamepad and the adapter must advertise an enabled mode,
or the device must have a saved gamepad choice other than Disabled. Unrelated keyboards and mice have no such
control unless they have a saved choice to clear. The native desktop saves a choice immediately.
The TUI uses Save and Discard. The CLI saves at once:

```sh
cordial --port PORT device set DEV gamepad-output hid
```

HID and XInput use the same profiles and mapped HID state. XInput only converts canonical HID
buttons and axes through a fixed mapping. On connection, firmware automatically selects a builtin
controller mapping only for a device with no assigned profiles. Any assignment disables it,
including an empty profile or one with only keyboard or mouse rules. Assigned profiles receive
original controller inputs, not builtin-normalized inputs. Assign an empty profile for raw input
mapping, or remove all assignments to restore automatic builtin mapping.

Builtins are firmware-only mappings, never stored, listed, or reported as user profiles.

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
`device list` uses `gamepad-output`, `gamepad-state`, and `gamepad-slot` tokens.
`adapter status` shows each advertised mode's pool, such as `Gamepad Slots (HID): 2 of 4`,
or `at most 4` while its size is unknown. See [output states and
pools](records.md#devices) and [controller mapping limits](hid-forwarding.md#usb-gamepad-outputs).

The native desktop and TUI save valid output preferences without a reconnect
confirmation or a USB-capacity pre-check. In the TUI, Save sends other
staged device settings before Gamepad Output, since its save can change USB allocation. If those
settings fail to save or their outcome is unknown, Gamepad Output stays staged. The native desktop
applies each committed setting or output choice immediately. A received success confirms the saved
preference; active USB exposure is reported separately. If the response is lost, clients report an
unknown outcome and may read the device after reconnecting. When `SessionEnding` authorizes
retention and the adapter has a usable USB serial number, automatic reconnect tracking keeps the
device page open while reading fresh status in the new session. The TUI retains staged changes. Neither output mode requires a running Host service or an additional
configuration interface.

Clients show both USB capacity errors when both appear in `Status.errors`. Required capacity has
a prominent warning and tray attention. Reserved capacity has a subtle indicator and no large
Overview banner. Resource displays use each reported optional value independently: count only,
limit only, both, or neither. Missing values are unknown and are never formatted as zero or
unlimited.

## Profile commands

The desktop application and TUI manage profiles, layers, and configuration interfaces, and never read
or change a profile's rules. Remaps of keys, media and system controls, and mouse buttons are edited
with VIA or Vial. Their fixed matrix addresses Keyboard, Mouse, Consumer Control, and System
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
| `profile delete PROFILE` | Remove a profile from its devices and configuration interfaces, then delete it. |
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
`07:39@01:06` for Keyboard Caps Lock, `09:01@01:02` for Mouse button 1, or `09:01@01:05` for Game
Pad button 1. The input collection is never inferred. AXIS is a USAGE in the input's Game
Pad collection. OUTPUTS is `disabled` or a comma-separated list of USAGEs, each optionally followed
by `@PAGE:USAGE` naming the output report's collection. An output collection may be omitted only
when exactly one advertised collection carries that usage. For example, button 1 needs an explicit
Mouse or Game Pad collection.

Clients match usage and collection against `ProfileSupport.ranges` with `kind` set to
`KIND_REMAP_INPUT`, `KIND_SCALE_INPUT`, or `KIND_AXIS_INPUT` for the chosen effect, and to
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
Axis rules add no deadzones, curves, or sensitivity settings.

See [Input profiles](../input-profiles.md) for external editors.
