# Identity and device state

[Protocol index](README.md)

Discovery returns an opaque `candidate_id`, such as `c_12`. It identifies a transport-specific discovery result, not a raw Bluetooth address. Candidate IDs remain usable after their scan finishes, until the next scan starts or the control session ends. If an unbonded BLE address changes and the stack can no longer resolve the candidate, pairing returns `candidate_expired` and the user scans again.

Each candidate includes a required `kind`: `unknown`, `keyboard`, `mouse`, or
`keyboard_mouse`. This is a discovery hint from BLE Appearance or Classic Class
of Device, not proof of HID support or pairability. Reports without a type hint
do not erase a known candidate kind. The adapter retains all candidates; clients
may filter unnamed discovery results for display. No device addresses are exposed
by this field.


A saved device receives an opaque `device_id`, such as `d_7`. It remains stable across scans, peripheral address changes resolved by the Bluetooth stack, reconnects, control sessions, and adapter restarts. It survives explicit pairing renewal with the same Bluetooth identity. It is removed by unpairing and never deliberately reused for a different device. A peripheral paired separately through Classic and BLE has two records. Names are display labels, not identities.

Device record:

```json
{
  "device_id": "d_7",
  "pairing_state": "paired",
  "name": "Example keyboard",
  "transport": "ble",
  "roles": ["keyboard", "consumer_control"],
  "state": "connected",
  "security": {"encrypted": true, "authenticated": false, "secure_connections": true, "key_size": 16, "bonded": true},
  "enabled": true,
  "effective_enabled": true,
  "enabled_reason": null,
  "transport_supported": true,
  "validation_error": null,
  "trusted": true,
  "blocked": false,
  "reconnect": "auto",
  "last_error": null,
  "hidpp_enabled": true,
  "hidpp_protocol": {"state": "detected", "major": 4, "minor": 2},
  "normalization_state": "active",
  "normalization_error": null,
  "settings_state": "ready",
  "settings_error": null,
  "settings_revision": 18
}
```

| Field | Meaning |
| --- | --- |
| `device_id` | Opaque saved-device identifier, at most 64 ASCII bytes. |
| `pairing_state` | `paired` or `needs_pairing`. Independent of connection state and observed security. Needs pairing means the saved bond is missing or invalid (`validation_error` is `bond_missing`, `bond_corrupt` or `bond_mismatch`); the device retains its settings but permits no connections until explicitly paired again. |
| `name` | Device-reported name saved at pairing, or `null`; at most 128 UTF-8 bytes, truncated on a character boundary if needed. |
| `transport` | `classic` or `ble`. |
| `roles` | Known supported roles from `keyboard`, `mouse`, and `consumer_control`; empty until known. A device can have several roles. |
| `state` | `disconnected`, `connecting`, `connected`, or `disconnecting`. |
| `security` | Current connection-security observation, described below, or `null`. Never persisted. |
| `enabled` | Persisted preferred Bluetooth enablement: whether the user selected this saved device for backend use and connection admission. Independent of `hidpp_enabled`. |
| `effective_enabled` | Runtime admission derived from `enabled`, valid bond data, transport support, blocking and backend enabled capacity. Only effectively enabled bonds are given to the Bluetooth stack; connected devices are always effectively enabled. |
| `enabled_reason` | `null` exactly when `effective_enabled`; otherwise the first applicable of `unsupported_transport`, `invalid` (`validation_error` or `needs_pairing`), `blocked`, `disabled` (preference false) and `capacity` (preferred, but no ordinary enabled capacity admitted it; the user selects which to disable). |
| `transport_supported` | Runtime: `transport` is advertised by `adapter.capabilities`. Derived, never persisted. An unsupported device remains saved with its bond and settings; connection actions are unavailable, while inspection, policy, HID++ preference, forgetting settings and removal remain available. |
| `validation_error` | `null` or the stored-record problem: `bond_missing`, `bond_corrupt`, `bond_mismatch` (bond identity or owner does not match the device), `device_corrupt` (policy/identity metadata invalid) or `read_failed` (a storage read error, not proof that data is missing). Invalid records are retained, never silently trusted or deleted. |
| `trusted` | Persisted Boolean authorizing unattended reconnection and incoming HID connections from this bonded device. New CLI pairings default to true. |
| `blocked` | Persisted Boolean denying new connections, including explicit `device.connect`, until unblocked. New pairings default to false. |
| `reconnect` | `auto` or `paused`. A user-requested disconnect pauses reconnect for the remainder of this adapter boot, until `device.connect` is issued. |
| `last_error` | `null` or an object containing `code` for the most recent connection/profile failure. Clear it on a successful connection. |
| `warnings` | Optional array of HID limitations, currently `unsupported_fields` or `led_output_unavailable`. The latter means recognized lock-indicator output cannot be safely encoded. Vendor/HID++ output alone and devices without indicator reports do not trigger it. Input can remain connected. |
| `hidpp_enabled` | Persisted Boolean permitting HID++ normalization and setting application for this saved device; read-only discovery remains available when false. False for a new pairing until the device's first connection detects HID++ 2.0 or newer with usable long reports and turns it on. Otherwise independent of whether the peripheral supports HID++. |
| `hidpp_protocol` | Protocol negotiation on the current link: `{"state":"unknown"}`, `{"state":"probing"}`, `{"state":"detected","major":4,"minor":2}`, `{"state":"unavailable"}`, or `{"state":"error","code":"hidpp_timeout"}`. Clients accept an absent field as `unknown`. Detected versions are HID++ versions defined by Logitech. |
| `normalization_state` | Runtime state: `off`, `pending`, `probing`, `resetting`, `configuring`, `active`, `unsupported`, or `error`; see the [HID++ lifecycle](commands.md#hid-settings-and-lifecycle). |
| `normalization_error` | `null` or a diagnostic string for the current normalization failure. Separate from `last_error`, which concerns the ordinary HID connection. |
| `settings_state` | `off`, `pending`, `discovering`, `ready`, `applying`, `unsupported`, or `error`. Independent of normalization support. |
| `settings_error` | Nullable settings discovery/session error. Individual setting errors remain in their setting records. |
| `settings_revision` | Shared revision at the last catalog change, or zero. |

Protocol detection is independent of the saved preference, special-key translation
and settings readiness. A successful version reply survives missing optional
features, preference changes and feature exchange failures on that link. A new
connection starts at `unknown`. `unavailable` means no qualified HID++ report
transport; a timeout, transport failure, device error or invalid version reply
reports `error` without establishing protocol support. A recognized HID++ 1.0
invalid-sub-ID response establishes version 1.0; legacy battery handling remains
available. A modern version discovered over short reports remains detected even
when feature exchanges require unavailable long reports.

Every record in the default `device.list` snapshot represents a saved device, including retained `needs_pairing`, disabled, unsupported-transport and invalid entries. Discovered but unpaired candidates are returned by `discovery.scan`, separately from saved devices. `connected` means required Bluetooth security setup, supported HID report setup, and input forwarding are ready, not merely that a radio link exists. Profile setup uses `connecting`; a failed setup leaves a saved, disconnected device with `last_error`.

`security` is `null` while disconnected, connecting or disconnecting, or when
no observation has arrived for this connection. Otherwise it contains exactly:

| Security field | Meaning |
| --- | --- |
| `encrypted` | Boolean: encryption is currently enabled on the link. |
| `authenticated` | Boolean: the pairing/key has man-in-the-middle protection. This is not merely successful connection authentication or the reconnect `trusted` policy. |
| `secure_connections` | Boolean: the key was established using Bluetooth Secure Connections. This does not imply authenticated pairing. |
| `key_size` | Encryption key length in bytes, 7–16, or `null` when unavailable/not encrypted. |
| `bonded` | Boolean: the stack reports a saved bond for this connection. |

Each property can be `null` when the backend cannot report it. `null` does not
mean false. Values describe the negotiated link/key, never requested pairing
policy. For example, an encrypted Secure Connections Just Works bond can report
`encrypted:true`, `authenticated:false`, `secure_connections:true`,
`key_size:16`, `bonded:true`. The firmware reads native stack state; no Bluetooth
keys or passcodes are included. Updates use the existing `device.changed` event
and shared revision. Identical observations do not cause additional events.
Closing/removing a link drops its observation; reconnect must obtain a new one.
Neither querying nor displaying security changes the pairing policy or writes
settings. CLI device lists include a `security=...` summary for connected devices;
`device.get` and TUI details show all five properties, with missing values labelled
"not reported". The descriptions live in the client.

Every device record includes the HID++ fields. HID++ preference and adapter platform commands are part of the standard contract. Ordinary input readiness does not wait for HID++ activation.

The adapter maintains one integer state `revision`, starting at zero on boot and increasing on each device-record, adapter-preference, setting observation or preference change. Adapter/device/setting notifications, status, and snapshots share this revision. Revisions are scoped to `boot_id` from `adapter.status`; they do not persist across reboots. Version 1 integer counters must remain within JSON's exact integer range, 0 through 9,007,199,254,740,991.
