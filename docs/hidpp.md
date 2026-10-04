# Logitech HID++ support

Cordial supports Logitech device configuration, status reporting, and translation into standard
USB input. The tables below list known features, their implementation status, and the scope of
planned support. Availability depends on the device's advertised capabilities and connection type.

## Status definitions

| Status | Meaning |
| --- | --- |
| Implemented | Supported by the current code for the operations described in the row. |
| Planned | Selected for implementation. Any remaining protocol or device verification is noted in the row. |
| Not Planned | Not selected for implementation. May be reconsidered, but there is no commitment to implement it. |
| Excluded | Outside Cordial's scope or ruled out for the reason given in the row. Reconsideration requires addressing that reason. |

A feature can have several statuses because its operations have different requirements. Apply the
[Logitech scope rules](../AGENTS.md#logitech-devices) to each operation. Research, interface design,
and device-verification requirements describe what would be needed for support; meeting them does
not automatically make an operation Planned.

Keyboard support includes these features:

- Fn-row preference, including legacy devices and the current connection on multi-host devices.
- Keyboard platform preference for the current connection.
- Fixed standard mappings for additional media keys and other special keys.
- Standard USB System/Radio input with correct presses, releases, and state handling.
- Automatic power-off timeout on supported keyboards.

Diversion of standard behavior is excluded without a demonstrated reason. Native HID remains the
input path when it provides the required behavior. A diversion implementation needs a concrete
missing capability or failure, evidence that diversion addresses it, and complete translation into
standard USB input. The existence of a HID++ reporting mode does not establish that need.
Native resolution settings and mechanical preferences, such as wheel inversion or ratchet mode,
are separate from input diversion.

The implementation column links to the relevant code. Features without handlers can still appear
in the device's HID++ inventory. Shared settings support lives in [the settings engine][features]
and [settings storage][settings]. See [Protocol behavior](#protocol-behavior) and [HID++ normalization](#hid-normalization) for
command and input behavior.

## HID++ features

Linked feature IDs open the manufacturer's command specification. Entries without specifications
identify known features whose command formats may still need verification.

| Feature ID | Feature | Description | Implementation | Status | Operations and status reasons |
| --- | --- | --- | --- | --- | --- |
| [`0x0000`](https://drive.google.com/file/d/1ULmw9uJL8b8iwwUo5xjSS9F5Zvno-86y/view) | Root | Protocol probe and feature lookup. | [discovery][features] and [input client][hidpp]. | Implemented | Protocol discovery; no model-name assumptions. |
| [`0x0001`](https://drive.google.com/file/d/10r9GSoVjZjyj2HqAj8s8yBgDjOYk-LR9/view) | Feature Set | Enumerate advertised features and revisions. | [discovery][features]. | Implemented | Unknown IDs remain inventory entries. Hidden and engineering features are not used. |
| `0x0002` | Feature Information | Feature metadata. | | Not Planned | Need documented semantics and a useful diagnostic purpose beyond the existing inventory. |
| [`0x0003`](https://drive.google.com/file/d/15-QuLJmICowrO4GjP70ekuh7boqBR1uH/view) | Device Information | Device identity and firmware/hardware entities. | [handlers][handlers] (`Handler::Firmware`). | Implemented | Firmware and bootloader versions, hardware revision, and serial when advertised. Reads up to eight entities and retains the first of each supported type. |
| `0x0004` | Device Unit ID | Separate device identity feature. | | Not Planned | Verify commands and benefit beyond the implemented Device Information identity fields. |
| [`0x0005`](https://drive.google.com/file/d/1V9UO0ToIIsMxhM36dEVfwkCFx4WH5qt2/view) | Device Name and Type | Read the device name and type. | [handlers][handlers] (`Handler::Name`). | Implemented | Read-only name, capped at 64 bytes; device types map to keyboard, mouse, or other. |
| `0x0006` | Device Groups | Device grouping metadata. | | Not Planned | Grouping semantics and a useful configuration or diagnostic purpose need research. |
| [`0x0007`](https://drive.google.com/file/d/1k3edRXkwuMdoSXd9BivjcFa9ITkheCUm/view) | Device Friendly Name | Configure the advertised device name. | | Not Planned | Bounded text, paged access, and full-name readback. |
| `0x0008` | Keep Alive | Connection keepalive facility. | | Not Planned | Need evidence of a connection problem requiring this operation and documented commands. |
| `0x0011` | Property Access | Generic device property access. | | Not Planned | Properties, write semantics, and useful supported controls need research. |
| `0x0020` | Configuration Change | Reset temporary reporting configuration. | [input client][hidpp] (`Client::reset`). | Implemented | Internal setup for fixed input translation; not a user-facing factory reset. |
| `0x0021` | 32-byte Unique Random Identifier | Device identity token. | | Not Planned | Verify commands and a useful Diagnostics presentation. |
| `0x0030` | Target Software | Target software metadata. | | Not Planned | Need a useful adapter-side purpose and documented semantics. |
| `0x0080` | Wireless Signal Strength | Radio signal information. | | Not Planned | Verify Bluetooth availability and meaningful Diagnostics values. |
| `0x00C0` | DFU Control, legacy | Legacy firmware-update entry. | | Excluded | Device firmware updates are outside configuration and input translation. |
| `0x00C1` | DFU Control, unsigned | Unsigned firmware-update entry. | | Excluded | Device firmware updates are outside configuration and input translation. |
| `0x00C2` | DFU Control, signed | Signed firmware-update entry. | | Excluded | Device firmware updates are outside configuration and input translation. |
| `0x00C3` | DFU Control | Firmware-update control. | | Excluded | Device firmware updates are outside configuration and input translation. |
| `0x00D0` | DFU | Firmware transfer and update. | | Excluded | Device firmware updates are outside configuration and input translation. |
| `0x1000` | Battery Status | Battery level and charging state. | [handlers][handlers] (`Engine::battery`). | Implemented | Use advertised mileage capability; otherwise report coarse levels. |
| `0x1001` | Battery Voltage | Voltage-based battery and charging information. | [handlers][handlers] (`Engine::battery`). | Implemented | Charging and documented full/critical indications; no guessed percentage from voltage. |
| `0x1004` | Unified Battery | Battery percentage or coarse level and charging. | [handlers][handlers] (`Engine::battery`). | Implemented | Use advertised capabilities and supported status fields. |
| `0x1010` | Charging Control | Configure charging behavior. | | Not Planned | Research must establish a stored preference and supported command format. |
| `0x1300` | LED Control | Direct indicator LED control. | | Excluded | One-shot or software-driven indicator overrides are outside ongoing configuration. |
| `0x1500` | Force Pairing | Device pairing command. | | Excluded | Direct Device pairing commands are outside stored configuration; this does not exclude normal Dongle Bluetooth pairing. |
| `0x1800` | Generic Test | Device test facility. | | Excluded | Factory/engineering test operations are outside configuration and input translation. |
| `0x1802` | Device Reset | Device reset command. | | Excluded | One-shot reset is outside ongoing configuration. |
| `0x1805` | Out-of-box State | Factory/setup state facility. | | Not Planned | Need documented semantics and an in-scope purpose; factory-reset actions remain excluded. |
| `0x1806` | Config Device Properties | Device property configuration. | | Not Planned | Property definitions and supported commands need verification. |
| [`0x1814`](https://drive.google.com/file/d/1EMHfOJwXikGdJfdYIa0v1a50EVV9Qb8N/view) | Change Host | Switch paired host slots and manage cookies. | | Excluded | Other hosts and host switching are outside scope. |
| [`0x1815`](https://drive.google.com/file/d/1SXlqlYIi4p3cN9d3dStHZAvxKmkCcV_h/view) | Hosts Info | Inspect or modify paired-host records. | | Excluded | Host lists, names, OS metadata, pairing moves/deletion, and slot bookkeeping are outside scope. |
| `0x1816` | BLE Pro Pre-pairing | BLE pre-pairing facility. | | Not Planned | Protocol and applicability to the Dongle's Bluetooth connection need research. |
| `0x1981` | Backlight | Older backlight duration controls. | | Not Planned | Persistent illumination duration or off; verify manufacturer commands before implementation. |
| [`0x1982`](https://drive.google.com/file/d/1o8SSMKCtxl07VCzBQr0o8VZPfQ-leK47/view) | Backlight 2 | Backlight enable, effects, level, mode, and timeouts. | [handlers][handlers] (`Engine::backlight`). | Implemented / Not Planned / Excluded | Advertised persistent settings and current level/status are implemented. Not planned: RAM-only effect configuration. Reconsideration requires that supported commands, readback, and reapplication of Dongle-stored preferences are verified. Temporary manual mode is observable; revision 3 does not allow software to select it. |
| `0x1983` | Backlight 3 | Backlight timeout controls. | | Not Planned | Autonomous illumination timeout; verify manufacturer commands before implementation. |
| [`0x1990`](https://drive.google.com/file/d/1q9vq-UDto2NGURB8O0R8LmScrAJAnHrH/view) | Illumination | Lighting enable, brightness, color temperature, and levels. | | Not Planned / Excluded | Illumination settings, level tables/presets, and diagnostics are not planned. Settings include on/off, brightness, and color temperature; effective maximum is a readout. Level tables/presets need an editor and paged-write design. Reconsideration requires that semantics and a useful presentation are established. Exclude raw bookkeeping without a useful user purpose. |
| `0x19B0` | Haptic | Native feedback settings and waveform playback. | | Not Planned / Excluded | Not planned: feedback enable/intensity, subject to command verification. Exclude waveform playback: direct effect triggering. |
| `0x19C0` | Force-sensing Buttons | Physical button activation force. | | Not Planned | Activation force per button, subject to command verification. |
| `0x1A00` | Presenter Control | Presenter-specific controls. | | Not Planned | Need documented controls that map to supported standard USB input. |
| `0x1A01` | 3D Sensor | Three-dimensional sensor data. | | Not Planned | Need documented semantics and a standard keyboard/mouse use; raw sensor readouts alone are insufficient. |
| `0x1B00` | Reprogrammable Controls, legacy | Older control enumeration/reporting. | | Not Planned | Need revision-specific protocol evidence and fixed standard USB mappings. |
| `0x1B01` | Reprogrammable Controls V2 | Older control enumeration/reporting. | | Not Planned | Need revision-specific protocol evidence and fixed standard USB mappings. |
| `0x1B02` | Reprogrammable Controls V2.2 | Older control enumeration/reporting. | | Not Planned | Need revision-specific protocol evidence and fixed standard USB mappings. |
| `0x1B03` | Reprogrammable Controls V3 | Older control enumeration/reporting. | | Not Planned | Need revision-specific protocol evidence and fixed standard USB mappings. |
| [`0x1B04`](https://drive.google.com/file/d/1t1u43_bnT7r4RgYImjpjekZ5jfThCeNL/view) | Reprogrammable Controls | Special keys/buttons, their task IDs, reporting, and mappings. | [input client][hidpp] and [translations][translation]. | Implemented / Not Planned / Excluded | Fixed keyboard, Consumer, System, and held-shortcut mappings are implemented. Additional standard mappings preserve native reporting when the descriptor can carry the input. Custom assignments are not planned and require a mapping UI and persistence design. Exclude diversion of working native key/button behavior without a demonstrated missing capability or failure. Raw-reporting controls require both that justification and complete standard USB translation. |
| `0x1B05` | Full Key Customization | Keyboard assignment customization. | | Not Planned | Needs command research, a mapping editor, and persistence design. |
| `0x1B0C` | Analog Button Tuning | Actuation depth, rapid trigger, and click haptics. | | Not Planned | Common, per-button, and per-key preferences are not planned. Commands need verification; per-key tuning also requires a dedicated editor. |
| `0x1B10` | Control List | Device control inventory. | | Not Planned | Need command research and a consumer for this metadata. |
| `0x1B20` | Switch Swapability | Cataloged switch-swap facility. | | Not Planned | Command semantics and an in-scope use need research. |
| `0x1B30` | Device Mode | Cataloged device operating-mode facility. | | Not Planned | Modes, persistence, and command semantics need research. |
| `0x1BC0` | Report HID Usage | Cataloged HID usage reporting facility. | | Not Planned | Need command semantics and a complete standard USB translation use. |
| [`0x1C00`](https://drive.google.com/file/d/1REDgXc_t7b4Wun2MIGfSDuEA5c12seJ5/view) | Persistent Remappable Action | Store native key/button actions. | | Not Planned / Excluded | Custom assignments and restoration of default assignments for the current connection are not planned. They need a mapping editor, persistence design, and verified reset scope. Assignment reset restores mappings, not the entire Device. Other-host assignments and resets remain excluded. |
| [`0x1D4B`](https://drive.google.com/file/d/1ORLJ0pEiR5gkkAjv0961wlbGJQ-2s-5G/view) | Wireless Device Status | Notify reconnection and request software reconfiguration. | | Not Planned | Evaluate whether Bluetooth connection handling misses a reset/reconfiguration request; there is no dedicated event handler. |
| `0x1DF0` | Remaining Pairing | Remaining pairing information. | | Excluded | Device pairing-slot bookkeeping is outside current-connection configuration. |
| `0x1E00` | Enable Hidden Features | Expose hidden device features. | | Excluded | Hidden/engineering features must not be enabled for supported operations. |
| `0x1F1F` | Firmware Properties | Firmware property metadata. | | Not Planned | Need useful fields beyond implemented firmware information and verified commands. |
| `0x1F20` | ADC Measurement | Battery measurement and revision-dependent power settings. | [handlers][handlers] (`Engine::battery`, `Handler::Power`). | Implemented | Battery full/charging indications are implemented. Automatic power-off uses revision 2 or newer and requires the device to identify as a keyboard or keyboard/mouse. The saved timeout is 0-15300 seconds in 60-second steps; 0 means never. |
| `0x2001` | Left/Right Swap | Swap primary and secondary button behavior. | | Not Planned | Verify native stored-setting commands and capabilities. |
| `0x2005` | Swap Button Cancel | Control button-swap cancellation behavior. | | Not Planned | Command semantics and suitability as a stored preference need research. |
| `0x2006` | Pointer Axis Orientation | Configure pointer axis orientation. | | Not Planned | Verify native stored-setting commands and capabilities. |
| [`0x2100`](https://drive.google.com/file/d/1zs9fVDF3X00WNKEE-fndWJuL_kiDUc_h/view) | Vertical Scrolling | Read roller type, ratchets per turn, and suggested scrolling speed. | | Not Planned / Excluded | Not planned: useful wheel readouts pending UI and device verification. Exclude applying suggested Host OS scrolling settings: Host runtime behavior. |
| [`0x2110`](https://drive.google.com/file/d/1MDt_PuLK8VKss5Mxa16XDvvb3ASmoR2_/view) | SmartShift | Wheel free-spin/ratchet mode and automatic disengagement threshold. | [handlers][handlers] (`Handler::Wheel`). | Implemented / Not Planned | Mode and current threshold are implemented. Not planned: stored default-threshold editing, preserving companion fields. |
| [`0x2111`](https://drive.google.com/file/d/1NgLzd_Dol0zDOZvJTN5_1lvOAWcxJnwJ/view) | Enhanced SmartShift | SmartShift with ratchet torque controls. | | Not Planned | Mode, threshold, supported torque, and default threshold/torque and maximum-force readouts. |
| [`0x2120`](https://drive.google.com/file/d/1zs9fVDF3X00WNKEE-fndWJuL_kiDUc_h/view) | High-resolution Scrolling | Select high-resolution wheel reporting. | | Not Planned | Not planned: native high-resolution selection. Reconsideration requires that a useful resolution benefit and correct scaling through standard USB input are verified. This feature controls resolution, not HID/HID++ routing. |
| [`0x2121`](https://drive.google.com/file/d/1WEfouBszkLA3Dl2WRIhzqJqyWZL5wKMJ/view) | High-resolution Wheel | Wheel inversion, resolution, routing, analytics, and physical information. | [handlers][handlers] (`Handler::Hires`). | Implemented / Not Planned / Excluded | Inversion and multiplier/ratchet-count/diameter information are implemented. Not planned: native resolution settings and conversion. Reconsideration requires that a useful benefit, startup mode, and correct USB scaling are verified. Exclude wheel diversion without a demonstrated need to replace native scrolling. Exclude opaque project-specific analytics. |
| [`0x2130`](https://lekensteyn.nl/files/logitech/x2130_ratchetwheel.html) | Ratchet Wheel | Choose native/diverted wheel reporting. | | Excluded | Wheel diversion and its decoder are excluded: native HID already provides wheel motion, and no missing capability or failure requiring diversion is demonstrated. |
| [`0x2150`](https://drive.google.com/file/d/1Op9dBdPkXfJWhwL7bXjvvhORDH9C9Eft/view) | Thumbwheel | Thumbwheel inversion and reporting configuration. | [handlers][handlers] (`Handler::Thumb`). | Implemented / Excluded | Inversion is implemented, requiring native routing for edits. Exclude diversion and its decoder: no demonstrated need to replace native horizontal scrolling. Native/diverted resolutions are device-reported characteristics, not independent setter choices. |
| [`0x2200`](https://drive.google.com/file/d/0BxbRzx7vEV7eeXJIS01aSlBVbkU/view) | Mouse Pointer | Read nominal sensor DPI and pointer-processing recommendations. | | Not Planned / Excluded | Not planned: useful sensor readouts pending UI and device verification. Exclude applying Host acceleration/orientation recommendations: Host runtime behavior. This feature has no DPI setter. |
| [`0x2201`](https://lekensteyn.nl/files/logitech/x2201_adjustabledpi.html) | Adjustable DPI | Sensor DPI choices or ranges. | [handlers][handlers] (`Handler::Dpi0` / `Dpi1`). | Implemented / Not Planned | DPI read/write for up to two sensors is implemented. Not planned: coverage for all advertised sensors. |
| [`0x2202`](https://drive.google.com/file/d/1kb1tnTvznLFrfHdHL271A5iQqJCi7yM-/view) | Extended Adjustable DPI | Per-sensor X/Y DPI, lift-off, calibration, and DPI indicator. | | Not Planned / Excluded | Per-sensor X/Y DPI, lift-off settings, and calibration correction are not planned. Reconsideration requires that Dongle-managed host mode is verified on profile-capable devices, where the setter requires it. Not planned: calibration correction. Reconsideration requires that its ongoing configuration semantics, readback, and persistence or reapplication are verified. Interactive calibration commands and temporary DPI LED display remain excluded as one-shot Device operations. |
| [`0x2205`](https://lekensteyn.nl/files/logitech/x2205_pointermotionscaling.html) | Pointer Motion Scaling | Device-side pointer sensitivity multiplier. | | Not Planned | Stored pointer scaling with readable units. |
| `0x2230` | Angle Snapping | Configure pointer angle snapping. | | Not Planned | Verify native stored-setting commands and capabilities. |
| `0x2240` | Surface Tuning | Pointer sensor surface calibration. | | Not Planned / Excluded | Not planned: ongoing surface-tuning preferences. Reconsideration requires that commands, useful behavior, and persistence or reapplication are verified. Separate configuration from interactive calibration; one-shot calibration operations remain excluded. |
| `0x2250` | XY Statistics | Pointer movement statistics. | | Excluded | Raw movement analytics are outside useful configuration/status reporting. |
| `0x2251` | Wheel Statistics | Wheel movement statistics. | | Excluded | Wheel analytics are explicitly outside scope. |
| `0x2400` | Hybrid Tracking | Configure hybrid sensor tracking behavior. | | Not Planned | Verify native stored-setting commands and capabilities. |
| `0x40A0` | Legacy Fn Inversion | Function keys versus special actions by default. | [handlers][handlers] (`Handler::Fn`). | Implemented | Saved legacy Fn-row preference with readback. |
| [`0x40A2`](https://docs.google.com/document/d/1nH63NvSTDFlFIoGOT9sozDvSTwsTFmvqYwPcygWnOq4/edit) | Fn Inversion | Function-row default behavior. | [handlers][handlers] (`Handler::Fn`). | Implemented | Read, write, and readback; preserve the separate device default-state field. |
| [`0x40A3`](https://drive.google.com/file/d/1TU1adbjmom4FUcAzZYpwZZi5mrkPBsU5/view) | Multi-host Fn Inversion | Function-row preference per host. | [handlers][handlers] (`Handler::Fn`). | Implemented / Excluded | Current-host read/write uses host-first parameters and readback. Devices rejecting the current-host selector report an application failure; no alternate host slot or byte order is tried. Other-slot edits are excluded: other hosts. |
| `0x4100` | Encryption | Read link encryption information. | | Not Planned | Confirm relevant Bluetooth devices advertise the feature and verify commands; candidate readout belongs in Diagnostics. |
| `0x4220` | Lock Key State | Proprietary lock-key state reporting. | | Not Planned / Excluded | Not planned: useful lock-state reporting. Reconsideration requires that protocol semantics and a purpose beyond standard HID lock indicators are verified. Direct lock-state control unrelated to configuration or standard HID indicator forwarding remains excluded. |
| [`0x4301`](https://drive.google.com/file/d/0BxbRzx7vEV7eR1Zra3VNaUlmOVE/view) | Solar Dashboard | Solar keyboard battery and indicator functions. | [handlers][handlers] (`Engine::battery`). | Implemented / Excluded | Battery percentage reporting is implemented. Exclude solar-check indicator overrides: direct indicator control. |
| `0x4520` | Keyboard Layout | Keyboard layout metadata or selection. | | Not Planned | Read/write semantics, persistence, and supported choices need research. |
| [`0x4521`](https://drive.google.com/file/d/1hAVtnJxQMo9UnkniFN4vOUYPFPCUGCDE/view) | Disable Keys | Disable advertised lock, Insert, and Windows keys. | | Not Planned | Supported switches, preserving unrelated disable bits. |
| [`0x4522`](https://drive.google.com/file/d/1PAs6A_oN3Z5eqjnSL_VJMuoY0XlmRAM4/view) | Disable Keys by HID Usage | Disable an arbitrary set of keys. | | Not Planned | Needs a key-list editor and correct add/remove persistence semantics. |
| `0x4523` | Disable Controls | Disable selected keyboard controls. | | Not Planned | Need command research and a control-selection interface. |
| [`0x4530`](https://drive.google.com/file/d/11CXizK2_yeH9eOwBeQlQRrC0gAw_Hrfl/view) | Dual Platform | Configure platform-specific keyboard behavior. | [handlers][handlers] (`Handler::Platform`). | Implemented | Saved device platform preference. Windows/Android and macOS/iOS share their respective hardware modes. |
| [`0x4531`](https://drive.google.com/file/d/1KyiBA5m_5V1s6jQ9eQrgRJN0SbbbI9_I/view) | Multi Platform | Configure keyboard platform per host. | [handlers][handlers] (`Handler::Platform`). | Implemented / Excluded | Current-host platform preference uses advertised capabilities and unambiguous OS descriptors covering all versions. Writes require the platform-selection capability. Version-specific or ambiguous choices remain unavailable. Exclude other slots and host bookkeeping: other-host scope; use capability descriptors for choices rather than raw rows. |
| `0x4540` | Keyboard International Layouts | Additional keyboard layout facility. | | Not Planned | Read/write semantics, persistence, and supported choices need research. |
| [`0x4600`](https://drive.google.com/file/d/1nStBTT3rdTmPPeJWFflNdBWlC_E0-0SM/view) | Crown | Crown mechanical mode, reporting, timing, and geometry. | | Not Planned / Excluded | Physical free/ratchet mode, timing settings, and geometry diagnostics are not planned. Timing settings cover rotation timeout, short/long press timeout, and double-tap timing. Reconsideration requires that a useful effect on native HID behavior is verified. Timings used only for diverted events require a demonstrated reason for diversion. Slot-count and ratchet-count diagnostics are not planned. Reconsideration requires that they answer a useful configuration or input question. Exclude diversion of native crown behavior without a demonstrated missing capability or failure. |
| `0x6010` | Touchpad Firmware Items | Touchpad firmware properties. | | Not Planned | Need documented fields and useful native preferences. |
| `0x6011` | Touchpad Software Items | Touchpad software properties. | | Not Planned | Need documented fields; Host-dependent runtime behavior is excluded. |
| `0x6012` | Touchpad Win8 Firmware Items | Windows-oriented touchpad firmware properties. | | Not Planned | Need documented fields and proof of autonomous behavior through standard USB input. |
| `0x6020` | Tap Enable | Native tap-to-click behavior. | | Not Planned | Verify native stored-setting commands and capabilities. |
| `0x6021` | Tap Enable Extended | Extended native tap-to-click behavior. | | Not Planned | Verify native stored-setting commands and capabilities. |
| `0x6030` | Cursor Ballistic | Native cursor acceleration. | | Not Planned | Verify native stored-setting commands and capabilities. |
| `0x6040` | Touchpad Resolution | Touchpad pointer resolution. | | Not Planned | Verify native stored-setting commands and capabilities. |
| [`0x6100`](https://drive.google.com/file/d/1B8uKxWyjBD7-0G47k5N1KXNwqbutz0jv/view) | Touchpad Raw XY | Raw touch position reporting. | | Excluded | Raw-touch replacement of working native pointer or gesture behavior is excluded: no demonstrated missing capability or failure requires it. Raw-reporting/format controls also lack complete standard USB translation. |
| [`0x6110`](https://drive.google.com/file/d/0BxbRzx7vEV7eV3R0eXE2ZGprVkU/view) | Touchmouse Raw Points | Raw touch points and reporting selection. | | Excluded | Raw-touch replacement of working native pointer or gesture behavior is excluded: no demonstrated missing capability or failure requires it. Raw-reporting/format controls also lack complete standard USB translation. |
| `0x6120` | Bluetooth Touchmouse Settings | Bluetooth touchmouse configuration. | | Not Planned | Command semantics and an in-scope use are unverified. |
| `0x6500` | Gestures | Older native gesture configuration. | | Not Planned / Excluded | Research native stored preferences. Exclude diversion of working native gestures without a demonstrated missing capability or failure. |
| `0x6501` | Gestures 2 | Native gesture enablement, parameters, and diversion. | | Not Planned / Excluded | Native enable switches, numeric parameters, and custom translations are not planned. Custom translations require a mapping UI and translation design. Exclude diversion of working native gestures without a demonstrated missing capability or failure, and standalone diversion controls without complete translation. |
| `0x8010` | G Keys | G/M-key reporting and assignments. | | Not Planned / Excluded | Custom assignments require a mapping editor and complete Dongle translation. Not planned: support for controls that lack usable native reports. Exclude diversion of working native keys without a demonstrated missing capability or failure; no standalone diversion control. |
| `0x8020` | M Keys | Profile-selection key LEDs. | | Excluded | Direct LED overrides are outside ongoing configuration. |
| `0x8030` | MR Key | Macro-record key LED. | | Excluded | Direct LED overrides are outside ongoing configuration. |
| [`0x8040`](https://drive.google.com/file/d/1EIiHKilvLXxFdZJMh2UrvfqYffVyRfH7/view) | Brightness Control | Illumination enable and brightness. | | Not Planned | On/off and brightness within device-advertised bounds. |
| `0x8051` | Logitech Modifiers | Device-specific modifier facility. | | Not Planned | Need protocol evidence and standard USB translation or stored native behavior. |
| [`0x8060`](https://drive.google.com/file/d/1PRpmCzYFQDcDnXe9l1_xxVpTVSOcsvR4/view) | Report Rate | Select the device reporting interval. | | Not Planned | Rate readouts and writes are not planned. Writes require a useful Bluetooth reporting-rate change and verified Dongle-managed host mode; the specification requires host mode for writes. Measure input and USB output rates to establish any reporting-rate benefit. |
| [`0x8061`](https://drive.google.com/file/d/1WF_F5bkVRqPx_dzv6TSFbDdQICDlgFjI/view) | Extended Report Rate | Select transport-specific reporting rates. | | Not Planned | Rate readouts and Bluetooth controls are not planned. Revision 0 defines wired/Lightspeed selectors only. Writes require host mode and may be lost when leaving it; verify an in-scope path. |
| [`0x8070`](https://drive.google.com/file/d/1vdRwDOmaDpHwusJoDsGxe2Q4XBmNr1bb/view) | Color LED Effects | Zone effects, colors, parameters, animations, and ownership. | | Not Planned / Excluded | Not planned: device-run zone effects, colors, timing, direction, supported persistence, startup animation, and autonomous demo preferences. Not planned: software ownership needed for configuration and useful change notifications. Reconsideration requires that autonomous operation, readback, and reapplication are verified. Any configuration interface would use zone/effect metadata as control labels and choices. Exclude streamed effects, LED-bin calibration, and live color polling: continuous software control, manufacturing data, or effect telemetry. |
| [`0x8071`](https://drive.google.com/file/d/1lecrJQgAC7wnXlo8PEnwVSb3L0GioALG/view) | RGB Effects | Zone effects, colors, animations, dimming, timeouts, and patterns. | | Not Planned / Excluded | Not planned: device-run zone effects/parameters, animation preferences, idle/off timeouts, and dimming. LED patterns are not planned and require a dedicated editor. Not planned: software ownership needed for configuration, useful change notifications, and power-mode setup. Reconsideration requires that an autonomous configuration sequence, readback, and reapplication are verified. Exclude streamed effects and runtime synchronization: continuous software control. Exclude LED-bin calibration and shutdown triggering: manufacturing data and one-shot commands. |
| `0x807A` | RPM Indicator | Engine-speed indication. | | Excluded | Gaming wheel/pedal or game-output functionality is outside keyboard/mouse configuration. |
| `0x807B` | RPM LED Pattern | Engine-speed LED patterns. | | Excluded | Gaming wheel/pedal or game-output functionality is outside keyboard/mouse configuration. |
| `0x8080` | Legacy Per-key Lighting | Configure individual key lighting. | | Not Planned | Requires a per-key editor and verified autonomous persistence. |
| [`0x8081`](https://drive.google.com/file/d/1tKUqcL1dXKj69nuejgOrzoQ8Xq69RO5v/view) | Per-key Lighting | Configure individual keys/zones or submit lighting frames. | | Not Planned / Excluded | Static configuration is not planned and requires a per-key editor and persistence verification. Revision 0 defines persistence values but says they currently behave identically. Exclude streamed frames: continuous software control. |
| [`0x8090`](https://drive.google.com/file/d/1sDIVLyckdp-28xkV_jPGEi46wf0wD-Fa/view) | Mode Status | Performance/endurance operating mode. | | Not Planned | Mode readouts and writes are not planned. Writes require a supported Bluetooth device that advertises software switching and demonstrates a useful effect over Bluetooth. Any reporting-rate benefit needs measured input and USB output rates; receiver-mode rates alone do not establish a benefit. |
| `0x80A3` | Legacy Axis Response Curve | Legacy gaming-axis response curve. | | Excluded | Gaming wheel/pedal or game-output functionality is outside keyboard/mouse configuration. |
| `0x80A4` | Axis Response Curve | Gaming-axis response curve. | | Excluded | Gaming wheel/pedal or game-output functionality is outside keyboard/mouse configuration. |
| `0x80B1` | Banded Axis | Banded gaming-axis behavior. | | Excluded | Gaming wheel/pedal or game-output functionality is outside keyboard/mouse configuration. |
| `0x80D0` | Combined Pedals | Combined pedal axes. | | Excluded | Gaming wheel/pedal or game-output functionality is outside keyboard/mouse configuration. |
| `0x80E0` | Bunny Hopping | Cataloged gaming behavior. | | Not Planned | Device class and command semantics need research; timed input automation is excluded. |
| `0x8100` | Onboard Profiles | Select device-owned profiles or host-controlled mode. | | Not Planned / Excluded | Device-owned profile selection and Logitech software/host mode are not planned. Reconsideration requires that the Dongle can manage it while preserving required standard USB input and stored preferences. The protocol term does not itself require software on Cordial's Host. Exclude operation requiring runtime Host software or unjustified input diversion. |
| `0x8101` | Profile Management | Additional profile management facility. | | Not Planned | Need device-owned operation semantics and a profile-management interface. |
| `0x8110` | Mouse Button Filter / Spy | Cataloged mouse-button facility; indexes use different names. | | Not Planned | Resolve manufacturer/catalog naming and command semantics before selecting a useful diagnostic or configuration operation. |
| `0x8111` | Latency Monitoring | Input latency monitoring. | | Not Planned | Need trustworthy measurements, Bluetooth applicability, and a useful Diagnostics presentation. |
| `0x8120` | Gaming Attachments | Gaming attachment functions. | | Excluded | Attachments are outside keyboard/mouse scope. |
| `0x8123` | Force Feedback | Force-feedback effects. | | Excluded | Outside keyboard/mouse configuration; direct effect control. |
| `0x8127` | Dual Clutch | Dual clutch behavior. | | Excluded | Gaming wheel/pedal or game-output functionality is outside keyboard/mouse configuration. |
| `0x812C` | Wheel Center Position | Racing-wheel center position. | | Excluded | Gaming wheel/pedal or game-output functionality is outside keyboard/mouse configuration. |
| `0x8130` | Display Game Data | Game-data display. | | Excluded | Gaming wheel/pedal or game-output functionality is outside keyboard/mouse configuration. |
| `0x8131` | Center Spring | Racing-wheel centering force. | | Excluded | Gaming wheel/pedal or game-output functionality is outside keyboard/mouse configuration. |
| `0x8132` | Axis Mapping | Gaming-axis mapping. | | Excluded | Gaming wheel/pedal or game-output functionality is outside keyboard/mouse configuration. |
| `0x8133` | Global Damping | Racing-wheel damping. | | Excluded | Gaming wheel/pedal or game-output functionality is outside keyboard/mouse configuration. |
| `0x8134` | Brake Force | Pedal brake force. | | Excluded | Gaming wheel/pedal or game-output functionality is outside keyboard/mouse configuration. |
| `0x8135` | Pedal Status | Pedal state information. | | Excluded | Gaming wheel/pedal or game-output functionality is outside keyboard/mouse configuration. |
| `0x8136` | Torque Limit | Racing-wheel torque limit. | | Excluded | Gaming wheel/pedal or game-output functionality is outside keyboard/mouse configuration. |
| `0x8137` | Configuration Profiles | Racing-wheel configuration profiles. | | Excluded | Gaming wheel/pedal or game-output functionality is outside keyboard/mouse configuration. |
| `0x8138` | Operating Range | Racing-wheel operating range. | | Excluded | Gaming wheel/pedal or game-output functionality is outside keyboard/mouse configuration. |
| `0x8139` | TrueForce | Racing-wheel force-feedback facility. | | Excluded | Gaming wheel/pedal or game-output functionality is outside keyboard/mouse configuration. |
| `0x8140` | Force-feedback Filter | Force-feedback filtering. | | Excluded | Gaming wheel/pedal or game-output functionality is outside keyboard/mouse configuration. |
| [`0x8300`](https://drive.google.com/file/d/1OpyACtEjPoQK17aaW7Wwk70aGBHNHtOH/view) | Sidetone | Microphone monitoring volume and mute. | | Excluded | Headset/audio functionality is outside keyboard/mouse scope. |
| [`0x8310`](https://drive.google.com/file/d/1Sk5q95tU0qgNtX3v-fJ8tmYYSeSUYLBI/view) | Audio Equalizer | Audio bands, gain, and microphone processing. | | Excluded | Headset/audio functionality is outside keyboard/mouse scope. |
| `0x8320` | Headset Output | Headset output configuration. | | Excluded | Headset/audio functionality is outside keyboard/mouse scope. |

## Related protocols and input

These entries are not HID++ 2.0 feature IDs.

| Protocol or operation | Description | Implementation | Status | Operations and status reasons |
| --- | --- | --- | --- | --- |
| HID++ 1.0 register `0x00` | Battery notification enable flags. | [Legacy battery polling][link] and [register transport][hidpp]. | Implemented | Internal notification setup for battery reporting; preserves other flag bits. |
| HID++ 1.0 register `0x0D` | Battery percentage and charging. | [Legacy battery decoding][link]. | Implemented | Read, notifications, and periodic polling. |
| HID++ 1.0 register `0x07` | Coarse battery and charging. | [Legacy battery decoding][link]. | Implemented | Fallback when the device rejects register `0x0D` as invalid. |
| USB System Control and Radio input | Standard USB output for supported System actions and native radio input. | [HID decoder](../rust/crates/cordial-core/src/hid.rs), [forwarder](../rust/crates/cordial-core/src/forward.rs), and [USB descriptor](../rust/adapters/usb/src/descriptor.rs). | Implemented | Dedicated reports retain simultaneous input and releases. Radio and rotation-lock buttons retain press edges. Sliders use the latest reported state across input sources, with a null value until known or after the source disconnects. Relative slider toggles use the corresponding momentary button. |
| Standard key and Consumer translation | Fixed mappings for Logitech special controls. | [Input client][hidpp] and [translation table][translation]. | Implemented / Excluded | Fixed standard mappings cover media, editing, navigation, application-launch usages, and System sleep. Additional controls remain native when their standard input is representable by the descriptor. Host command execution, typed launch sequences, and timed macros are excluded. |

## Centurion and audio catalog

Centurion uses a separate protocol from HID++ and has no implementation in Cordial. Generic
configuration and reporting are not planned. Reconsideration requires an applicable Bluetooth
keyboard or mouse and its command documentation. Headset, mixer, and microphone features remain outside
the keyboard/mouse scope.

| Feature ID | Catalog feature | Description | Status | Operations and status reasons |
| --- | --- | --- | --- | --- |
| `0x0100` | CENTURION DEVICE INFO | Device identity. | Not Planned | Need an applicable Bluetooth keyboard/mouse and documented identity fields. |
| `0x0101` | CENTURION DEVICE NAME | Device name. | Not Planned | Need an applicable Bluetooth keyboard/mouse and documented name operations. |
| `0x0102` | CENTURION ROOT | Protocol entry point. | Not Planned | Need an applicable Bluetooth keyboard/mouse and documented protocol discovery. |
| `0x0103` | CENTURION MEMFAULT | Fault diagnostics. | Not Planned | Need an applicable Bluetooth keyboard/mouse, documented diagnostics, and a useful reporting purpose. |
| `0x0104` | CENTURION BATTERY SOC | Battery state of charge. | Not Planned | Need an applicable Bluetooth keyboard/mouse and documented battery reporting. |
| `0x0108` | CENTURION AUTO SLEEP | Sleep timing. | Not Planned | Need an applicable Bluetooth keyboard/mouse and verified sleep preferences and reapplication. |
| `0x010A` | CENTURION GENERIC DFU | Firmware updates. | Excluded | Device firmware updates are one-shot maintenance, outside configuration scope. |
| `0x0110` | CENTURION LED BRIGHTNESS | LED brightness. | Not Planned | Need an applicable Bluetooth keyboard/mouse and verified brightness configuration and reapplication. |
| `0x0115` | CENTURION EU POWER MODE | Regional power mode. | Not Planned | Need an applicable Bluetooth keyboard/mouse and documented ongoing power preferences. |
| `0x0116` | CENTURION DEVICE BOOL STATE | Boolean device state. | Not Planned | Need an applicable Bluetooth keyboard/mouse, documented state semantics, and a useful reporting or configuration purpose. |
| `0x0200` | HEADSET VOLUME | Headset volume. | Excluded | Audio/headset or associated Centurion functionality, outside keyboard/mouse scope. |
| `0x0201` | HEADSET EQ | Headset equalizer. | Excluded | Audio/headset or associated Centurion functionality, outside keyboard/mouse scope. |
| `0x020D` | HEADSET ADVANCED PARA EQ | Headset parametric equalizer. | Excluded | Audio/headset or associated Centurion functionality, outside keyboard/mouse scope. |
| `0x020E` | HEADSET MIC TEST | Headset mic test. | Excluded | Audio/headset or associated Centurion functionality, outside keyboard/mouse scope. |
| `0x0213` | HEADSET EQ STYLES | Headset equalizer presets. | Excluded | Audio/headset or associated Centurion functionality, outside keyboard/mouse scope. |
| `0x0305` | BT HOST INFO | Bluetooth host records. | Excluded | Audio/headset or associated Centurion functionality, outside keyboard/mouse scope. |
| `0x0309` | LIGHTSPEED PAIRING | Lightspeed pairing. | Excluded | Audio/headset or associated Centurion functionality, outside keyboard/mouse scope. |
| `0x030A` | BT GAMING MODE | Bluetooth gaming mode. | Not Planned | Need an applicable Bluetooth keyboard/mouse, documented mode semantics, and a demonstrated benefit with Dongle-only operation. |
| `0x0600` | HEADSET RGB EFFECTS | Headset RGB effects. | Excluded | Audio/headset or associated Centurion functionality, outside keyboard/mouse scope. |
| `0x0601` | HEADSET MIC MUTE | Headset mic mute. | Excluded | Audio/headset or associated Centurion functionality, outside keyboard/mouse scope. |
| `0x0602` | HEADSET MIC SNR | Microphone signal-to-noise control. | Excluded | Audio/headset or associated Centurion functionality, outside keyboard/mouse scope. |
| `0x0604` | HEADSET AUDIO SIDETONE | Headset audio sidetone. | Excluded | Audio/headset or associated Centurion functionality, outside keyboard/mouse scope. |
| `0x0607` | HEADSET HOST SWITCH | Headset host switch. | Excluded | Audio/headset or associated Centurion functionality, outside keyboard/mouse scope. |
| `0x0609` | HEADSET MIX | Headset mix. | Excluded | Audio/headset or associated Centurion functionality, outside keyboard/mouse scope. |
| `0x060B` | HEADSET TONES | Headset tones. | Excluded | Audio/headset or associated Centurion functionality, outside keyboard/mouse scope. |
| `0x060D` | HEADSET NOISE EXPOSURE | Headset noise exposure. | Excluded | Audio/headset or associated Centurion functionality, outside keyboard/mouse scope. |
| `0x060E` | HEADSET AI NOISE REDUCTION | Headset AI noise reduction. | Excluded | Audio/headset or associated Centurion functionality, outside keyboard/mouse scope. |
| `0x0611` | HEADSET MIC GAIN | Headset mic gain. | Excluded | Audio/headset or associated Centurion functionality, outside keyboard/mouse scope. |
| `0x0617` | HEADSET USAGE TRACKING | Headset usage tracking. | Excluded | Audio/headset or associated Centurion functionality, outside keyboard/mouse scope. |
| `0x0618` | HEADSET BATTERY SAVER | Headset battery saver. | Excluded | Audio/headset or associated Centurion functionality, outside keyboard/mouse scope. |
| `0x0620` | HEADSET RGB HOSTMODE | Software-controlled RGB. | Excluded | Audio/headset or associated Centurion functionality, outside keyboard/mouse scope. |
| `0x0621` | HEADSET RGB ONBOARD EFFECTS | Headset RGB onboard effects. | Excluded | Audio/headset or associated Centurion functionality, outside keyboard/mouse scope. |
| `0x0622` | HEADSET RGB SIGNATURE EFFECTS | Headset RGB signature effects. | Excluded | Audio/headset or associated Centurion functionality, outside keyboard/mouse scope. |
| `0x0631` | HEADSET DO NOT DISTURB | Headset do not disturb. | Excluded | Audio/headset or associated Centurion functionality, outside keyboard/mouse scope. |
| `0x0634` | CENTURION ONBOARD PROFILES | Onboard profiles. | Not Planned | Need an applicable Bluetooth keyboard/mouse and documented autonomous profile selection. |
| `0x0635` | HEADSET RGB STREAMING | Headset RGB streaming. | Excluded | Audio/headset or associated Centurion functionality, outside keyboard/mouse scope. |
| `0x0636` | HEADSET ONBOARD EQ | Headset onboard EQ. | Excluded | Audio/headset or associated Centurion functionality, outside keyboard/mouse scope. |
| `0x0800` | MIXER AUDIO | Mixer audio. | Excluded | Audio/headset or associated Centurion functionality, outside keyboard/mouse scope. |
| `0x0801` | MIXER MIC | Mixer mic. | Excluded | Audio/headset or associated Centurion functionality, outside keyboard/mouse scope. |
| `0x0900` | LOGIVOICE | Microphone voice processing. | Excluded | Audio/headset or associated Centurion functionality, outside keyboard/mouse scope. |
| `0x0901` | LOGIVOICE NOISE REDUCTION | Logivoice noise reduction. | Excluded | Audio/headset or associated Centurion functionality, outside keyboard/mouse scope. |
| `0x0902` | LOGIVOICE NOISE GATE | Logivoice noise gate. | Excluded | Audio/headset or associated Centurion functionality, outside keyboard/mouse scope. |
| `0x0903` | LOGIVOICE COMPRESSOR | Logivoice compressor. | Excluded | Audio/headset or associated Centurion functionality, outside keyboard/mouse scope. |
| `0x0904` | LOGIVOICE DE ESSER | Logivoice de esser. | Excluded | Audio/headset or associated Centurion functionality, outside keyboard/mouse scope. |
| `0x0905` | LOGIVOICE DE POPPER | Logivoice de popper. | Excluded | Audio/headset or associated Centurion functionality, outside keyboard/mouse scope. |
| `0x0906` | LOGIVOICE LIMITER | Logivoice limiter. | Excluded | Audio/headset or associated Centurion functionality, outside keyboard/mouse scope. |
| `0x0907` | LOGIVOICE HIGH PASS FILTER | Logivoice high pass filter. | Excluded | Audio/headset or associated Centurion functionality, outside keyboard/mouse scope. |
| `0x0908` | LOGIVOICE EQUALIZER | Logivoice equalizer. | Excluded | Audio/headset or associated Centurion functionality, outside keyboard/mouse scope. |
| `0x0909` | LOGIVOICE AINR | AI noise reduction. | Excluded | Audio/headset or associated Centurion functionality, outside keyboard/mouse scope. |
| `0x0B01` | METERING | Audio level metering. | Excluded | Audio/headset or associated Centurion functionality, outside keyboard/mouse scope. |
| `0x0B02` | MIC GAIN AUTO MODE | Automatic microphone gain. | Excluded | Audio/headset or associated Centurion functionality, outside keyboard/mouse scope. |

The catalog's `0xFE00` Mouse Gesture entry is an application-internal identifier, not a device feature.

## Protocol behavior

HID++ requests share one transport owner and exchange slot with input normalization. Parameter byte
offsets in the table below exclude the three-byte device/feature/function header. Multibyte values
are big endian unless the feature specifies otherwise. Software ID zero identifies notifications
and never completes a request.

Feature revisions extend existing operations compatibly. The revisions below identify the
implemented layouts, not a maximum accepted revision. Cordial does not interpret unknown feature
IDs, settings, or option values. Hidden and engineering features are excluded from settings and
input normalization.

### Implemented command layouts

| Feature and implemented revisions | Operations and fields used | Constraints and preservation |
| --- | --- | --- |
| Root `0x0000` | Protocol ping supplies major/minor. Function 0 takes two-byte feature ID and returns index, flags, revision. | Discovery does not depend on model names. Read-only discovery also runs while integration is disabled. General settings proceed after a successful protocol ping even if normalization lacks `0x0020`/`0x1B04`. |
| FeatureSet `0x0001`, revisions 0-2 | Function 0 returns non-root count. Function 1 takes index and returns ID bytes 0-1, flags byte 2, revision byte 3 from revision 1 onward. Revision 0 inventories query Root for each visible feature's revision. | Inventory includes Root, bounded to 256 records. Hidden/engineering features are shown as unsupported and never queried for settings. Unknown feature IDs remain inventory only. Compatible newer revisions keep their documented operations. |
| DeviceInfo `0x0003`, revisions 0-4 | Function 0 returns entity count; revision 1 adds unit ID, transport flags and dense model IDs; revision 2 adds extended model ID; revision 3 adds motor-drive entity type; revision 4 adds serial capability and function 2 for the 12-character serial. Function 1 returns entity information, including raw SoftDevice build numbers. | Cordial reads up to eight entities and reports the first application firmware, bootloader, and hardware revision. Application and bootloader versions use the `firmware.version` and `bootloader.version` information keys. The Dongle formats each version as text, prefix then major, minor and build, such as `RQK 12.01.0013`. Serial is queried only when advertised. No firmware-update commands. |
| DeviceName `0x0005`, revisions 0-2 | Function 0 returns byte length. Function 1 takes character offset and returns a name fragment. Function 2 returns device type; revisions 1-2 add documented type values through 19. | Read-only name capped at 64 bytes; sanitize nonprintable name bytes. Device types map to keyboard, mouse, or other; unrecognized values map to other. Type labels live on the Host. |
| Battery `0x1000`, revision 0 | Function 1 returns level count and flags. Function 0/event 0 supply current level, next level, charge status. | Level 0 is unknown. Percentage is exposed only when mileage capability is set and at least ten levels exist. Coarse levels map to 5%, 20%, 75%, and 100%, using boundaries of 10%, 30%, and 80%. Charging Boolean uses documented charging states and is omitted when unavailable. Events update observations only. |
| Fn inversion `0x40A2`, revision 0 | Function 0 returns state/default-state. Function 1 takes state only. Readback function 0 verifies application. | State 0 means function keys, 1 means special actions. The separate default-state field is never written. Device scope. |
| Multi-host Fn `0x40A3`, revision 0 | Function 0 uses selector `0xFF` for current host. Function 1 sends `[0xFF, state]`, followed by function 0 readback. Response/event 0 fields are host, state, default-state, capability mask. | The host-first byte order follows the vendor function signature and [Solaar's implementation](https://github.com/pwr-Solaar/Solaar/blob/master/lib/logitech_receiver/settings_templates.py); the vendor request table disagrees. Solaar reports an MX Keys S firmware bug with `0xFF` and instead obtains the current host through `0x1815`. Cordial uses `0xFF`; affected devices may reject this setting. A rejected current-host selector fails the write. No other-slot fallback or byte-order probing. Events address the discovered current host or `0xFF`. When the getter echoes `0xFF`, numeric host indices are accepted because the event is defined as the current host's change. |
| Legacy Fn `0x40A0`, revision 0 | Getter 0 returns the current state; setter 1 takes state, followed by getter 0 readback. | The getter/setter layout follows [Solaar's FnSwap](https://github.com/pwr-Solaar/Solaar/blob/master/lib/logitech_receiver/settings_templates.py). Uses the same saved `keyboard.fn_row` preference as newer Fn features. Selection prefers `0x40A3`, then `0x40A2`, then `0x40A0`. |
| Dual Platform `0x4530`, revision 0 | Getter 1 and setter 2 use 0 for macOS/iOS and 1 for Windows/Android. | Function numbers follow the manufacturer specification. Solaar uses getter 0 instead of 1; device compatibility needs hardware verification. OS aliases preserve the user's saved choice while selecting the shared device mode. Setter is followed by readback. |
| Multi Platform `0x4531`, revision 1 | Function 0 supplies capabilities, descriptor counts, and current host. Function 1 enumerates OS descriptors. Getter 2 takes `0xFF`; setter 3 takes `[0xFF, platform]`. | Only the current connection is configurable. OS choices require an all-versions descriptor and agreement among every descriptor naming that OS, including version-specific descriptors. The platform-selection capability controls writability. Other-host events are ignored. |
| ADC power `0x1F20`, revision 2 or newer | Getter 1 reads timeout minutes; setter 2 takes minutes, followed by readback. | The revision and command layout follow [Solaar's ADCPower](https://github.com/pwr-Solaar/Solaar/blob/master/lib/logitech_receiver/settings_templates.py). Exposed as `power.auto_off` in seconds, 0-15300 with step 60. Zero means never. Requires the device's advertised keyboard or keyboard/mouse type. |
| Backlight `0x1982`, revisions 0-3 | Function 2 reads level count/status. Function 0 returns enabled at byte 0, option values at byte 1 and capability bits at byte 2. Function 1 writes enabled/options. Revision 2 adds effect choices and selector; `0xFF` preserves effect. Revision 3 getter additionally has configured level byte 5 and little-endian delays at bytes 6, 8, 10. Revision 3 setter has configured level byte 3 and delays at bytes 4, 6, 8. | Only advertised configuration fields are exposed: enable, power-on effect, crown effect, critical-battery power saving, supported effect choices, and revision 3 mode/level/delays. Current level and status are read-only. Read effect back through function 2 after a write. Revision 3 mode is bits 3-4: automatic 1, temporary manual 2, permanent manual 3. Temporary manual is observable, never a setter choice. Level edits require permanent manual. Delays are 1-1440 units of five seconds, displayed as 5-7200 seconds. Getter/setter layouts are deliberately separate. Fresh configuration is merged with only the requested fields; unrequested options and effect are preserved. Event 0 can require one coalesced configuration read, never an apply. The setter is documented as device NVM; there is no temporary/persistent classification in the UI. |
| AdjustableDPI `0x2201`, revisions 0-1 | Function 0 returns sensor count. Function 1 takes sensor index and returns echo plus BE16 DPI values. Function 2 returns echo/current/default. Function 3 takes sensor plus BE16 desired DPI. | At most two sensors. A list contains at most six ascending DPI values terminated by zero. A range is minimum, `0xE000 \| step`, maximum, zero. DPI bounds 1-57343; positive step and exact range alignment required. Revision 0 lacks the revision 1 setter echo, so every application independently reads back. No notifications are defined. |
| SmartShift `0x2110`, revision 0 | Function 0 returns mode/current threshold/default threshold. Function 1 accepts those three fields; zero means leave unchanged. | Mode 1 free-spin, 2 ratchet. Threshold 1-255; 255 disables automatic switching. The default-threshold setter byte is always zero. An apply merges every saved field into one setter. |
| HiResWheel `0x2121`, revisions 0-1 | Function 0 returns multiplier/capabilities; revision 1 adds ratchets per rotation and wheel diameter. Function 1 returns mode, adding the analytics bit in revision 1. Function 2 changes inversion while preserving every unrelated mode bit. Event 1 reports ratchet-switch state. | The capability response is decoded into the `wheel.resolution_multiplier`, `wheel.ratchets_per_rotation` and `wheel.diameter` information keys. Inversion requires existing native routing and standard resolution. No automatic routing/resolution change. Preserve analytics state; do not collect project-specific statistics or clear them by reading the analytics operation. Events update observations only. |
| Thumbwheel `0x2150`, revision 0 | Function 0 returns native/diverted resolution and capabilities. Function 1 returns routing at byte 0 and inversion/touch/proximity at bits 0–2 of byte 1. Function 2 takes routing and inversion only. | Inversion edits require native routing. Touch/proximity are observations, not setter bits. Scale/resolution is never changed. Diverted motion is not forwarded or interpreted by this settings module. |

Battery reporting also supports `0x1001`, `0x1004`, `0x1F20`, and `0x4301`, as listed in the feature
table. Provider selection uses advertised features, not device-model assumptions. Keyboard settings
use protocol simulations for validation; physical-device and Host behavior still require hardware
verification, especially current-host Fn writes and System/Radio handling. The null-capable two-bit radio and
rotation-lock slider fields have not been verified on physical Windows, macOS, or Linux hosts.

### Settings application and storage

Cordial reads the device's current configuration before each setting write and reads it again to
verify the result. An equal value produces no setter. A transmitted setter invalidates the affected
observations; SmartShift fields sent as leave-unchanged retain theirs. An unanswered setter reports
a timeout until the next apply reads the device. Unknown backlight status or effect values invalidate
freshness without substituting a value.

Notifications and Refresh update observations without changing saved preferences or reapplying them.
A normalization reset invalidates observations, and automatic application resumes when the transport
owner completes normalization. Forget changes only adapter storage. Settings edited while HID++ is
off are saved on the Dongle and applied when it is enabled; notifications and Refresh remain available.

Disconnect releases pending settings work with the transport client. Disabling HID++ during an
exchange settles the sent transaction before resetting temporary reporting and resuming read-only
discovery. Connecting with HID++ disabled performs no reset. If a disabled protocol probe fails,
Refresh can retry that read-only probe once; there is no automatic retry loop.

Only explicitly saved preferences enter persistent storage. They retain semantic setting IDs,
feature/revision information, and validation metadata. The inventory, observations, and unsaved
records are not persisted. A capability change can make a saved preference unsupported without
deleting it. Storage capacity is checked before saving; a failed save restores the previous
preference before a device setter can be queued.

The firmware owns protocol operations, setting identifiers, validation, and runtime state. Host
clients own labels, categories, units, and error messages. See the [serial protocol documentation](protocol/README.md)
for Host configuration commands and reporting.

## HID++ normalization

The Dongle translates supported special controls into standard USB input. Forwarding works without
a running Host client, and ordinary HID reports continue through their normal path. Automatic
normalization preserves the keyboard's PC/Mac mode, Fn-lock, and native usages and modifier
combinations. Explicit Fn-row configuration is available separately through settings.

The per-device `hidpp_enabled` preference defaults to `false` and survives device renewal. During
first-connection setup, Cordial enables it when the device answers the read-only probe as HID++ 2.0
or newer and has usable long reports. A user choice made first takes precedence. An unanswered or
refused probe leaves the setup decision for a later connection.

The adapter's `host_platform` preference selects `linux`, `windows`, or `mac`, with `linux` as the
default. Both preferences survive Bluetooth disconnects, client exit, and adapter restarts. Removing
a device deletes its HID++ preference, saved settings, and cached metadata. Pairing does not reset
the adapter's platform.

| Trigger | HID++ behavior |
| --- | --- |
| Connect while enabled | Run the activation routine. |
| Enable while connected | Run the same activation routine. |
| Change adapter platform | Save once, then run activation on each ready HID++-enabled device with the new translations. Disconnected devices use it on their next connection; disabled devices receive no platform-triggered reconfiguration. |
| Disable while connected | Reset temporary reporting, then allow read-only discovery and settings queries. |
| Connect while disabled | Read-only discovery and settings queries; no resets or setting writes. |
| Change a device's HID++ preference while disconnected | Save it for the next connection; perform no keyboard operations and queue no deferred reset. |

### Activation and recovery

Activation qualifies the descriptor's bidirectional HID++ long report, ID `0x11`, with a 19-byte
payload under vendor usage page `0xFF00` or the MX Keys Bluetooth Application usage `0xFF43:0202`.
Protocol discovery uses reported capabilities, not device names or addresses. Normalization requires
HID++ 2.0 or newer, Config Change `0x0020`, and Reprogrammable Controls `0x1B04`. General settings
discovery does not require those two normalization features.

Config Change function 1, with parameters `00 00`, resets temporary reporting. Cordial then
enumerates controls, reads their reporting settings, and enables temporary diversion for supported
translations. Additional standard controls must advertise keyboard hotkey or F-key flags and must not advertise
a mouse-button flag. They divert only when their documented standard usage is missing from all
native input maps. In that case, the missing input representation is the reason for diversion.
Controls with usable native representations keep their native reporting. Existing platform-specific
normalization retains its fixed mappings. Repeating an unchanged preference does not restart
activation.

The configuration reset leaves factory settings intact. [Controls revision 4](https://drive.google.com/file/d/1UGDCuqnKBm7U8a6t6g3QlEZgKeaAzmAx/view)
specifies that temporary diversion returns to its default on a configuration reset. The full vendor
policy for `0x0020` is not public; the command encoding follows the [Logiops reset implementation](https://github.com/PixlOne/logiops/blob/main/src/logid/backend/hidpp20/features/Reset.cpp).
Individual reporting writes use `pvalid=0` and preserve persistent diversion. Cordial does not repair
pre-existing persistent configuration or retain historical configuration snapshots or offline
diversion state. The [controls specification](https://lekensteyn.nl/files/logitech/x1b04_specialkeysmsebuttons.html)
describes the reporting fields.

Unsupported devices and nonfatal protocol failures retain ordinary HID forwarding. The HID++
preference, protocol detection, and runtime status are reported separately: enabling HID++ does not
mean a device supports it or that normalization is active. Special-key translation has no separate
reported status.

A failed live-disable reset causes neither unbounded retries nor a reset on a later disabled
connection. During a platform change, previous translations remain active until the reset is
acknowledged. A failure before that reset preserves those translations and reports the error. If
the reset fails or its reply is lost, Cordial releases held translated input to prevent stuck keys
and continues accepting previous control notifications if the device still sends them.

### Translation table

All IDs and usages below are hexadecimal. `K` is USB Keyboard/Keypad page `0x07`; `C` is Consumer page `0x0C`; `S` is Generic Desktop page `0x01`. `GUI` is Super on Linux, Windows on Windows, and Command on macOS. A `+` denotes one held shortcut chord, never a typed sequence. The platform selection affects only these HID++ translations.

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

Additional mappings use the same standard output on every platform and apply only when native
HID cannot carry that input:

| HID++ control ID | Control | USB output |
| --- | --- | --- |
| `0001`-`0007` | Volume up, volume down, mute, play/pause, next, previous, stop | `C:00E9`, `C:00EA`, `C:00E2`, `C:00CD`, `C:00B5`, `C:00B6`, `C:00B7` |
| `000B`, `000E`, `0011`-`0013`, `0028`, `0032` | Calendar, mail, word processor, spreadsheet, presentation, media player, computer | Standard Consumer application-launch usages; the Host decides which application handles them. |
| `000C`, `000D`, `0015`, `0017`, `0019`, `001B` | Close, eject, undo, redo, print, save | `C:0203`, `C:00B8`, `C:021A`, `C:0279`, `C:0208`, `C:0207` |
| `000F`, `0010` | Help, F1 | `C:0095`, `K:003A` |
| `0020`, `0022`, `0024`, `0026` | Favorites, home page, maximize, minimize | `C:022A`, `C:0223`, `C:0205`, `C:0206` |
| `003B`, `003C`, `003E`, `003F`, `0041` | Record, refresh, search, shuffle, browser stop | `C:00B2`, `C:0227`, `C:0221`, `C:00B9`, `C:0226` |
| `0040` | Sleep | `S:0082` |
| `0044`, `0047`, `004B` | Zoom in, zoom out, full screen | `C:022D`, `C:022E`, `C:0230` |
| `004C`-`004F` | Print Screen, Pause, Scroll Lock, Application | `K:0046`, `K:0048`, `K:0047`, `K:0065` |
| `0054`, `0057` | Back, forward | `C:0224`, `C:0225` |
| `00C0`, `00C1`, `0118`, `0119` | Page Down, Page Up, Home, End | `K:004E`, `K:004B`, `K:004A`, `K:004D` |

| HID++ control ID | Control | Linux | Windows | macOS |
| --- | --- | --- | --- | --- |
| `00E0` | Window overview | GUI | GUI+Tab | `C:029F` |
| `00E1` | App overview / action center | GUI+A | GUI+A | `C:02A0` |
| `006E` | Show desktop | Leave undiverted | GUI+D | GUI+`C:029F` |
| `006F` | Lock screen | `C:019E` | GUI+L | Ctrl+GUI+Q |
| `00BF` | Screenshot | Print Screen, `K:0046` | Print Screen, `K:0046` | Shift+GUI+3 |
| `00EA` | Context menu | Application, `K:0065` | Application, `K:0065` | Ctrl+Return |

Linux shortcut mappings target GNOME defaults. Show desktop is left undiverted on Linux. Custom desktop bindings may differ. Windows GUI+A opens Quick Settings on Windows 11 and Action Center on Windows 10. macOS Ctrl+Return context menus require macOS 15 or newer. The macOS Mission Control and Launchpad interpretations of `C:029F` and `C:02A0` are host conventions also used by ordinary QMK keyboards, not universal meanings of those USB usages. Calculator support is host-dependent; a host may ignore the valid usage and receives no fallback automation. References: [QMK report usages](https://github.com/qmk/qmk_firmware/blob/master/tmk_core/protocol/report.h), [GNOME shortcuts](https://help.gnome.org/gnome-help/shell-keyboard-shortcuts.html), [Windows shortcuts](https://support.microsoft.com/en-us/accessibility/windows/keyboard-shortcuts-in-windows), [Apple shortcuts](https://support.apple.com/en-us/102650), and [macOS context-menu behavior](https://developer.apple.com/videos/play/wwdc2024/10124/).

Easy-Switch controls `00D1` through `00D3`, keyboard backlight controls `00E2`/`00E3`, Fn `0034`, and Fn-lock `00DE` remain under the keyboard's control. Cordial does not divert them or substitute screen brightness for keyboard backlighting.

The diverted-controls notification reports up to four held control IDs. That is separate from the ordinary keyboard's NKRO path. Cordial combines translated presses and releases with ordinary input without releasing another source's keys or modifiers. It does not infer keys that the device did not report.

## Finding updated documentation

[Logitech's documentation repository](https://github.com/Logitech/cpg-docs) provides the
[HID++ feature index](https://github.com/Logitech/cpg-docs/blob/master/hidpp20/README.rst) and
[individual specifications](https://github.com/Logitech/cpg-docs/tree/master/hidpp20/features).
[Logitech's public document folder](https://drive.google.com/drive/folders/0BxbRzx7vEV7eWmgwazJ3NUFfQ28)
contains additional specifications, newer revisions, and control/task ID lists. Check both locations
for updates. A document can cover several feature IDs, such as Vertical Scrolling `0x2100` and
High-resolution Scrolling `0x2120`.

[Mirrored manufacturer documents](https://lekensteyn.nl/files/logitech/) include older feature
specifications and the [HID++ 2.0 protocol draft](https://lekensteyn.nl/files/logitech/logitech_hidpp_2.0_specification_draft_2012-06-04.pdf).
Compare their revisions with Logitech's published documents before using them.

[Solaar's feature catalog](https://github.com/pwr-Solaar/Solaar/blob/master/lib/logitech_receiver/hidpp20_constants.py)
provides additional feature names and IDs. Use manufacturer specifications to establish command
formats, transport restrictions, capabilities, persistence, and confirmation behavior. A catalog
entry alone does not establish that a feature supports configuration.

Update each row as support changes, keeping status reasons and verification requirements alongside the affected
operations. Follow the [Logitech scope rules](../AGENTS.md#logitech-devices).

Additional specifications for the implemented command layouts:

- [Root specification](https://github.com/Logitech/cpg-docs/blob/master/hidpp20/features/0x0000-IRoot.rst)
- [Backlight revision 2](https://drive.google.com/file/d/1QfBDVyfHihbVk2yOpJsQiKekKZGwARWm/view)
- [SmartShift](https://lekensteyn.nl/files/logitech/x2110_smartshift.html)
- [HiRes wheel](https://lekensteyn.nl/files/logitech/x2121_hires_wheel.pdf)

[features]: ../rust/crates/cordial-core/src/features.rs
[handlers]: ../rust/crates/cordial-core/src/features/handlers.rs
[hidpp]: ../rust/crates/cordial-core/src/hidpp.rs
[translation]: ../rust/crates/cordial-core/src/model/translation.rs
[settings]: ../rust/crates/cordial-core/src/settings.rs
[link]: ../rust/crates/cordial-core/src/link.rs
