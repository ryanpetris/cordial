# Transport and framing

[Protocol index](README.md)

Automatic USB discovery requires VID/PID `cafe:4014` and manufacturer `Cordial`.
Hosts read these descriptors before opening the serial port. Explicit CLI port
selection bypasses discovery filtering.
CDC occupies interfaces 0 and 1; HID occupies interface 2 with report IDs 1, 2 and 3. Endpoint numbers, optional interface strings and class-descriptor revisions are supplied by the USB implementation and are not host discovery keys. The USB serial number string equals the adapter's `adapter_id`, so a host can recognize an adapter before opening its port; opening and `adapter.status` still confirm it.

The USB manufacturer string is `Cordial`. The product string is the firmware's
programmed `default_adapter_name`, independent of the user's saved adapter name.
The serial number is 16 uppercase hexadecimal digits, including leading zeros.
Pico W uses its flash unique ID; RP2350 boards use their chip ID; ESP32-S3 uses
bits 0..63 of its factory `OPTIONAL_UNIQUE_ID` eFuse field, read as a little-endian
64-bit integer. USB identity is available before radio startup.

- Encode each message as one UTF-8 JSON object followed by LF. Accept CRLF from the host; emit LF from the adapter. Newlines inside strings use JSON escaping.
- A message is at most 4,096 bytes, including its line terminator. Reject duplicate object keys, invalid UTF-8, and values of the wrong type.
- USB transfers are not message boundaries. A line may arrive in pieces, and one transfer may contain multiple lines. Ignore empty lines. For an oversized line, discard through its next LF before accepting another message.
- Serialize complete lines in each direction. Concurrent requests, responses, and events must never interleave their bytes within a line.
- Every message has `"v":1`. Reject any other value without executing the command. Firmware and CLI implement this contract and are updated together.
- Emit no prompts, debug text, startup banners, or raw keyboard/mouse reports on this interface. Human-readable formatting belongs to the CLI. Device names are untrusted text; the CLI must escape terminal control characters.
- Use the conventional 115200, 8-N-1 host serial settings. CDC line coding does not set the USB transfer speed or change the protocol. Opening the port or changing baud rate must not reboot the adapter.

The CLI asserts DTR while it owns the port. A DTR transition from low to high starts a control session; DTR going low, USB reset, or USB disconnection ends it. Clear partial lines, queued control output, request IDs, and subscriptions at the session boundary. The host queries `adapter.status` and `adapter.capabilities` on every new connection, then starts `session.heartbeat`. Before normal management commands, the host completes the [`adapter.wait_ready` handshake](commands.md#adapter-readiness). Status, capability discovery, file access and development bootloader entry can bypass that wait. DTR provides an immediate teardown signal when available; the heartbeat also handles a crashed client or a serial stack that leaves DTR asserted.

On opening the port, the CLI explicitly cycles DTR low then high and discards stale received bytes before sending its first request. This establishes a fresh request-ID namespace even if a previous process crashed with DTR high; it does not reset the dongle or its HID connections. The desktop application instead opens the port from a closed state with HUPCL: opening raises DTR (a low-to-high transition, because the kernel lowered DTR when the previous owner's descriptor closed, including after a crash) and the serial library discards the input received so far. The separator arrives as an ignored empty line if it was not discarded with that input. Because a packet of the previous session can still arrive afterwards, the application ignores lines that are not valid JSON, events, and responses that don't match its request until the first valid response; after that, invalid input ends the session. Closing the port lowers DTR. The desktop application cannot toggle DTR on an open port because its serial library always issues a break request with modem-line changes, and the adapter's CDC interface does not support break. Its web version opens the port through Web Serial, which changes DTR without a break: like the CLI, it lowers DTR, waits 60 ms and raises it, then ignores earlier-session input the same way as the desktop application. It lowers DTR before closing the port.

A USB IN packet already submitted before DTR falls may still reach the host. After DTR rises, the adapter sends an LF separator before any new-session responses. The client drains input while DTR is low, ignores empty lines, and discards any old fragment up to that separator before sending its initial `adapter.capabilities`, followed by `adapter.status`. It then requires a matching status response with the adapter identity and session ID. This handshake must have a bounded timeout. Do not reset USB or interrupt HID merely to reopen the control session.

Closing a control session stops its scan and cancels pairing that has not committed a bond. An explicit connection attempt already in progress may finish. Disconnect/unpair operations already accepted finish independently of the host. Existing bonds, reconnect policy, and HID forwarding do not depend on an open CLI. Loss of the physical USB connection naturally prevents delivery to that computer.

## Client heartbeat and exit

The CLI sends `session.heartbeat` every 5,000 ms while open, including while waiting for user input or another command. Each heartbeat refreshes a 15,000 ms client-presence deadline measured by the dongle's monotonic clock. These values are fixed for version 1 and advertised by `adapter.status`. Heartbeats are normal requests with terminal responses, multiplexed with all other traffic.

A new control session starts with a 15-second grace period. When its presence deadline expires:

- Disable monitoring and discard queued optional notifications. Stop discovery and cancel any pairing that has not committed its bond; affected requests receive terminal error `client_timeout` if the control channel remains usable.
- Keep bonds, trust/block settings, existing HID connections, and automatic reconnection operating independently. A missed heartbeat must not release held keys or disconnect a working peripheral.
- Retain request-ID ordering for the still-open serial session. A later heartbeat restores client presence but does not restart monitoring, scanning, or pairing. The CLI explicitly re-enables the desired activity and refreshes its device snapshot.

After expiry, new `discovery.scan`, `pairing.start`, and monitor-enable requests return `heartbeat_required` until another heartbeat arrives. Other management commands remain available. Arbitrary commands and outgoing events do not refresh this deadline.

On `quit`, `exit`, EOF, or orderly process shutdown, send `session.monitor.set` with `enabled:false` when supported, stop heartbeat scheduling, and cancel any scan or uncommitted pairing. Allow at most one second total for best-effort cleanup acknowledgements, then lower DTR and close the port. Do not send disconnect/remove commands for the user's devices. A crash or forced kill relies on DTR teardown or heartbeat expiry instead.

Monitoring and discovery are RAM-only session state. They always start off after a power cycle, firmware restart, or new control session. Never persist either state to flash. Standalone HID operation needs no heartbeat.
