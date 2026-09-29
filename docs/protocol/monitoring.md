# Monitoring and multiplexing

[Protocol index](README.md)

`session.monitor.set` changes a control-session subscription and returns immediately. It does not occupy a request slot for the rest of the session. With `enabled:true` and valid client presence, send every subsequent adapter-preference, device-state and setting notification until disabled, heartbeat expiry, or session end. There are no historical events to replay. The dongle's subscription defaults to off and is never persisted; an interactive CLI explicitly enables it on startup.

The enable response is a boundary: report the current revision, then deliver later adapter-preference, device-state and setting changes. Re-enabling an already enabled subscription is idempotent and establishes another such boundary. Flush earlier queued notifications before acknowledging a subscription change. After the disable response, send no optional adapter or device notifications until enabled again.

For example, after establishing the session's initial heartbeat:

```json
{"v":1,"id":7,"cmd":"session.monitor.set","args":{"enabled":true}}
{"v":1,"type":"response","id":7,"ok":true,"done":true,"result":{"enabled":true,"revision":20}}
{"v":1,"id":8,"cmd":"session.heartbeat","args":{}}
{"v":1,"type":"response","id":8,"ok":true,"done":true,"result":{"timeout_ms":15000,"monitor":true}}
```

| Event | `data` | Meaning |
| --- | --- | --- |
| `device.info.changed` | `{revision,device_id,fields}` | Changed information fields only. |
| `adapter.changed` | `revision`, `name`, `host_platform` | Adapter preferences changed, including saved values loaded during startup. Carries the complete resulting preferences. |
| `device.paired` | `revision`, full `device` | A bond was durably saved and a device added or renewed in place. |
| `device.connected` | `revision`, full `device` | The device became ready to forward HID input. |
| `device.disconnected` | `revision`, full `device`, `reason` | The device stopped being ready to forward HID input; its bond remains. |
| `device.changed` | `revision`, full `device` | Another record change, including enablement, connection policy, validation, HID++ preferences/runtime status, metadata, or an error. |
| `device.unpaired` | `revision`, `device_id` | A bond was durably removed and its record deleted. |
| `hidpp.setting.changed` | `revision`, `device_id`, full `setting` | A preference or last observation changed. No automatic adoption or corrective write. |
| `events.lost` | `revision`, `dropped` | Optional notifications were discarded; refresh adapter and device state. Always delivered when loss has occurred. |

Each change to adapter preferences emits one adapter event at its own revision. For device changes, choose one device event per revision: use the specific add/remove/ready/not-ready event where applicable, otherwise `device.changed`. Each event supplies the complete resulting record or an explicit deletion. `reason` is `requested`, `remote`, `link_loss`, or `unknown`; do not claim the peripheral was sleeping unless the stack can establish that separately. Transitioning out of `connected`, including into `disconnecting`, emits `device.disconnected` immediately so the event reflects forwarding state.

Discovery, authentication, and protocol-error events are always delivered when relevant, regardless of `session.monitor.set`. The `events.lost` marker concerns optional adapter and device notifications. Monitoring does not expose keys, mouse movement, raw reports, bond keys, or general debug logs.

To build a device view without a subscription/list race:

1. Establish client presence with `session.heartbeat`, enable monitoring, and wait for its response. Keep heartbeats running throughout this procedure.
2. Request `device.list` with `filter:saved`, buffering adapter and device notifications while the snapshot arrives. Then refresh `adapter.status` so adapter preferences cover changes through that snapshot.
3. Replace the local device view with the completed snapshot at revision R.
4. Apply buffered device events with revision greater than R in order, then continue applying live events. Account for adapter and setting events in this shared sequence even though they do not change a device record. Track the revision of the latest accepted adapter preferences separately so a device snapshot cannot discard an adapter update or make stale status/command responses roll it backward.

On `events.lost`, a revision gap while continuously subscribed, or a local event-buffer overflow, refresh both status and the device snapshot, invalidate settings caches and reload any open settings view. If loss occurs during a snapshot and includes changes newer than its revision, obtain another snapshot. On a new control session, begin again with `adapter.status` and a new subscription. A connected-only view can be derived from the complete device view.

Example of interleaving while scan 10 remains outstanding:

```json
{"v":1,"id":10,"cmd":"discovery.scan","args":{"transport":"both","duration_ms":10000}}
{"v":1,"id":11,"cmd":"device.list","args":{"filter":"connected"}}
{"v":1,"type":"response","id":11,"ok":true,"done":true,"result":{"count":0,"revision":20}}
{"v":1,"type":"event","event":"discovery.result","request_id":10,"data":{"candidate_id":"c_12","kind":"mouse","name":"Example mouse","transport":"ble","rssi":-48}}
{"v":1,"type":"event","event":"device.connected","data":{"revision":21,"device":{"device_id":"d_7","pairing_state":"paired","name":"Example keyboard","transport":"classic","roles":["keyboard"],"state":"connected","trusted":true,"blocked":false,"reconnect":"auto","last_error":null}}}
{"v":1,"type":"response","id":10,"ok":true,"done":true,"result":{"count":1,"truncated":false}}
```

This example assumes monitoring was enabled by an earlier request. The keyboard reconnects independently of the scan, and list request 11 completes before scan request 10.
