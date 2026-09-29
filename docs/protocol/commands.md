# Commands

[Protocol index](README.md)

All arguments shown as optional have the defaults listed below. An omitted optional argument and its default have the same meaning. `args` itself is always present.

| Wire command | Arguments | Terminal success result |
| --- | --- | --- |
| `adapter.status` | `{}` | Adapter identity, readiness, limits, counts, revision, monitor setting, and outstanding operations. |
| `adapter.wait_ready` | `{}` | Optional `initializing` progress, then `{ "state": "ready", "status": STATUS }`, or an initialization error. |
| `session.heartbeat` | `{}` | `{ "timeout_ms": 15000, "monitor": BOOL }`; renews client presence without enabling monitoring. |
| `device.list` | Optional `filter`: `saved` (default), `paired`, or `connected`. Saved includes every saved device; Paired includes only `pairing_state:paired`; Connected includes only ready input connections. | After streaming records: `{ "count": N, "revision": R }`. |
| `device.info` | Required `device_id`. | Complete volatile information snapshot `{revision,device_id,fields}`. |
| `device.info.refresh` | Required `device_id`; connected device. | Refresh optional information and return a complete snapshot. |
| `device.get` | Required `device_id`. | `{ "device": DEVICE, "revision": R }` for a saved device. |
| `discovery.scan` | Optional `transport`: `both` (default), `classic`, or `ble`; optional `duration_ms`: 10000 by default, 1000–60000, or 0 for continuous discovery while client presence remains valid. | `{ "count": N, "truncated": false }`; `truncated` is true if the candidate limit was reached. |
| `pairing.start` | Required `candidate_id`; optional `timeout_ms`: 120000 by default, 1000–180000. | `{ "device": DEVICE }` once the bond is durably saved; the device may be saved disabled. |
| `pairing.reply` | Required `request_id`, `prompt_id`, and `action`: `accept` or `reject`; `value` only for entry prompts. | `{ "accepted": true }`, meaning the reply was accepted for that prompt, not that pairing succeeded. |
| `device.connect` | Required `device_id`; optional `timeout_ms`: 30000 by default, 1000–60000. | `{ "device": DEVICE }` once HID forwarding is ready. |
| `device.disconnect` | Required `device_id`. | `{ "device": DEVICE }` after the link is down and reconnect is paused. |
| `device.unpair` | Required `device_id`. | `{ "device_id": ID, "removed": true }` after disconnection and durable bond removal; `removed` is false if already absent. |
| `device.enabled.set` | Required `device_id` and Boolean `enabled`. | `{ "device": DEVICE }` after persisting the Bluetooth enablement preference; false also waits for disconnection. |
| `device.trusted.set` | Required `device_id` and Boolean `trusted`. | `{ "device": DEVICE }` after persisting the trust setting. |
| `device.blocked.set` | Required `device_id` and Boolean `blocked`. | `{ "device": DEVICE }` after persisting the block setting; true also waits for disconnection. |
| `device.hidpp.set` | Required `device_id` and Boolean `enabled`. | `{ "device": DEVICE }` after durably saving the preference; HID++ setup/reset can still be in progress. |
| `adapter.name.set` | Required `name`: string or `null`. A string is trimmed and must contain 1..64 UTF-8 bytes without control characters. `null` clears the saved override and uses the image default. | `{ "revision": R, "name": NAME, "host_platform": PLATFORM }` after durably saving the name. Requires ready storage. An unchanged string name, or resetting an adapter without an override, performs no write and emits no event. Clearing an override commits and emits an event even if the effective name stays the same. |
| `adapter.platform.set` | Required `platform`: `linux`, `windows`, or `mac`; no device argument. | `{ "revision": R, "name": NAME, "host_platform": PLATFORM }` after durably saving the adapter-wide platform; any resulting per-device HID++ setup is asynchronous. |
| `hidpp.feature.list` / `hidpp.setting.list` / `hidpp.setting.get` / `hidpp.setting.set` / `hidpp.setting.forget` / `hidpp.setting.refresh` / `hidpp.setting.apply` | See Device settings below. | Bounded records and job summaries. |
| `session.monitor.set` | Required Boolean `enabled`. | `{ "enabled": BOOL, "revision": R }`. |
| `request.cancel` | Required `request_id` naming a pending `discovery.scan`, `pairing.start`, or `device.connect`, `storage.list`, or `storage.read`. | `{ "request_id": ID, "requested": true }`; the target request later receives its own terminal response. |
| `adapter.bootloader.enter` | `{}`; [development firmware only](development.md#development-bootloader-entry). | `{ "rebooting": true, "mode": "bootsel" }` on Pico, or `mode: "download"` on ESP32-S3, acknowledges imminent reboot, not successful firmware installation. |
| `adapter.capabilities` | Empty. | Array of capability enum values. |

Development firmware also provides [filesystem access](development.md#development-filesystem-access)
and [diagnostics](development.md).

## Status

Return these fields in `result`:

- `protocol`: `1`; `firmware_version`, `hardware_config`, `hardware_digest`, and `adapter_id`: strings. `hardware_config` names the build-time configuration; `hardware_digest` distinguishes its pin, flash and clock values. `adapter_id` identifies the physical adapter and is stable across boots.
- `radio_backend`: `pico-sdk-cyw43`, `embassy-cyw43`, or `esp-idf`. This identifies the compiled controller driver. Changing the Pico radio backend preserves hardware identity and the storage format.
- `build_profile`: `development` or `production`, fixed at build time. This is diagnostic metadata; hosts use capabilities to determine optional functionality.
- `boot_id` and `session_id`: opaque strings that change on adapter boot and control-session creation respectively. Clients must not assume a UUID format.
- `limits`: integer fields `max_line_bytes` (4096), `max_pending_requests`, `saved_devices` (an upper estimate from saved records plus free filesystem blocks, not a configured device limit), `active_connections`, and `scan_candidates`. HID++ ceilings are `hidpp_settings` and `hidpp_saved_settings` derived from the supported setting keys, `hidpp_sensors:2`, `hidpp_firmware_entities:2`, `hidpp_setting_choices:65536`, the u16 value domain, and `hidpp_features:256`. Choice arrays must also fit the 4096-byte response frame and available RAM; the domain size is not a promise that an arbitrary array of that size can be returned. Advertise implemented limits, not theoretical radio maxima.
- `counts`: integer fields `saved` (saved devices, including Needs pairing), `paired` (usable saved pairings), `preferred_enabled` (saved devices with `enabled:true`), `enabled` (effectively enabled), and `connected` (ready input connections). `paired` ≤ `saved`, `enabled` ≤ `preferred_enabled` ≤ `saved`, `enabled` ≤ `paired`, and `connected` ≤ `enabled`.
- `capacity`: advisory object with `enabled` and `pairing` arrays. Each `enabled` entry `{ "transports": [...], "limit": N, "enabled": N }` is one native constraint, per-transport or shared, on ordinarily enabled bonds. `limit` is max(0, n−1) for native capacity n: one native entry is reserved for temporary pairing and is never used for ordinary enablement. `enabled` counts effectively enabled devices under that constraint and never exceeds `limit`. A transport may appear in several constraints; its remaining enabled capacity is the minimum of `limit − enabled` over them. `transports` is non-empty, unique and a subset of the transport capabilities. Each `pairing` entry `{ "transport": T, "available": BOOL, "reason": R|null, "estimated_additional": N }` appears exactly once per supported transport. `estimated_additional` estimates additional new saved pairings from storage after reclaimable data and write/GC reserves, bounded by the saved-record limit; actual RAM allocation is checked when reserving a Pair; entries share resources and must not be added together. `reason` is `null` exactly when available, otherwise `storage_full`, `setup_capacity` (no temporary native pairing entry), `connections_full`, `pairing_active`, `radio_unavailable` or `storage_unavailable`. Exhausted enabled capacity does not make pairing unavailable, and neither condition blocks scanning. Firmware makes the authoritative reservation on every `pairing.start`. Capacity changes produce no event of their own; clients refresh `adapter.status` after device events and their own mutations.
- `name`: effective adapter name, using the saved override or the board default baked into the image. Before storage is ready, this is the image default. Renaming preserves adapter identity, bonds and platform.
- `host_platform`: persisted adapter-wide normalization platform, `linux`, `windows`, or `mac`, initially `linux`. The value is authoritative as a saved preference only when `storage_ready` is true; until storage loads or if loading fails, firmware reports its default and clients must display the preference as unavailable. It survives removal of all bonds and does not change keyboard layout mode or host settings.
- `revision`: current shared adapter/device-state revision; `monitor`: current Boolean subscription setting.
- `radio_ready` and `storage_ready`: Boolean availability of the Bluetooth stack
  and saved bond/policy storage. A false value does not disable USB control.
- `heartbeat`: integer fields `interval_ms` (5000), `timeout_ms` (15000), and `remaining_ms` (zero after expiry).
- `pending`: entries containing `id`, `cmd`, and `device_id` and/or `candidate_id` where applicable, for outstanding requests other than this `adapter.status` request. A pairing carries its `candidate_id`, plus `device_id` only once its final validated identity has matched a saved device.

Status is a bounded summary. Use `device.list` for full records. Hardware-dependent capacities are reported by `adapter.status`.

The BTstack backend recovers from failed BLE connection completions by restarting Bluetooth through its public lifecycle API. During recovery, `radio_ready` is false, active Bluetooth devices disconnect and held USB keys are released. Saved bonds and preferences remain intact; ordinary automatic reconnects resume when Bluetooth is ready. Normal cancellation does not restart Bluetooth.

## Capabilities

`adapter.capabilities` takes `{}` and returns one terminal result, a bare enum
array such as `["classic","ble","debug","storage_management"]`.

- `classic` and `ble` independently include discovery, pairing and connections.
- `debug` enables development operations, including `adapter.bootloader.enter`.
- `storage_management` enables `storage.list` and `storage.read`.

Empty is valid; duplicates and unknown values are invalid. The client queries
capabilities on every connection and clears them on disconnect or adapter switch.
It checks transport/capacity consistency against status. Do not infer capabilities
from a board name, build profile or readiness. Capabilities remain available before
radio initialization and use reserved session capacity. File inspection works
without Bluetooth readiness. Current development firmware advertises both optional
capabilities; production compiles out their handlers and advertises neither.

All host builds contain the same functionality. Ordinary management, monitoring
and session commands are part of this protocol. No command list is advertised.
An empty capability set still permits diagnostics, saved-device inspection and
local adapter selection, help and exit. Explicitly requesting an unavailable
transport fails with `unsupported_transport`; scanning `both` uses all advertised
transports and fails if neither is supported.

The client hides unsupported actions in menus, connected help and completion,
and rejects explicit unsupported commands before serial transmission. Supported
actions that are temporarily unavailable retain their existing disabled-state
explanations. Firmware still validates requests independently. For one supported
transport, default scanning and its label use that transport; a combined scan
choice appears only when both independent transport entries are present. No
transports means no scan, pair or connect actions. Other advertised management
commands remain available. Capabilities are reloaded for each adapter session.
Per-device HID++ discovery and current connection state still determine which
device settings can be read or edited. User-facing descriptions stay in the host.

## Adapter readiness

`adapter.wait_ready` accepts an empty argument object. It waits for the Bluetooth stack and
saved bond/policy storage to initialize successfully. Peripheral connections
are not part of readiness: an adapter with no bonds or only sleeping devices
can be ready. The command never pairs, connects, resets storage, or changes
saved settings.

An already-ready adapter sends one terminal success. Otherwise it sends one
nonterminal response immediately, then a terminal success when initialization
finishes. Both responses use the same request ID. The final `status` object has
the same fields as an ordinary `adapter.status` result and reflects the initialized
adapter, including its saved platform and current boot/session identity.

```json
{"v":1,"id":3,"cmd":"adapter.wait_ready","args":{}}
{"v":1,"type":"response","id":3,"ok":true,"done":false,"result":{"state":"initializing"}}
{"v":1,"type":"response","id":3,"ok":true,"done":true,"result":{"state":"ready","status":{"protocol":1,"adapter_id":"example","boot_id":"boot","session_id":"boot-1","radio_ready":true,"storage_ready":true}}}
```

The example abbreviates the nested status object; actual replies include the
full status. A known storage initialization failure returns `storage_failed`;
a known Bluetooth initialization failure returns `radio_unavailable`. A request
whose adapter is still initializing at 30 seconds returns `timeout`. If it is
already ready when output space becomes available, return success rather than
turning a delayed reply into an initialization failure. Failed requests have the usual
terminal error envelope and no success result. Status remains callable after
failure, and development firmware still permits `adapter.bootloader.enter` recovery.

Only one `adapter.wait_ready` request may wait per control session. Another receives `busy`.
It uses a reserved slot, does not consume ordinary request capacity, and is
omitted from `status.pending`, like core heartbeat/monitor requests. It does
not depend on `session.monitor.set` and its responses are mandatory. There are no repeated
progress messages and the client does not poll readiness. Ending the session
discards its pending wait. Accepted development bootloader entry also ends the
wait without sending a later ready response. A new session requests readiness
again; the pending wait is never persisted.

The host starts heartbeats before waiting and continues reading responses and
notifications. Its readiness deadline is 35 seconds, allowing delivery of the
firmware's 30-second timeout, subject to any shorter caller deadline. A final
success must match the validated boot/session identity and have both readiness
flags true. Install that fresh status before fetching saved devices or enabling
ordinary management actions. The interactive UI displays "Waiting for
adapter…" and remains responsive to exit while it waits. CLI diagnostic status
and development bootloader commands remain available without completing readiness.
The host always requests `adapter.wait_ready` before ordinary management actions.

## Device snapshots

`device.list` streams records at one revision without copying the full device list. If that revision changes before completion, it terminates with `busy`; discard the partial list and retry. Emit one successful nonterminal response per matching record, followed by the terminal count and revision. All chunks use the same snapshot revision. There is no pagination cursor or second request to finish a list.

```json
{"v":1,"type":"response","id":3,"ok":true,"done":false,"result":{"revision":18,"device":{"device_id":"d_7","pairing_state":"paired","name":"Example keyboard","transport":"ble","roles":["keyboard"],"state":"connected","trusted":true,"blocked":false,"reconnect":"auto","last_error":null}}}
{"v":1,"type":"response","id":3,"ok":true,"done":true,"result":{"count":1,"revision":18}}
```

An empty snapshot has only the terminal response with `count:0`. A failed or interrupted list is incomplete; discard its partial records. Device events and responses to other requests may occur between chunks. Retain the completed snapshot's revision when applying buffered events.

## Discovery

Only one scan runs at a time. A second returns `busy`; cancel the first if needed. Starting a new scan invalidates the previous scan's candidates, but must not invalidate a candidate already captured by an outstanding pairing request.

Results arrive as `discovery.result` events associated with the scan request:

```json
{"v":1,"type":"event","event":"discovery.result","request_id":4,"data":{"candidate_id":"c_12","kind":"keyboard","name":"Example keyboard","transport":"ble","rssi":-53}}
```

Candidates are never associated with saved devices; a saved identity is recognized only from pairing's final validated identity. `name` follows the device-name bounds; `rssi` is an integer in dBm or `null` if unavailable. A refreshed result reuses its candidate ID; clients update that row. Candidate IDs are at most 64 ASCII bytes. Do not forward every repeated advertisement.

Scanning `both` covers both transports within the requested overall duration; scheduling may interleave discovery work. Results are candidates, not a guarantee of HID compatibility. `count` counts distinct retained candidates. Once the advertised candidate limit is reached, retain existing selections, ignore additional candidates, and report `truncated:true` at completion. Previously returned candidates remain usable after a cancelled scan.

With `duration_ms:0`, continue scheduling discovery until cancelled, the client-presence deadline expires, or the control session ends. Keep the same candidate IDs throughout that scan. An interactive `discovery scan off` cancels this request; its `cancelled` result is presented as normal discovery shutdown. The CLI tracks candidate-capacity saturation as soon as it sees the advertised maximum count; the final count/truncation summary is available only when a finite scan completes successfully.

## Pairing and authentication

Only one pairing operation runs at a time, through teardown and cleanup. `pairing.start` captures the candidate and authorizes fresh pairing, including renewing the bond of a saved device with the same identity. The request arguments remain `candidate_id` and `timeout_ms`; there is no replacement flag or confirmation. Connect and background reconnect never replace bonds, and no client path substitutes Connect for Pair.

Before any native pairing activity, every `pairing.start` reserves what a genuinely new device needs: RAM, a temporary native pairing entry, new device and bond storage, and commit headroom outside the protected maintenance reserves. This applies even when the candidate later turns out to be a saved device. If the reservation fails, `pairing.start` returns `capacity` with `details.reason` `storage_full`, `setup_capacity` or `connections_full`, and no existing bond changes. A full connection table can block pairing; the adapter never disconnects an unrelated established device to make room. Full ordinary enabled capacity does not block pairing. Other established input continues; conflicting automatic and incoming setup waits for the pairing to end. Persistent setting and policy mutations return `busy` during pairing so they cannot spend its storage reservation.

The captured advertised address establishes the connection; the final validated identity (transport, address type and identity address, never the name) determines which saved device it is. A matching saved device keeps its device ID, name, enabled preference, trust/block policy, HID++ enable preference and individually saved HID++ settings, and its bond is renewed in place. A blocked final target fails with `blocked`. Invalid or contradictory identity results fail and change no saved device. Otherwise the result is a new saved device and unused reserved capacity is released for a match.

The new bond is committed only after pairing completes and is validated. Failure, cancellation, client loss or restart before that commit leaves every existing saved device and bond unchanged, though a peripheral that already accepted new keys may need another Pair. The pairing's native entry is released after its temporary link closes.

An explicit pairing request authorizes the standard pairing procedure for that selected peripheral. Just Works may complete without another prompt. Where the authentication method requires user interaction, send either:

| Event | Data in addition to `candidate_id` | Required action |
| --- | --- | --- |
| `pairing.prompt` | `prompt_id`, `method`, `expires_in_ms`; `value` for numeric comparison. | Reply with `pairing.reply` before the prompt expires. |
| `pairing.display` | `method`: `passkey` or `pin`; `value`, `expires_in_ms`. | Display the value for entry on the peripheral. No protocol reply is required. |

Prompt methods are `enter_passkey`, `enter_pin`, and `confirm_passkey`. A passkey is a six-digit decimal string, including any leading zeroes. An entered legacy PIN is 1–16 printable ASCII characters. For `enter_passkey`/`enter_pin`, an accepted reply requires `value`; for `confirm_passkey`, accept/reject carries no value. A rejection carries no value and ends pairing with `authentication_rejected`. Prompt IDs are opaque, at most 64 ASCII bytes, and unique within the pairing request.

```json
{"v":1,"type":"event","event":"pairing.prompt","request_id":5,"data":{"candidate_id":"c_12","prompt_id":"p_1","method":"enter_passkey","expires_in_ms":30000}}
{"v":1,"id":6,"cmd":"pairing.reply","args":{"request_id":5,"prompt_id":"p_1","action":"accept","value":"042731"}}
{"v":1,"type":"response","id":6,"ok":true,"done":true,"result":{"accepted":true}}
```

Pairing request 5 remains pending until its own terminal response. Reject replies for expired, answered, or unrelated prompts with `stale_prompt`. Bound prompt expiry by both the Bluetooth stack's deadline and the overall pairing timeout. Authentication events are delivered even with monitoring disabled; the CLI must show what to enter, where to enter it, and which request it belongs to.

A successful wire `pairing.start` of a new device saves it with `trusted:true`, `blocked:false` and `hidpp_enabled:true`. It is saved with `enabled:true` when ordinary enabled capacity is available, otherwise with `enabled:false` (`enabled_reason:"disabled"`): the temporary link closes, HID/HID++ does not start, and no automatic reconnect is scheduled. A renewed saved device keeps its enabled preference, subject to current effective availability; a disabled device is not silently enabled. Pairing leaves the adapter platform unchanged. For an effectively enabled result, enable automatic connection/profile setup; the returned device may still be `connecting`. Report `device.connected` when HID forwarding becomes ready. The CLI's `pairing.start` command follows an effectively enabled result with `device.connect` for the returned device ID, joining any automatic connection attempt already in progress, so it can report both the saved bond and readiness for input. For a result that is not effectively enabled it sends no `device.connect` and reports "Paired and saved; enable it to connect." (or the `enabled_reason` when enabling is not the remedy). An unsupported HID report format does not silently remove a successfully saved bond.

Commit the bond before reporting pairing success or `device.paired`. Cancellation before that commit leaves no new saved bond, keeps any existing one, and closes the temporary link. Once committed, pairing has succeeded and cannot be undone with `request.cancel`; removal requires `device.unpair`.

## Connection, disconnection, and removal

`device.connect` applies only to a saved, effectively enabled device. It returns `blocked` for a blocked device, `disabled` when the enabled preference is false, `unsupported_transport` when the backend lacks its transport, `pairing_required` for Needs pairing, and `capacity` with `details.reason:"enabled_full"` when it is preferred but not admitted. Otherwise it changes its reconnect policy to `auto`, and attempts a connection immediately. If already ready, return success immediately. If an automatic attempt is already running, wait for its outcome. Only one explicit connection request per device may wait at a time. A timeout ends that request and reports `timeout`; normal automatic reconnect remains enabled for trusted, unblocked devices and the device may connect later. For an untrusted device, an explicit connect authorizes this connection only; it does not persist trust or authorize future unattended reconnection.

`device.disconnect` keeps the bond, sets `reconnect:paused`, stops forwarding that device's input, releases only its held contributions, and closes its link. Prevent both outbound reconnection and acceptance of a new incoming connection for that device while paused. If already disconnected, setting the pause is sufficient for success. The pause survives CLI closure, but resets to `auto` on adapter reboot; `device.connect` also clears it. Unexpected radio loss or peripheral sleep leaves `auto` enabled.

`device.unpair` first pauses reconnect, stops forwarding, and disconnects the device, then durably deletes its bond and removes its record. Success means removal will survive restart. A missing device ID is an idempotent success with `removed:false`. If disconnection or persistence fails, return an error and keep the remaining bond paused; do not claim successful removal. This deletes only the adapter's bond. The peripheral may also need its old pairing removed or its pairing mode re-entered before pairing again.

Disconnect and unpair have a 15-second operation deadline. On timeout, retain the reconnect pause and expose the actual remaining state through `device.list`. A teardown already started may still finish. Serialize persistent bond changes; a conflicting mutation may return `busy`.

A requested disconnect or unpair takes precedence over a pending explicit connect for the same device: terminate that connect with `cancelled` and continue teardown. Other incompatible operations on the same device return `busy` rather than being silently reordered. Read-only commands do not wait for Bluetooth operations, subject to request capacity; heartbeat renewal, monitoring, authentication replies, and cancellation have reserved capacity.

## Bluetooth enablement

Saved means a persistent bond and settings; enabled means selected for backend use and normal connection admission; connected means a live, ready connection. `device.enabled.set` with `enabled:true` and `device.enabled.set` with `enabled:false` change only the persisted `enabled` preference, never the bond, trust/block policy or settings, and never require pairing again.

`device.enabled.set` with `enabled:true` persists `enabled:true`. If the device would become effectively enabled and no ordinary enabled capacity remains in an applicable constraint, it returns `capacity` with `details.reason:"enabled_full"` and leaves the preference unchanged; it never evicts another device. Otherwise it succeeds, even if the device stays inactive for another `enabled_reason` such as `blocked` or `unsupported_transport`, which consume no capacity. `device.enabled.set` with `enabled:false` persists `enabled:false`, cancels a pending explicit connect for the device with `cancelled`, disconnects it, releases its held input, and keeps its bond and settings. Disabled status is enforced on explicit Connect, background reconnect, incoming admission, native bond reads and controller lists. A change conflicting with a pending pairing reservation returns `busy`. Both are idempotent, not cancellable after acceptance, and have a 15-second deadline.

When a backend with less capacity starts, all records and enabled preferences remain. Preferred devices beyond enabled capacity are admitted deterministically; the rest report `enabled_reason:"capacity"` until the user disables others. No bond is deleted and no preference is rewritten because the backend changed.

## Trust and blocking

Automatic reconnect and unsolicited incoming connections require an effectively enabled saved bond, `trusted:true`, `blocked:false`, and `reconnect:auto`. This policy is enforced on the dongle with the CLI absent.

While eligible BLE devices are disconnected and connection setup has capacity, the adapter uses the controller's filter accept list to connect whichever saved peer advertises first. A sleeping device consumes no connection slot and does not delay another peer with a timed connection attempt. The application checks current policy again before accepting the established link. Pairing and manual Connect temporarily take priority. Discovery runs alongside accept-list reconnection when the backend and controller support concurrent active BLE scanning and initiation. Otherwise, while saved BLE peers are eligible, the adapter alternates one-second BLE discovery and reconnection windows. Pauses retain the logical discovery request, its token, candidates and deadline; cancellation or session closure prevents discovery from resuming. Classic inquiry remains independent. No control session or discovery candidates are required. Transient connection failures retain the five-second to five-minute retry backoff; eligible BLE peers rejoin the list when their delay expires. Classic retains timed connection attempts. Authentication and unsupported-device failures still require explicit action.

BTstack checks the controller's LE Supported States bit 23 to determine whether
active BLE discovery and connection initiation can run concurrently. Unavailable
support uses exclusive scheduling. NimBLE uses exclusive scheduling.

`device.trusted.set` with `trusted:true` and `device.trusted.set` with `trusted:false` persist the trust Boolean. Untrusting does not remove the bond or disconnect a current connection, but prevents future unattended connections. Trusting permits normal reconnection subject to the block and pause settings; it does not clear a deliberate disconnect pause.

`device.blocked.set` with `blocked:true` durably sets `blocked:true`, then stops forwarding, cancels any pending explicit connect, and disconnects the device. Keep its bond and its existing trust/pause settings. `device.blocked.set` with `blocked:false` durably clears the block and permits normal connection policy to resume; it does not promise immediate connection. `device.connect` on a blocked device returns `blocked`.

Repeated trust/block settings are idempotent. These mutations are not cancellable after acceptance and have a 15-second deadline. A storage failure returns `storage_failed` without claiming the new setting is durable. If teardown fails after a block was persisted, retain the block, return the teardown error with `details.blocked:true`, and expose the actual state through `device.get`/`device.list`. Control-session loss does not undo an accepted policy change.

## HID++ settings and lifecycle

`device.hidpp.set` addresses a saved device, including one that is disconnected or blocked. `adapter.platform.set` addresses the adapter and works even with no saved devices. Both participate in persistent-mutation serialization and are not cancellable once accepted. Validate all arguments before changing settings: `enabled` must be a JSON Boolean, and platform names are the exact lowercase strings listed above. A missing saved device returns `not_found`; malformed or extra arguments return `invalid_args`. A storage failure returns `storage_failed` and leaves both the saved preference and current keyboard configuration unchanged.

The terminal response acknowledges durable preference storage, not successful keyboard configuration. HID++ runtime progress and errors appear in device records and subsequent `device.changed` events. A changed platform produces `adapter.changed` before any resulting device events; the command response captures its platform and revision at commit. Changing a setting during an ordinary connection attempt is allowed; activation uses the saved settings when the link becomes ready. Control-session loss does not undo an accepted change or stop ongoing normalization. Repeating the same value neither rewrites storage nor restarts HID++.

| Trigger | Action |
| --- | --- |
| Ready connection with HID++ enabled | Activate using the saved adapter platform. |
| Live change from disabled to enabled | Run the same activation routine. |
| Adapter platform change | After saving, activate every ready HID++-enabled connection independently using the new platform. Other devices use it on their next enabled connection. |
| Live change from enabled to disabled | Reset temporary reporting, then continue read-only settings discovery. |
| Connection with HID++ disabled | Discover and read supported information; send no reset or setting writes. |
| Preference change while disconnected | Persist only; no deferred reset on the next connection. |
| Adapter platform change, for a HID++-disabled device | Send no HID++ commands to that device. |

Activation qualifies the HID++ descriptor, discovers the protocol and required features, issues Config Change `0x0020` function 1 with parameters `00 00`, then enumerates controls and reads reporting settings. Enable only the temporary diversions required for the platform's selected translations. Individual diversion commands leave persistent diversion untouched. Disabling uses the discovered reset feature without starting new feature discovery. An unsupported device retains ordinary HID forwarding. Existing persistent keyboard configuration is not repaired, and no previous keyboard settings are saved for restoration. See [HID++ normalization and translations](../hidpp.md#hid-normalization).

| `normalization_state` | Meaning |
| --- | --- |
| `off` | Normalization and setting application are disabled; read-only discovery and settings work remain available. |
| `pending` | Enabled preference awaiting a ready connection or activation. |
| `probing` | Discovering HID++ protocol/features on a qualified device. |
| `resetting` | A configuration reset is in progress, including live disable after the preference has become false. |
| `configuring` | Reading controls/reporting and enabling selected temporary diversions. |
| `active` | Activation completed; supported selected controls can be normalized. |
| `unsupported` | Required reports, protocol version, or features are unavailable; ordinary HID remains usable. |
| `error` | HID++ activation or reset failed. Inspect `normalization_error`; ordinary input continues unless the underlying link also failed. |

The current diagnostic strings are `hidpp_reports_unavailable`, `hidpp_protocol_unsupported`, `hidpp_reset_unavailable`, `hidpp_controls_unavailable`, `hidpp_timeout`, `hidpp_transport_error`, `hidpp_device_error`, and `hidpp_invalid_response`. Display these strings as untrusted text. On disconnect, clear runtime status/errors to `pending` when enabled or `off` when disabled. Failed live disable can expose `error` even though the saved enabled preference is false; it does not schedule a reset on a later disabled connection.

The per-device HID++ preference and adapter platform persist across reconnects and adapter restarts. Unpair removes that device's policy, bond and saved settings; a later pairing starts enabled and uses the existing adapter platform. Device policy and portable bond commit together in `device.json`. Each device's explicitly saved HID++ settings share `hidpp.json`. Pairing renewal preserves the ID, policy and preferences. Removing `device.json` commits unpair; preference cleanup follows and startup retries it if needed. See [storage-format.md](../storage-format.md) for the current JSON contract. Invalid storage is reported as unavailable and is not automatically erased.

Firmware includes adapter preferences in `adapter.status` as `name` and `host_platform`. `adapter.name.set` and `adapter.platform.set` each return both preferences with their revision. Names live on the adapter and survive restarts and firmware updates that preserve storage. Resetting with `name: null` restores the image default and follows defaults in later firmware images. Erasing storage also restores the default.

Example settings requests:

```json
{"v":1,"id":20,"cmd":"device.hidpp.set","args":{"device_id":"d_7","enabled":false}}
{"v":1,"id":21,"cmd":"adapter.platform.set","args":{"platform":"windows"}}
{"v":1,"id":22,"cmd":"adapter.name.set","args":{"name":"Desk"}}
```

## Cancellation

`request.cancel` asks a pending scan, pairing, or explicit connection request to stop. After cleanup, that request returns terminal error `cancelled`. Cancelling an explicit connect also pauses reconnect and tears down its attempted link; it retains the bond. Cancelling a scan retains the candidates already returned.

An accepted cancellation is resolved before committing new success for its target. If the target has already finished, return `not_pending`; if it is pending but not cancellable, return `not_cancellable`. The acknowledgement of `request.cancel` is sent before the target's cancellation response. Disconnect and unpair are not cancellable once accepted.

Pair/connect cancellation allows 15 seconds for teardown. If cleanup has not completed, return `timeout` instead of claiming cancellation completed. Pairing keeps its device/controller reservation until native teardown and bond cleanup finish. Only an explicit Connect cancellation changes the user reconnect pause. Cancelling any pairing before its commit keeps every existing bond, including the old bond of a matching saved device.

## Device information

`device.info` returns one complete RAM snapshot for a saved device, including
while disconnected. `device.info.refresh` requires a connected device, refreshes
supported information without writing preferences, and returns a complete
snapshot after the reads settle. A busy device returns `busy`; a lost connection
returns `not_connected`; the operation has a 120-second deadline. Optional
service/read failures leave affected information unavailable or stale and do not
prevent HID input. Refresh is not cancellable after acceptance.

Each response is `{revision,device_id,fields}`. Each field has `key`, `instance`,
`value`, `available`, and `fresh`. A missing value is exactly
`value:null,available:false,fresh:false`. A retained last-known value has
`available:true,fresh:false`. There are no provider names, feature IDs, UUIDs,
raw protocol bytes or provider-specific errors in these records. The host only
formats and caches the resolved values.

Keys are `name`, `kind`, `manufacturer`, `model`, `serial`, `firmware`, `hardware`,
`software`, `vendor_id_namespace`, `vendor_id`, `product_id`, `product_version`,
`battery_percent` and `battery_charging`. Text is bounded to
64 UTF-8 bytes and contains no control characters. Kind is `keyboard`, `mouse`,
`keyboard_mouse`, or `other`. IDs are 16-bit integers; the vendor namespace is
`usb` or `bluetooth`. Battery percentage is an integer 0–100; charging is Boolean.
Either field can independently be unknown. Percentage alone never implies charging.
Coarse levels map to full 100, high/good 75, medium 50, low 20, critical 5, empty 0.

Instance is zero except for firmware components (0 main, 1 bootloader; first of
each type). The host receives exactly one battery percentage and charging field.
For multiple standard batteries, select the explicitly identified main battery,
otherwise the lowest known percentage, using discovery order to break ties.
Charging belongs to that same selected battery.

Battery sources are pinned:

| Connection | Battery source |
| --- | --- |
| BLE or Classic with HID++ support and integration enabled | Logitech HID++ |
| Other BLE, including HID++ disabled | Standard GATT Battery Service |
| Other Classic, including HID++ disabled | Standard HID battery reports |

HID++ support is identified by usable HID++ reports in the HID descriptor.
A failed HID++ probe or missing battery feature leaves battery fields unknown.
Disabling HID++ on a connected BLE device requests fresh BAS readings, including
when notifications are already subscribed.

No other provider supplies a missing or failed battery field. Disconnect and
source changes clear battery observations. Battery values never survive reboot.
Within BAS, percentage preference is Battery Level, Battery Level Status,
Available Energy/Available Battery Capacity, coarse Level Status, then Critical
Status. Charging uses Level Status before Available Battery Energy Status.
Within HID++ 2, select one advertised interface: Unified Battery, Battery Status,
Solar, Battery Voltage, then ADC Measurement. HID++ 1 reads register 0x0d or,
when unsupported, register 0x07. Values outside documented ranges remain unknown.
Standard HID Input and Feature reports are queried initially; Input notifications
update dynamically and Feature reports are polled every 60 seconds. Classic
GET_REPORT times out after ten seconds; late replies cannot populate a new read.
HID++ 1 enables battery notifications and polls every 60 seconds. HID++ 2 uses
its supported events and explicit refresh reads.

For non-battery metadata, valid current HID++ information wins while integration
is enabled; otherwise use valid standard BLE information. A failed preferred
metadata read allows a valid fallback. Disabling HID++ excludes its metadata;
re-enabling requires new reads before it can win again.
The client uses the pairing name only when it has no reported name.
HID descriptor roles provide device-kind hints. Disconnect marks
non-battery observations stale. Reconnect retains stale metadata until new reads replace it;
reboot discards them. Descriptor-derived keyboard/mouse roles take precedence over
generic Appearance hints. `Device.name` remains the saved name; clients overlay
the last valid reported name from device information. Names are not user-editable
in the current protocol. A missing, blank, or invalid name never replaces a known
name; only a valid new name updates it. The information Name field does not
repeat the pairing-name fallback. `device get` in the CLI uses the same resolved
name as the device list.

Both BLE backends discover Battery, Device Information and Generic Access
services after HID admission. They read supported characteristics, subscribe to
battery notifications/indications, and poll battery characteristics without a
successful subscription every 60 seconds. Explicit refresh reads all discovered
information. Optional transactions share the existing ATT slot and yield between requests so
HID output can proceed. An explicit refresh joins pending reads and then reads
all endpoints; it can be interrupted by disconnect, unpair, or a HID++ toggle.

`device.info.changed` carries the same envelope with **only changed fields**.
An omitted field is unchanged; an explicit unavailable field clears the cache.
Names retain their last valid value when a later read omits the name. Failed
transport reads clear affected battery readings; metadata retains its previous value as stale.
The event uses the adapter's existing shared revision sequence. Identical reads
and updates to a non-selected fallback do not emit information events. Clients
fetch complete snapshots on initial synchronization, for new devices, and after
`events.lost`, local overflow or a revision gap. Apply deltas by key/instance and
retain newer deltas received while a snapshot request was in flight.

Battery values, charging state and all related observations are RAM-only.
**Never write battery information to flash**, including caches or timestamps.
After restart it is unknown until read again. This contract changes no bonds or
saved writable preferences and adds no storage migration.

## Device settings

The adapter discovers documented settings from feature IDs, revisions and capabilities. A new bond starts with no saved setting values. HID++ normalization and settings have separate runtime states; missing normalization features do not hide unrelated device settings.

| Wire command | Required arguments | Result |
| --- | --- | --- |
| `hidpp.feature.list` | `device_id` | Feature inventory chunks, then catalog summary. |
| `hidpp.setting.list` | `device_id` | Cached setting chunks, then catalog summary. No device requests. |
| `hidpp.setting.get` | `device_id`, `key` | One cached setting in a terminal response. |
| `hidpp.setting.set` | `device_id`, `key`, typed `value` | Durable saved preference and pending/applying record. Device application continues after the client exits. |
| `hidpp.setting.forget` | `device_id`, `key` | Durably remove that preference. Send no peripheral setter or reset. |
| `hidpp.setting.refresh` | `device_id` | Read supported settings, stream individual outcomes, then summary counts. Never adopt or correct a value. |
| `hidpp.setting.apply` | `device_id` | Read and apply saved differences, stream outcomes, then summary counts. |

A setting chunk is `{revision,device_id,setting}`; a feature chunk is `{revision,device_id,feature}`. A catalog terminal result is `{revision,device_id,count,settings_state,settings_error}`. All chunks and the terminal response of one cached catalog share its captured revision. Single-setting get/set/forget responses use the setting-chunk shape with `done:true`.

Example setting:

```json
{"key":"wheel.mode","type":"enum","writable":true,"feature":8464,"feature_version":0,"scope":"device","choices":["freespin","ratchet"],"min":null,"max":null,"step":null,"managed":true,"desired":"ratchet","observed":"freespin","fresh":true,"observed_at_ms":12345,"observation_source":"event","state":"changed_on_device","error":null}
```

All illustrated fields are present. Types are `bool`, `integer`, `enum` and `text`; observed and desired values use the matching JSON type, or null when unavailable/unmanaged. `scope` is `device` or `current_host`. `choices` holds legal enum strings or integer choices and otherwise is empty; integers can instead advertise `min`, `max`, and `step`. Enum observations can contain an observed state that is absent from the writable choices. Temporary-manual backlighting is one example. Do not offer that state as a writable value.

`managed` means a desired value is saved on the dongle. `observed` is separate and always means last observed, with `fresh`, adapter-uptime `observed_at_ms`, and source `read`, `event`, or null. A writable unmanaged setting uses `desired:null` and state `unmanaged`. Read-only records additionally have `writable:false`; they cannot be saved or forgotten. Record states are `unmanaged`, `pending`, `applying`, `applied`, `changed_on_device`, `unsupported`, `error`, and `uncertain`. An uncertain write may have reached the device but was not confirmed. Forgetting its preference does not make the last observation fresh.

`wheel.info` remains a read-only control diagnostic containing documented response bytes as lowercase hexadecimal: 2 bytes for revision 0 and 4 thereafter. Device identity, firmware and battery observations use `device.info` instead of HID++ settings. No product IDs or names select device behavior.

Keys contain 1–64 ASCII characters from `[a-z0-9._-]`. Host bounds are 128 bytes for text/enum values and errors. Labels and categories are host-owned and are absent from setting records. Firmware currently emits text observations of at most 64 bytes. Feature IDs are unsigned 16-bit values; feature versions, indexes and flags are unsigned bytes. Every record fits the line/token limits. A feature entry is `{"index":4,"id":8464,"version":0,"flags":0,"supported":true}`. Unknown feature IDs and hidden/engineering features remain inventory entries with `supported:false`; they have no speculative setters. Newer revisions of known feature IDs retain documented operations. Unknown settings and option values are not exposed as editable controls.

Refresh/Apply chunks add `outcome`, one of `read`, `applied`, `unchanged`, `unsupported`, `failed`, or `uncertain`. Each row has the shared revision current at emission. The terminal result contains `revision`, `device_id`, `count` and a count for each outcome. Partial results remain valid on failure. If any attempted setting fails, is unsupported, or remains uncertain, return `settings_refresh_failed` or `settings_apply_failed` with summary counts in `error.details`. These jobs have a 90-second deadline while heartbeats and input continue. At expiry the adapter discards unsent requests, settles any transmitted exchange, and reports per-setting failure or uncertainty before the terminal summary. A value unavailable in the current mode may be reported as read with a stale observation; unavailable unmanaged capabilities are omitted from Refresh outcomes. Commands for another device can proceed concurrently. Cached get/list/features do not compete for the HID++ transaction owner.

Only explicitly managed entries and their validation metadata are stored. Discovery, reads, notifications and Apply never create saved entries. Set validates known metadata, reserves an idle device job before saving, and saves before writing the device. A storage failure retains the old preference and sends no setter. When storage has no room, Set returns `storage_full` with `details.outcome:"not_saved"`, optionally naming `operation`, `device_id` and `key`; clients keep the last confirmed state and explain that removing unused devices or setting saved preferences back to Default frees room. Storage full for additions never disables editing existing settings or scanning, and a zero pairing estimate alone does not mean a setting cannot be saved. An ambiguous write returns `storage_failed` with `details.outcome:"unknown"` and the value is not applied; a successful save followed by a peripheral failure is reported as saved but not applied through the setting's `state` and `error`. Repeating an identical preference avoids a flash write. Default/forget means "Forget saved value; leave device unchanged" and supports disconnected or disabled devices. Forget retains discovered metadata only in RAM; restarting without a saved entry discards it. Unpair removes all settings records and preferences for that bond.

Saved values apply on enabled connect/reconnect, live HID++ enable, adapter platform changes that reset normalization, explicit Set for that field, and explicit Apply. Read before writing, skip matching values, and confirm writes by readback. Shared setters preserve freshly read unrelated fields. Editing one field never reasserts a saved sibling that the device changed. Physical/device events update only the observation and mismatch state; they do not save or send corrective writes. A later matching observation returns to `applied` without a setter.

Disconnect, live disable and normalization resets invalidate observations. Live disable settles a transmitted exchange, runs the existing supported reporting reset, then resumes read-only discovery. It does not restore individual setting values. Connecting disabled discovers features and reads settings without resetting or applying them; unified device information excludes HID++ values while disabled. A platform change while busy waits for the transmitted exchange before resetting and reconciling; no two operations consume the same HID++ reply. Serial-session loss does not cancel committed preference application.

Set, Get, features and Refresh require a connected device. Apply additionally requires HID++ integration enabled. Set while disabled validates and durably saves the desired value without issuing a peripheral setter. Cached settings lists and Forget remain available so saved preferences can be cleared. Offline edits are rejected without saving. Unseen keys return `not_found`; invalid values return `invalid_args`; read-only edits return `read_only`; unavailable metadata returns `settings_unavailable`. Device operations reject disconnected devices with `not_connected`; Apply rejects disabled integration with `hidpp_disabled`. Known unavailable capabilities return `unsupported_setting`. A second set/forget/refresh/apply for a busy device returns `busy` before any save.

`hidpp.setting.changed` carries `{revision,device_id,setting}` and consumes one revision from the shared event sequence. Clients consume these revisions even when the settings view is closed. Catalog changes and observations that update multiple rows together instead emit `device.changed` with `settings_revision` set to that event's revision. This is an invalidation watermark, not another counter. Buffer newer events around snapshots. A disconnect or `normalization_state:resetting` invalidates cached observations; a later `active` record does not freshen them. On `events.lost`, a revision gap or local overflow, reload status/devices, invalidate settings caches, and reload any open settings view. Monitoring alone does not enable device polling.
