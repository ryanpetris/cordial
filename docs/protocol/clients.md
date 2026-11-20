# CLI, TUI, and Desktop Behavior

[Protocol index](README.md)

The desktop application, TUI, and CLI are built into one `cordial` executable on top of `cordial-protocol` and `cordial-client`. None runs a background service; the adapter operates independently once configured.

## Client State Model

All three interfaces maintain adapter state consistently:

- **Listings**: Each `List*` request receives one complete merged frame and replaces the collection atomically (incomplete frames are discarded on disconnect). On a per-record `RecordError`, clients retain last-known device data without confirming it until a fresh record, absence from a complete list, or `device_removed` arrives; profile placeholders need not keep cached data.
- **Events**: `adapter`, `device`, and `profile` events replace the entire record; `settings_changed` replaces whole domain objects by `SettingsRef` and ignores removals of unknown entries; error lists replace prior lists completely.
- **Confirmed writes**: An empty success response means the adapter saved the normalized values sent (`SetAdapter` trims `name`; `SetProfileRules` normalizes rules). Clients update local state directly without re-querying (except `SetAdapter(name: "")`, where the subsequent `adapter` event supplies the board default).
- **Errors and outcomes**: Clients preserve full recursive `Error` trees (`code`, `reason`, `outcome`, `related`) and lead user feedback with the known outcome in scope. A confirmed save paired with a Bluetooth failure keeps saved values while showing the Bluetooth error. Unknown, partial, no-effect, or unrecognized outcomes never trigger automatic retries.
- **Diagnostics queue**: `Connection::set_diagnostics` taps raw responses and events on the reader thread. `--json` logs every wire response and event (including background reads, migrations, and file transfers) as protobuf JSON lines on stderr via a best-effort 256-entry queue (terminal modes defer tty stderr until restoring terminal ownership; shutdown drains up to 2 s).

### Intentional Disconnects

Only a received `SessionEnding` event authorizes reconnect waiting in ordinary client sessions (for all reasons, including `BOOTLOADER` and unknown future reasons; no fallback heuristics exist):

- Starts a **15-second absolute deadline** scoped to the receiving session (repeated events and retries never extend it).
- Keeps the adapter and device views locked in place while retrying discovery by the first 16 characters of the USB serial number, ending once `GetStatus` confirms the matching adapter ID (the same 15-second window bounds subsequent reload).
- A replacement session that drops without its own `SessionEnding`, an explicit disconnect, adapter switch, quit, or deadline expiration clears unconfirmed retained data immediately.
- `SessionEnding` never confirms an in-flight command; interrupted writes keep an unknown outcome and are never auto-replayed.

## Rust Client Library and Protocol Selection

- `Connection` provides `list_devices`, `list_profiles`, `list_settings`, `list_profile_rules`, `list_features`, and `list_files` (plus `all_*` conveniences), enforcing the 1024-byte outgoing request limit and the local 32 MiB decoded receive limit (see [Rust host receive limit](transport.md#rust-host-receive-limit)).
- The opening request runs alone until answered; subsequent requests may pipeline in FIFO order. Request timeouts track incoming-byte inactivity.
- Every session opens with the permanent handshake (`ClientHello` -> `AdapterHello`). Clients connect directly when revisions match, refuse newer revisions, and require a converter for older revisions (revision `1` has none). Protocol selection runs in-process across desktop, TUI, and CLI.

## Storage Migration

After a compatible handshake, clients read bootstrap `GetStatus` and verify exact `target_storage_format` support before sending other commands.

- Host-assisted migration runs automatically **only** when `Status.errors` contains solely `STORAGE_MIGRATION_REQUIRED` (`STORAGE`, `NO_EFFECT`, no related errors). Any other error blocks migration, leaving only `GetStatus` and `Restart`.
- Desktop, TUI, and CLI run the Rust migration coordinator in-process after suspending discovery/reconnects and closing all adapter sessions. The worker reconnects indefinitely by stable `Status.id` (port paths are hints only) until verified success or explicit cancellation.
- **Subprocess IPC (`cordial migrate --ipc`)**: Uses [`proto/migration.proto`](../../proto/migration.proto) with 4-byte unsigned little-endian length prefixes (`1..65,536` protobuf bytes) on stdin (`MigrationControl`) and stdout (`MigrationEvent`), diagnostics on stderr. Exits `0` only on verified success, `1` on cancellation or failure.

See [Preserving migration](../storage-format.md#preserving-migration) for download, validation, readback, and restart rules.

## CLI

`cordial --port PORT` launches the interactive shell; `cordial --port PORT COMMAND ...` runs one command; piped stdin runs as a script; `cordial tui` opens the TUI. Without `--port`, the CLI selects the sole attached adapter (failing if multiple are attached). Quit or disconnect an adapter in the desktop app before accessing it from the CLI.

### Command Reference

| Command | Behavior |
| --- | --- |
| `adapter list` / `adapter select PORT` | List attached adapters, or switch active port. |
| `adapter status` | Show identity, readiness, platform, storage format/usage, transports, and capacities. |
| `adapter set name NAME` / `adapter reset name` | Set adapter name (quote spaces) or restore board default. |
| `adapter set platform linux\|windows\|mac` | Set host platform for HID++ translations. |
| `adapter set transport classic\|ble enabled on\|off` | Enable or disable a supported transport. |
| `adapter restart` | Restart the adapter (works while unready or migrating). |
| `adapter bootloader` | Debug firmware: reboot into ROM bootloader. |
| `migrate` / `migrate cancel` | Run or cancel assisted storage migration (`cordial migrate --ipc` for framed IPC). |
| `scan start [classic\|ble] [SECONDS]` / `scan stop` | Scan enabled transports (`10` s default) or stop early. |
| `pair start CANDIDATE` | Start pairing a discovered candidate. |
| `pair accept [VALUE]` / `pair reject` / `pair cancel` | Answer a passkey/comparison prompt or cancel pairing. |
| `device list [Saved\|Enabled\|Connected\|Trusted]` / `device get DEV` | List saved devices (optionally filtered) and candidates, or show one device's details. |
| `device connect DEV` / `device disconnect DEV` | Connect, or disconnect and pause auto-reconnect. |
| `device set DEV enabled\|trusted\|blocked\|hidpp on\|off` | Update device policy or HID++ preference. |
| `device set DEV gamepad-output disabled\|hid\|xinput` | Save Gamepad Output mode. |
| `device unpair DEV` | Delete the device's bond and saved settings. |
| `device refresh DEV` | Re-read live info and settings from a connected device. |
| `setting list DEV` / `setting get DEV KEY` | List device settings or inspect one field (paths like `keyboard.function_row`, `pointer_sensor.0.dpi`). |
| `setting set DEV KEY VALUE` / `setting forget DEV KEY` | Save or forget a device setting. |
| `feature list DEV` | Debug firmware: list integration features. |
| `file list PATH` / `file get [--raw] PATH DEST` | Debug firmware: list directory or download a file. |
| `file put LOCAL_FILE PATH` / `file delete PATH` | Debug firmware: upload (with readback) or delete a file. |
| `help [COMMAND]` / `quit` / `exit` | Show help or exit cleanly. |

Identifier and shell behavior:

- `DEV` and `CANDIDATE` accept a numeric ID or an unambiguous name (numeric arguments always match IDs; duplicate names are rejected).
- The interactive shell supports history, tab completion, asynchronous `[NEW]`/`[CHG]`/`[DEL]` updates without disrupting typed input, inline pairing prompts (use `/COMMAND` for normal commands while a prompt is open), and `SessionEnding`-gated reconnect waiting.
- One-shot commands and scripts fail fast with nonzero exit codes and accept `--timeout SECONDS` (time spent in automatic migration does not count against `--timeout`; one-shot `pair start` resolves candidates by name within the invocation).
- `--json` works with commands, scripts, shell, TUI, and desktop (for desktop, quit any running instance first because the JSON stream attaches to the primary process's stderr).

Example interactive session:

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

### Downloaded Records

`file get` decodes known LittleFS record paths (see [Files](../storage-format.md#files)) from `proto/storage/v2/storage.proto` into indented protobuf JSON (replacing `.pb` with `.json`). Unrecognized paths, empty files, unsupported formats, decode errors, or unknown enums save raw bytes unchanged (with a note explaining why). `--raw` always saves raw bytes verbatim to `DEST`.

### Uploaded Files

`file put LOCAL_FILE PATH` uploads raw bytes without conversion (<= 512 B atomically in place; larger files via `/.file-upload` + readback + `MoveFile` + readback) and fails without overwriting if `/.file-upload` already exists (clear leftovers with `file delete /.file-upload`). `file delete` is idempotent on missing files and forbids `/format.pb`.

## TUI

`cordial tui` (Ratatui/Crossterm) manages all attached adapters simultaneously (`--port` limits to one) with full keyboard and mouse navigation:

- **Discovery and reconnect**: Polls ports every second (exponential backoff up to 60 s on repeated open failures; `F5` refreshes immediately). Holds pages locked across `SessionEnding` within the 15-second window; unsupported protocol revisions stay listed under **Needs Attention** with read-only details.
- **Layout and staging**: Sidebar with Overview, devices, adapters, and Add Device; pages have Details, Settings, Profiles, and Diagnostics tabs. Settings changes stage until **Save** (`Ctrl+S`) or **Discard**; connection, rename, forget, and profile management actions act immediately.
- **Attention indicators**: Highlights low battery (<= 20%, not charging), critical battery (<= 5%), unloaded device profiles, >= 85% profile memory, and adapter faults.
- **Exit**: `Ctrl+C`/`SIGINT` restores the terminal, stops scans, cancels unsaved pairings, and never disconnects paired devices.

## Desktop Application

`cordial desktop` (GTK4/libadwaita; `--hidden` starts minimized to tray) shares the Rust controller and session rules:

- **Discovery and lifecycle**: Polls serial ports at startup, every second, and on manual refresh (`F5`/`Ctrl+R`), with a 4-second absolute handshake deadline followed by `GetStatus`. Explicitly disconnected adapters stay closed until **Connect** or restart. Closing or hiding the window lets pending writes finish; explicit **Quit** commits focused numeric edits and drains pending writes in a brief status window (offering **Continue Editing** or **Quit Anyway** on failure).
- **Navigation and search**: Responsive sidebar (collapsing at or below 660 sp, minimum width 480 sp) with **Overview**, **Settings** (GSettings-backed, immediate save, `Ctrl+,`), and each adapter followed by its devices. Adapters and devices show settings and info on one **Details** page, with **Profiles** and **Diagnostics** subpages. `Ctrl+F` searches pages, settings, displayed facts, and cached profiles/layers without mutating state.
- **Immediate editing**: Adapter/device switches, dropdowns, Gamepad Output, and drag-and-drop profile layer reordering apply immediately (numeric fields commit on step, Enter, or focus loss), showing only failures inline. Destructive actions (Forget Device, profile deletion) and profile creation/copying use confirmation dialogs.
- **Pairing**: **Add Device** (`Ctrl+N` or header `+` on Details) scans enabled transports for that page's adapter and handles enter-code, compare-code, display-code, and Just Works flows, following the adapter across a `SessionEnding` reconnect within the 15-second window.
- **Notifications and tray**: Notifies once per transition into low/critical battery (configurable threshold, default 20%), profile memory >= 85%, or profile load failures. StatusNotifierItem tray icon reflects connection state and attention badges, with per-adapter Connect/Disconnect, Open, and Add Device submenus.

## Gamepad Outputs

- Shown on a gamepad's Details page (desktop) or Settings tab (TUI) when `Status.output_support` advertises an enabled mode or a saved choice needs clearing (`Disabled`, `HID`, `XInput`).
- CLI saves immediately (`device set DEV gamepad-output hid`); `device get`/`device list` display live state and 1-based slot numbers (raw protocol/JSON slots are 0-based); `adapter status` displays each mode's pool (for example, `Gamepad Slots (HID): 2 of 4`).
- Valid choices save without reconnect confirmation dialogs; in the TUI, staged settings save before Gamepad Output. Both USB capacity warnings (`USB_REQUIRED_CAPACITY_EXCEEDED` prominently; `USB_RESERVED_CAPACITY_EXCEEDED` subtly) display independently.

## Profile Commands

Desktop and TUI manage profiles, layers, and VIA/Vial interface selection; rule editing uses VIA, Vial, or the CLI. All commands require `Status.profile_support`. `PROFILE` accepts an ID or unique name (numbers always match IDs); `LAYERS` is a space-separated list of profiles or `none`.

| Command | Behavior |
| --- | --- |
| `profile list` / `profile show PROFILE` | List profiles with derived roles, or show one profile. |
| `profile create NAME` / `profile copy PROFILE NAME` | Create an empty profile or copy an existing one on the adapter. |
| `profile delete PROFILE` | Remove from all devices and interfaces, then delete. |
| `adapter set interface IFACE on\|off [PROFILE]` | Enable/disable `via` or `vial`, optionally selecting its profile. |
| `adapter set interface IFACE profile PROFILE` | Assign a profile to a configuration interface. |
| `adapter reset interface IFACE profile` | Clear a disabled interface's profile. |
| `device set DEV profiles LAYERS` | Set a device's ordered profile layers (`none` clears). |

### Profile Rules

| Command | Behavior |
| --- | --- |
| `profile rule list PROFILE` | List all rules in the profile. |
| `profile rule remap PROFILE INPUT OUTPUTS` | Map an on/off input to `disabled` or comma-separated held outputs. |
| `profile rule scale PROFILE INPUT N/D` | Multiply a relative input by `N/D` (negative `N` inverts). |
| `profile rule axis PROFILE INPUT AXIS [invert]` | Route an absolute Game Pad axis within the Game Pad collection. |
| `profile rule forget PROFILE INPUT` | Remove a rule so the input passes through unchanged. |

Usage syntax: `PAGE:USAGE` in hex (such as `07:39` for Caps Lock). `INPUT` requires an explicit collection `USAGE@PAGE:USAGE` (such as `07:39@01:06`, `09:01@01:02` for Mouse button 1, or `09:01@01:05` for Game Pad button 1). Output collections may be omitted only when unique. Saving a no-op rule forgets it.

Examples (profile named `Controls`):

```sh
# Remap Game Pad button 1 to Mouse button 1
cordial --port PORT profile rule remap Controls 09:01@01:05 09:01@01:02

# Invert Game Pad Y axis
cordial --port PORT profile rule axis Controls 01:31@01:05 01:31 invert

# Swap Game Pad X and Y axes (evaluated simultaneously within one profile)
cordial --port PORT profile rule axis Controls 01:30@01:05 01:31
cordial --port PORT profile rule axis Controls 01:31@01:05 01:30
```

See [Input profiles](../input-profiles.md) for VIA and Vial editing details.
