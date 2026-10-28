//! Firmware rules for verified Bluetooth controller input layouts.
//!
//! Descriptor research is credited to DJm00n/ControllersInfo:
//! https://github.com/DJm00n/ControllersInfo/tree/3013812fa478e09e70de7dde77b5e74919829177/lunacontroller
//! https://github.com/DJm00n/ControllersInfo/tree/3013812fa478e09e70de7dde77b5e74919829177/dualshock4
//! Physical meanings are documented by SDL's controller-specific decoders:
//! https://github.com/libsdl-org/SDL/blob/release-3.2.0/src/joystick/hidapi/SDL_hidapi_luna.c
//! https://github.com/libsdl-org/SDL/blob/release-3.2.0/src/joystick/hidapi/SDL_hidapi_xboxone.c
//! https://github.com/bentiss/hid-tools/blob/0579ecdb4e041ce93c18ca45b3e8a2584f9ce952/hidtools/device/sony_gamepad.py#L461-L486
//!
//! Outputs use USB HID Game Pad usages: A/B/X/Y = 1/2/5/4, shoulders = 7/8,
//! Back/Start/Guide = 11/12/13, and stick presses = 14/15. Sticks use X/Y
//! and Rx/Ry; triggers use Brake/Accelerator. These rules have no saved IDs.
use super::{Found, GAMEPAD, GAMEPAD_AXES, Output, RuleRef};
use crate::hid::GamepadLayout;

const fn button(id: u32) -> Output {
    Output {
        collection: GAMEPAD,
        usage: 0x90000 | id,
    }
}
const BUTTONS: [Output; 32] = {
    let mut buttons = [button(1); 32];
    let mut i = 0;
    while i < 32 {
        buttons[i] = button(i as u32 + 1);
        i += 1;
    }
    buttons
};

pub(super) fn lookup(layout: GamepadLayout, input: RuleRef) -> Option<Found<'static>> {
    use GamepadLayout::*;
    if layout == Generic || input.collection != GAMEPAD {
        return None;
    }
    let destination = match input.input {
        0x90001..=0x90020 => {
            let bindings: &[u8] = match layout {
                XboxBluetooth => &[1, 2, 0, 5, 4, 0, 7, 8, 0, 0, 11, 12, 13, 14, 15],
                LunaBluetooth => &[1, 2, 0, 5, 4, 0, 7, 8, 0, 0, 0, 12, 0, 14, 15],
                XboxCompact => &[1, 2, 5, 4, 7, 8, 11, 12, 14, 15],
                Ds4Basic => &[5, 1, 2, 4, 7, 8, 0, 0, 11, 12, 14, 15, 13, 0],
                Generic => &[],
            };
            bindings
                .get((input.input - 0x90001) as usize)
                .copied()
                .unwrap_or(0)
        }
        0xc0224 if matches!(layout, XboxBluetooth | LunaBluetooth) => 11,
        0xc0223 if matches!(layout, XboxBluetooth | LunaBluetooth) => 13,
        0x10085 if layout == XboxCompact => 13,
        // Share, microphone, and touchpad have no ordinary XInput destination.
        0xc0001..=0xcffff | 0x10085 => 0,
        axis if GAMEPAD_AXES.contains(&axis) => {
            let destination = match (layout, axis) {
                (XboxBluetooth | LunaBluetooth | Ds4Basic, 0x10032) => 0x10033,
                (XboxBluetooth | LunaBluetooth | Ds4Basic, 0x10035) => 0x10034,
                (Ds4Basic, 0x10033) | (XboxCompact, 0x10032) => 0x200c5,
                (Ds4Basic, 0x10034) | (XboxCompact, 0x10035) => 0x200c4,
                _ => return None,
            };
            return Some(Found::Axis(destination, false));
        }
        _ => return None,
    };
    Some(Found::Remap(if destination == 0 {
        &[]
    } else {
        &BUTTONS[usize::from(destination - 1)..usize::from(destination)]
    }))
}
