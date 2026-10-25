# Gamepad outputs

Pico W, Pico 2 W and Waveshare RP2350B-Plus-W support HID and XInput gamepad outputs.

After pairing a controller, open its Settings tab, select **HID** or **XInput** under
**Gamepad Output**, and save. The control appears when the adapter advertises an enabled mode or a
saved choice needs clearing. The default is **Disabled** for any controller without a saved
choice. Keyboard and mouse forwarding is unaffected.

The adapter saves each controller's choice and forwards input without a Cordial app or host
service running. HID and XInput each have an independent USB pool of
`min(4, paired devices selecting that mode)` slots. Disconnected devices still count; devices with
gamepad output Disabled do not. More than four paired devices may select a mode. Bluetooth
connections have independent allowances per transport, separate from the USB pools. See
[Bluetooth capacity](protocol/limits-and-errors.md#bluetooth-capacity).

Connecting or disconnecting a controller assigns or releases a slot without reconnecting USB. An
empty HID slot stays visible with neutral controls; an empty XInput channel reports the controller
absent. Pairing, unpairing or changing a saved mode reconnects USB when a pool's size changes.

Controller support depends on its reported HID layout. See the
[mapping limits](protocol/hid-forwarding.md#usb-gamepad-outputs).
