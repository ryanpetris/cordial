# HID++ protocol

Sources below are manufacturer protocol documents, including mirrored manufacturer documents. Parameter byte offsets exclude the three-byte device/feature/function header. Unless a feature explicitly says otherwise, multibyte values are big endian. Requests use the existing qualified long-report transport and response owner. Software ID zero identifies notifications and never completes a request. Feature revisions extend existing operations compatibly under the manufacturer specification. The table names the documented revisions implemented, not a maximum accepted revision. Unknown feature IDs, settings and option values are not interpreted. Device-reported hidden/engineering flags suppress use in settings and normalization.

| Feature/revision accepted | Operations and fields used | Constraints and preservation |
| --- | --- | --- |
| Root `0x0000` | Existing protocol ping supplies major/minor. Function 0 takes two-byte feature ID and returns index, flags, revision. | No new model-name qualification. Read-only discovery also runs while integration is disabled. General settings proceed after a successful protocol ping even if normalization lacks `0x0020`/`0x1B04`. |
| FeatureSet `0x0001`, revisions 0–2 | Function 0 returns non-root count. Function 1 takes index and returns ID bytes 0–1, flags byte 2, revision byte 3 from revision 1 onward. Revision 0 inventories query Root for each visible feature's revision. | Inventory includes Root, bounded to 256 records. Hidden/engineering features are shown as unsupported and never queried for settings. Unknown feature IDs remain inventory only. Compatible newer revisions keep their documented operations. |
| DeviceInfo `0x0003`, revisions 0–4 | Function 0 returns entity count; revision 1 adds unit ID, transport flags and dense model IDs; revision 2 adds extended model ID; revision 3 adds motor-drive entity type; revision 4 adds serial capability and function 2 for the 12-character serial. Function 1 returns entity information, including raw SoftDevice build numbers. | At most two firmware entities, reported as the `firmware.version` and `bootloader.version` information keys. The Dongle formats each version as text, prefix then major, minor and build, such as `RQK 12.01.0013`. Serial is queried only when advertised. No firmware-update commands. |
| DeviceName `0x0005`, revisions 0–2 | Function 0 returns byte length. Function 1 takes character offset and returns a name fragment. Function 2 returns device type; revisions 1–2 add documented type values through 19. | Read-only name capped at 64 bytes; sanitize nonprintable name bytes. Unknown type values are not exposed. Type labels live on the host. |
| Battery `0x1000`, revision 0 | Function 1 returns level count and flags. Function 0/event 0 supply current level, next level, charge status. | Level 0 is unknown. Percentage is exposed only when mileage capability is set and at least ten levels exist. Coarse critical/low/good/full boundaries are 10/30/80/100. Charging Boolean uses documented charging states and is omitted when unavailable. Events update observations only. |
| Fn inversion `0x40A2`, revision 0 | Function 0 returns state/default-state. Function 1 takes state only. Readback function 0 verifies application. | State 0 means function keys, 1 means special actions. The separate default-state field is never written. Device scope. |
| Multi-host Fn `0x40A3`, revision 0 | Function 0 uses selector `0xFF` for current host. Response/event 0 fields are host, state, default-state, capability mask. | Read-only current-host observation. Setter stays unavailable because vendor signature and request table disagree about host/state byte order. Events must address the discovered current host or `0xFF`. No host-slot switch or another host's settings. |
| Backlight `0x1982`, revisions 0–3 | Function 2 reads level count/status. Function 0 returns enabled at byte 0, option values at byte 1 and capability bits at byte 2. Function 1 writes enabled/options. Revision 2 adds effect choices and selector; `0xFF` preserves effect. Revision 3 getter additionally has configured level byte 5 and little-endian delays at bytes 6, 8, 10. Revision 3 setter has configured level byte 3 and delays at bytes 4, 6, 8. | Only advertised configuration fields are exposed: enable, power-on effect, crown effect, critical-battery power saving, supported effect choices, and revision 3 mode/level/delays. Current level and status are read-only. Read effect back through function 2 after a write. Revision 3 mode is bits 3–4: automatic 1, temporary manual 2, permanent manual 3. Temporary manual is observable, never a setter choice. Level edits require permanent manual. Delays are 1–1440 units of five seconds, displayed as 5–7200 seconds. Getter/setter layouts are deliberately separate. Fresh configuration is merged with only the requested fields; unrequested options and effect are preserved. Event 0 can require one coalesced configuration read, never an apply. The setter is documented as device NVM; there is no temporary/persistent classification in the UI. |
| AdjustableDPI `0x2201`, revisions 0–1 | Function 0 returns sensor count. Function 1 takes sensor index and returns echo plus BE16 DPI values. Function 2 returns echo/current/default. Function 3 takes sensor plus BE16 desired DPI. | At most two sensors. A list contains at most six ascending DPI values terminated by zero. A range is minimum, `0xE000 | step`, maximum, zero. DPI bounds 1–57343; positive step and exact range alignment required. Revision 0 lacks the revision 1 setter echo, so every application independently reads back. No notifications are defined. |
| SmartShift `0x2110`, revision 0 | Function 0 returns mode/current threshold/default threshold. Function 1 accepts those three fields; zero means leave unchanged. | Mode 1 free-spin, 2 ratchet. Threshold 1–255; 255 disables automatic switching. The default-threshold setter byte is always zero. An apply merges every saved field into one setter. |
| HiResWheel `0x2121`, revisions 0–1 | Function 0 returns multiplier/capabilities; revision 1 adds ratchets per rotation and wheel diameter. Function 1 returns mode, adding the analytics bit in revision 1. Function 2 changes inversion while preserving every unrelated mode bit. Event 1 reports ratchet-switch state. | The capability response is decoded into the `wheel.resolution_multiplier`, `wheel.ratchets_per_rotation` and `wheel.diameter` information keys. Inversion requires existing native routing and standard resolution. No automatic routing/resolution change. Preserve analytics state; do not collect project-specific statistics or clear them by reading the analytics operation. Events update observations only. |
| Thumbwheel `0x2150`, revision 0 | Function 0 returns native/diverted resolution and capabilities. Function 1 returns routing at byte 0 and inversion/touch/proximity at bits 0–2 of byte 1. Function 2 takes routing and inversion only. | Inversion edits require native routing. Touch/proximity are observations, not setter bits. Scale/resolution is never changed. Diverted motion is not forwarded or interpreted by this settings module. |

`0x40A0` is inventory-only and has no supported setter. No other battery feature encoding is inferred from a device model.

All application paths read current configuration before a setter and read it again afterward. An equal value generates no setter. A transmitted setter invalidates affected observations; SmartShift fields sent as leave-unchanged retain their observations. An unanswered setter is reported as a timeout until the next apply reads the device. Notifications and Refresh never adopt current values into adapter storage or restore saved values. Normalization reset invalidates all observations; automatic application resumes after the single transport owner completes normalization. Forgetting a setting changes only adapter storage. Disconnect immediately releases logical settings work because the transport client will be destroyed; live-disable settles a sent transaction before normalization reset and then resumes read-only discovery. Connecting disabled issues no reset. A failed disabled protocol probe reports its transport error; an explicit Refresh can retry that read-only probe once. There is no automatic retry loop. Unknown backlight status/effect values invalidate freshness without guessing a new value. Edits while HID++ is off are saved on the Dongle and applied once it is turned on. Notifications and Refresh remain available.

Storage uses semantic key IDs, feature/revision and validation metadata only for saved preferences. It omits the inventory, observations and unsaved records. A capability change can leave a saved preference unsupported without deleting it. Aggregate snapshot capacity is checked before storage, and failed storage restores the old preference before any settings setter can be queued.

Firmware contains only semantic setting identifiers, catalog keys, protocol operations, validation and runtime state. The host owns labels, categories, help, error prose and metadata formatting. Only explicitly saved preferences enter persistent storage. There are no keyboard-model branches or old-software compatibility paths.

## HID++ normalization

Normalize supported special controls inside the dongle. The CLI and TUI only change preferences and display status; forwarding works with neither running. Ordinary HID reports continue through the existing path. Automatic normalization must not change the physical keyboard's PC/Mac mode, Fn-lock, or the usages and modifier combinations it reports through ordinary HID. The settings interface permits explicit Fn-row configuration separately from normalization.

Store `hidpp_enabled` with each saved device, defaulting to `false` when newly added and retaining its value during renewal. A new device's first-connection setup turns it on when the device answers the read-only protocol request as HID++ 2.0 or newer and has usable long reports; a choice the user makes first stands, and an unanswered or refused request leaves the decision to a later connection. Store `host_platform` once for the adapter; supported values are `linux`, `windows`, and `mac`, with `linux` as the initial default. Both survive Bluetooth disconnects, CLI exit, and adapter restarts. Explicit unpair/removal deletes that device's HID++ enable preference, saved settings, and cached metadata; pairing does not reset the adapter's platform.

| Trigger | HID++ behavior |
| --- | --- |
| Connect while enabled | Run the activation routine. |
| Enable while connected | Run the same activation routine. |
| Change adapter platform | Save once, then run activation on each ready HID++-enabled device with the new translations. Disconnected devices use it on their next connection; disabled devices receive no platform-triggered reconfiguration. |
| Disable while connected | Reset temporary reporting, then allow read-only discovery and settings queries. |
| Connect while disabled | Read-only discovery and settings queries; no resets or setting writes. |
| Change a device's HID++ preference while disconnected | Save it for the next connection; perform no keyboard operations and queue no deferred reset. |

Activation first qualifies the descriptor's bidirectional 19-byte HID++ long report, ID `0x11`, under vendor usage page `0xFF00` or the MX Keys Bluetooth Application usage `0xFF43:0202`. It discovers protocol support and features rather than relying on device names or addresses. Require HID++ 2.0 or newer, Config Change `0x0020`, and Reprogrammable Controls `0x1B04`. Use Config Change function 1 with parameters `00 00` to reset temporary reporting to defaults. Enumerate controls, read their resulting reporting settings, and enable the temporary diversions required by the translation table. Repeating an unchanged preference does not restart activation.

This is a HID++ configuration reset, not a keyboard factory reset. Version 4 of the controls specification describes temporary diversion returning to its default on a configuration reset. The full vendor policy for feature `0x0020` is not available in the public feature documentation; the command encoding follows the existing [Logiops reset implementation](https://github.com/PixlOne/logiops/blob/main/src/logid/backend/hidpp20/features/Reset.cpp). Individual reporting writes use `pvalid=0` and leave persistent diversion unchanged. Do not repair pre-existing persistent configuration or keep historical settings snapshots or offline diversion tracking. See [Logitech's controls specification](https://lekensteyn.nl/files/logitech/x1b04_specialkeysmsebuttons.html) and [version 4 specification](https://drive.google.com/file/d/1UGDCuqnKBm7U8a6t6g3QlEZgKeaAzmAx/view).

Unsupported HID++ devices and nonfatal protocol failures retain ordinary HID forwarding. Expose the HID++ preference, detection and status through the device record's HID++ integration and `device` events, and the selected platform through `Status` and `adapter` events. Enabled is a preference, not a claim that HID++ is supported or active. Special-key translation is not reported on its own. A failed live-disable reset does not cause unbounded retries or a reset on a later disabled connection. During a live platform change, retain the previous translations until the reset is acknowledged. A failure before that reset leaves the previous translations working and reports the error. If the reset fails or its reply is lost, release held translated input so it cannot remain stuck; continue accepting previous control notifications if the device still sends them.

### Translation table

All IDs and usages below are hexadecimal. `K` is USB Keyboard/Keypad page `0x07`; `C` is Consumer page `0x0C`. `GUI` is Super on Linux, Windows on Windows, and Command on macOS. A `+` denotes one held shortcut chord, never a typed sequence. The platform selection affects only these HID++ translations.

| HID++ control ID | Control | USB output on every platform |
| --- | --- | --- |
| `00C7` | Screen brightness down | `C:0070` |
| `00C8` | Screen brightness up | `C:006F` |
| `00E4` | Previous track | `C:00B6` |
| `00E5` | Play/pause | `C:00CD` |
| `00E6` | Next track | `C:00B5` |
| `00E7` | Mute | `C:00E2` |
| `00E8` | Volume down | `C:00EA` |
| `00E9` | Volume up | `C:00E9` |
| `000A` | Calculator | `C:0192` |
| `00EC` | Left arrow | `K:0050` |
| `00EB` | Right arrow | `K:004F` |

| HID++ control ID | Control | Linux | Windows | macOS |
| --- | --- | --- | --- | --- |
| `00E0` | Window overview | GUI | GUI+Tab | `C:029F` |
| `00E1` | App overview / action center | GUI+A | GUI+A | `C:02A0` |
| `006E` | Show desktop | Leave undiverted | GUI+D | GUI+`C:029F` |
| `006F` | Lock screen | `C:019E` | GUI+L | Ctrl+GUI+Q |
| `00BF` | Screenshot | Print Screen, `K:0046` | Print Screen, `K:0046` | Shift+GUI+3 |
| `00EA` | Context menu | Application, `K:0065` | Application, `K:0065` | Ctrl+Return |

Linux shortcut mappings target GNOME defaults. Show desktop is left undiverted on Linux. Custom desktop bindings may differ. Windows GUI+A opens Quick Settings on Windows 11 and Action Center on Windows 10. macOS Ctrl+Return context menus require macOS 15 or newer. The macOS Mission Control and Launchpad interpretations of `C:029F` and `C:02A0` are host conventions also used by ordinary QMK keyboards, not universal meanings of those USB usages. Calculator support is host-dependent; a host may ignore the valid usage and receives no fallback automation. References: [QMK report usages](https://github.com/qmk/qmk_firmware/blob/master/tmk_core/protocol/report.h), [GNOME shortcuts](https://help.gnome.org/gnome-help/shell-keyboard-shortcuts.html), [Windows shortcuts](https://support.microsoft.com/en-us/accessibility/windows/keyboard-shortcuts-in-windows), [Apple shortcuts](https://support.apple.com/en-us/102650), and [macOS context-menu behavior](https://developer.apple.com/videos/play/wwdc2024/10124/).

Leave Easy-Switch controls `00D1` through `00D3`, keyboard backlight controls `00E2`/`00E3`, Fn `0034`, and Fn-lock `00DE` under the keyboard's own control. Do not divert them or substitute screen brightness for keyboard backlighting.

The diverted-controls notification reports up to four held control IDs. That is separate from the ordinary keyboard's NKRO path. Preserve presses/releases and combine normalized state with ordinary state without releasing another source's keys or modifiers. Never infer keys that the peripheral did not report.

## Source documents

- [Root specification](https://github.com/Logitech/cpg-docs/blob/master/hidpp20/features/0x0000-IRoot.rst)
- [Published feature documents](https://drive.google.com/drive/folders/0BxbRzx7vEV7eWmgwazJ3NUFfQ28)
- [FeatureSet revision 2](https://drive.google.com/file/d/10r9GSoVjZjyj2HqAj8s8yBgDjOYk-LR9/view)
- [Device type/name revision 2](https://drive.google.com/file/d/1V9UO0ToIIsMxhM36dEVfwkCFx4WH5qt2/view)
- [Device information revision 4](https://drive.google.com/file/d/15-QuLJmICowrO4GjP70ekuh7boqBR1uH/view)
- [Protocol draft with battery status](https://lekensteyn.nl/files/logitech/logitech_hidpp_2.0_specification_draft_2012-06-04.pdf)
- [Fn inversion 0x40A2](https://docs.google.com/document/d/1nH63NvSTDFlFIoGOT9sozDvSTwsTFmvqYwPcygWnOq4/edit)
- [Multi-host Fn inversion](https://drive.google.com/file/d/1TU1adbjmom4FUcAzZYpwZZi5mrkPBsU5/view)
- [Backlight revision 2](https://drive.google.com/file/d/1QfBDVyfHihbVk2yOpJsQiKekKZGwARWm/view)
- [Backlight revision 3](https://drive.google.com/file/d/1o8SSMKCtxl07VCzBQr0o8VZPfQ-leK47/view)
- [Adjustable DPI](https://lekensteyn.nl/files/logitech/x2201_adjustabledpi.html)
- [SmartShift](https://lekensteyn.nl/files/logitech/x2110_smartshift.html)
- [HiRes wheel](https://lekensteyn.nl/files/logitech/x2121_hires_wheel.pdf)
- [HiRes wheel revision 1](https://drive.google.com/file/d/1WEfouBszkLA3Dl2WRIhzqJqyWZL5wKMJ/view)
- [Thumbwheel](https://drive.google.com/file/d/1Op9dBdPkXfJWhwL7bXjvvhORDH9C9Eft/view)
