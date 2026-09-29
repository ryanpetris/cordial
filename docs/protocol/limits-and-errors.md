# Resource limits and errors

[Protocol index](README.md)

Support at least four pending ordinary requests per session, subject to the separate one-scan, one-pairing, and per-device mutation rules. Advertise the configured limit through `adapter.status`. Reserve dispatch and response capacity for `adapter.status`, `session.heartbeat`, `adapter.wait_ready`, `session.monitor.set`, `pairing.reply`, and `request.cancel` so pending Bluetooth work cannot prevent client-presence renewal, authentication, or cancellation. Extra ordinary requests return `busy`; do not silently queue unbounded work. A backend conflict that prevents scanning or pairing alongside a particular radio operation also returns `busy` with the conflicting operation identified in `details` where known.

All control queues are bounded, and HID forwarding takes priority. Under output pressure, discard optional monitor notifications first and deliver `events.lost` before resuming optional notifications. Its `dropped` count covers the discarded adapter/device/setting events; its `revision` is the current revision when the marker is emitted. Snapshot responses, command responses, discovery results, and authentication prompts must not be silently dropped.

If mandatory output cannot be delivered within five seconds because the host has stopped reading, fault the control session, disable monitoring, stop accepting commands, and require the host to close/reopen the port with a DTR cycle. Cancel uncommitted pairing and scanning as on session close, while allowing committed state changes to finish. Report `protocol.error` with code `session_fault` if delivery is possible. Do not block HID forwarding or accumulate unlimited output while attempting that report. Buffered data from a faulted session is discarded at the next session boundary.

| Error code | Meaning |
| --- | --- |
| `invalid_request` | The message is not a JSON object, or envelope fields or the request ID are missing, invalid, unknown, or duplicated. |
| `invalid_json` | JSON cannot be parsed because of syntax or encoding. |
| `message_too_large` | Line exceeded the framing limit. |
| `unsupported_version` | Unsupported protocol version. |
| `unknown_command` | No such command in this version. |
| `invalid_args` | The envelope is valid, but command arguments contain unknown or duplicate fields, wrong types, invalid values, or out-of-range parameters. |
| `busy` | Request slots or a conflicting operation prevent execution now. |
| `not_found` | No saved device with that ID. |
| `blocked` | A saved device is blocked; explicitly unblock before connecting. |
| `heartbeat_required` | Client presence expired; send a heartbeat before starting interactive activity. |
| `client_timeout` | A scan or uncommitted pairing was stopped because client presence expired. |
| `candidate_expired` | Candidate is unknown, invalidated, or no longer usable. |
| `disabled` | The saved device's Bluetooth enablement preference is off; enable it before connecting. |
| `pairing_required` | Saved entry needs explicit pairing before it can connect. |
| `capacity` | A resource is exhausted. `details.reason` is `enabled_full` (no ordinary enabled capacity), `storage_full` (no room for a new device/bond outside reserves), `setup_capacity` (no temporary native pairing entry) or `connections_full` (active-connection table full). |
| `unsupported_transport` | The selected firmware backend does not support the requested Bluetooth transport. |
| `unsupported_hid` | The peripheral's HID services/report format are unsupported. |
| `authentication_failed` | Bluetooth authentication failed. |
| `authentication_rejected` | User or peripheral rejected authentication. |
| `stale_prompt` | Pairing reply no longer matches an open prompt. |
| `connection_failed` | Link or profile setup failed. |
| `radio_unavailable` | The Bluetooth controller is not ready. |
| `input_overflow` | The bounded input queue filled before the USB host consumed it. |
| `storage_failed` | Saved storage could not initialize, or a bond or setting could not be durably saved or removed. `details.outcome` is `unknown` when the adapter cannot determine whether the write took effect; clients show "save status unknown", do not assume either value, and the new value is not applied. `not_saved`, or no outcome, means nothing changed. |
| `storage_full` | A persistent change could not fit after reclaiming eligible data and outside protected reserves. `details.outcome` is `not_saved`; `details` may also name `operation`, `device_id` and `key`. The previous saved value is kept and the new value is not applied. |
| `timeout` | The operation or authentication deadline expired. |
| `cancelled` | A pending operation was cancelled. |
| `not_pending` | Cancellation target has already finished or is unknown. |
| `not_cancellable` | The pending operation cannot be cancelled. |
| `session_fault` | The control session must be reopened. |
| `internal_error` | An unexpected adapter failure prevented the operation. |

Structured error details contain a mutation outcome, capacity reason, existing device ID, or HID++ operation summary, as defined in the shared payload types and schemas. A lost response leaves the outcome uncertain; reconnect and inspect `device.list` before retrying a mutation. Request IDs do not provide replay or exactly-once execution across sessions. Do not automatically retry pairing or unpairing after a transport failure.
