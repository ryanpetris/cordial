use super::*;
#[path = "../support/gamepad_descriptors.rs"]
#[allow(dead_code)]
mod known;
use cordial_core::{
    bluetooth::Descriptor,
    outputs::{Enumeration, Plan, Support, gamepad},
};

fn pending() -> Test {
    let mut t = Test::new(true);
    t.app.manager.output_support = &[Support {
        role: p::Role::Gamepad,
        modes: &[p::OutputMode::Hid, p::OutputMode::Xinput],
    }];
    pending_setup(&mut t);
    t
}

fn descriptors() -> Vec<Descriptor> {
    vec![
        Descriptor::from_slice(
            ServiceId(8),
            &[
                5, 1, 9, 5, 0xa1, 1, 5, 9, 0x19, 1, 0x29, 8, 0x15, 0, 0x25, 1, 0x75, 1, 0x95, 8,
                0x81, 2, 0xc0,
            ],
        )
        .unwrap(),
    ]
}

fn finish(t: &mut Test) -> Vec<Ev> {
    t.settle();
    t.events()
}

fn reconnect(t: &mut Test, link: cordial_core::link::LinkId, maps: Vec<Descriptor>) {
    t.event(Event::Disconnected { link, error: None });
    t.now += FIRST_RETRY;
    let next = connect_saved(t, maps);
    assert_ne!(link, next);
    finish(t);
}

fn reserve_nonresident(t: &mut Test, count: u8) {
    for n in 2..=count + 1 {
        let device = t.add_saved(n);
        t.set_device(p::SetDevice {
            device,
            enabled: Some(false),
            output_preferences: vec![output(p::OutputMode::Hid)],
            ..Default::default()
        });
        assert!(t.app.manager.find(device.into()).is_none());
    }
    assert_eq!(t.app.manager.outputs.plan().hid_gamepads, count);
}

#[test]
fn new_gamepad_enables_hid_after_saving_and_reports_the_output() {
    let mut t = pending();
    let link = connect_saved(&mut t, descriptors());
    assert_eq!(t.app.manager.outputs.plan(), Plan::default());
    let events = finish(&mut t);
    let saved = t.saved(SAVED);
    assert_eq!(gamepad(&saved.output_preferences), p::OutputMode::Hid);
    assert!(!saved.setup_pending);
    assert_eq!(t.app.manager.outputs.plan().hid_gamepads, 1);
    let device = events
        .iter()
        .rev()
        .find_map(|e| match e {
            Ev::Device(d) if d.id == SAVED => Some(d),
            _ => None,
        })
        .unwrap();
    assert_eq!(device.output_preferences, [output(p::OutputMode::Hid)]);
    assert_eq!(
        device.outputs[0].status,
        Some(p::output_status::Status::State(
            p::OutputState::Pending as i32
        ))
    );
    t.app.set_output_enumeration(Some(Enumeration {
        plan: t.app.manager.outputs.plan(),
        generation: 1,
        ready: true,
    }));
    assert!(t.events().iter().any(|e| matches!(e, Ev::Adapter(_))));
    assert_eq!(
        t.device(SAVED).outputs[0].status,
        Some(p::output_status::Status::State(
            p::OutputState::Active as i32
        ))
    );
    assert_eq!(t.app.gamepad_outputs().hid[0].occupant, Some(link));

    let plan = t.app.manager.outputs.plan();
    t.poll();
    reconnect(&mut t, link, descriptors());
    assert_eq!(t.app.manager.outputs.plan(), plan);
}

#[test]
fn a_gamepad_name_and_saved_role_do_not_make_a_keyboard_a_gamepad() {
    let mut t = pending();
    t.write_policy(SAVED, |p| {
        p.name = "Gamepad".into();
        p.roles.0 = cordial_core::outputs::GAMEPAD;
    });
    let link = connect_saved(&mut t, descriptor());
    finish(&mut t);
    assert!(!t.saved(SAVED).setup_pending);
    assert_eq!(
        gamepad(&t.saved(SAVED).output_preferences),
        p::OutputMode::Disabled
    );
    assert!(t.device(SAVED).output_preferences.is_empty());
    assert!(t.device(SAVED).outputs.is_empty());
    reconnect(&mut t, link, descriptors());
    assert_eq!(t.app.manager.outputs.plan(), Plan::default());
}

#[test]
fn a_gamepad_collection_without_validated_controls_does_not_enable_hid() {
    let mut t = pending();
    let maps = vec![
        Descriptor::from_slice(
            ServiceId(7),
            &[
                5, 1, 9, 5, 0xa1, 1, 0xc0, 5, 1, 9, 6, 0xa1, 1, 5, 7, 0x19, 4, 0x29, 11, 0x15, 0,
                0x25, 1, 0x75, 1, 0x95, 8, 0x81, 2, 0xc0,
            ],
        )
        .unwrap(),
    ];
    connect_saved(&mut t, maps);
    finish(&mut t);
    assert!(!t.saved(SAVED).setup_pending);
    assert_eq!(t.app.manager.outputs.plan(), Plan::default());
}

#[test]
fn saved_nonresident_choices_fill_hid_capacity_and_finish_setup() {
    let mut t = pending();
    reserve_nonresident(&mut t, 4);
    let link = connect_saved(&mut t, descriptors());
    finish(&mut t);
    assert!(!t.saved(SAVED).setup_pending);
    assert_eq!(
        gamepad(&t.saved(SAVED).output_preferences),
        p::OutputMode::Disabled
    );
    assert_eq!(t.app.manager.outputs.plan().hid_gamepads, 4);
    t.set_device(p::SetDevice {
        device: 78,
        output_preferences: vec![output(p::OutputMode::Disabled)],
        ..Default::default()
    });
    reconnect(&mut t, link, descriptors());
    assert_eq!(t.app.manager.outputs.plan().hid_gamepads, 3);
    assert_eq!(
        gamepad(&t.saved(SAVED).output_preferences),
        p::OutputMode::Disabled
    );
}

#[test]
fn a_board_without_gamepad_outputs_finishes_setup() {
    let mut t = pending();
    t.app.manager.output_support = &[];
    let link = connect_saved(&mut t, descriptors());
    finish(&mut t);
    assert!(!t.saved(SAVED).setup_pending);
    assert_eq!(t.app.manager.outputs.plan(), Plan::default());
    t.app.manager.output_support = &[Support {
        role: p::Role::Gamepad,
        modes: &[p::OutputMode::Hid],
    }];
    reconnect(&mut t, link, descriptors());
    assert_eq!(t.app.manager.outputs.plan(), Plan::default());
}

#[test]
fn stored_modes_and_choices_before_detection_win() {
    for mode in [
        p::OutputMode::Disabled,
        p::OutputMode::Hid,
        p::OutputMode::Xinput,
    ] {
        for connected in [false, true] {
            let mut t = pending();
            if connected {
                connect_saved(&mut t, descriptors());
            }
            t.set_device(p::SetDevice {
                device: SAVED,
                output_preferences: vec![output(mode)],
                ..Default::default()
            });
            assert_eq!(gamepad(&t.saved(SAVED).output_preferences), mode);
            if !connected {
                // Reload the pending choice as a fresh adapter instance does.
                t = Test::with(t.store, Radio::default(), build(false, None));
                t.app.manager.output_support = &[Support {
                    role: p::Role::Gamepad,
                    modes: &[p::OutputMode::Hid, p::OutputMode::Xinput],
                }];
                connect_saved(&mut t, descriptors());
            }
            finish(&mut t);
            assert!(!t.saved(SAVED).setup_pending);
            assert_eq!(gamepad(&t.saved(SAVED).output_preferences), mode);
        }
    }
}

#[test]
fn a_later_user_choice_survives_reconnection() {
    let mut t = pending();
    let link = connect_saved(&mut t, descriptors());
    finish(&mut t);
    t.set_device(p::SetDevice {
        device: SAVED,
        output_preferences: vec![output(p::OutputMode::Disabled)],
        ..Default::default()
    });
    reconnect(&mut t, link, descriptors());
    assert_eq!(
        gamepad(&t.saved(SAVED).output_preferences),
        p::OutputMode::Disabled
    );
    assert_eq!(t.app.manager.outputs.plan(), Plan::default());
}

#[test]
fn gamepad_setup_keeps_hidpp_detection_pending() {
    let mut t = pending();
    let mut maps = hidpp_descriptor();
    maps.extend(descriptors());
    let link = connect_saved(&mut t, maps);
    finish(&mut t);
    assert!(t.saved(SAVED).setup_pending);
    assert!(!t.saved(SAVED).hidpp_enabled());
    assert_eq!(
        gamepad(&t.saved(SAVED).output_preferences),
        p::OutputMode::Hid
    );
    let plan = t.app.manager.outputs.plan();
    answer_protocol(&mut t, link, 4);
    t.quiet();
    t.events();
    assert!(!t.saved(SAVED).setup_pending);
    assert!(t.saved(SAVED).hidpp_enabled());
    assert_eq!(t.app.manager.outputs.plan(), plan);
}

#[test]
fn a_settled_disabled_choice_survives_pending_hidpp_and_reboot() {
    let mut t = pending();
    reserve_nonresident(&mut t, 4);
    let mut maps = hidpp_descriptor();
    maps.extend(descriptors());
    connect_saved(&mut t, maps);
    finish(&mut t);
    assert!(t.saved(SAVED).setup_pending);
    assert_eq!(
        gamepad(&t.saved(SAVED).output_preferences),
        p::OutputMode::Disabled
    );
    assert_eq!(t.saved(SAVED).output_preferences.len(), 1);
    t.set_device(p::SetDevice {
        device: 78,
        output_preferences: vec![output(p::OutputMode::Disabled)],
        ..Default::default()
    });
    t = Test::with(t.store, Radio::default(), build(false, None));
    t.app.manager.output_support = &[Support {
        role: p::Role::Gamepad,
        modes: &[p::OutputMode::Hid],
    }];
    connect_saved(&mut t, descriptors());
    finish(&mut t);
    assert!(!t.saved(SAVED).setup_pending);
    assert_eq!(
        gamepad(&t.saved(SAVED).output_preferences),
        p::OutputMode::Disabled
    );
    assert_eq!(t.app.manager.outputs.plan().hid_gamepads, 3);
}

#[test]
fn failed_saves_keep_the_mode_and_plan_until_the_next_connection_retries() {
    let mut t = pending();
    let link = connect_saved(&mut t, descriptors());
    t.store.fail_save = Some((record_key(2, SAVED), false));
    finish(&mut t);
    assert!(t.saved(SAVED).setup_pending);
    assert_eq!(t.app.manager.outputs.plan(), Plan::default());
    assert!(!t.live(SAVED).setup.gamepad);
    assert!(t.app.manager.connection(link).unwrap().setup_failed);
    t.store.fail_save = None;
    finish(&mut t);
    assert_eq!(t.app.manager.outputs.plan(), Plan::default());
    reconnect(&mut t, link, descriptors());
    assert!(!t.saved(SAVED).setup_pending);
    assert_eq!(
        gamepad(&t.saved(SAVED).output_preferences),
        p::OutputMode::Hid
    );
    assert_eq!(t.app.manager.outputs.plan().hid_gamepads, 1);
}

#[test]
fn two_new_gamepads_share_the_last_slot_by_sequential_saves() {
    let mut t = pending();
    reserve_nonresident(&mut t, 3);
    let other = t.add_saved(5);
    t.write_policy(other, |p| p.setup_pending = true);
    t.entry(other).paused = false;
    let first = connect_saved(&mut t, descriptors());
    t.poll();
    let second = t.radio.connects.last().unwrap().0;
    assert_ne!(first, second);
    t.event(Event::Connected {
        link: second,
        descriptors: descriptors(),
        max_output: 255,
        layout: None,
    });
    finish(&mut t);
    assert!(!t.saved(SAVED).setup_pending && !t.saved(other).setup_pending);
    let modes = [SAVED, other].map(|id| gamepad(&t.saved(id).output_preferences));
    assert_eq!(
        modes.iter().filter(|&&m| m == p::OutputMode::Hid).count(),
        1
    );
    assert_eq!(t.app.manager.outputs.plan().hid_gamepads, 4);
}

#[test]
fn pairing_a_new_gamepad_enables_hid() {
    let mut t = Test::new(false);
    t.app.manager.output_support = &[Support {
        role: p::Role::Gamepad,
        modes: &[p::OutputMode::Hid],
    }];
    let candidate = t.candidate(peer(2), peer(2));
    t.pair(candidate);
    t.poll();
    let link = t.radio.connects.last().unwrap().0;
    t.radio.bonds.push(peer(2));
    t.event(Event::Bonded {
        link,
        identity: peer(2),
    });
    let events = t.events();
    let device = pairing_steps(&events)
        .into_iter()
        .find_map(|step| match step {
            p::pairing::Step::Done(done) => Some(done.device),
            _ => None,
        })
        .unwrap();
    assert!(t.saved(device).setup_pending);
    t.event(Event::Connected {
        link,
        descriptors: descriptors(),
        max_output: 255,
        layout: None,
    });
    finish(&mut t);
    assert!(!t.saved(device).setup_pending);
    assert_eq!(
        gamepad(&t.saved(device).output_preferences),
        p::OutputMode::Hid
    );
    assert_eq!(t.app.manager.outputs.plan().hid_gamepads, 1);
}

#[test]
fn repairing_a_gamepad_preserves_each_output_choice() {
    for mode in [
        p::OutputMode::Disabled,
        p::OutputMode::Hid,
        p::OutputMode::Xinput,
    ] {
        let mut t = pending();
        t.write_policy(SAVED, |p| p.setup_pending = false);
        t.entry(SAVED).paused = true;
        let identity = Peer {
            transport: Transport::Ble,
            ..peer(1)
        };
        let address = Peer {
            random: true,
            address: [0x55; 6],
            ..identity
        };
        move_saved(&mut t, identity);
        t.set_device(p::SetDevice {
            device: SAVED,
            output_preferences: vec![output(mode)],
            ..Default::default()
        });
        let plan = t.app.manager.outputs.plan();
        let candidate = t.candidate(address, address);
        t.pair(candidate);
        t.poll();
        let link = t.radio.connects.last().unwrap().0;
        t.event(Event::Bonded { link, identity });
        assert!(
            pairing_steps(&t.events())
                .iter()
                .any(|step| matches!(step, p::pairing::Step::Done(done) if done.device == SAVED))
        );
        t.event(Event::Connected {
            link,
            descriptors: descriptors(),
            max_output: 255,
            layout: None,
        });
        finish(&mut t);
        assert!(!t.saved(SAVED).setup_pending);
        assert_eq!(gamepad(&t.saved(SAVED).output_preferences), mode);
        assert_eq!(t.app.manager.outputs.plan(), plan);
    }
}

#[test]
fn an_assigned_profile_keeps_builtin_mapping_independent_of_automatic_hid() {
    for assigned in [false, true] {
        let mut t = pending();
        if assigned {
            let profile = create(&mut t, "Controls");
            t.set_device(p::SetDevice {
                device: SAVED,
                profiles: Some(p::ProfileLayers {
                    profiles: vec![profile],
                }),
                ..Default::default()
            });
        }
        let maps = vec![Descriptor::from_slice(ServiceId(8), known::DS4_BASIC).unwrap()];
        let link = connect_saved(&mut t, maps);
        finish(&mut t);
        assert_eq!(
            gamepad(&t.saved(SAVED).output_preferences),
            p::OutputMode::Hid
        );
        t.app.set_output_enumeration(Some(Enumeration {
            plan: t.app.manager.outputs.plan(),
            generation: 1,
            ready: true,
        }));
        let mut report = [0; 9];
        report[4] = 8 | 0x10;
        t.event(Event::Input(
            InputReport::new(link, ServiceId(8), 1, &report).unwrap(),
        ));
        let slot = t.app.gamepad_outputs().hid[0];
        assert_eq!(slot.occupant, Some(link));
        assert_eq!(slot.gamepad.buttons, if assigned { 1 } else { 1 << 4 });
    }
}

#[test]
fn an_uncertain_save_stops_runtime_output_until_storage_recovers() {
    let mut t = pending();
    connect_saved(&mut t, descriptors());
    t.store.unknown = Some(record_key(2, SAVED));
    finish(&mut t);
    assert!(!t.app.manager.storage_ready);
    assert_eq!(t.app.manager.outputs.plan(), Plan::default());
    t.store.unknown = None;
    t = Test::with(t.store, Radio::default(), build(false, None));
    assert_eq!(
        gamepad(&t.saved(SAVED).output_preferences),
        p::OutputMode::Hid
    );
    assert!(!t.saved(SAVED).setup_pending);
    assert_eq!(t.app.manager.outputs.plan().hid_gamepads, 1);
}
