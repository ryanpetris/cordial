#[path = "support/gamepad_descriptors.rs"]
mod descriptors;
#[allow(dead_code)]
mod support;

use cordial_core::{
    bluetooth::{Descriptor, InputReport},
    forward::{Forwarder, REPORT_KEYBOARD, REPORT_MOUSE},
    hid::{Gamepad, GamepadLayout, Held, Map},
    link::{LinkId, ServiceId},
    manager::Manager,
    model::identifiers::Transport,
    outputs::{Enumeration, Plan, Support},
    profiles::{self, Effect, GAMEPAD, KEYBOARD, MOUSE, Output, Rule, Rules},
};
use cordial_protocol::{OutputMode, Role};
use embassy_futures::block_on;
use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::{Cell, RefCell},
    rc::Rc,
};
use support::{Radio, Store, setup};

thread_local! {
    static TRACK_ALLOCATIONS: Cell<bool> = const { Cell::new(false) };
    static ALLOCATIONS: Cell<usize> = const { Cell::new(0) };
}
struct Allocator;
unsafe impl GlobalAlloc for Allocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if TRACK_ALLOCATIONS.try_with(Cell::get).unwrap_or(false) {
            ALLOCATIONS.with(|count| count.set(count.get() + 1));
        }
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        unsafe { System.dealloc(pointer, layout) }
    }
}
#[global_allocator]
static ALLOCATOR: Allocator = Allocator;

fn output(collection: u32, usage: u32) -> Output {
    Output { collection, usage }
}
fn remap(collection: u32, input: u32, outputs: &[Output]) -> Rule {
    Rule {
        collection,
        input,
        effect: Effect::Remap(outputs.to_vec()),
    }
}
fn layer(rules: Vec<Rule>) -> profiles::Map {
    Rc::new(RefCell::new(Rules::new(rules).unwrap()))
}
fn drain(forward: &mut Forwarder) -> Vec<(u8, Vec<u8>)> {
    let mut packets = Vec::new();
    while let Some(packet) = forward.packet() {
        packets.push((packet.id, packet.bytes().to_vec()));
        forward.complete();
        assert!(packets.len() < 100);
    }
    packets
}
fn input(manager: &mut Manager, link: LinkId, id: u8, bytes: &[u8]) {
    manager
        .input(&InputReport::new(link, ServiceId(7), id, bytes).unwrap(), 1)
        .unwrap();
}
fn connect(manager: &mut Manager, radio: &mut Radio, descriptor: &[u8]) -> LinkId {
    let link = manager
        .connect(0, true, 30_000, None, radio)
        .unwrap()
        .unwrap();
    manager
        .connected(
            link,
            vec![Descriptor::from_slice(ServiceId(7), descriptor).unwrap()],
            255,
            0,
        )
        .unwrap();
    link
}
fn runtime(descriptor: &[u8], transport: Transport) -> (Manager, Store, Radio, LinkId) {
    let (mut manager, store, mut radio) = setup();
    manager.devices[0].as_mut().unwrap().peer.transport = transport;
    manager.output_support = &[Support {
        role: Role::Gamepad,
        modes: &[OutputMode::Hid, OutputMode::Xinput],
    }];
    manager.outputs.prepare(77, OutputMode::Hid).unwrap();
    manager.outputs.set(77, OutputMode::Hid);
    let link = connect(&mut manager, &mut radio, descriptor);
    drain(&mut manager.forward);
    (manager, store, radio, link)
}
fn enumerate(manager: &mut Manager) {
    manager.set_output_enumeration(Some(Enumeration {
        plan: Plan {
            hid_gamepads: 1,
            xinput_gamepads: 1,
        },
        generation: 1,
        ready: true,
    }));
}
fn assign(manager: &mut Manager, link: LinkId, layers: Vec<profiles::Map>) {
    let slot = manager.connection(link).unwrap().device.unwrap();
    manager.devices[slot].as_mut().unwrap().layers = (1..=layers.len() as u64).collect();
    let update = manager
        .forward
        .prepare_maps(link.slot as usize, layers)
        .unwrap();
    manager.forward.apply_maps(update);
    manager.reconcile_outputs();
}
fn mapped(manager: &Manager, link: LinkId) -> Gamepad {
    manager.forward.gamepad(link.slot as usize)
}

#[test]
fn verified_builtin_buttons_use_usb_hid_positions_and_drop_unassigned_controls() {
    let cases: &[(&[u8], &[u8])] = &[
        (
            descriptors::LUNA_BLE,
            &[1, 2, 0, 5, 4, 0, 7, 8, 0, 0, 0, 12, 0, 14, 15],
        ),
        (
            descriptors::XBOX_ONE_S,
            &[1, 2, 0, 5, 4, 0, 7, 8, 0, 0, 11, 12, 13, 14, 15],
        ),
        (
            descriptors::XBOX_SERIES,
            &[1, 2, 0, 5, 4, 0, 7, 8, 0, 0, 11, 12, 13, 14, 15],
        ),
        (
            descriptors::XBOX_COMPACT,
            &[1, 2, 5, 4, 7, 8, 11, 12, 14, 15],
        ),
        (
            descriptors::DS4_BASIC,
            &[5, 1, 2, 4, 7, 8, 0, 0, 11, 12, 14, 15, 13, 0],
        ),
    ];
    for &(descriptor, destinations) in cases {
        let map = Map::compile(descriptor).unwrap();
        let layout = map.gamepad(&map.state().unwrap()).layout;
        assert_ne!(layout, GamepadLayout::Generic);
        for (i, &destination) in destinations.iter().enumerate() {
            let mut mapper = profiles::Mapper::new();
            let (held, pad) = mapper
                .held_with_gamepad(
                    Held::default(),
                    Gamepad {
                        layout,
                        buttons: 1 << i,
                        ..Default::default()
                    },
                )
                .unwrap();
            assert_eq!(held, Held::default());
            assert_eq!(
                pad.buttons,
                if destination == 0 {
                    0
                } else {
                    1 << (destination - 1)
                },
                "{layout:?}, source {}",
                i + 1
            );
        }
    }
}

#[test]
fn builtin_axes_use_simultaneous_routes_and_clear_absent_destinations() {
    for (layout, expected, valid) in [
        (GamepadLayout::LunaBluetooth, [1, 2, 0, 3, 6, 0, 7, 8], 0xdb),
        (GamepadLayout::XboxBluetooth, [1, 2, 0, 3, 6, 0, 7, 8], 0xdb),
        (GamepadLayout::Ds4Basic, [1, 2, 0, 3, 6, 0, 4, 5], 0xdb),
        (GamepadLayout::XboxCompact, [1, 2, 0, 4, 5, 0, 3, 6], 0xdb),
    ] {
        let mut mapper = profiles::Mapper::new();
        let declared = if matches!(
            layout,
            GamepadLayout::LunaBluetooth | GamepadLayout::XboxBluetooth
        ) {
            0xe7
        } else {
            0x3f
        };
        let (_, mapped) = mapper
            .held_with_gamepad(
                Held::default(),
                Gamepad {
                    layout,
                    axes: [1, 2, 3, 4, 5, 6, 7, 8],
                    axes_valid: declared,
                    axes_declared: declared,
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(mapped.axes, expected);
        assert_eq!(mapped.axes_valid, valid);
        assert_eq!(mapped.axes_declared, valid);
        let (_, released) = mapper
            .held_with_gamepad(
                Held::default(),
                Gamepad {
                    layout,
                    axes_declared: declared,
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(released.axes, [0; 8]);
        assert_eq!(released.axes_valid, 0);
        assert_eq!(released.axes_declared, valid);
    }
}

#[test]
fn builtin_works_without_saved_profile_budget_and_late_slot_uses_mapped_state() {
    let (mut manager, _, _, link) = runtime(descriptors::DS4_BASIC, Transport::Classic);
    assert_eq!(manager.profile_budget, None);
    let mut report = [0; 9];
    report[4] = 8 | 0x10; // Square and a neutral hat.
    report[5] = 0xc; // Digital triggers have no output buttons.
    report[6] = 2; // Touchpad has no output button.
    report[2] = 255;
    report[7] = 255;
    input(&mut manager, link, 1, &report);
    assert_eq!(mapped(&manager, link).buttons, 1 << 4);
    assert_eq!(mapped(&manager, link).axes[3], 65535);
    assert_eq!(mapped(&manager, link).axes[6], 65535);
    assert_eq!(manager.forward.pending(), 0);
    enumerate(&mut manager);
    let snapshot = manager.gamepad_outputs().hid[0];
    assert_eq!(snapshot.occupant, Some(link));
    assert_eq!(snapshot.gamepad, mapped(&manager, link));
    manager.outputs.set(77, OutputMode::Xinput);
    let snapshots = manager.gamepad_outputs();
    assert_eq!(snapshots.hid[0].occupant, None);
    assert_eq!(snapshots.xinput[0].gamepad, snapshot.gamepad);
    manager.disconnected(link, None, 2).unwrap();
    assert_eq!(manager.gamepad_outputs().xinput[0].occupant, None);
}

#[test]
fn luna_fresh_identity_settles_held_buttons_and_independent_consumer_reports() {
    let (mut manager, _, mut radio, link) = runtime(descriptors::LUNA_BLE, Transport::Ble);
    let mut report = [0; 16];
    report[13] = 8; // Original Button 4, X.
    report[15] = 3; // Back and microphone.
    input(&mut manager, link, 1, &report);
    input(&mut manager, link, 2, &[1]);
    assert_eq!(mapped(&manager, link).buttons, 1 << 3);
    manager
        .gamepad_identity(link, true, &[2, 0x71, 1, 0x19, 4, 0, 1])
        .unwrap();
    assert_eq!(mapped(&manager, link).buttons, 1 << 3); // Wrong vendor namespace.
    manager
        .gamepad_identity(link, true, &[1, 0x71, 1, 0x19, 4, 0, 1])
        .unwrap();
    assert_eq!(
        mapped(&manager, link).buttons,
        (1 << 4) | (1 << 10) | (1 << 12)
    );
    enumerate(&mut manager);
    report[13] = 0;
    report[15] = 0;
    input(&mut manager, link, 1, &report);
    assert_eq!(manager.gamepad_outputs().hid[0].gamepad.buttons, 1 << 12);
    input(&mut manager, link, 0xf0, &[0; 80]);
    assert_eq!(mapped(&manager, link).buttons, 1 << 12);
    input(&mut manager, link, 2, &[0]);
    assert_eq!(mapped(&manager, link).buttons, 0);
    manager.disconnected(link, None, 2).unwrap();
    let next = connect(&mut manager, &mut radio, descriptors::LUNA_BLE);
    assert_ne!(next, link);
    report[13] = 8;
    input(&mut manager, next, 1, &report);
    manager
        .gamepad_identity(link, true, &[1, 0x71, 1, 0x19, 4, 0, 1])
        .unwrap();
    assert_eq!(mapped(&manager, next).buttons, 1 << 3);
    manager
        .gamepad_identity(next, true, &[1, 0x71, 1, 0x19, 4, 0, 1])
        .unwrap();
    assert_eq!(mapped(&manager, next).buttons, 1 << 4);
}

#[test]
fn identities_must_match_the_specific_verified_geometry() {
    for (descriptor, correct, wrong) in [
        (
            descriptors::XBOX_ONE_S,
            [2, 0x5e, 4, 0xfd, 2, 0, 1],
            [2, 0x5e, 4, 0x13, 0xb, 0, 1],
        ),
        (
            descriptors::XBOX_SERIES,
            [2, 0x5e, 4, 0x13, 0xb, 0, 1],
            [2, 0x5e, 4, 0xfd, 2, 0, 1],
        ),
        (
            descriptors::XBOX_COMPACT,
            [2, 0x5e, 4, 0xfd, 2, 0, 1],
            [2, 0x5e, 4, 0x13, 0xb, 0, 1],
        ),
        (
            descriptors::DS4_BASIC,
            [2, 0x4c, 5, 0xc4, 5, 0, 1],
            [1, 0x4c, 5, 0xc4, 5, 0, 1],
        ),
    ] {
        let (mut manager, _, _, link) = runtime(descriptor, Transport::Ble);
        manager.gamepad_identity(link, true, &wrong).unwrap();
        assert_eq!(
            manager
                .connection(link)
                .unwrap()
                .runtime
                .as_ref()
                .unwrap()
                .gamepad()
                .layout,
            GamepadLayout::Generic
        );
        manager.gamepad_identity(link, true, &correct).unwrap();
        assert_ne!(
            manager
                .connection(link)
                .unwrap()
                .runtime
                .as_ref()
                .unwrap()
                .gamepad()
                .layout,
            GamepadLayout::Generic
        );
    }
    let mut changed = descriptors::LUNA_BLE.to_vec();
    let offset = changed
        .windows(3)
        .position(|b| b == [0x26, 0xff, 3])
        .unwrap();
    changed[offset + 1] = 0xfe;
    let map = Map::compile(&changed).unwrap();
    assert_eq!(
        map.gamepad(&map.state().unwrap()).layout,
        GamepadLayout::Generic
    );
}

#[test]
fn xbox_one_s_ble_uses_only_the_verified_modern_geometry_and_usb_identity_namespace() {
    // SDL identifies 0x0b20 as Xbox One S revision 2 BLE:
    // https://github.com/libsdl-org/SDL/blob/release-2.30.10/src/joystick/usb_ids.h
    // xpadneo gives it the modern Share-button descriptor capability:
    // https://github.com/atar-axis/xpadneo/blob/v0.10.5/hid-xpadneo/src/xpadneo/core.c
    // The modern 283-byte descriptor is the XBOX_SERIES fixture, CRC16 0x931d:
    // https://github.com/atar-axis/xpadneo/blob/v0.10.5/docs/descriptors/xbxs.md
    // A 0x0b20 capture also reports a 283-byte descriptor:
    // https://github.com/atar-axis/xpadneo/issues/490#issuecomment-2518380984
    // SDL's Bluetooth decoder handles its 17-byte state packet:
    // https://github.com/libsdl-org/SDL/blob/release-2.30.10/src/joystick/hidapi/SDL_hidapi_xboxone.c
    for descriptor in [
        descriptors::XBOX_SERIES,
        descriptors::XBOX_ONE_S,
        descriptors::XBOX_COMPACT,
    ] {
        let (mut manager, _, _, link) = runtime(descriptor, Transport::Ble);
        for identity in [
            [1, 0x5e, 4, 0x20, 0xb, 0, 1],
            [2, 0x4c, 5, 0x20, 0xb, 0, 1],
            [2, 0x5e, 4, 0x21, 0xb, 0, 1],
            [2, 0x5e, 4, 0x22, 0xb, 0, 1],
        ] {
            manager.gamepad_identity(link, true, &identity).unwrap();
            assert_eq!(
                manager
                    .connection(link)
                    .unwrap()
                    .runtime
                    .as_ref()
                    .unwrap()
                    .gamepad()
                    .layout,
                GamepadLayout::Generic
            );
        }
        let mut report = [0; 16];
        report[13] = 1 << 3;
        input(&mut manager, link, 1, &report);
        manager
            .gamepad_identity(link, true, &[2, 0x5e, 4, 0x20, 0xb, 0, 1])
            .unwrap();
        let pad = manager
            .connection(link)
            .unwrap()
            .runtime
            .as_ref()
            .unwrap()
            .gamepad();
        if descriptor == descriptors::XBOX_SERIES {
            assert_eq!(pad.layout, GamepadLayout::XboxBluetooth);
            assert_eq!(mapped(&manager, link).buttons, 1 << 4);
            assign(&mut manager, link, vec![layer(vec![])]);
            input(&mut manager, link, 1, &[0; 16]);
            input(&mut manager, link, 1, &report);
            assert_eq!(mapped(&manager, link).buttons, 1 << 3);
        } else {
            assert_eq!(pad.layout, GamepadLayout::Generic);
        }
    }
    let (mut manager, _, _, link) = runtime(descriptors::XBOX_SERIES, Transport::Classic);
    manager
        .gamepad_identity(link, true, &[2, 0x5e, 4, 0x20, 0xb, 0, 1])
        .unwrap();
    assert_eq!(
        manager
            .connection(link)
            .unwrap()
            .runtime
            .as_ref()
            .unwrap()
            .gamepad()
            .layout,
        GamepadLayout::Generic
    );
}

#[test]
fn custom_profiles_receive_original_controls_and_replace_the_entire_builtin() {
    let (mut manager, _, _, link) = runtime(descriptors::LUNA_BLE, Transport::Ble);
    manager
        .gamepad_identity(link, true, &[1, 0x71, 1, 0x19, 4, 0, 1])
        .unwrap();
    assign(
        &mut manager,
        link,
        vec![layer(vec![
            remap(GAMEPAD, 0x90004, &[output(KEYBOARD, 0x70004)]),
            remap(GAMEPAD, 0xc0221, &[output(MOUSE, 0x90001)]),
            remap(GAMEPAD, 0xc0223, &[output(KEYBOARD, 0x70005)]),
        ])],
    );
    let mut report = [0; 16];
    report[13] = 0x18; // Original buttons 4 and 5.
    report[15] = 2;
    input(&mut manager, link, 1, &report);
    input(&mut manager, link, 2, &[1]);
    let pad = mapped(&manager, link);
    assert_eq!(pad.buttons, 1 << 4); // Button 5 stays original; built-in would turn it into 4.
    assert_eq!(pad.axes_declared, 0xe7);
    let packets = drain(&mut manager.forward);
    assert!(
        packets
            .iter()
            .any(|(id, b)| *id == REPORT_KEYBOARD && b[0] & 0x30 == 0x30)
    );
    assert!(
        packets
            .iter()
            .any(|(id, b)| *id == REPORT_MOUSE && b[0] == 1)
    );
    report[13] = 0;
    report[15] = 0;
    input(&mut manager, link, 1, &report);
    let packets = drain(&mut manager.forward);
    assert!(
        packets
            .iter()
            .any(|(id, b)| *id == REPORT_KEYBOARD && b[0] == 0x20)
    );
    assert!(
        packets
            .iter()
            .any(|(id, b)| *id == REPORT_MOUSE && b[0] == 0)
    );
    assert_eq!(
        manager.input(&InputReport::new(link, ServiceId(7), 1, &[]).unwrap(), 2),
        Err(cordial_core::model::errors::ErrorCode::ConnectionFailed)
    );
    assert_eq!(mapped(&manager, link).buttons, 0);
    assert!(
        drain(&mut manager.forward)
            .iter()
            .any(|(id, b)| *id == REPORT_KEYBOARD && b[0] == 0)
    );
    assert_eq!(manager.output_slots(OutputMode::Hid), None);
}

#[test]
fn empty_and_keyboard_profiles_disable_builtin() {
    for rules in [
        layer(vec![]),
        layer(vec![remap(KEYBOARD, 0x70004, &[output(KEYBOARD, 0x70005)])]),
    ] {
        let (mut manager, _, _, link) = runtime(descriptors::DS4_BASIC, Transport::Classic);
        assign(&mut manager, link, vec![rules]);
        let mut report = [0; 9];
        report[4] = 8 | 0x10;
        report[2] = 255;
        input(&mut manager, link, 1, &report);
        assert_eq!(mapped(&manager, link).buttons, 1);
        assert_eq!(mapped(&manager, link).axes[2], 65535);
        assert_eq!(mapped(&manager, link).axes_declared, 0x3f);
        assert!(drain(&mut manager.forward).is_empty());
    }
}

#[test]
fn assigned_profile_edits_and_removal_keep_press_time_outputs() {
    let (mut manager, _, _, link) = runtime(descriptors::DS4_BASIC, Transport::Classic);
    let rules = layer(vec![remap(KEYBOARD, 0x70004, &[output(KEYBOARD, 0x70005)])]);
    assign(&mut manager, link, vec![layer(vec![]), rules.clone()]);
    let mut report = [0; 9];
    report[4] = 8 | 0x10;
    input(&mut manager, link, 1, &report);
    assert_eq!(mapped(&manager, link).buttons, 1);
    *rules.borrow_mut() =
        Rules::new(vec![remap(GAMEPAD, 0x90001, &[output(KEYBOARD, 0x70006)])]).unwrap();
    manager.reconcile_outputs();
    assert_eq!(mapped(&manager, link).buttons, 1);
    assert!(drain(&mut manager.forward).is_empty());
    report[4] = 8;
    input(&mut manager, link, 1, &report);
    assert_eq!(mapped(&manager, link).buttons, 0);
    report[4] = 8 | 0x10;
    input(&mut manager, link, 1, &report);
    assert_eq!(mapped(&manager, link).buttons, 0);
    assert!(
        drain(&mut manager.forward)
            .iter()
            .any(|(id, b)| *id == REPORT_KEYBOARD && b[0] == 0x40)
    );
    *rules.borrow_mut() = Rules::default();
    manager.reconcile_outputs();
    assert_eq!(mapped(&manager, link).buttons, 0);
    assert!(drain(&mut manager.forward).is_empty());
    report[4] = 8;
    input(&mut manager, link, 1, &report);
    assert!(
        drain(&mut manager.forward)
            .iter()
            .any(|(id, b)| *id == REPORT_KEYBOARD && b[0] == 0)
    );
    report[4] = 0x18;
    input(&mut manager, link, 1, &report);
    assert_eq!(mapped(&manager, link).buttons, 1);
    assign(&mut manager, link, vec![]);
    assert_eq!(mapped(&manager, link).buttons, 1);
    assert_eq!(mapped(&manager, link).axes_declared, 0xdb);
    report[4] = 8;
    input(&mut manager, link, 1, &report);
    report[4] = 0x18;
    input(&mut manager, link, 1, &report);
    assert_eq!(mapped(&manager, link).buttons, 1 << 4);
}

#[test]
fn saved_assignments_disable_builtin_across_load_failure_retry_and_reconnect() {
    use cordial_core::model::errors::ErrorCode;
    for storage_failure in [false, true] {
        let (mut manager, mut store, mut radio, link) =
            runtime(descriptors::DS4_BASIC, Transport::Classic);
        manager.profile_budget = Some(if storage_failure { 4096 } else { 0 });
        manager.devices[0].as_mut().unwrap().layers = vec![23];
        manager.reconcile_outputs();
        let mut report = [0; 9];
        report[4] = 0x18;
        input(&mut manager, link, 1, &report);
        assert_eq!(mapped(&manager, link).buttons, 1);
        store.fail = storage_failure;
        block_on(manager.load_profiles(0, &mut store));
        assert_eq!(
            manager.devices[0]
                .as_ref()
                .unwrap()
                .live
                .as_ref()
                .unwrap()
                .profile_error,
            Some(if storage_failure {
                ErrorCode::StorageFailed
            } else {
                ErrorCode::Capacity
            })
        );
        assert!(manager.forward.layers(link.slot as usize).is_empty());
        report[4] = 8;
        input(&mut manager, link, 1, &report);
        report[4] = 0x18;
        input(&mut manager, link, 1, &report);
        assert_eq!(mapped(&manager, link).buttons, 1);
        assert_eq!(mapped(&manager, link).axes_declared, 0x3f);
        store.fail = false;
        manager.profile_budget = Some(4096);
        block_on(manager.load_profiles(0, &mut store));
        assert_eq!(manager.forward.layers(link.slot as usize).len(), 1);
        assert!(
            manager.devices[0]
                .as_ref()
                .unwrap()
                .live
                .as_ref()
                .unwrap()
                .profile_error
                .is_none()
        );
        manager.disconnected(link, None, 2).unwrap();
        let next = connect(&mut manager, &mut radio, descriptors::DS4_BASIC);
        input(&mut manager, next, 1, &report);
        assert_eq!(mapped(&manager, next).buttons, 1);
        block_on(manager.load_profiles(0, &mut store));
        manager.reconcile_outputs();
        assert_eq!(mapped(&manager, next).buttons, 1);
        manager.devices[0].as_mut().unwrap().layers.clear();
        block_on(manager.load_profiles(0, &mut store));
        assert_eq!(mapped(&manager, next).buttons, 1);
        report[4] = 8;
        input(&mut manager, next, 1, &report);
        report[4] = 0x18;
        input(&mut manager, next, 1, &report);
        assert_eq!(mapped(&manager, next).buttons, 1 << 4);
    }
}

#[test]
fn corrupt_assigned_remaps_keep_builtin_mapping_disabled() {
    use cordial_core::{model::errors::ErrorCode, storage::record_key};
    use cordial_protocol::storage as saved;
    use prost::Message;

    for output in [
        saved::Output {
            collection: 0x000c_0002,
            usage: profiles::usage(profiles::BUTTON_PAGE, 1),
        },
        saved::Output {
            collection: KEYBOARD,
            usage: 0x0107_0004,
        },
    ] {
        let (mut manager, mut store, _, link) = runtime(descriptors::DS4_BASIC, Transport::Classic);
        manager.profile_budget = Some(4096);
        manager.devices[0].as_mut().unwrap().layers = vec![23];
        store.records.insert(
            record_key(profiles::RULES, 23),
            saved::Rules {
                rules: vec![saved::Rule {
                    input: profiles::usage(profiles::BUTTON_PAGE, 5),
                    collection: GAMEPAD,
                    effect: Some(saved::rule::Effect::Remap(saved::Remap {
                        outputs: vec![output],
                    })),
                }],
            }
            .encode_to_vec(),
        );
        block_on(manager.load_profiles(0, &mut store));
        let device = manager.devices[0].as_ref().unwrap();
        assert_eq!(device.layers, [23]);
        assert_eq!(
            device.live.as_ref().unwrap().profile_error,
            Some(ErrorCode::StorageFailed)
        );
        assert!(manager.forward.layers(link.slot as usize).is_empty());
        let mut report = [0; 9];
        report[4] = 0x18;
        input(&mut manager, link, 1, &report);
        assert_eq!(mapped(&manager, link).buttons, 1);
        assert_eq!(mapped(&manager, link).axes_declared, 0x3f);
    }
}

#[test]
fn hats_are_remappable_directions_without_passthrough_and_analog_updates_do_not_queue() {
    let (mut manager, _, _, link) = runtime(descriptors::DS4_BASIC, Transport::Classic);
    assign(
        &mut manager,
        link,
        vec![layer(vec![remap(
            GAMEPAD,
            0x10090,
            &[output(KEYBOARD, 0x70004)],
        )])],
    );
    let mut report = [0; 9];
    input(&mut manager, link, 1, &report); // North.
    assert_eq!(mapped(&manager, link).hat, None);
    assert_eq!(mapped(&manager, link).dpad, 0);
    let packets = drain(&mut manager.forward);
    assert_eq!(
        packets
            .iter()
            .filter(|(id, b)| *id == REPORT_KEYBOARD && b[0] == 0x10)
            .count(),
        1
    );
    report[4] = 1; // North-east keeps the mapped north and passes through east.
    input(&mut manager, link, 1, &report);
    assert_eq!(mapped(&manager, link).dpad, 4);
    assert!(drain(&mut manager.forward).is_empty());
    report[4] = 8;
    input(&mut manager, link, 1, &report);
    drain(&mut manager.forward);
    for i in 0..10_000u32 {
        report[0] = i as u8;
        input(&mut manager, link, 1, &report);
        manager.reconcile_outputs();
    }
    assert_eq!(manager.forward.pending(), 0);
}

#[test]
fn keyboard_outputs_reach_gamepad_slot_and_release_after_profile_edit() {
    let (mut manager, _, _, link) = runtime(
        &[
            5, 1, 9, 6, 0xa1, 1, 5, 7, 0x19, 4, 0x29, 11, 0x15, 0, 0x25, 1, 0x75, 1, 0x95, 8, 0x81,
            2, 0xc0,
        ],
        Transport::Classic,
    );
    let rules = layer(vec![remap(KEYBOARD, 0x70004, &[output(GAMEPAD, 0x90005)])]);
    assign(&mut manager, link, vec![rules.clone()]);
    enumerate(&mut manager);
    input(&mut manager, link, 0, &[1]);
    assert_eq!(manager.gamepad_outputs().hid[0].occupant, Some(link));
    assert_eq!(manager.gamepad_outputs().hid[0].gamepad.buttons, 1 << 4);
    *rules.borrow_mut() = Rules::default();
    manager.reconcile_outputs();
    assert_eq!(manager.gamepad_outputs().hid[0].gamepad.buttons, 1 << 4);
    input(&mut manager, link, 0, &[0]);
    assert_eq!(manager.gamepad_outputs().hid[0].occupant, None);
    assert!(drain(&mut manager.forward).is_empty());
}

#[test]
fn late_identity_preserves_custom_press_time_outputs() {
    let (mut manager, _, _, link) = runtime(descriptors::LUNA_BLE, Transport::Ble);
    let rules = layer(vec![remap(GAMEPAD, 0x90004, &[output(KEYBOARD, 0x70004)])]);
    assign(&mut manager, link, vec![rules.clone()]);
    let mut report = [0; 16];
    report[13] = 8;
    input(&mut manager, link, 1, &report);
    drain(&mut manager.forward);
    *rules.borrow_mut() =
        Rules::new(vec![remap(GAMEPAD, 0x90004, &[output(KEYBOARD, 0x70005)])]).unwrap();
    manager
        .gamepad_identity(link, true, &[1, 0x71, 1, 0x19, 4, 0, 1])
        .unwrap();
    assert!(drain(&mut manager.forward).is_empty());
    report[13] = 0;
    input(&mut manager, link, 1, &report);
    assert!(
        drain(&mut manager.forward)
            .iter()
            .any(|(id, b)| *id == REPORT_KEYBOARD && b[0] == 0)
    );
}

#[test]
fn xbox_consumer_reports_compose_with_numbered_buttons_and_release_independently() {
    let (mut manager, _, _, link) = runtime(descriptors::XBOX_ONE_S, Transport::Classic);
    let mut report = [0; 16];
    report[14] = 4; // Button 11, Back.
    report[15] = 1; // AC Back also holds Back.
    input(&mut manager, link, 1, &report);
    input(&mut manager, link, 2, &[1]);
    assert_eq!(mapped(&manager, link).buttons, (1 << 10) | (1 << 12));
    report[14] = 0;
    input(&mut manager, link, 1, &report);
    assert_eq!(mapped(&manager, link).buttons, (1 << 10) | (1 << 12));
    report[15] = 0;
    input(&mut manager, link, 1, &report);
    assert_eq!(mapped(&manager, link).buttons, 1 << 12);
    input(&mut manager, link, 2, &[0]);
    assert_eq!(mapped(&manager, link).buttons, 0);
    assert!(drain(&mut manager.forward).is_empty());
}

#[test]
fn relayout_releases_cross_collection_controls_and_reconnect_drops_the_old_mapper() {
    let (mut manager, _, mut radio, link) = runtime(descriptors::DS4_BASIC, Transport::Classic);
    assign(
        &mut manager,
        link,
        vec![layer(vec![remap(
            GAMEPAD,
            0x90001,
            &[output(KEYBOARD, 0x70004)],
        )])],
    );
    let mut report = [0; 9];
    report[4] = 0x18;
    input(&mut manager, link, 1, &report);
    drain(&mut manager.forward);
    manager
        .relayout(
            link,
            vec![Descriptor::from_slice(ServiceId(7), descriptors::DS4_BASIC).unwrap()],
        )
        .unwrap();
    assert!(
        drain(&mut manager.forward)
            .iter()
            .any(|(id, b)| *id == REPORT_KEYBOARD && b[0] == 0)
    );
    input(&mut manager, link, 1, &report);
    assert_eq!(mapped(&manager, link).buttons, 1);
    manager.disconnected(link, None, 2).unwrap();
    let next = connect(&mut manager, &mut radio, descriptors::DS4_BASIC);
    input(&mut manager, next, 1, &report);
    assert_eq!(mapped(&manager, next).buttons, 1);
    assert!(drain(&mut manager.forward).is_empty());
}

#[test]
fn gamepad_consumers_keep_collection_identity() {
    let gamepad = [
        5, 1, 9, 5, 0xa1, 1, 5, 12, 0xa, 0x23, 2, 0x15, 0, 0x25, 1, 0x75, 1, 0x95, 1, 0x81, 2,
        0x75, 7, 0x81, 3, 0xc0,
    ];
    let consumer = [
        5, 12, 9, 1, 0xa1, 1, 0xa, 0x23, 2, 0x15, 0, 0x25, 1, 0x75, 1, 0x95, 1, 0x81, 2, 0x75, 7,
        0x81, 3, 0xc0,
    ];
    let map = Map::compile(&[gamepad.as_slice(), consumer.as_slice()].concat()).unwrap();
    let mut state = map.state().unwrap();
    let input = map.decode(&mut state, 0, &[1, 1]).unwrap();
    assert_eq!(input.held.consumers[0], 0x223);
    assert_eq!(map.gamepad(&state).consumers[0], 0x223);
    let mut mapper = profiles::Mapper::new();
    mapper.layers = vec![layer(vec![remap(
        GAMEPAD,
        0xc0223,
        &[output(KEYBOARD, 0x70004)],
    )])];
    let (held, _) = mapper
        .held_with_gamepad(input.held, map.gamepad(&state))
        .unwrap();
    assert_eq!(held.keys[0], 0x10);
    assert_eq!(held.consumers[0], 0x223);
}

#[test]
fn mouse_and_gamepad_buttons_with_the_same_usage_forward_once_to_separate_collections() {
    let descriptor = [
        5, 1, 9, 5, 0xa1, 1, 5, 9, 9, 1, 0x15, 0, 0x25, 1, 0x75, 1, 0x95, 1, 0x81, 2, 0x75, 7,
        0x81, 3, 0xc0, 5, 1, 9, 2, 0xa1, 1, 5, 9, 9, 1, 0x15, 0, 0x25, 1, 0x75, 1, 0x95, 1, 0x81,
        2, 0x75, 7, 0x81, 3, 0xc0,
    ];
    let (mut manager, _, _, link) = runtime(&descriptor, Transport::Classic);
    assign(
        &mut manager,
        link,
        vec![layer(vec![
            remap(GAMEPAD, 0x90001, &[output(KEYBOARD, 0x70004)]),
            remap(MOUSE, 0x90001, &[output(GAMEPAD, 0x90005)]),
        ])],
    );
    enumerate(&mut manager);
    input(&mut manager, link, 0, &[1, 1]);
    assert_eq!(manager.gamepad_outputs().hid[0].gamepad.buttons, 1 << 4);
    let packets = drain(&mut manager.forward);
    assert_eq!(
        packets
            .iter()
            .filter(|(id, b)| *id == REPORT_KEYBOARD && b[0] == 0x10)
            .count(),
        1
    );
    assert!(!packets.iter().any(|(id, _)| *id == REPORT_MOUSE));
    input(&mut manager, link, 0, &[0, 1]);
    assert_eq!(mapped(&manager, link).buttons, 1 << 4);
    assert!(
        drain(&mut manager.forward)
            .iter()
            .any(|(id, b)| *id == REPORT_KEYBOARD && b[0] == 0)
    );
    input(&mut manager, link, 0, &[0, 0]);
    assert_eq!(mapped(&manager, link).buttons, 0);
    manager.output_support = &[];
    assert_eq!(manager.gamepad_outputs().hid[0].occupant, None);
}

#[test]
fn gamepad_input_and_reconciliation_do_not_allocate_or_queue_after_connection() {
    for assigned in [false, true] {
        let (mut manager, _, _, link) = runtime(descriptors::LUNA_BLE, Transport::Ble);
        manager
            .gamepad_identity(link, true, &[1, 0x71, 1, 0x19, 4, 0, 1])
            .unwrap();
        if assigned {
            assign(&mut manager, link, vec![layer(vec![])]);
        }
        enumerate(&mut manager);
        let mut report = [0; 16];
        ALLOCATIONS.set(0);
        TRACK_ALLOCATIONS.set(true);
        for i in 0..512u16 {
            report[0..2].copy_from_slice(&i.to_le_bytes());
            report[12] = (i % 9) as u8;
            report[13] = i as u8;
            report[14] = (i >> 8) as u8 | 0x68;
            report[15] = (i & 3) as u8;
            input(&mut manager, link, 1, &report);
            input(&mut manager, link, 2, &[(i & 1) as u8]);
            manager.gamepad_outputs();
        }
        TRACK_ALLOCATIONS.set(false);
        assert_eq!(ALLOCATIONS.get(), 0);
        assert_eq!(manager.forward.pending(), 0);
    }
}

#[test]
fn excess_consumers_and_repeated_spans_map_and_release_without_allocating() {
    for descriptor in [descriptors::CONSUMER_ARRAY, descriptors::CONSUMER_VARIABLES] {
        let (mut manager, _, _, link) = runtime(descriptor, Transport::Ble);
        assign(
            &mut manager,
            link,
            vec![layer(vec![remap(
                GAMEPAD,
                0xc0009,
                &[output(KEYBOARD, 0x70004)],
            )])],
        );
        enumerate(&mut manager);
        let array = descriptor == descriptors::CONSUMER_ARRAY;
        let report_id = if array { 2 } else { 0 };
        let mut selectors = [0; 18];
        for (i, slot) in selectors.as_chunks_mut::<2>().0.iter_mut().enumerate() {
            slot.copy_from_slice(&(i as u16 + 1).to_le_bytes());
        }
        let mut presses = 0;
        let mut releases = 0;
        ALLOCATIONS.set(0);
        TRACK_ALLOCATIONS.set(true);
        for i in 0..512 {
            if array {
                selectors[..2].copy_from_slice(&(if i % 4 == 0 { 1u16 } else { 0 }).to_le_bytes());
                if i % 4 == 3 {
                    input(&mut manager, link, report_id, &[0; 18]);
                } else {
                    input(&mut manager, link, report_id, &selectors);
                }
            } else {
                input(
                    &mut manager,
                    link,
                    report_id,
                    &[[255, 255], [0, 255], [0, 128], [0, 0]][i % 4],
                );
            }
            manager.gamepad_outputs();
            while let Some(packet) = manager.forward.packet() {
                if packet.id == REPORT_KEYBOARD {
                    if packet.bytes()[0] & 0x10 != 0 {
                        presses += 1;
                    } else {
                        releases += 1;
                    }
                }
                manager.forward.complete();
            }
        }
        TRACK_ALLOCATIONS.set(false);
        assert_eq!(ALLOCATIONS.get(), 0);
        assert_eq!(presses, 128);
        assert_eq!(releases, 128);
        assert_eq!(manager.forward.pending(), 0);
    }
}

#[test]
fn controls_split_between_gamepad_applications_do_not_select_one_builtin() {
    let mut descriptor = descriptors::DS4_BASIC.to_vec();
    let offset = descriptor
        .windows(4)
        .position(|b| b == [5, 9, 0x19, 1])
        .unwrap();
    descriptor.splice(offset..offset, [0xc0, 5, 1, 9, 5, 0xa1, 1]);
    let map = Map::compile(&descriptor).unwrap();
    assert_eq!(
        map.gamepad(&map.state().unwrap()).layout,
        GamepadLayout::Generic
    );
}
