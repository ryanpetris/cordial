# Gamepad Outputs

Cordial forwards Bluetooth game controllers as standard USB **HID** gamepads or **XInput** controllers on RP2040 and RP2350 boards (`pico_w`, `pico2_w`, `pimoroni_pico_plus2_w`, and `waveshare_rp2350b_plus_w`). ESP32-S3 firmware does not advertise gamepad outputs.

Each controller's output mode is saved on the adapter. Once configured, gamepads work on any host without running Cordial or any background service. Keyboard and mouse forwarding are unaffected.

## Configuring a Controller

When you pair a new controller that advertises supported gamepad controls, Cordial automatically selects **HID** on its first connection if the adapter supports HID output and fewer than four paired devices already select it; otherwise it leaves gamepad output **Disabled**.

To change a controller's mode at any time:

- **Desktop app**: Open the controller's page and choose **Disabled**, **HID**, or **XInput** under **Gamepad Output**. Changes apply immediately.
- **TUI**: Open the controller's **Settings** tab, select the mode under **Gamepad Output**, and press **Save**.
- **CLI**:

```sh
cordial --port PORT device set DEV gamepad-output hid
cordial --port PORT device set DEV gamepad-output xinput
cordial --port PORT device set DEV gamepad-output disabled
```

The control appears for devices identified as gamepads when the adapter supports at least one output mode, or whenever a device has an existing saved mode to clear.

## USB Slot Pools

HID and XInput each have an independent pool of up to 4 USB slots (`min(4, paired devices selecting that mode)`):

- Disconnected, disabled, and blocked paired devices still count toward their mode's configured pool; devices set to **Disabled** do not.
- More than four paired devices can select a mode, though at most four per mode can be active at once.
- Connecting or disconnecting a controller claims or releases an already exposed slot without reconnecting USB. An unused HID slot stays visible with neutral inputs, while an unused XInput channel reports the controller disconnected.
- Pairing, unpairing, or changing a device's output mode reconnects USB only when the set of exposed USB interfaces actually changes.
- USB gamepad slot limits are independent of the adapter's Bluetooth connection limits; see [Bluetooth capacity](protocol/limits-and-errors.md#bluetooth-capacity).

## Controller Mappings and Profiles

Both HID and XInput share the same normalized HID state. XInput applies a fixed mapping from canonical HID buttons, sticks, and triggers to standard XInput controls.

- **Automatic builtin mappings**: When a controller has no assigned [input profiles](input-profiles.md), firmware automatically normalizes recognized Xbox Bluetooth, DualShock 4, and Amazon Luna controller layouts.
- **Custom profiles**: Assigning any input profile (even an empty one) disables the builtin mapping and passes the controller's raw HID inputs to your profile layers. Removing all assigned profiles restores automatic builtin selection.

For supported HID usages, builtin layouts, and XInput encoding details, see [USB gamepad outputs](protocol/hid-forwarding.md#usb-gamepad-outputs).
