# Logitech HID++ Support

Cordial supports Logitech HID++ device discovery, persistent configuration, status reporting, and special-key translation into standard USB input. All configuration and translation run on the adapter without host software once configured. Feature availability depends on each device's advertised capabilities and connection type.

## Status Definitions

| Status | Meaning |
| --- | --- |
| Implemented | Supported by current firmware for the operations listed in the row. |
| Planned | Selected for implementation; remaining protocol or hardware verification is noted in the row. |
| Not Planned | Not currently selected for implementation; may be reconsidered later. |
| Excluded | Outside Cordial's scope or ruled out for the stated reason; reconsideration requires addressing that reason. |

A single HID++ feature ID can carry multiple statuses because Cordial evaluates individual operations against the [Logitech scope rules](../AGENTS.md#logitech-devices). Resolving research, UI design, or hardware verification questions does not automatically promote an operation to Planned.

### Scope Highlights

- **Keyboard features**: Fn-row preference (legacy, single-host, and current-host multi-host), keyboard platform preference (current connection), fixed standard USB mappings for media and special keys, standard USB System/Radio input with clean press/release tracking, and automatic power-off timeout on supported keyboards.
- **No unnecessary diversion**: Standard HID reporting is preserved whenever it provides the required behavior. Diverting a control requires a demonstrated missing native capability or failure and complete adapter-owned translation to standard USB input. Mechanical preferences (such as wheel inversion or SmartShift ratchet mode) and native resolution settings are distinct from diversion.
- **Code locations**: Shared settings orchestration lives in [the settings engine][features] and [settings storage][settings]. Features without dedicated handlers still appear in the device's HID++ inventory. See [Protocol behavior](#protocol-behavior) and [HID++ normalization](#hid-normalization) below.

## HID++ 2.0 Feature Inventory

Linked feature IDs open the manufacturer's specification. Unlinked entries are known catalog IDs whose command layouts may still require verification.

| Feature ID | Feature | Description | Implementation | Status | Operations and Status Reasons |
| --- | --- | --- | --- | --- | --- |
| [`0x0000`](https://drive.google.com/file/d/1ULmw9uJL8b8iwwUo5xjSS9F5Zvno-86y/view) | Root | Protocol probe and feature lookup. | [discovery][features], [input client][hidpp] | Implemented | Protocol discovery without model-name assumptions. |
| [`0x0001`](https://drive.google.com/file/d/10r9GSoVjZjyj2HqAj8s8yBgDjOYk-LR9/view) | Feature Set | Enumerate advertised features and revisions. | [discovery][features] | Implemented | Unknown IDs stay inventory-only; hidden and engineering features are never used. |
| `0x0002` | Feature Information | Feature metadata. | | Not Planned | Needs documented semantics and diagnostic value beyond the existing inventory. |
| [`0x0003`](https://drive.google.com/file/d/15-QuLJmICowrO4GjP70ekuh7boqBR1uH/view) | Device Information | Device identity and firmware/hardware entities. | [handlers][handlers] (`Handler::Firmware`) | Implemented | Reads up to 8 entities; retains first application firmware, bootloader, hardware revision, and advertised serial. |
| `0x0004` | Device Unit ID | Separate device identity feature. | | Not Planned | Needs command verification and clear value beyond `0x0003`. |
| [`0x0005`](https://drive.google.com/file/d/1V9UO0ToIIsMxhM36dEVfwkCFx4WH5qt2/view) | Device Name and Type | Read device name and type. | [handlers][handlers] (`Handler::Name`) | Implemented | Read-only name (capped at 64 bytes); types map to keyboard, mouse, or other. |
| `0x0006` | Device Groups | Device grouping metadata. | | Not Planned | Needs research into grouping semantics and configuration/diagnostic usefulness. |
| [`0x0007`](https://drive.google.com/file/d/1k3edRXkwuMdoSXd9BivjcFa9ITkheCUm/view) | Device Friendly Name | Configure advertised device name. | | Not Planned | Requires bounded text, paged access, and full-name readback. |
| `0x0008` | Keep Alive | Connection keepalive facility. | | Not Planned | Needs documented commands and evidence of a connection issue requiring it. |
| `0x0011` | Property Access | Generic device property access. | | Not Planned | Needs research into properties, write semantics, and useful controls. |
| `0x0020` | Configuration Change | Reset temporary reporting configuration. | [input client][hidpp] (`Client::reset`) | Implemented | Internal setup for fixed input translation; not a user-facing factory reset. |
| `0x0021` | 32-byte Unique Random ID | Device identity token. | | Not Planned | Needs command verification and a useful Diagnostics presentation. |
| `0x0030` | Target Software | Target software metadata. | | Not Planned | Needs documented semantics and an adapter-side purpose. |
| `0x0080` | Wireless Signal Strength | Radio signal readout. | | Not Planned | Needs Bluetooth availability verification and meaningful Diagnostics values. |
| `0x00C0` | DFU Control (legacy) | Legacy firmware-update entry. | | Excluded | Device firmware updates are outside configuration and input translation. |
| `0x00C1` | DFU Control (unsigned) | Unsigned firmware-update entry. | | Excluded | Device firmware updates are outside configuration and input translation. |
| `0x00C2` | DFU Control (signed) | Signed firmware-update entry. | | Excluded | Device firmware updates are outside configuration and input translation. |
| `0x00C3` | DFU Control | Firmware-update control. | | Excluded | Device firmware updates are outside configuration and input translation. |
| `0x00D0` | DFU | Firmware transfer and update. | | Excluded | Device firmware updates are outside configuration and input translation. |
| `0x1000` | Battery Status | Battery level and charging state. | [handlers][handlers] (`Engine::battery`) | Implemented | Uses advertised mileage capability; otherwise maps coarse levels. |
| `0x1001` | Battery Voltage | Voltage-based battery and charging state. | [handlers][handlers] (`Engine::battery`) | Implemented | Reports charging and documented full/critical states without guessing percentages. |
| `0x1004` | Unified Battery | Battery percentage or coarse level and charging. | [handlers][handlers] (`Engine::battery`) | Implemented | Uses advertised capabilities and supported status fields. |
| `0x1010` | Charging Control | Configure charging behavior. | | Not Planned | Needs research to establish a stored preference and command format. |
| `0x1300` | LED Control | Direct indicator LED control. | | Excluded | One-shot or software-driven indicator overrides are outside ongoing configuration. |
| `0x1500` | Force Pairing | Device pairing command. | | Excluded | Direct device pairing commands are outside stored configuration (normal adapter pairing is unaffected). |
| `0x1800` | Generic Test | Device test facility. | | Excluded | Factory/engineering test operations are out of scope. |
| `0x1802` | Device Reset | Device reset command. | | Excluded | One-shot reset is outside ongoing configuration. |
| `0x1805` | Out-of-box State | Factory/setup state facility. | | Not Planned | Needs documented semantics and an in-scope purpose; factory resets remain excluded. |
| `0x1806` | Config Device Properties | Device property configuration. | | Not Planned | Needs verification of property definitions and supported commands. |
| [`0x1814`](https://drive.google.com/file/d/1EMHfOJwXikGdJfdYIa0v1a50EVV9Qb8N/view) | Change Host | Switch paired host slots and manage cookies. | | Excluded | Other hosts and host switching are out of scope. |
| [`0x1815`](https://drive.google.com/file/d/1SXlqlYIi4p3cN9d3dStHZAvxKmkCcV_h/view) | Hosts Info | Inspect or modify paired-host records. | | Excluded | Other-host lists, names, OS metadata, and slot management are out of scope. |
| `0x1816` | BLE Pro Pre-pairing | BLE pre-pairing facility. | | Not Planned | Needs research into protocol and applicability to the adapter's Bluetooth link. |
| `0x1981` | Backlight | Older backlight duration controls. | | Not Planned | Persistent duration/off; verify manufacturer commands before implementing. |
| [`0x1982`](https://drive.google.com/file/d/1o8SSMKCtxl07VCzBQr0o8VZPfQ-leK47/view) | Backlight 2 | Backlight enable, effects, level, mode, and timeouts. | [handlers][handlers] (`Engine::backlight`) | Implemented / Not Planned / Excluded | **Implemented**: advertised persistent settings and current level/status readout. **Not Planned**: RAM-only effect config (needs verified commands, readback, and reapplication). **Excluded**: selecting temporary manual mode (observable only; rev 3 forbids software selection). |
| `0x1983` | Backlight 3 | Backlight timeout controls. | | Not Planned | Autonomous illumination timeout; verify manufacturer commands first. |
| [`0x1990`](https://drive.google.com/file/d/1q9vq-UDto2NGURB8O0R8LmScrAJAnHrH/view) | Illumination | Lighting enable, brightness, color temperature, and levels. | | Not Planned / Excluded | **Not Planned**: on/off, brightness, color temperature, effective-max readout, and level tables/presets (need editor and paged-write design). **Excluded**: raw bookkeeping without user value. |
| `0x19B0` | Haptic | Native feedback settings and waveform playback. | | Not Planned / Excluded | **Not Planned**: feedback enable/intensity (needs command verification). **Excluded**: waveform playback (direct effect triggering). |
| `0x19C0` | Force-sensing Buttons | Physical button activation force. | | Not Planned | Per-button activation force; needs command verification. |
| `0x1A00` | Presenter Control | Presenter-specific controls. | | Not Planned | Needs documented controls that map to standard USB input. |
| `0x1A01` | 3D Sensor | Three-dimensional sensor data. | | Not Planned | Needs documented semantics and a standard keyboard/mouse use beyond raw readouts. |
| `0x1B00` | Reprogrammable Controls (legacy) | Older control enumeration/reporting. | | Not Planned | Needs revision-specific protocol evidence and fixed standard USB mappings. |
| `0x1B01` | Reprogrammable Controls V2 | Older control enumeration/reporting. | | Not Planned | Needs revision-specific protocol evidence and fixed standard USB mappings. |
| `0x1B02` | Reprogrammable Controls V2.2 | Older control enumeration/reporting. | | Not Planned | Needs revision-specific protocol evidence and fixed standard USB mappings. |
| `0x1B03` | Reprogrammable Controls V3 | Older control enumeration/reporting. | | Not Planned | Needs revision-specific protocol evidence and fixed standard USB mappings. |
| [`0x1B04`](https://drive.google.com/file/d/1t1u43_bnT7r4RgYImjpjekZ5jfThCeNL/view) | Reprogrammable Controls | Special keys/buttons, task IDs, reporting, and mappings. | [input client][hidpp], [translations][translation] | Implemented / Not Planned / Excluded | **Implemented**: fixed keyboard, Consumer, System, and held-shortcut mappings (native reporting preserved when representable). **Not Planned**: custom assignments (needs UI and persistence design). **Excluded**: unjustified diversion of working native keys/buttons or raw controls without complete USB translation. |
| `0x1B05` | Full Key Customization | Keyboard assignment customization. | | Not Planned | Needs command research, mapping editor, and persistence design. |
| `0x1B0C` | Analog Button Tuning | Actuation depth, rapid trigger, and click haptics. | | Not Planned | Needs command verification; per-key tuning also requires a dedicated editor. |
| `0x1B10` | Control List | Device control inventory. | | Not Planned | Needs command research and a concrete consumer for the metadata. |
| `0x1B20` | Switch Swapability | Cataloged switch-swap facility. | | Not Planned | Needs research into command semantics and in-scope use. |
| `0x1B30` | Device Mode | Cataloged operating-mode facility. | | Not Planned | Needs research into modes, persistence, and command semantics. |
| `0x1BC0` | Report HID Usage | Cataloged HID usage reporting facility. | | Not Planned | Needs command semantics and a complete standard USB translation use. |
| [`0x1C00`](https://drive.google.com/file/d/1REDgXc_t7b4Wun2MIGfSDuEA5c12seJ5/view) | Persistent Remappable Action | Store native key/button actions. | | Not Planned / Excluded | **Not Planned**: current-connection custom assignments and assignment-only default reset (needs mapping UI, persistence, and reset-scope verification). **Excluded**: other-host assignments and resets. |
| [`0x1D4B`](https://drive.google.com/file/d/1ORLJ0pEiR5gkkAjv0961wlbGJQ-2s-5G/view) | Wireless Device Status | Notify reconnection and request software reconfiguration. | | Not Planned | Evaluate whether Bluetooth links ever miss a reconfiguration request; no dedicated handler currently. |
| `0x1DF0` | Remaining Pairing | Remaining pairing slot count. | | Excluded | Pairing-slot bookkeeping is outside current-connection configuration. |
| `0x1E00` | Enable Hidden Features | Expose hidden device features. | | Excluded | Hidden/engineering features must not be enabled. |
| `0x1F1F` | Firmware Properties | Firmware property metadata. | | Not Planned | Needs useful fields beyond `0x0003` and verified commands. |
| `0x1F20` | ADC Measurement | Battery measurement and rev 2+ auto power-off. | [handlers][handlers] (`Engine::battery`, `Handler::Power`) | Implemented | Battery full/charging status; auto power-off (`0-15300` s in 60 s steps; `0` = never) on rev 2+ keyboards/combos. |
| `0x2001` | Left/Right Swap | Swap primary and secondary mouse buttons. | | Not Planned | Needs verification of native stored-setting commands and capabilities. |
| `0x2005` | Swap Button Cancel | Control button-swap cancellation behavior. | | Not Planned | Needs research into command semantics and stored-preference suitability. |
| `0x2006` | Pointer Axis Orientation | Configure pointer axis orientation. | | Not Planned | Needs verification of native stored-setting commands and capabilities. |
| [`0x2100`](https://drive.google.com/file/d/1zs9fVDF3X00WNKEE-fndWJuL_kiDUc_h/view) | Vertical Scrolling | Read roller type, ratchets per turn, and suggested speed. | | Not Planned / Excluded | **Not Planned**: wheel readouts (pending UI and device verification). **Excluded**: applying host OS scroll-speed suggestions. |
| [`0x2110`](https://drive.google.com/file/d/1MDt_PuLK8VKss5Mxa16XDvvb3ASmoR2_/view) | SmartShift | Wheel free-spin/ratchet mode and auto-disengage threshold. | [handlers][handlers] (`Handler::Wheel`) | Implemented / Not Planned | **Implemented**: mode and current threshold. **Not Planned**: stored default-threshold editing. |
| [`0x2111`](https://drive.google.com/file/d/1NgLzd_Dol0zDOZvJTN5_1lvOAWcxJnwJ/view) | Enhanced SmartShift | SmartShift with ratchet torque controls. | | Not Planned | Mode, threshold, torque, and default/max-force readouts. |
| [`0x2120`](https://drive.google.com/file/d/1zs9fVDF3X00WNKEE-fndWJuL_kiDUc_h/view) | High-resolution Scrolling | Select high-resolution wheel reporting. | | Not Planned | Native resolution selection (not diversion); needs verified benefit and standard USB scaling. |
| [`0x2121`](https://drive.google.com/file/d/1WEfouBszkLA3Dl2WRIhzqJqyWZL5wKMJ/view) | High-resolution Wheel | Wheel inversion, resolution, routing, analytics, and geometry. | [handlers][handlers] (`Handler::Hires`) | Implemented / Not Planned / Excluded | **Implemented**: inversion and multiplier/ratchet-count/diameter info. **Not Planned**: native resolution settings (needs verified benefit, startup mode, and USB scaling). **Excluded**: wheel diversion without demonstrated need; opaque analytics. |
| [`0x2130`](https://lekensteyn.nl/files/logitech/x2130_ratchetwheel.html) | Ratchet Wheel | Choose native/diverted wheel reporting. | | Excluded | Wheel diversion has no demonstrated missing capability over native HID scrolling. |
| [`0x2150`](https://drive.google.com/file/d/1Op9dBdPkXfJWhwL7bXjvvhORDH9C9Eft/view) | Thumbwheel | Thumbwheel inversion and reporting configuration. | [handlers][handlers] (`Handler::Thumb`) | Implemented / Excluded | **Implemented**: inversion (requires native routing). **Excluded**: diversion and decoder (no demonstrated need over native horizontal scroll). |
| [`0x2200`](https://drive.google.com/file/d/0BxbRzx7vEV7eeXJIS01aSlBVbkU/view) | Mouse Pointer | Read nominal sensor DPI and host processing hints. | | Not Planned / Excluded | **Not Planned**: nominal sensor readouts (no DPI setter exists). **Excluded**: host OS acceleration/orientation hints. |
| [`0x2201`](https://lekensteyn.nl/files/logitech/x2201_adjustabledpi.html) | Adjustable DPI | Sensor DPI choices or ranges. | [handlers][handlers] (`Handler::Dpi0`, `Dpi1`) | Implemented / Not Planned | **Implemented**: DPI read/write for up to 2 sensors. **Not Planned**: coverage beyond 2 sensors. |
| [`0x2202`](https://drive.google.com/file/d/1kb1tnTvznLFrfHdHL271A5iQqJCi7yM-/view) | Extended Adjustable DPI | Per-sensor X/Y DPI, lift-off, calibration, and DPI LED. | | Not Planned / Excluded | **Not Planned**: X/Y DPI, lift-off, and ongoing calibration correction (needs verified adapter-managed host mode and persistence). **Excluded**: interactive calibration and temporary DPI LED triggers (one-shot commands). |
| [`0x2205`](https://lekensteyn.nl/files/logitech/x2205_pointermotionscaling.html) | Pointer Motion Scaling | Device-side pointer sensitivity multiplier. | | Not Planned | Stored pointer scaling with readable units. |
| `0x2230` | Angle Snapping | Configure pointer angle snapping. | | Not Planned | Needs verification of native stored-setting commands and capabilities. |
| `0x2240` | Surface Tuning | Pointer sensor surface calibration. | | Not Planned / Excluded | **Not Planned**: ongoing surface-tuning preferences (needs command/persistence verification). **Excluded**: interactive one-shot calibration. |
| `0x2250` | XY Statistics | Pointer movement statistics. | | Excluded | Raw movement analytics lack useful configuration or status value. |
| `0x2251` | Wheel Statistics | Wheel movement statistics. | | Excluded | Raw wheel analytics are out of scope. |
| `0x2400` | Hybrid Tracking | Configure hybrid sensor tracking. | | Not Planned | Needs verification of native stored-setting commands and capabilities. |
| `0x40A0` | Legacy Fn Inversion | Function keys vs. special actions by default. | [handlers][handlers] (`Handler::Fn`) | Implemented | Saved legacy Fn-row preference with readback. |
| [`0x40A2`](https://docs.google.com/document/d/1nH63NvSTDFlFIoGOT9sozDvSTwsTFmvqYwPcygWnOq4/edit) | Fn Inversion | Function-row default behavior. | [handlers][handlers] (`Handler::Fn`) | Implemented | Read, write, and readback; preserves separate device default-state field. |
| [`0x40A3`](https://drive.google.com/file/d/1TU1adbjmom4FUcAzZYpwZZi5mrkPBsU5/view) | Multi-host Fn Inversion | Function-row preference per host. | [handlers][handlers] (`Handler::Fn`) | Implemented / Excluded | **Implemented**: current-host read/write (`0xFF` host-first) with readback; devices rejecting `0xFF` report a failure without slot fallback. **Excluded**: editing other host slots. |
| `0x4100` | Encryption | Read link encryption info. | | Not Planned | Confirm Bluetooth device support and commands; Diagnostics readout candidate. |
| `0x4220` | Lock Key State | Proprietary lock-key state reporting. | | Not Planned / Excluded | **Not Planned**: status reporting (needs purpose beyond standard HID lock LEDs). **Excluded**: direct lock-state overrides. |
| [`0x4301`](https://drive.google.com/file/d/0BxbRzx7vEV7eR1Zra3VNaUlmOVE/view) | Solar Dashboard | Solar keyboard battery and indicator checks. | [handlers][handlers] (`Engine::battery`) | Implemented / Excluded | **Implemented**: battery percentage reporting. **Excluded**: solar-check indicator overrides. |
| `0x4520` | Keyboard Layout | Keyboard layout metadata or selection. | | Not Planned | Needs research on read/write semantics, persistence, and choices. |
| [`0x4521`](https://drive.google.com/file/d/1hAVtnJxQMo9UnkniFN4vOUYPFPCUGCDE/view) | Disable Keys | Disable advertised lock, Insert, and Windows keys. | | Not Planned | Supported disable switches while preserving unrelated bits. |
| [`0x4522`](https://drive.google.com/file/d/1PAs6A_oN3Z5eqjnSL_VJMuoY0XlmRAM4/view) | Disable Keys by HID Usage | Disable an arbitrary set of keys. | | Not Planned | Requires key-list UI and verified add/remove persistence. |
| `0x4523` | Disable Controls | Disable selected keyboard controls. | | Not Planned | Needs command research and a control-selection UI. |
| [`0x4530`](https://drive.google.com/file/d/11CXizK2_yeH9eOwBeQlQRrC0gAw_Hrfl/view) | Dual Platform | Configure platform-specific keyboard behavior. | [handlers][handlers] (`Handler::Platform`) | Implemented | Saved platform preference (Windows/Android and macOS/iOS share hardware modes). |
| [`0x4531`](https://drive.google.com/file/d/1KyiBA5m_5V1s6jQ9eQrgRJN0SbbbI9_I/view) | Multi Platform | Configure keyboard platform per host. | [handlers][handlers] (`Handler::Platform`) | Implemented / Excluded | **Implemented**: current-host preference using advertised capability descriptors covering all OS versions; writes require platform-selection capability. **Excluded**: other host slots and raw slot bookkeeping. |
| `0x4540` | Keyboard International Layouts | Additional keyboard layout facility. | | Not Planned | Needs research on read/write semantics, persistence, and choices. |
| [`0x4600`](https://drive.google.com/file/d/1nStBTT3rdTmPPeJWFflNdBWlC_E0-0SM/view) | Crown | Crown mechanical mode, reporting, timing, and geometry. | | Not Planned / Excluded | **Not Planned**: free/ratchet mode, native timing settings, and slot/ratchet diagnostics (needs verified native HID benefit). **Excluded**: unjustified crown diversion. |
| `0x6010` | Touchpad Firmware Items | Touchpad firmware properties. | | Not Planned | Needs documented fields and useful native preferences. |
| `0x6011` | Touchpad Software Items | Touchpad software properties. | | Not Planned | Needs documented fields; host-dependent runtime behavior is excluded. |
| `0x6012` | Touchpad Win8 Firmware Items | Windows-oriented touchpad firmware properties. | | Not Planned | Needs documented fields and proof of autonomous standard USB behavior. |
| `0x6020` | Tap Enable | Native tap-to-click behavior. | | Not Planned | Needs verification of native stored-setting commands and capabilities. |
| `0x6021` | Tap Enable Extended | Extended native tap-to-click behavior. | | Not Planned | Needs verification of native stored-setting commands and capabilities. |
| `0x6030` | Cursor Ballistic | Native cursor acceleration. | | Not Planned | Needs verification of native stored-setting commands and capabilities. |
| `0x6040` | Touchpad Resolution | Touchpad pointer resolution. | | Not Planned | Needs verification of native stored-setting commands and capabilities. |
| [`0x6100`](https://drive.google.com/file/d/1B8uKxWyjBD7-0G47k5N1KXNwqbutz0jv/view) | Touchpad Raw XY | Raw touch position reporting. | | Excluded | Raw replacement of working native pointer/gestures lacks justification and complete USB translation. |
| [`0x6110`](https://drive.google.com/file/d/0BxbRzx7vEV7eV3R0eXE2ZGprVkU/view) | Touchmouse Raw Points | Raw touch points and reporting selection. | | Excluded | Raw replacement of working native pointer/gestures lacks justification and complete USB translation. |
| `0x6120` | Bluetooth Touchmouse Settings | Bluetooth touchmouse configuration. | | Not Planned | Unverified command semantics and in-scope use. |
| `0x6500` | Gestures | Older native gesture configuration. | | Not Planned / Excluded | **Not Planned**: native stored preferences (needs research). **Excluded**: unjustified diversion of working native gestures. |
| `0x6501` | Gestures 2 | Native gesture enablement, parameters, and diversion. | | Not Planned / Excluded | **Not Planned**: native enable switches, numeric parameters, and custom translations (needs mapping UI). **Excluded**: unjustified diversion or standalone diversion toggles without translation. |
| `0x8010` | G Keys | G/M-key reporting and assignments. | | Not Planned / Excluded | **Not Planned**: custom assignments and controls lacking usable native reports (needs mapping UI and complete USB translation). **Excluded**: unjustified diversion of working native keys or standalone diversion toggles. |
| `0x8020` | M Keys | Profile-selection key LEDs. | | Excluded | Direct LED overrides are outside ongoing configuration. |
| `0x8030` | MR Key | Macro-record key LED. | | Excluded | Direct LED overrides are outside ongoing configuration. |
| [`0x8040`](https://drive.google.com/file/d/1EIiHKilvLXxFdZJMh2UrvfqYffVyRfH7/view) | Brightness Control | Illumination enable and brightness. | | Not Planned | On/off and brightness within advertised bounds. |
| `0x8051` | Logitech Modifiers | Device-specific modifier facility. | | Not Planned | Needs protocol evidence and standard USB translation or stored native behavior. |
| [`0x8060`](https://drive.google.com/file/d/1PRpmCzYFQDcDnXe9l1_xxVpTVSOcsvR4/view) | Report Rate | Select device reporting interval. | | Not Planned | Needs measured Bluetooth/USB rate benefit and verified adapter-managed host mode (required by spec for writes). |
| [`0x8061`](https://drive.google.com/file/d/1WF_F5bkVRqPx_dzv6TSFbDdQICDlgFjI/view) | Extended Report Rate | Select transport-specific reporting rates. | | Not Planned | Rev 0 defines wired/Lightspeed selectors only; writes require host mode and may not persist. |
| [`0x8070`](https://drive.google.com/file/d/1vdRwDOmaDpHwusJoDsGxe2Q4XBmNr1bb/view) | Color LED Effects | Zone effects, colors, parameters, animations, and ownership. | | Not Planned / Excluded | **Not Planned**: autonomous device-run zone effects, persistence, startup/demo prefs, and required setup ownership (needs verified readback/reapplication). **Excluded**: streamed effects, LED-bin calibration, and live color polling. |
| [`0x8071`](https://drive.google.com/file/d/1lecrJQgAC7wnXlo8PEnwVSb3L0GioALG/view) | RGB Effects | Zone effects, colors, animations, dimming, timeouts, and patterns. | | Not Planned / Excluded | **Not Planned**: autonomous device-run zone effects, timeouts, dimming, and patterns (needs editor and verified autonomous setup). **Excluded**: streamed effects, runtime sync, LED-bin calibration, and shutdown triggers. |
| `0x807A` | RPM Indicator | Engine-speed indication. | | Excluded | Racing-wheel/game-output feature; outside standard keyboard, mouse, and gamepad scope. |
| `0x807B` | RPM LED Pattern | Engine-speed LED patterns. | | Excluded | Racing-wheel/game-output feature; outside standard keyboard, mouse, and gamepad scope. |
| `0x8080` | Legacy Per-key Lighting | Configure individual key lighting. | | Not Planned | Requires per-key editor and verified autonomous persistence. |
| [`0x8081`](https://drive.google.com/file/d/1tKUqcL1dXKj69nuejgOrzoQ8Xq69RO5v/view) | Per-key Lighting | Configure individual keys/zones or submit frames. | | Not Planned / Excluded | **Not Planned**: static per-key config (needs editor and persistence verification; rev 0 persistence flags behave identically). **Excluded**: streamed lighting frames. |
| [`0x8090`](https://drive.google.com/file/d/1sDIVLyckdp-28xkV_jPGEi46wf0wD-Fa/view) | Mode Status | Performance/endurance operating mode. | | Not Planned | Needs a Bluetooth device advertising software switching and a measured Bluetooth/USB benefit. |
| `0x80A3` | Legacy Axis Response Curve | Legacy gaming-axis response curve. | | Excluded | Racing-wheel/pedal feature; outside standard keyboard, mouse, and gamepad scope. |
| `0x80A4` | Axis Response Curve | Gaming-axis response curve. | | Excluded | Racing-wheel/pedal feature; outside standard keyboard, mouse, and gamepad scope. |
| `0x80B1` | Banded Axis | Banded gaming-axis behavior. | | Excluded | Racing-wheel/pedal feature; outside standard keyboard, mouse, and gamepad scope. |
| `0x80D0` | Combined Pedals | Combined pedal axes. | | Excluded | Racing-wheel/pedal feature; outside standard keyboard, mouse, and gamepad scope. |
| `0x80E0` | Bunny Hopping | Cataloged gaming behavior. | | Not Planned | Needs device-class and command research; timed input automation is excluded. |
| `0x8100` | Onboard Profiles | Select device-owned profiles or host-controlled mode. | | Not Planned / Excluded | **Not Planned**: device-owned profile selection and adapter-managed host mode (needs verification that standard USB input and stored prefs stay intact). **Excluded**: host-runtime software or unjustified diversion. |
| `0x8101` | Profile Management | Additional profile management facility. | | Not Planned | Needs device-owned operation semantics and a management UI. |
| `0x8110` | Mouse Button Filter / Spy | Cataloged mouse-button facility (varies by index). | | Not Planned | Resolve naming and command semantics before selecting any operation. |
| `0x8111` | Latency Monitoring | Input latency monitoring. | | Not Planned | Needs trustworthy measurements, Bluetooth applicability, and useful Diagnostics output. |
| `0x8120` | Gaming Attachments | Gaming attachment functions. | | Excluded | Gaming attachments are outside standard keyboard, mouse, and gamepad scope. |
| `0x8123` | Force Feedback | Force-feedback effects. | | Excluded | Direct effect control; outside standard keyboard, mouse, and gamepad scope. |
| `0x8127` | Dual Clutch | Dual clutch behavior. | | Excluded | Racing-wheel/pedal feature; outside standard keyboard, mouse, and gamepad scope. |
| `0x812C` | Wheel Center Position | Racing-wheel center position. | | Excluded | Racing-wheel/pedal feature; outside standard keyboard, mouse, and gamepad scope. |
| `0x8130` | Display Game Data | Game-data display. | | Excluded | Game-output feature; outside standard keyboard, mouse, and gamepad scope. |
| `0x8131` | Center Spring | Racing-wheel centering force. | | Excluded | Racing-wheel/pedal feature; outside standard keyboard, mouse, and gamepad scope. |
| `0x8132` | Axis Mapping | Gaming-axis mapping. | | Excluded | Racing-wheel/pedal feature; outside standard keyboard, mouse, and gamepad scope. |
| `0x8133` | Global Damping | Racing-wheel damping. | | Excluded | Racing-wheel/pedal feature; outside standard keyboard, mouse, and gamepad scope. |
| `0x8134` | Brake Force | Pedal brake force. | | Excluded | Racing-wheel/pedal feature; outside standard keyboard, mouse, and gamepad scope. |
| `0x8135` | Pedal Status | Pedal state information. | | Excluded | Racing-wheel/pedal feature; outside standard keyboard, mouse, and gamepad scope. |
| `0x8136` | Torque Limit | Racing-wheel torque limit. | | Excluded | Racing-wheel/pedal feature; outside standard keyboard, mouse, and gamepad scope. |
| `0x8137` | Configuration Profiles | Racing-wheel configuration profiles. | | Excluded | Racing-wheel/pedal feature; outside standard keyboard, mouse, and gamepad scope. |
| `0x8138` | Operating Range | Racing-wheel operating range. | | Excluded | Racing-wheel/pedal feature; outside standard keyboard, mouse, and gamepad scope. |
| `0x8139` | TrueForce | Racing-wheel force-feedback facility. | | Excluded | Racing-wheel/pedal feature; outside standard keyboard, mouse, and gamepad scope. |
| `0x8140` | Force-feedback Filter | Force-feedback filtering. | | Excluded | Racing-wheel/pedal feature; outside standard keyboard, mouse, and gamepad scope. |
| [`0x8300`](https://drive.google.com/file/d/1OpyACtEjPoQK17aaW7Wwk70aGBHNHtOH/view) | Sidetone | Microphone monitoring volume and mute. | | Excluded | Headset/audio feature; outside standard keyboard, mouse, and gamepad scope. |
| [`0x8310`](https://drive.google.com/file/d/1Sk5q95tU0qgNtX3v-fJ8tmYYSeSUYLBI/view) | Audio Equalizer | Audio bands, gain, and microphone processing. | | Excluded | Headset/audio feature; outside standard keyboard, mouse, and gamepad scope. |
| `0x8320` | Headset Output | Headset output configuration. | | Excluded | Headset/audio feature; outside standard keyboard, mouse, and gamepad scope. |

## Related Protocols and Input

These operations are outside the HID++ 2.0 feature table.

| Protocol or Operation | Description | Implementation | Status | Operations and Status Reasons |
| --- | --- | --- | --- | --- |
| HID++ 1.0 register `0x00` | Battery notification enable flags. | [legacy battery][link], [register transport][hidpp] | Implemented | Internal battery notification setup; preserves unrelated flag bits. |
| HID++ 1.0 register `0x0D` | Battery percentage and charging. | [legacy battery][link] | Implemented | Read, notifications, and periodic polling. |
| HID++ 1.0 register `0x07` | Coarse battery and charging. | [legacy battery][link] | Implemented | Fallback when register `0x0D` is rejected as invalid. |
| USB System Control and Radio input | Standard USB output for System actions and native radio/slider input. | [HID decoder](../rust/crates/cordial-core/src/hid.rs), [forwarder](../rust/crates/cordial-core/src/forward.rs), [USB descriptor](../rust/adapters/usb/src/descriptor.rs) | Implemented | Dedicated reports preserve simultaneous presses and releases. Radio and rotation-lock buttons preserve press edges; sliders report latest known state (null until known or after disconnect); relative slider toggles emit momentary button presses. |
| Standard key and Consumer translation | Fixed mappings for Logitech special controls. | [input client][hidpp], [translation table][translation] | Implemented / Excluded | **Implemented**: media, editing, navigation, application-launch usages, and System sleep; native reporting preserved when representable. **Excluded**: host command execution, typed launch sequences, and timed macros. |

## Centurion and Audio Catalog

Centurion is a separate protocol with no implementation in Cordial. Generic device features remain Not Planned pending an applicable Bluetooth keyboard or mouse and command documentation; audio, headset, mixer, and microphone features are Excluded.

| Feature ID | Catalog Feature | Description | Status | Reason |
| --- | --- | --- | --- | --- |
| `0x0100` | `CENTURION DEVICE INFO` | Device identity. | Not Planned | Needs applicable keyboard/mouse and documented fields. |
| `0x0101` | `CENTURION DEVICE NAME` | Device name. | Not Planned | Needs applicable keyboard/mouse and documented operations. |
| `0x0102` | `CENTURION ROOT` | Protocol entry point. | Not Planned | Needs applicable keyboard/mouse and documented discovery. |
| `0x0103` | `CENTURION MEMFAULT` | Fault diagnostics. | Not Planned | Needs applicable keyboard/mouse, docs, and useful reporting. |
| `0x0104` | `CENTURION BATTERY SOC` | Battery state of charge. | Not Planned | Needs applicable keyboard/mouse and documented reporting. |
| `0x0108` | `CENTURION AUTO SLEEP` | Sleep timing. | Not Planned | Needs applicable keyboard/mouse and verified reapplication. |
| `0x010A` | `CENTURION GENERIC DFU` | Firmware updates. | Excluded | One-shot firmware update; outside configuration scope. |
| `0x0110` | `CENTURION LED BRIGHTNESS` | LED brightness. | Not Planned | Needs applicable keyboard/mouse and verified reapplication. |
| `0x0115` | `CENTURION EU POWER MODE` | Regional power mode. | Not Planned | Needs applicable keyboard/mouse and documented preferences. |
| `0x0116` | `CENTURION DEVICE BOOL STATE` | Boolean device state. | Not Planned | Needs applicable keyboard/mouse, docs, and useful purpose. |
| `0x0200` | `HEADSET VOLUME` | Headset volume. | Excluded | Audio/headset feature; outside standard keyboard, mouse, and gamepad scope. |
| `0x0201` | `HEADSET EQ` | Headset equalizer. | Excluded | Audio/headset feature; outside standard keyboard, mouse, and gamepad scope. |
| `0x020D` | `HEADSET ADVANCED PARA EQ` | Parametric equalizer. | Excluded | Audio/headset feature; outside standard keyboard, mouse, and gamepad scope. |
| `0x020E` | `HEADSET MIC TEST` | Headset mic test. | Excluded | Audio/headset feature; outside standard keyboard, mouse, and gamepad scope. |
| `0x0213` | `HEADSET EQ STYLES` | Equalizer presets. | Excluded | Audio/headset feature; outside standard keyboard, mouse, and gamepad scope. |
| `0x0305` | `BT HOST INFO` | Bluetooth host records. | Excluded | Audio/headset and host-slot scope. |
| `0x0309` | `LIGHTSPEED PAIRING` | Lightspeed pairing. | Excluded | Audio/headset and pairing scope. |
| `0x030A` | `BT GAMING MODE` | Bluetooth gaming mode. | Not Planned | Needs applicable keyboard/mouse, docs, and demonstrated adapter-only benefit. |
| `0x0600` | `HEADSET RGB EFFECTS` | Headset RGB effects. | Excluded | Audio/headset feature; outside standard keyboard, mouse, and gamepad scope. |
| `0x0601` | `HEADSET MIC MUTE` | Headset mic mute. | Excluded | Audio/headset feature; outside standard keyboard, mouse, and gamepad scope. |
| `0x0602` | `HEADSET MIC SNR` | Microphone SNR control. | Excluded | Audio/headset feature; outside standard keyboard, mouse, and gamepad scope. |
| `0x0604` | `HEADSET AUDIO SIDETONE` | Headset sidetone. | Excluded | Audio/headset feature; outside standard keyboard, mouse, and gamepad scope. |
| `0x0607` | `HEADSET HOST SWITCH` | Headset host switch. | Excluded | Audio/headset and host-switching scope. |
| `0x0609` | `HEADSET MIX` | Headset mix. | Excluded | Audio/headset feature; outside standard keyboard, mouse, and gamepad scope. |
| `0x060B` | `HEADSET TONES` | Headset tones. | Excluded | Audio/headset feature; outside standard keyboard, mouse, and gamepad scope. |
| `0x060D` | `HEADSET NOISE EXPOSURE` | Noise exposure. | Excluded | Audio/headset feature; outside standard keyboard, mouse, and gamepad scope. |
| `0x060E` | `HEADSET AI NOISE REDUCTION` | AI noise reduction. | Excluded | Audio/headset feature; outside standard keyboard, mouse, and gamepad scope. |
| `0x0611` | `HEADSET MIC GAIN` | Headset mic gain. | Excluded | Audio/headset feature; outside standard keyboard, mouse, and gamepad scope. |
| `0x0617` | `HEADSET USAGE TRACKING` | Usage tracking. | Excluded | Audio/headset feature; outside standard keyboard, mouse, and gamepad scope. |
| `0x0618` | `HEADSET BATTERY SAVER` | Headset battery saver. | Excluded | Audio/headset feature; outside standard keyboard, mouse, and gamepad scope. |
| `0x0620` | `HEADSET RGB HOSTMODE` | Software-controlled RGB. | Excluded | Audio/headset feature; outside standard keyboard, mouse, and gamepad scope. |
| `0x0621` | `HEADSET RGB ONBOARD EFFECTS` | Onboard RGB effects. | Excluded | Audio/headset feature; outside standard keyboard, mouse, and gamepad scope. |
| `0x0622` | `HEADSET RGB SIGNATURE EFFECTS` | Signature RGB effects. | Excluded | Audio/headset feature; outside standard keyboard, mouse, and gamepad scope. |
| `0x0631` | `HEADSET DO NOT DISTURB` | Do not disturb. | Excluded | Audio/headset feature; outside standard keyboard, mouse, and gamepad scope. |
| `0x0634` | `CENTURION ONBOARD PROFILES` | Onboard profiles. | Not Planned | Needs applicable keyboard/mouse and documented autonomous selection. |
| `0x0635` | `HEADSET RGB STREAMING` | Headset RGB streaming. | Excluded | Audio/headset feature; outside standard keyboard, mouse, and gamepad scope. |
| `0x0636` | `HEADSET ONBOARD EQ` | Headset onboard EQ. | Excluded | Audio/headset feature; outside standard keyboard, mouse, and gamepad scope. |
| `0x0800` | `MIXER AUDIO` | Mixer audio. | Excluded | Audio/headset feature; outside standard keyboard, mouse, and gamepad scope. |
| `0x0801` | `MIXER MIC` | Mixer mic. | Excluded | Audio/headset feature; outside standard keyboard, mouse, and gamepad scope. |
| `0x0900` | `LOGIVOICE` | Microphone voice processing. | Excluded | Audio/headset feature; outside standard keyboard, mouse, and gamepad scope. |
| `0x0901` | `LOGIVOICE NOISE REDUCTION` | Logivoice noise reduction. | Excluded | Audio/headset feature; outside standard keyboard, mouse, and gamepad scope. |
| `0x0902` | `LOGIVOICE NOISE GATE` | Logivoice noise gate. | Excluded | Audio/headset feature; outside standard keyboard, mouse, and gamepad scope. |
| `0x0903` | `LOGIVOICE COMPRESSOR` | Logivoice compressor. | Excluded | Audio/headset feature; outside standard keyboard, mouse, and gamepad scope. |
| `0x0904` | `LOGIVOICE DE ESSER` | Logivoice de-esser. | Excluded | Audio/headset feature; outside standard keyboard, mouse, and gamepad scope. |
| `0x0905` | `LOGIVOICE DE POPPER` | Logivoice de-popper. | Excluded | Audio/headset feature; outside standard keyboard, mouse, and gamepad scope. |
| `0x0906` | `LOGIVOICE LIMITER` | Logivoice limiter. | Excluded | Audio/headset feature; outside standard keyboard, mouse, and gamepad scope. |
| `0x0907` | `LOGIVOICE HIGH PASS FILTER` | Logivoice high-pass filter. | Excluded | Audio/headset feature; outside standard keyboard, mouse, and gamepad scope. |
| `0x0908` | `LOGIVOICE EQUALIZER` | Logivoice equalizer. | Excluded | Audio/headset feature; outside standard keyboard, mouse, and gamepad scope. |
| `0x0909` | `LOGIVOICE AINR` | AI noise reduction. | Excluded | Audio/headset feature; outside standard keyboard, mouse, and gamepad scope. |
| `0x0B01` | `METERING` | Audio level metering. | Excluded | Audio/headset feature; outside standard keyboard, mouse, and gamepad scope. |
| `0x0B02` | `MIC GAIN AUTO MODE` | Automatic microphone gain. | Excluded | Audio/headset feature; outside standard keyboard, mouse, and gamepad scope. |

Note: `0xFE00` (Mouse Gesture) in some catalogs is an application-internal ID, not a device feature.

## Protocol Behavior

HID++ settings requests share one transport owner and single in-flight exchange slot with input normalization. Parameter byte offsets below exclude the 3-byte `(device, feature, function)` header; multi-byte values are big-endian unless noted. Software ID `0` identifies asynchronous notifications and never completes a request.

Revisions listed below denote the implemented command layouts, not maximum accepted revisions (newer compatible revisions continue to work). Unknown feature IDs, settings, and option values are never interpreted, and hidden/engineering features are ignored.

### Implemented Command Layouts

| Feature & Revisions | Operations and Fields | Constraints and Preservation |
| --- | --- | --- |
| Root `0x0000` | Ping returns protocol major/minor. Fn `0` maps a 2-byte feature ID to `(index, flags, revision)`. | No model-name assumptions. Read-only discovery runs even while HID++ integration is off. Settings proceed after ping even without `0x0020`/`0x1B04`. |
| FeatureSet `0x0001` (rev 0-2) | Fn `0` returns non-root count. Fn `1(index)` returns ID bytes `0..1`, flags byte `2`, and revision byte `3` (rev 1+). Rev 0 queries Root for each visible feature's revision. | Inventory includes Root (max 256 entries). Hidden/engineering features are marked unsupported and never queried. Unknown IDs stay inventory-only. |
| DeviceInfo `0x0003` (rev 0-4) | Fn `0` returns entity count (rev 1 adds unit ID/transport/dense model ID; rev 2 extended model ID; rev 3 motor-drive entity; rev 4 serial capability + Fn `2` for 12-char serial). Fn `1` reads entity info (including raw SoftDevice builds). | Reads up to 8 entities; reports first application firmware (`identity.firmware_version`), bootloader (`identity.bootloader_version`), and hardware revision (`identity.hardware_revision`). Versions are formatted as text (for example, `RQK 12.01.0013`). Serial queried only when advertised. |
| DeviceName `0x0005` (rev 0-2) | Fn `0` length, Fn `1(offset)` name chunk, Fn `2` device type (rev 1-2 add values through 19). | Name capped at 64 bytes with nonprintable bytes sanitized. Types map to keyboard, mouse, or other. |
| Battery `0x1000` (rev 0) | Fn `1` returns level count and flags. Fn `0` / event `0` supply current level, next level, and charge status. | Level `0` is unknown. Percentage reported only with mileage capability and >= 10 levels; coarse levels map to 5%, 20%, 75%, and 100% (boundaries 10%, 30%, 80%). Events update observations only. |
| Fn Inversion `0x40A2` (rev 0) | Fn `0` reads `(state, default_state)`; Fn `1` writes `state`, followed by Fn `0` readback. | `0` = function keys, `1` = special actions. Never writes default-state. Device scope. |
| Multi-host Fn `0x40A3` (rev 0) | Fn `0(0xFF)` reads current host; Fn `1([0xFF, state])` writes, followed by Fn `0` readback. Response/event `0`: `(host, state, default_state, capability_mask)`. | Uses host-first byte order matching vendor signature and [Solaar](https://github.com/pwr-Solaar/Solaar/blob/master/lib/logitech_receiver/settings_templates.py). Devices rejecting `0xFF` (such as MX Keys S firmware quirks) fail cleanly without probing other slots or byte orders. |
| Legacy Fn `0x40A0` (rev 0) | Fn `0` getter, Fn `1` setter with Fn `0` readback (following [Solaar](https://github.com/pwr-Solaar/Solaar/blob/master/lib/logitech_receiver/settings_templates.py)). | Maps to `KeyboardSettings.function_row`. Priority order: `0x40A3`, then `0x40A2`, then `0x40A0`. |
| Dual Platform `0x4530` (rev 0) | Fn `1` getter, Fn `2` setter (`0` = macOS/iOS, `1` = Windows/Android), with readback. | Follows manufacturer spec (Solaar uses Fn `0`; hardware verification pending). OS aliases preserve the user's saved choice while selecting the shared hardware mode. |
| Multi Platform `0x4531` (rev 1) | Fn `0` capabilities/descriptor counts/current host; Fn `1` enumerates OS descriptors; Fn `2(0xFF)` getter; Fn `3([0xFF, platform])` setter. | Current connection only. Choices require an all-versions descriptor agreeing with every descriptor naming that OS. Writability gated on platform-selection capability. Other-host events ignored. |
| ADC Power `0x1F20` (rev 2+) | Fn `1` reads timeout minutes; Fn `2` writes minutes with readback (following [Solaar](https://github.com/pwr-Solaar/Solaar/blob/master/lib/logitech_receiver/settings_templates.py)). | Exposed as `power.auto_off` (`0-15300` s, step 60; `0` = never). Requires advertised keyboard or keyboard/mouse type. |
| Backlight `0x1982` (rev 0-3) | Fn `2` level count/status; Fn `0` enabled (`[0]`), options (`[1]`), capabilities (`[2]`); Fn `1` writes enabled/options. Rev 2 adds effect choices/selector (`0xFF` preserves). Rev 3 getter adds level (`[5]`) and LE16 delays (`[6, 8, 10]`); setter uses level (`[3]`) and delays (`[4, 6, 8]`). | Exposes only advertised fields. Current level/status are read-only; read effect back via Fn `2`. Rev 3 mode (bits 3-4): automatic (`1`), temporary manual (`2`, observable only), permanent manual (`3`, required for level edits). Delays are `1-1440` units of 5 s (`5-7200` s). Merges only requested fields on write. Event `0` triggers at most one coalesced read. |
| AdjustableDPI `0x2201` (rev 0-1) | Fn `0` sensor count; Fn `1(sensor)` echo + BE16 DPI list/range; Fn `2(sensor)` echo/current/default; Fn `3(sensor, BE16 dpi)` setter. | Up to 2 sensors. Lists hold up to 6 ascending values (0-terminated); ranges use `(min, 0xE000 \| step, max, 0)` within `1-57343`. Always reads back after write (rev 0 lacks echo). No notifications. |
| SmartShift `0x2110` (rev 0) | Fn `0` reads `(mode, current_threshold, default_threshold)`; Fn `1` writes all three (`0` = unchanged). | Mode `1` = free-spin, `2` = ratchet. Threshold `1-255` (`255` disables auto-switch). Default-threshold byte always sent as `0`. Saved fields merge into one write. |
| HiResWheel `0x2121` (rev 0-1) | Fn `0` multiplier/capabilities (rev 1 adds ratchets/rotation and diameter); Fn `1` mode (rev 1 adds analytics bit); Fn `2` writes inversion preserving all other bits. Event `1` reports ratchet-switch state. | Decodes `wheel.resolution_multiplier`, `wheel.ratchets_per_rotation`, and `wheel.diameter`. Inversion requires existing native routing and standard resolution; preserves analytics bits without polling or clearing them. |
| Thumbwheel `0x2150` (rev 0) | Fn `0` native/diverted resolution and capabilities; Fn `1` routing (`[0]`) and inversion/touch/proximity (`[1]` bits `0-2`); Fn `2` writes routing and inversion. | Inversion requires native routing. Touch/proximity are observations only. Resolution/scale never modified; diverted motion ignored. |

Battery reporting also supports `0x1001`, `0x1004`, `0x1F20`, and `0x4301` based on advertised features. Keyboard settings are verified with protocol simulations; physical hardware testing remains pending for some operations, notably current-host Fn writes and null-capable two-bit radio/rotation-lock sliders across Windows, macOS, and Linux hosts.

### Settings Application and Storage

- **Read-modify-verify**: Before writing a setting, Cordial reads the device's current state, skips writes whose value already matches, and reads back afterward to confirm. An unanswered setter reports a timeout until the next apply reads the device.
- **Observations vs. saved preferences**: Notifications and **Refresh** update observations without mutating saved preferences or reapplying them. Only explicitly saved preferences enter flash (retaining semantic keys, feature/revision metadata, and validation bounds). Unsaved fields, inventories, and observations are never stored.
- **Disabling HID++ or disconnecting**: Edits saved while HID++ is off are stored on the adapter and applied once HID++ is enabled; read-only discovery, notifications, and Refresh stay available. Disabling HID++ mid-exchange settles the in-flight transaction before resetting temporary reporting. Disconnect cancels pending settings work.
- **Capacity and errors**: Storage space is checked before saving; failed saves queue no device write and report exact persistence status (an unresolved save never claims the prior preference stayed intact).

Firmware owns protocol exchanges, setting keys, validation, and runtime state; clients own labels, categories, units, and user-facing messages (see [Serial API](protocol/README.md)).

## HID++ Normalization

When HID++ integration is enabled for a device, the adapter translates supported special keys into standard USB input without host software. Ordinary HID reports continue on their normal path, preserving the keyboard's PC/Mac mode, Fn-lock, and native modifier combinations.

- **Per-device preference**: Defaults to off. On first connection, Cordial enables it automatically if the read-only probe detects HID++ 2.0+ with usable long reports (an explicit user choice takes precedence; a failed probe retries on a later connection).
- **Adapter platform**: `host_platform` selects `linux` (default), `windows`, or `mac`. Both preferences persist across disconnects, client exits, and reboots. Unpairing removes the device's preferences; pairing does not reset the adapter platform.

| Event | Behavior |
| --- | --- |
| Connect while enabled | Run activation routine. |
| Enable while connected | Run activation routine. |
| Change adapter platform | Save once, then rerun activation on each ready HID++-enabled device (disabled devices untouched; disconnected devices apply on next connect). |
| Disable while connected | Reset temporary reporting (`0x0020`), then allow read-only discovery and settings queries. |
| Connect while disabled | Read-only discovery and settings queries; no resets or setting writes. |
| Toggle while disconnected | Save preference for next connection; no deferred reset queued. |

### Activation and Recovery

Activation looks for a bidirectional HID++ long report (ID `0x11`, 19-byte payload under vendor page `0xFF00` or MX Keys usage `0xFF43:0202`). Normalization requires HID++ 2.0+, Configuration Change (`0x0020`), and Reprogrammable Controls (`0x1B04`); general settings discovery does not require `0x0020` or `0x1B04`.

1. Sends `0x0020` function `1` with parameters `00 00` to reset temporary reporting (leaving factory and persistent settings intact; encoding follows [Logiops](https://github.com/PixlOne/logiops/blob/main/src/logid/backend/hidpp20/features/Reset.cpp) and [Controls rev 4](https://drive.google.com/file/d/1UGDCuqnKBm7U8a6t6g3QlEZgKeaAzmAx/view)).
2. Enumerates controls and enables temporary diversion (`pvalid=0`, preserving persistent diversion; see [controls spec](https://lekensteyn.nl/files/logitech/x1b04_specialkeysmsebuttons.html)) only for supported platform translations and for hotkey/F-key controls (excluding mouse buttons) whose standard USB usage is absent from all native input maps.
3. Repeating an unchanged preference does not restart activation.

Unsupported devices and nonfatal failures keep ordinary HID forwarding running. During a platform switch, prior translations stay active until the reset succeeds; if a reset fails or times out, held translated keys are released to prevent stuck input while still accepting any incoming notifications.

### Translation Tables

All IDs and usages are hexadecimal. `K` = Keyboard/Keypad (`0x07`), `C` = Consumer (`0x0C`), `S` = Generic Desktop (`0x01`), `GUI` = Super (Linux), Windows (Windows), or Command (macOS). `+` denotes a simultaneous held chord, never a timed sequence.

#### Cross-Platform Special Keys

| Control ID | Control | USB Output |
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

#### Fallback Standard Mappings (Only When Native HID Lacks the Usage)

| Control ID | Controls | USB Output |
| --- | --- | --- |
| `0001`-`0007` | Volume up, down, mute, play/pause, next, previous, stop | `C:00E9`, `C:00EA`, `C:00E2`, `C:00CD`, `C:00B5`, `C:00B6`, `C:00B7` |
| `000B`, `000E`, `0011`-`0013`, `0028`, `0032` | Calendar, mail, word processor, spreadsheet, presentation, media player, computer | Standard Consumer application-launch usages (handled by host OS) |
| `000C`, `000D`, `0015`, `0017`, `0019`, `001B` | Close, eject, undo, redo, print, save | `C:0203`, `C:00B8`, `C:021A`, `C:0279`, `C:0208`, `C:0207` |
| `000F`, `0010` | Help, F1 | `C:0095`, `K:003A` |
| `0020`, `0022`, `0024`, `0026` | Favorites, home page, maximize, minimize | `C:022A`, `C:0223`, `C:0205`, `C:0206` |
| `003B`, `003C`, `003E`, `003F`, `0041` | Record, refresh, search, shuffle, browser stop | `C:00B2`, `C:0227`, `C:0221`, `C:00B9`, `C:0226` |
| `0040` | Sleep | `S:0082` |
| `0044`, `0047`, `004B` | Zoom in, zoom out, full screen | `C:022D`, `C:022E`, `C:0230` |
| `004C`-`004F` | Print Screen, Pause, Scroll Lock, Application | `K:0046`, `K:0048`, `K:0047`, `K:0065` |
| `0054`, `0057` | Back, forward | `C:0224`, `C:0225` |
| `00C0`, `00C1`, `0118`, `0119` | Page Down, Page Up, Home, End | `K:004E`, `K:004B`, `K:004A`, `K:004D` |

#### Platform-Specific Mappings

| Control ID | Control | Linux (GNOME) | Windows | macOS |
| --- | --- | --- | --- | --- |
| `00E0` | Window overview | `GUI` | `GUI+Tab` | `C:029F` |
| `00E1` | App overview / action center | `GUI+A` | `GUI+A` | `C:02A0` |
| `006E` | Show desktop | Undiverted | `GUI+D` | `GUI+C:029F` |
| `006F` | Lock screen | `C:019E` | `GUI+L` | `Ctrl+GUI+Q` |
| `00BF` | Screenshot | Print Screen (`K:0046`) | Print Screen (`K:0046`) | `Shift+GUI+3` |
| `00EA` | Context menu | Application (`K:0065`) | Application (`K:0065`) | `Ctrl+Return` |

Platform notes:

- Linux mappings target GNOME defaults; custom desktop bindings may differ.
- Windows `GUI+A` opens Quick Settings (Windows 11) or Action Center (Windows 10).
- macOS `Ctrl+Return` requires macOS 15+; `C:029F` and `C:02A0` follow the QMK/macOS convention for Mission Control and Launchpad.
- Easy-Switch (`00D1`-`00D3`), keyboard backlight (`00E2`/`00E3`), Fn (`0034`), and Fn-lock (`00DE`) always stay under keyboard control.
- Diverted notifications report up to 4 held control IDs alongside (and never disrupting) the keyboard's normal NKRO report path.

Shortcut references: [QMK report usages](https://github.com/qmk/qmk_firmware/blob/master/tmk_core/protocol/report.h), [GNOME shortcuts](https://help.gnome.org/gnome-help/shell-keyboard-shortcuts.html), [Windows shortcuts](https://support.microsoft.com/en-us/accessibility/windows/keyboard-shortcuts-in-windows), [Apple shortcuts](https://support.apple.com/en-us/102650), and [macOS context-menu behavior](https://developer.apple.com/videos/play/wwdc2024/10124/).

## Specifications and Maintenance

- [Logitech `cpg-docs` repository](https://github.com/Logitech/cpg-docs): [HID++ feature index](https://github.com/Logitech/cpg-docs/blob/master/hidpp20/README.rst) and [feature specifications](https://github.com/Logitech/cpg-docs/tree/master/hidpp20/features).
- [Logitech public document folder](https://drive.google.com/drive/folders/0BxbRzx7vEV7eWmgwazJ3NUFfQ28): additional specs, newer revisions, and control/task ID lists.
- [Mirrored manufacturer documents](https://lekensteyn.nl/files/logitech/) and the [HID++ 2.0 draft](https://lekensteyn.nl/files/logitech/logitech_hidpp_2.0_specification_draft_2012-06-04.pdf) (compare revisions against Logitech's published copies).
- [Solaar's feature catalog](https://github.com/pwr-Solaar/Solaar/blob/master/lib/logitech_receiver/hidpp20_constants.py) for feature IDs and names (always verify write semantics against manufacturer specs).
- Additional implemented feature specs: [Root](https://github.com/Logitech/cpg-docs/blob/master/hidpp20/features/0x0000-IRoot.rst), [Backlight rev 2](https://drive.google.com/file/d/1QfBDVyfHihbVk2yOpJsQiKekKZGwARWm/view), [SmartShift](https://lekensteyn.nl/files/logitech/x2110_smartshift.html), and [HiRes wheel](https://lekensteyn.nl/files/logitech/x2121_hires_wheel.pdf).

Update this inventory whenever feature support or scope changes, keeping scope exclusions distinct from priorities and open research/verification items per the [Logitech scope rules](../AGENTS.md#logitech-devices).

[features]: ../rust/crates/cordial-core/src/features.rs
[handlers]: ../rust/crates/cordial-core/src/features/handlers.rs
[hidpp]: ../rust/crates/cordial-core/src/hidpp.rs
[translation]: ../rust/crates/cordial-core/src/model/translation.rs
[settings]: ../rust/crates/cordial-core/src/settings.rs
[link]: ../rust/crates/cordial-core/src/link.rs
