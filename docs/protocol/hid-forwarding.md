# HID forwarding limits

[Protocol index](README.md)

The input decoder handles keyboard arrays and NKRO bitmaps, report IDs, global
Push/Pop, relative mouse movement, 16 buttons, both wheels, and consumer usages.
Each map is limited to 2,048 descriptor bytes, 512 report bytes, 16 report IDs,
96 fields, and 128 compressed usage spans, including a 128-span local list.
The shared forwarder has four source slots and 64 queued transitions. Each
input sample and queued motion axis is limited to 1,048,576 counts and split
into 16-bit packets. Coalescing that would exceed a queue entry's motion limit
starts another entry, preserving both movements until the queue is full.
Exceeding an active input limit is an explicit error, not a silent key drop.

USB suspend or lack of configuration switches input to current-state tracking.
On host return, the adapter
publishes currently held keys and buttons and discards old motion and taps.
At most one report already submitted to the USB controller can still arrive
when polling resumes, followed by the current held state.
This preserves Bluetooth connections through host sleep without replaying input.
Endpoint congestion retains key/button transitions in order, including when
a completion takes longer than 500 ms. A configured host that never polls HID
still has the documented queue limit; its delay alone does not prove suspension.

Truncated reports are rejected; bounded trailing padding is ignored. Empty or
out-of-range array entries do not prevent other keys from being released.
Unsupported relative consumer fields and unusual LED outputs are skipped and
marked in the compiled map for device-status reporting by the radio backend.
Standard Caps Lock/Num Lock indicator forwarding remains enabled. An indicator
warning means a recognized lock-indicator output format cannot be safely
encoded; vendor/HID++ reports alone do not trigger it. This does not control
keyboard backlighting. LED reports sharing unrelated writable state are not
generated. Absolute pointer axes, HID delimiters and reserved long items remain unsupported; their
presence can make a combined device's entire map unsupported. The adapter does
not translate relative volume-knob counts into synthetic media-key presses.

BLE profiles preserve separate report maps for up to three HID services.
Descriptor and native characteristic limits still apply.
