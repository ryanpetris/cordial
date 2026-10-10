//! VIA protocol 9 and Vial protocol 6 over 32-byte Raw HID reports.
//! Matrix coordinates select incoming usages; they do not describe a Device's switches.
use crate::{
    application::{Application, Editor},
    interfaces::{self, Interface},
    profiles::{
        self, BUTTON_PAGE, CONSUMER, CONSUMER_PAGE, DESKTOP_PAGE, KEYBOARD, KEYBOARD_PAGE, MOUSE,
        Output, SYSTEM, Usage, usage,
    },
    storage::RecordStore,
};
use alloc::vec::Vec;

pub const ROWS: usize = 14;
pub const COLS: usize = 16;
pub const KEYS: usize = ROWS * COLS;
const DEFINITION: &[u8] = include_bytes!("vial-definition.xz");

/// QMK's transparent keycode. It reads for an input without a rule whose own usage has no keycode
/// in the editor's table, and writing it forgets the input's rule.
const TRANSPARENT: u16 = 0x01;

const fn system(code: u16, id: u16) -> (u16, Output) {
    (
        code,
        Output {
            usage: usage(DESKTOP_PAGE, id),
            collection: SYSTEM,
        },
    )
}
const fn consumer(code: u16, id: u16) -> (u16, Output) {
    (
        code,
        Output {
            usage: usage(CONSUMER_PAGE, id),
            collection: CONSUMER,
        },
    )
}
/// QMK's System and Consumer control keycodes, the same in both keycode tables, with the usages
/// QMK reports for them, in keycode order.
const CONTROLS: [(u16, Output); 26] = [
    system(0xa5, 0x81),
    system(0xa6, 0x82),
    system(0xa7, 0x83),
    consumer(0xa8, 0xe2),
    consumer(0xa9, 0xe9),
    consumer(0xaa, 0xea),
    consumer(0xab, 0xb5),
    consumer(0xac, 0xb6),
    consumer(0xad, 0xb7),
    consumer(0xae, 0xcd),
    consumer(0xaf, 0x183),
    consumer(0xb0, 0xcc),
    consumer(0xb1, 0x18a),
    consumer(0xb2, 0x192),
    consumer(0xb3, 0x194),
    consumer(0xb4, 0x221),
    consumer(0xb5, 0x223),
    consumer(0xb6, 0x224),
    consumer(0xb7, 0x225),
    consumer(0xb8, 0x226),
    consumer(0xb9, 0x227),
    consumer(0xba, 0x22a),
    consumer(0xbb, 0xb3),
    consumer(0xbc, 0xb4),
    consumer(0xbd, 0x6f),
    consumer(0xbe, 0x70),
];
/// The index in `CONTROLS` of the first control after the media controls. The virtual keyboard's
/// last control positions select these controls in order.
const MORE_CONTROLS: usize = 10;

/// The held Consumer controls the virtual keyboard's media positions select, in matrix order.
const MEDIA_INPUTS: [u16; 7] = [0xb5, 0xb6, 0xb7, 0xcd, 0xe2, 0xe9, 0xea];

/// The input usage a matrix position selects.
pub fn input(index: usize) -> Option<Usage> {
    match index {
        0..161 => Some(usage(KEYBOARD_PAGE, index as u16 + 4)),
        161..169 => Some(usage(KEYBOARD_PAGE, (index - 161) as u16 + 0xe0)),
        169..176 => Some(usage(CONSUMER_PAGE, MEDIA_INPUTS[index - 169])),
        176..179 => Some(CONTROLS[index - 176].1.usage),
        179..195 => Some(CONTROLS[index - 179 + MORE_CONTROLS].1.usage),
        195..211 => Some(usage(BUTTON_PAGE, (index - 194) as u16)),
        _ => None,
    }
}
/// The first mouse button keycode and the number of buttons in `interface`'s keycode table. VIA
/// protocol 9 uses QMK's original keycodes and Vial protocol 6 QMK's current ones.
fn buttons(interface: Interface) -> (u16, u16) {
    match interface {
        Interface::Via => (0xf4, 5),
        Interface::Vial => (0xd1, 8),
    }
}
/// The output an input produces without a rule.
fn own(input: Usage) -> Output {
    Output {
        usage: input,
        collection: profiles::input_collection(input),
    }
}
/// The outputs writing `code` for `input` sets in `interface`'s table, or `None` when Cordial has
/// no equivalent.
fn written(interface: Interface, input: Usage, code: u16) -> Option<Vec<Output>> {
    if code == TRANSPARENT {
        Some(alloc::vec![own(input)])
    } else {
        decode(interface, code)
    }
}
fn key(id: u16) -> Output {
    Output {
        usage: usage(KEYBOARD_PAGE, id),
        collection: KEYBOARD,
    }
}
/// The outputs of a QMK keycode in `interface`'s table, or `None` when Cordial has no equivalent.
fn decode(interface: Interface, code: u16) -> Option<Vec<Output>> {
    if code == 0 {
        return Some(Vec::new());
    }
    if let Some(&(_, output)) = CONTROLS.iter().find(|&&(key, _)| key == code) {
        return Some(alloc::vec![output]);
    }
    let (first, count) = buttons(interface);
    if (first..first + count).contains(&code) {
        return Some(alloc::vec![Output {
            usage: usage(BUTTON_PAGE, code - first + 1),
            collection: MOUSE,
        }]);
    }
    if matches!(code, 4..=0xa4 | 0xe0..=0xe7) {
        return Some(alloc::vec![key(code)]);
    }
    // Modifier masks apply to keyboard keys only.
    if (0x0100..=0x1fff).contains(&code) {
        let id = code & 255;
        if id != 0 && !matches!(id, 4..=0xa4 | 0xe0..=0xe7) {
            return None;
        }
        let mods = ((code >> 8) & 15) << if code & 0x1000 != 0 { 4 } else { 0 };
        let mut outputs: Vec<Output> = (0..8)
            .filter(|bit| mods & (1 << bit) != 0)
            .map(|bit| key(0xe0 + bit))
            .collect();
        if id != 0 && !outputs.contains(&key(id)) {
            outputs.push(key(id));
        }
        return Some(outputs);
    }
    None
}
/// The QMK keycode of an input's outputs in `interface`'s table, or `None` when it cannot
/// represent them.
fn encode(interface: Interface, outputs: &[Output]) -> Option<u16> {
    if outputs.is_empty() {
        return Some(0);
    }
    if let [output] = outputs {
        if let Some(&(code, _)) = CONTROLS.iter().find(|(_, o)| o == output) {
            return Some(code);
        }
        if output.collection == MOUSE && profiles::page(output.usage) == BUTTON_PAGE {
            let (first, count) = buttons(interface);
            let button = profiles::id(output.usage);
            return (1..=count).contains(&button).then(|| first + button - 1);
        }
        if output.collection == KEYBOARD {
            let id = profiles::id(output.usage);
            return matches!(id, 4..=0xa4 | 0xe0..=0xe7).then_some(id);
        }
    }
    let mut mods = 0u8;
    let mut keys = Vec::new();
    for output in outputs {
        let id = profiles::id(output.usage);
        if output.collection != KEYBOARD || profiles::page(output.usage) != KEYBOARD_PAGE {
            return None;
        }
        if (0xe0..=0xe7).contains(&id) {
            mods |= 1 << (id - 0xe0);
        } else if matches!(id, 4..=0xa4) {
            keys.push(id);
        } else {
            return None;
        }
    }
    let id = match keys.as_slice() {
        [id] => *id,
        [] if mods & 15 != 0 && mods & 0xf0 != 0 => {
            // Modifiers from both sides: one of them is the key, the rest one side's modifiers.
            let side = if (mods & 15).count_ones() == 1 {
                mods & 15
            } else if (mods & 0xf0).count_ones() == 1 {
                mods & 0xf0
            } else {
                return None;
            };
            mods &= !side;
            0xe0 + side.trailing_zeros() as u16
        }
        [] => 0,
        _ => return None,
    };
    let prefix = if mods & 0xf0 == 0 {
        u16::from(mods) << 8
    } else if mods & 15 == 0 {
        0x1000 | (u16::from(mods >> 4) << 8)
    } else {
        return None;
    };
    Some(prefix | id)
}
impl Application {
    /// The profile `interface` edits, loaded for its editor. `None` when the interface is not
    /// enabled or the profile cannot be loaded.
    async fn editing<S: RecordStore>(
        &mut self,
        interface: Interface,
        store: &mut S,
        now: u64,
    ) -> Option<(u64, profiles::Map)> {
        let budget = self.manager.profile_budget?;
        let saved =
            interfaces::preference(&self.manager.preference.configuration_interfaces, interface);
        let id = saved
            .profile
            .filter(|_| saved.enabled && self.manager.storage_ready)?;
        if let Some(editor) = &mut self.editor
            && editor.interface == interface
            && editor.profile == id
        {
            editor.last = now;
            return Some((id, editor.map.clone()));
        }
        if self.editor.is_some() {
            // The released profile's edits are written at once, as whenever an editor is
            // released, so its table stops counting against the budget before this one loads.
            self.drop_editor();
            // Edits there is no memory to hand over are written from the editor first.
            if self.editor.is_some() && self.save_editor(store, now, false).await {
                self.drop_editor();
            }
            if self.editor.is_some() {
                return None;
            }
            self.save_all(store, now).await;
            // A save that left storage not ready leaves the new profile unedited.
            if !self.manager.storage_ready {
                return None;
            }
        }
        let map = match self.loaded(id) {
            Some(map) => map,
            None => match self.manager.profiles.load(store, &[id], budget, &[]).await {
                Ok(mut maps) => maps.pop()?,
                Err(profiles::LoadError::Lost(id)) => {
                    if !self.manager.lost_profiles.contains(&id) {
                        self.manager.lost_profiles.push(id);
                    }
                    return None;
                }
                Err(_) => return None,
            },
        };
        self.editor = Some(Editor {
            interface,
            profile: id,
            map: map.clone(),
            last: now,
            edited: None,
            failure: None,
            retry: Default::default(),
        });
        Some((id, map))
    }
    /// Answers one packet from `interface`'s editor. A write changes the loaded rules and is
    /// echoed at once, before it reaches flash: the editor's edits are saved once it pauses. An
    /// unsupported command, or a write the profile memory cannot hold, returns FF.
    pub async fn configure<S: RecordStore>(
        &mut self,
        interface: Interface,
        mut data: [u8; 32],
        store: &mut S,
        now: u64,
    ) -> [u8; 32] {
        let Some((id, map)) = self.editing(interface, store, now).await else {
            data[0] = 0xff;
            return data;
        };
        // The editor shows the profile's own remap rules. Unused matrix positions read as disabled
        // and ignore writes.
        let code_at = |index: usize| {
            let Some(input) = input(index) else {
                return Some(0);
            };
            match map.borrow().get(input) {
                None => Some(encode(interface, &[own(input)]).unwrap_or(TRANSPARENT)),
                Some(rule) => match rule.effect {
                    profiles::Effect::Remap(outputs) => encode(interface, &outputs),
                    profiles::Effect::Scale(..) => None,
                },
            }
        };
        let mut edits: Vec<(Usage, Vec<Output>)> = Vec::new();
        let mut reset = false;
        let valid = match data[0] {
            0x01 => {
                data[1..3].copy_from_slice(&9u16.to_be_bytes());
                true
            }
            0x02 => match data[1] {
                1 => {
                    data[2..6].copy_from_slice(&(now as u32).to_be_bytes());
                    true
                }
                2 | 4 => {
                    data[2..6].fill(0);
                    true
                }
                _ => false,
            },
            0x03 if data[1] == 2 && data[2..6] == [0; 4] => true,
            0x04 | 0x05 => {
                let position =
                    (data[1] == 0 && usize::from(data[2]) < ROWS && usize::from(data[3]) < COLS)
                        .then(|| usize::from(data[2]) * COLS + usize::from(data[3]));
                match position {
                    Some(index) if data[0] == 4 => match code_at(index) {
                        Some(code) => {
                            data[4..6].copy_from_slice(&code.to_be_bytes());
                            true
                        }
                        None => false,
                    },
                    Some(index) => match input(index) {
                        Some(input) => {
                            match written(interface, input, u16::from_be_bytes([data[4], data[5]]))
                            {
                                Some(outputs) => {
                                    edits.push((input, outputs));
                                    true
                                }
                                None => false,
                            }
                        }
                        None => true,
                    },
                    None => false,
                }
            }
            // Resetting the keymap forgets every rule in the profile, leaving it empty as created.
            0x06 => {
                reset = true;
                true
            }
            0x0c => {
                data[1] = 0;
                true
            }
            0x0d => {
                data[1..3].fill(0);
                true
            }
            0x11 => {
                data[1] = 1;
                true
            }
            0x12 | 0x13 => {
                let start = usize::from(u16::from_be_bytes([data[1], data[2]]));
                let count = usize::from(data[3]);
                if count > 28 || start + count > KEYS * 2 {
                    false
                } else {
                    let mut valid = true;
                    // Support partial keycodes too, validating the resulting whole action before save.
                    for i in start / 2..(start + count).div_ceil(2) {
                        // A write that replaces both bytes of a keycode needs nothing from the
                        // current rule, which this editor may not be able to represent.
                        let whole = i * 2 >= start && i * 2 + 2 <= start + count;
                        let current = if data[0] == 0x13 && whole {
                            Some(0)
                        } else {
                            code_at(i)
                        };
                        let Some(code) = current else {
                            valid = false;
                            break;
                        };
                        let mut bytes = code.to_be_bytes();
                        for (half, value) in bytes.iter_mut().enumerate() {
                            let offset = i * 2 + half;
                            if offset >= start && offset < start + count {
                                if data[0] == 0x12 {
                                    data[4 + offset - start] = *value;
                                } else {
                                    *value = data[4 + offset - start];
                                }
                            }
                        }
                        if data[0] == 0x13
                            && let Some(input) = input(i)
                        {
                            match written(interface, input, u16::from_be_bytes(bytes)) {
                                Some(outputs) => edits.push((input, outputs)),
                                None => {
                                    valid = false;
                                    break;
                                }
                            }
                        }
                    }
                    valid
                }
            }
            0xfe if interface == Interface::Vial => match data[1] {
                0 => {
                    data.fill(0);
                    data[..4].copy_from_slice(&6u32.to_le_bytes());
                    data[4..12].copy_from_slice(b"Cordial\0");
                    true
                }
                1 => {
                    data.fill(0);
                    data[..4].copy_from_slice(&(DEFINITION.len() as u32).to_le_bytes());
                    true
                }
                2 => {
                    let offset = usize::from(u16::from_le_bytes([data[2], data[3]])) * 32;
                    if offset < DEFINITION.len() {
                        data.fill(0);
                        let size = (DEFINITION.len() - offset).min(32);
                        data[..size].copy_from_slice(&DEFINITION[offset..offset + size]);
                        true
                    } else {
                        false
                    }
                }
                // Explicitly enabling Vial in Cordial authorizes fixed-action editing. No macros,
                // bootloader commands, matrix spying, or executable actions are exposed.
                5..=8 => {
                    data.fill(0xff);
                    data[0] = 1;
                    data[1] = 0;
                    true
                }
                9 => {
                    data.fill(0xff);
                    true
                }
                13 if data[2] == 0 => {
                    data.fill(0);
                    true
                }
                _ => false,
            },
            _ => false,
        };
        if !valid {
            data[0] = 0xff;
            return data;
        }
        if reset || !edits.is_empty() {
            let mut changes = Vec::new();
            for (input, outputs) in edits {
                let rule = profiles::Rule {
                    input,
                    effect: profiles::Effect::Remap(outputs),
                };
                match rule.normalized() {
                    Ok(rule) => changes.push(profiles::Change::Set(rule)),
                    Err(_) => {
                        data[0] = 0xff;
                        return data;
                    }
                }
            }
            if self.edit_rules(id, &map, reset, changes, now).is_err() {
                data[0] = 0xff;
            }
        }
        data
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::interfaces::Interface::{Via, Vial};
    fn button(id: u16) -> Output {
        Output {
            usage: usage(BUTTON_PAGE, id),
            collection: MOUSE,
        }
    }
    #[test]
    fn supported_actions_roundtrip() {
        for interface in Interface::ALL {
            for code in 0..=u16::MAX {
                if let Some(mut outputs) = decode(interface, code) {
                    let mut again = encode(interface, &outputs)
                        .and_then(|code| decode(interface, code))
                        .unwrap();
                    outputs.sort();
                    again.sort();
                    assert_eq!(again, outputs, "{interface:?} keycode {code:#06x}");
                    let rule = profiles::Rule {
                        input: usage(KEYBOARD_PAGE, 4),
                        effect: profiles::Effect::Remap(outputs),
                    };
                    assert!(
                        rule.normalized().is_ok(),
                        "{interface:?} keycode {code:#06x}"
                    );
                }
            }
            assert_eq!(
                decode(interface, 0x0204),
                Some(alloc::vec![key(0xe1), key(0x04)])
            );
            // Modifier masks apply to keyboard keys only; mouse movement and wheel keys are timed.
            for code in [
                1, 2, 3, 0x01a8, 0x01d1, 0x01f4, 0xcd, 0xd9, 0x4000, 0x5200, 0x7700,
            ] {
                assert_eq!(decode(interface, code), None, "{interface:?} {code:#06x}");
            }
            // Two modifiers from each side have no QMK keycode.
            assert_eq!(encode(interface, &[key(0xe0), key(0xe4)]), Some(0x11e0));
            assert_eq!(
                encode(interface, &[key(0xe0), key(0xe1), key(0xe4), key(0xe5)]),
                None
            );
            assert_eq!(encode(interface, &[key(0xe0), consumer(0, 0xe2).1]), None);
            assert_eq!(encode(interface, &[button(9)]), None);
            assert_eq!(
                decode(interface, 0xa5),
                Some(alloc::vec![Output {
                    usage: usage(DESKTOP_PAGE, 0x81),
                    collection: SYSTEM,
                }])
            );
            assert_eq!(
                decode(interface, 0xb0),
                Some(alloc::vec![consumer(0, 0xcc).1])
            );
        }
    }
    #[test]
    fn mouse_buttons_follow_each_interface_table() {
        assert_eq!(decode(Via, 0xf4), Some(alloc::vec![button(1)]));
        assert_eq!(decode(Via, 0xf8), Some(alloc::vec![button(5)]));
        assert_eq!(encode(Via, &[button(5)]), Some(0xf8));
        assert_eq!(encode(Via, &[button(6)]), None);
        assert_eq!(decode(Vial, 0xd1), Some(alloc::vec![button(1)]));
        assert_eq!(decode(Vial, 0xd8), Some(alloc::vec![button(8)]));
        assert_eq!(encode(Vial, &[button(8)]), Some(0xd8));
        // Each table's mouse button keycodes are not buttons in the other.
        for code in [0xd1, 0xd5, 0xd6, 0xd8] {
            assert_eq!(decode(Via, code), None);
        }
        for code in 0xf4..=0xf8 {
            assert_eq!(decode(Vial, code), None);
        }
    }
    #[test]
    fn matrix_positions_select_remappable_inputs() {
        let inputs: Vec<Usage> = (0..KEYS).map_while(input).collect();
        assert_eq!(inputs.len(), 211);
        assert!((inputs.len()..KEYS).all(|i| input(i).is_none()));
        let mut unique = inputs.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(unique.len(), inputs.len());
        assert_eq!(input(169), Some(usage(CONSUMER_PAGE, 0xb5)));
        assert_eq!(input(176), Some(usage(DESKTOP_PAGE, 0x81)));
        assert_eq!(input(178), Some(usage(DESKTOP_PAGE, 0x83)));
        assert_eq!(input(179), Some(usage(CONSUMER_PAGE, 0x183)));
        assert_eq!(input(194), Some(usage(CONSUMER_PAGE, 0x70)));
        assert_eq!(input(195), Some(usage(BUTTON_PAGE, 1)));
        assert_eq!(input(210), Some(usage(BUTTON_PAGE, 16)));
        for input in inputs {
            let rule = profiles::Rule {
                input,
                effect: profiles::Effect::Remap(alloc::vec![own(input)]),
            };
            assert!(rule.normalized().is_ok(), "{input:#x}");
            // An input reads as its own keycode when the table has one.
            let buttons = |interface| buttons(interface).1;
            for interface in Interface::ALL {
                let own = encode(interface, &[own(input)]);
                let unrepresentable = profiles::page(input) == BUTTON_PAGE
                    && profiles::id(input) > buttons(interface);
                assert_eq!(own.is_none(), unrepresentable, "{interface:?} {input:#x}");
            }
        }
    }
}
