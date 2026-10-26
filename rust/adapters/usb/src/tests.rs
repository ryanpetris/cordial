use super::*;
#[path = "control_tests.rs"]
mod control;
use cordial_core::link::LinkId;
use cordial_core::storage::RecordStore;
use cordial_core::{
    application::{Application, Build},
    hid::{Held, Input},
    interfaces::InterfacePreference,
};
use embassy_futures::block_on;
use owner::{Owner, Shared};
use std::vec::Vec;
#[allow(dead_code)]
pub(super) mod support {
    use std::vec::Vec;
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../crates/cordial-core/tests/support/mod.rs"
    ));
}
pub(super) fn application() -> Application {
    Application::new(Build {
        development: true,
        version: "test",
        board: "test",
        default_adapter_name: "Test adapter",
        adapter_id: "adapter".into(),
        profile_memory_budget: None,
        restart: || panic!("restarted"),
        bootloader: None,
    })
}
/// An application on loaded storage with profiles 1 and 2, each empty.
fn profile_application() -> (Application, support::Store, support::Radio) {
    let mut app = application();
    let (manager, mut store, radio) = support::setup();
    app.manager = manager;
    app.manager.profile_budget = Some(65536);
    for id in [1, 2] {
        let created = block_on(cordial_core::profiles::create(
            &mut store,
            "Map",
            &cordial_core::profiles::Rules::default(),
        ))
        .unwrap();
        assert_eq!(created.0, id);
    }
    (app, store, radio)
}
fn enabled(interface: Interface, profile: u64) -> InterfacePreference {
    InterfacePreference {
        interface,
        enabled: true,
        profile: Some(profile),
    }
}
/// VIA dynamic_keymap_set_keycode: row 0, column 0 (input usage A) to Y.
fn set_keycode() -> [u8; 32] {
    let mut bytes = [0; 32];
    bytes[..6].copy_from_slice(&[5, 0, 0, 0, 0, 28]);
    bytes
}
fn rules(store: &mut support::Store, id: u64) -> usize {
    block_on(cordial_core::profiles::rules(store, id))
        .unwrap()
        .len()
}
/// The rules profile `id` holds while loaded.
fn loaded_rules(app: &Application, id: u64) -> usize {
    app.manager
        .profiles
        .get(id)
        .map_or(0, |map| map.borrow().len())
}
fn ok() -> cordial_protocol::Response {
    cordial_protocol::Response { result: None }
}
/// Both loops' own state, run one priority pass and one secondary step at a time.
struct Loops<'a> {
    priority: owner::Priority<'a>,
    secondary: owner::Secondary<'a>,
}
impl<'a> Loops<'a> {
    fn new(io: &'a Io, interfaces: u8) -> Self {
        io.set_interfaces(interfaces);
        Self {
            priority: owner::Priority::new(io),
            secondary: owner::Secondary::new(io, interfaces),
        }
    }
    fn poll<S: RecordStore>(&mut self, shared: &mut Shared<S, support::Radio>, now: u64) {
        block_on(self.priority.pass(shared, now));
        block_on(self.secondary.step(shared, now));
    }
}
fn shared<S>(app: Application, store: S) -> Shared<S, support::Radio> {
    Shared {
        app,
        store,
        radio: support::Radio::default(),
    }
}
#[test]
fn usb_serial_marker_follows_vial() {
    let io = Io::new();
    let mut bytes = *b"0123456789ABCDEF..............";
    bytes[16..].copy_from_slice(configuration::VIAL_SERIAL_SUFFIX);
    let mut serial = configuration::SerialNumber { io: &io, bytes };
    for set in [0, Interface::Via.bit(), Interface::Vial.bit(), 0] {
        io.set_interfaces(set);
        let expected = if set == Interface::Vial.bit() {
            "0123456789ABCDEF-vial:f64c2b3c"
        } else {
            "0123456789ABCDEF"
        };
        assert_eq!(
            serial.get_string(
                embassy_usb::types::StringIndex(configuration::SERIAL_INDEX),
                0x0409
            ),
            Some(expected)
        );
        assert_eq!(
            serial.get_string(embassy_usb::types::StringIndex(5), 0x0409),
            None
        );
    }
}
#[test]
fn slots_cover_every_valid_interface_set_in_interface_order() {
    let mut most = 0;
    for set in 0..1u8 << Interface::ALL.len() {
        let chosen: std::vec::Vec<Interface> = Interface::ALL
            .into_iter()
            .enumerate()
            .filter(|(i, _)| set & (1 << i) != 0)
            .map(|(_, interface)| interface)
            .collect();
        let saved: std::vec::Vec<InterfacePreference> = chosen
            .iter()
            .enumerate()
            .map(|(i, &interface)| enabled(interface, i as u64 + 1))
            .collect();
        if !cordial_core::interfaces::valid(&saved) {
            continue;
        }
        let bits = cordial_core::interfaces::enabled(&saved);
        most = most.max(chosen.len());
        assert!(chosen.len() <= SLOTS);
        assert_eq!(used_slots(bits), chosen.len());
        for (slot, &interface) in chosen.iter().enumerate() {
            assert_eq!(slot_interface(bits, slot), Some(interface));
        }
        assert_eq!(slot_interface(bits, chosen.len()), None);
    }
    assert_eq!(most, SLOTS, "every allocated Raw HID slot can be used");
}
#[test]
fn serial_waits_for_completion_and_rejects_old_session_packets() {
    let io = Io::new();
    let mut loops = Loops::new(&io, 0);
    let mut s = shared(application(), support::Store::default());
    BusHandler(&io).configured(true);
    io.dtr(true);
    loops.poll(&mut s, 1);
    assert!(s.app.serial.open());
    send_bytes(&io, io.status().session, &client_hello());
    let separator = io.serial_tx.try_receive().ok().unwrap();
    assert_eq!(
        &separator.bytes[..separator.length],
        &[cordial_protocol::frame::DELIMITER]
    );
    loops.poll(&mut s, 2);
    assert!(io.serial_tx.is_empty());
    io.serial_done
        .try_send(SerialDone {
            session: separator.session,
            token: None,
            length: 1,
        })
        .ok()
        .unwrap();
    loops.poll(&mut s, 3);
    let packet = io.serial_tx.try_receive().ok().unwrap();
    assert!(packet.token.is_some());
    assert_eq!(s.app.serial.queued(), 2);
    loops.poll(&mut s, 4);
    assert_eq!(s.app.serial.queued(), 2);
    io.dtr(false);
    io.dtr(true);
    send_bytes(&io, packet.session, &client_hello());
    loops.poll(&mut s, 5);
    io.serial_done
        .try_send(SerialDone {
            session: packet.session,
            token: packet.token,
            length: packet.length,
        })
        .ok()
        .unwrap();
    loops.poll(&mut s, 6);
    assert_eq!(s.app.serial.queued(), 0);
    assert!(s.app.serial.open());
    assert_eq!(io.serial_tx.len(), 1); // New session's separator, still pending.
}
#[test]
fn suspend_completion_cannot_consume_resumed_input_and_leds_use_standard_report() {
    let io = Io::new();
    io.set_topology(Topology {
        desktop: true,
        ..Default::default()
    });
    let mut loops = Loops::new(&io, 0);
    let mut s = shared(application(), support::Store::default());
    BusHandler(&io).configured(true);
    loops.poll(&mut s, 1);
    let old = io.hid_tx.try_receive().ok().unwrap();
    BusHandler(&io).suspended(true);
    loops.poll(&mut s, 2);
    let mut held = Held::default();
    held.keys[0] = 16;
    s.app
        .manager
        .forward
        .input(
            0,
            Input {
                held,
                motion: [0; 4],
                ..Input::default()
            },
        )
        .unwrap();
    BusHandler(&io).suspended(false);
    loops.poll(&mut s, 3);
    let new = io.hid_tx.try_receive().ok().unwrap();
    assert_eq!(&new.bytes[..3], &[1, 16, 0]);
    io.hid_done
        .try_send(HidDone {
            generation: old.generation,
            sequence: old.sequence,
            success: true,
        })
        .ok()
        .unwrap();
    loops.poll(&mut s, 4);
    assert_eq!(s.app.manager.forward.packet().unwrap().id, 1);
    io.hid_done
        .try_send(HidDone {
            generation: new.generation,
            sequence: new.sequence,
            success: true,
        })
        .ok()
        .unwrap();
    loops.poll(&mut s, 5);
    assert_eq!(s.app.manager.forward.packet().unwrap().id, 3);
    let mut handler = ReportHandler(&io);
    use hid::RequestHandler;
    assert!(matches!(
        handler.set_report(hid::ReportId::Out(1), &[1, 0xff]),
        OutResponse::Accepted
    ));
    assert_eq!(io.status().leds, 0x1f);
    let mut leds = [0; 2];
    assert_eq!(
        handler.get_report(hid::ReportId::Out(1), &mut leds),
        Some(2)
    );
    assert_eq!(leds, [1, 0x1f]);
    BusHandler(&io).reset();
    assert_eq!(io.status().leds, 0);
    assert!(!io.status().configured);
}

#[test]
fn protocol_switch_replays_held_keys_and_ignores_pending_completion() {
    use hid::RequestHandler;
    let io = Io::new();
    io.set_topology(Topology {
        desktop: true,
        ..Default::default()
    });
    let mut loops = Loops::new(&io, 0);
    let mut s = shared(application(), support::Store::default());
    BusHandler(&io).configured(true);
    loops.poll(&mut s, 1);
    let old = io.hid_tx.try_receive().unwrap();
    let mut held = Held::default();
    held.keys[0] = 1 << 4;
    s.app
        .manager
        .forward
        .input(
            0,
            Input {
                held,
                ..Input::default()
            },
        )
        .unwrap();
    ReportHandler(&io).set_protocol(hid::HidProtocolMode::Boot);
    loops.poll(&mut s, 2);
    let new = io.hid_tx.try_receive().unwrap();
    assert_eq!(new.generation, old.generation);
    assert_ne!(new.sequence, old.sequence);
    assert_eq!(&new.bytes[..3], &[1, 16, 0]);
    io.hid_done
        .try_send(HidDone {
            generation: old.generation,
            sequence: old.sequence,
            success: true,
        })
        .ok()
        .unwrap();
    loops.poll(&mut s, 3);
    assert!(io.hid_tx.is_empty());
    assert_eq!(s.app.manager.forward.packet().unwrap().id, 1);
    s.app.manager.forward.input(0, Input::default()).unwrap();
    io.hid_done
        .try_send(HidDone {
            generation: new.generation,
            sequence: new.sequence,
            success: true,
        })
        .ok()
        .unwrap();
    let mut released = false;
    for now in 4..20 {
        loops.poll(&mut s, now);
        if let Ok(packet) = io.hid_tx.try_receive() {
            if packet.bytes[0] == 1 {
                assert!(packet.bytes[1..packet.length].iter().all(|&byte| byte == 0));
                released = true;
            }
            io.hid_done
                .try_send(HidDone {
                    generation: packet.generation,
                    sequence: packet.sequence,
                    success: true,
                })
                .ok()
                .unwrap();
        }
    }
    assert!(released);
    ReportHandler(&io).set_protocol(hid::HidProtocolMode::Report);
    loops.poll(&mut s, 20);
    let packet = io.hid_tx.try_receive().unwrap();
    assert_eq!(packet.bytes[0], 1);
    assert!(packet.bytes[1..packet.length].iter().all(|&byte| byte == 0));
}

#[test]
fn hid_control_reports_include_the_report_id() {
    use hid::RequestHandler;
    let io = Io::new();
    let mut handler = ReportHandler(&io);
    assert!(
        matches!(
            handler.set_report(hid::ReportId::Out(1), &[1, 2]),
            OutResponse::Accepted
        ),
        "Embassy passes SET_REPORT data including the report ID unchanged"
    );
    let mut report = [0; 33];
    assert_eq!(
        handler.get_report(hid::ReportId::In(1), &mut report),
        Some(33)
    );
    assert_eq!(report[0], 1);
}

#[test]
fn dtr_during_dispatch_discards_remaining_old_commands() {
    use cordial_core::model::identifiers::HostPlatform;
    use cordial_core::storage::{Error, RecordKey, RecordStore};
    struct ClosingStore<'a> {
        io: &'a Io,
        writes: usize,
    }
    impl RecordStore for ClosingStore<'_> {
        async fn keys(&mut self) -> Result<std::vec::Vec<RecordKey>, Error> {
            Ok(std::vec::Vec::new())
        }
        async fn available(&mut self) -> Result<usize, Error> {
            Ok(65536)
        }

        async fn load(&mut self, _: RecordKey, _: &mut [u8]) -> Result<Option<usize>, Error> {
            Ok(None)
        }
        async fn save(&mut self, _: RecordKey, _: &[u8]) -> Result<(), Error> {
            self.writes += 1;
            if self.writes == 1 {
                self.io.dtr(false);
            }
            Ok(())
        }
        async fn remove(&mut self, _: RecordKey) -> Result<(), Error> {
            Ok(())
        }
    }
    let io = Io::new();
    let mut loops = Loops::new(&io, 0);
    let mut app = application();
    app.manager.storage_ready = true;
    let mut s = shared(app, ClosingStore { io: &io, writes: 0 });
    BusHandler(&io).configured(true);
    io.dtr(true);
    let mut host = Host::new();
    negotiate(&mut loops, &mut s, &mut host, &io, 1);
    let request = |platform: cordial_protocol::Platform| {
        let mut bytes = std::vec::Vec::new();
        cordial_protocol::frame::encode(
            &cordial_protocol::Request {
                command: Some(cordial_protocol::request::Command::SetAdapter(
                    cordial_protocol::SetAdapter {
                        platform: Some(platform as i32),
                        ..Default::default()
                    },
                )),
            },
            &mut bytes,
        );
        bytes
    };
    let first = request(cordial_protocol::Platform::Mac);
    let prefix = &first[..first.len() - 1];
    assert_eq!(s.app.serial.feed(prefix).0, prefix.len());
    let mut tail = first[first.len() - 1..].to_vec();
    tail.extend(request(cordial_protocol::Platform::Windows));
    let old_session = io.status().session;
    for chunk in tail.chunks(64) {
        let mut packet = SerialRx {
            session: old_session,
            bytes: [0; 64],
            length: chunk.len(),
        };
        packet.bytes[..chunk.len()].copy_from_slice(chunk);
        io.serial_rx.try_send(packet).ok().unwrap();
        loops.poll(&mut s, 2);
    }
    for _ in 0..3 {
        loops.poll(&mut s, 2);
    }
    assert!(!io.status().dtr);
    assert_eq!(
        s.store.writes, 1,
        "The second old-session command must not save after DTR falls"
    );
    assert_eq!(s.app.manager.preference.host_platform, HostPlatform::Mac);
}

#[test]
fn descriptor_ranges_are_valid_for_signed_global_item_parsers() {
    let mut minimum = 0i32;
    let mut maximum = 0i32;
    let mut offset = 0;
    while offset < descriptor::REPORT_DESCRIPTOR.len() {
        let tag = descriptor::REPORT_DESCRIPTOR[offset];
        offset += 1;
        let size = match tag & 3 {
            3 => 4,
            value => usize::from(value),
        };
        let data = &descriptor::REPORT_DESCRIPTOR[offset..offset + size];
        offset += size;
        let mut bytes = [0u8; 4];
        bytes[..size].copy_from_slice(data);
        if size > 0 && data[size - 1] & 0x80 != 0 {
            bytes[size..].fill(0xff);
        }
        let value = i32::from_le_bytes(bytes);
        match tag & 0xfc {
            0x14 => minimum = value,
            0x24 => maximum = value,
            0x80 | 0x90 | 0xb0 => assert!(
                minimum <= maximum,
                "invalid signed range {minimum}..{maximum}"
            ),
            _ => {}
        }
    }
    let map = cordial_core::hid::Map::compile(descriptor::REPORT_DESCRIPTOR).unwrap();
    assert_eq!(
        map.reports()
            .iter()
            .find(|report| report.id == 4)
            .unwrap()
            .bits[0],
        17 * 32
    );
    let volume = cordial_core::hid::CONSUMER_AXES
        .iter()
        .position(|&usage| usage == 0xe0)
        .unwrap();
    let other = volume + 1;
    for (raw, expected) in [(2i32, 1), (-2, -1)] {
        let mut payload = [0u8; 68];
        payload[volume * 4..volume * 4 + 4].copy_from_slice(&raw.to_le_bytes());
        payload[other * 4..other * 4 + 4].copy_from_slice(&i32::MIN.to_le_bytes());
        let input = map.decode(&mut map.state().unwrap(), 4, &payload).unwrap();
        assert_eq!(input.consumer_motion[volume], expected);
        assert_eq!(input.consumer_motion[other], i64::from(i32::MIN));
    }
    let mut payload = [0; 16];
    payload[..2].copy_from_slice(&0x500u16.to_le_bytes());
    assert_eq!(
        map.decode(&mut map.state().unwrap(), 3, &payload)
            .unwrap()
            .held
            .consumers[0],
        0x500
    );
}

#[test]
fn system_and_radio_reports_round_trip_and_start_with_unknown_sliders() {
    use cordial_core::{
        forward::Forwarder,
        hid::{ROTATION_KNOWN, ROTATION_STATE},
    };
    use embassy_usb::class::hid::RequestHandler;
    let io = Io::new();
    let mut handler = ReportHandler(&io);
    let map = cordial_core::hid::Map::compile(descriptor::REPORT_DESCRIPTOR).unwrap();
    let mut state = map.state().unwrap();
    for (id, length) in [(9, 9), (10, 2)] {
        let mut bytes = [0; 16];
        assert_eq!(
            handler.get_report(hid::ReportId::In(id), &mut bytes),
            Some(length)
        );
        assert_eq!(bytes[0], id);
        assert_eq!(
            map.decode(&mut state, id, &bytes[1..length]).unwrap().held,
            Held::default()
        );
    }
    let mut forward = Forwarder::default();
    while forward.packet().is_some() {
        forward.complete();
    }
    let held = Held {
        system: 3 | ROTATION_KNOWN | ROTATION_STATE,
        radio: 7,
        ..Held::default()
    };
    forward
        .input(
            0,
            Input {
                held,
                sliders: [Some(true); 2],
                ..Input::default()
            },
        )
        .unwrap();
    let mut output = Held::default();
    while let Some(packet) = forward.packet() {
        output = map
            .decode(&mut state, packet.id, packet.bytes())
            .unwrap()
            .held;
        forward.complete();
    }
    assert_eq!(output, held);
}

#[test]
fn raw_edit_from_previous_enumeration_cannot_change_the_new_target() {
    let io = Io::new();
    let (mut app, store, _) = profile_application();
    app.manager.preference.configuration_interfaces = std::vec![enabled(Interface::Via, 1)];
    let mut s = shared(app, store);
    let mut loops = Loops::new(&io, Interface::Via.bit());
    io.set_interfaces(Interface::Via.bit());
    BusHandler(&io).configured(true);
    loops.poll(&mut s, 0);
    assert!(!io.reconnect.signaled());
    let generation = io.status().generation;
    s.app.manager.preference.configuration_interfaces = std::vec![enabled(Interface::Via, 2)];
    io.raw[0]
        .rx
        .try_send(RawPacket {
            generation,
            interface: Interface::Via,
            bytes: set_keycode(),
        })
        .ok()
        .unwrap();
    loops.poll(&mut s, 1);
    assert!(!io.reconnect.signaled());
    assert!(io.raw[0].rx.is_empty());
    assert!(io.raw[0].tx.is_empty());
    for id in [1, 2] {
        assert_eq!(rules(&mut s.store, id), 0);
    }
}

#[test]
fn raw_packets_reach_the_interface_they_arrived_on() {
    let io = Io::new();
    let (mut app, store, _) = profile_application();
    app.manager.preference.configuration_interfaces = std::vec![
        InterfacePreference {
            interface: Interface::Via,
            enabled: false,
            profile: Some(2),
        },
        enabled(Interface::Vial, 1),
    ];
    let mut s = shared(app, store);
    let mut loops = Loops::new(&io, Interface::Vial.bit());
    io.set_interfaces(Interface::Vial.bit());
    BusHandler(&io).configured(true);
    let mut handler = configuration::ReportHandler { io: &io, slot: 0 };
    use embassy_usb::class::hid::{ReportId, RequestHandler};
    assert_eq!(
        handler.set_report(ReportId::Out(0), &set_keycode()),
        OutResponse::Accepted
    );
    assert_eq!(
        io.raw[0].rx.try_receive().ok().unwrap().interface,
        Interface::Vial
    );
    let generation = io.status().generation;
    // A packet tagged for an interface the slot no longer exposes is refused. An accepted edit
    // changes the loaded rules at once and reaches flash once the editor pauses.
    for (interface, answer, edited) in [(Interface::Via, 255, 0), (Interface::Vial, 5, 1)] {
        io.raw[0]
            .rx
            .try_send(RawPacket {
                generation,
                interface,
                bytes: set_keycode(),
            })
            .ok()
            .unwrap();
        loops.poll(&mut s, 1);
        let reply = io.raw[0].tx.try_receive().ok().unwrap();
        assert_eq!(reply.interface, interface);
        assert_eq!(reply.bytes[0], answer);
        assert_eq!(loaded_rules(&s.app, 1), edited);
        assert_eq!(rules(&mut s.store, 1), 0);
        assert_eq!(rules(&mut s.store, 2), 0);
    }
    loops.poll(&mut s, 1 + cordial_core::application::EDITOR_BATCH_MS);
    assert_eq!(rules(&mut s.store, 1), 1);
    assert!(!io.reconnect.signaled());
}

#[test]
fn edits_are_saved_before_usb_enumerates_again() {
    let io = Io::new();
    let (mut app, store, _) = profile_application();
    app.manager.preference.configuration_interfaces = std::vec![enabled(Interface::Vial, 1)];
    let mut s = shared(app, store);
    let mut loops = Loops::new(&io, Interface::Vial.bit());
    io.set_interfaces(Interface::Vial.bit());
    BusHandler(&io).configured(true);
    let generation = io.status().generation;
    io.raw[0]
        .rx
        .try_send(RawPacket {
            generation,
            interface: Interface::Vial,
            bytes: set_keycode(),
        })
        .ok()
        .unwrap();
    loops.poll(&mut s, 1);
    assert_eq!(io.raw[0].tx.try_receive().ok().unwrap().bytes[0], 5);
    assert_eq!(rules(&mut s.store, 1), 0);
    s.app.manager.preference.configuration_interfaces = std::vec![enabled(Interface::Via, 1)];
    // Changing the interface flushes its pending edits before detaching.
    loops.poll(&mut s, 2);
    assert_eq!(
        io.reconnect.try_take().map(|t| t.interfaces),
        Some(Interface::Via.bit())
    );
    assert_eq!(rules(&mut s.store, 1), 1);
}

#[test]
fn usb_reconnects_only_when_the_exposed_interfaces_must_change() {
    let io = Io::new();
    let (mut app, store, _) = profile_application();
    app.manager.preference.configuration_interfaces = std::vec![enabled(Interface::Vial, 1)];
    let mut s = shared(app, store);
    // Started with the saved set: no reconnect, even once storage is ready.
    let mut loops = Loops::new(&io, Interface::Vial.bit());
    loops.poll(&mut s, 0);
    assert!(!io.reconnect.signaled());
    // Storage that is not ready yet leaves the started set alone.
    s.app.manager.storage_ready = false;
    let mut loops = Loops::new(&io, 0);
    loops.poll(&mut s, 1);
    assert!(!io.reconnect.signaled());
    // Started without it, the saved set is applied once storage is ready.
    s.app.manager.storage_ready = true;
    loops.poll(&mut s, 2);
    assert_eq!(
        io.reconnect.try_take().map(|t| t.interfaces),
        Some(Interface::Vial.bit())
    );
    loops.poll(&mut s, 3);
    assert!(!io.reconnect.signaled());
    // Retargeting the same exposed interface changes no USB descriptors.
    s.app.manager.preference.configuration_interfaces = std::vec![enabled(Interface::Vial, 2)];
    loops.poll(&mut s, 4);
    assert!(!io.reconnect.signaled());
    // Switching interfaces changes the set.
    s.app.manager.preference.configuration_interfaces = std::vec![enabled(Interface::Via, 2)];
    loops.poll(&mut s, 5);
    assert_eq!(
        io.reconnect.try_take().map(|t| t.interfaces),
        Some(Interface::Via.bit())
    );
    s.app.manager.preference.configuration_interfaces.clear();
    loops.poll(&mut s, 6);
    assert_eq!(io.reconnect.try_take().map(|t| t.interfaces), Some(0));
    // A board without profiles exposes no interfaces whatever is saved.
    s.app.manager.preference.configuration_interfaces = std::vec![enabled(Interface::Via, 2)];
    s.app.manager.profile_budget = None;
    loops.poll(&mut s, 7);
    assert!(!io.reconnect.signaled());
}

#[test]
fn usb_starts_with_saved_interfaces_whose_profiles_exist() {
    use cordial_core::devices::{AdapterPreference, Policies};
    let (_, mut store, _) = profile_application();
    assert_eq!(block_on(saved_interfaces(&mut store, true)), 0);
    let save = |store: &mut support::Store, saved| {
        let preference = AdapterPreference {
            configuration_interfaces: saved,
            ..Default::default()
        };
        block_on(Policies { store }.save_adapter(&preference)).unwrap();
    };
    save(&mut store, std::vec![enabled(Interface::Vial, 1)]);
    assert_eq!(
        block_on(saved_interfaces(&mut store, true)),
        Interface::Vial.bit()
    );
    assert_eq!(block_on(saved_interfaces(&mut store, false)), 0);
    save(&mut store, std::vec![enabled(Interface::Via, 3)]);
    assert_eq!(block_on(saved_interfaces(&mut store, true)), 0);
    store.records.insert(
        cordial_core::storage::record_key(1, 0),
        b"not a preference".to_vec(),
    );
    assert_eq!(block_on(saved_interfaces(&mut store, true)), 0);
}

#[test]
fn raw_control_reports_require_enabled_output_and_queue_capacity() {
    use embassy_usb::class::hid::{ReportId, RequestHandler};
    let io = Io::new();
    let mut handler = configuration::ReportHandler { io: &io, slot: 0 };
    let bytes = [1; 32];
    BusHandler(&io).configured(true);
    assert_eq!(
        handler.set_report(ReportId::Out(0), &bytes),
        OutResponse::Rejected
    );
    io.set_interfaces(Interface::Via.bit());
    for id in [ReportId::Out(1), ReportId::In(0), ReportId::Feature(0)] {
        assert_eq!(handler.set_report(id, &bytes), OutResponse::Rejected);
    }
    assert_eq!(
        handler.set_report(ReportId::Out(0), &bytes[..31]),
        OutResponse::Rejected
    );
    assert_eq!(
        handler.set_report(ReportId::Out(0), &[0; 33]),
        OutResponse::Rejected
    );
    assert_eq!(
        handler.set_report(ReportId::Out(0), &bytes),
        OutResponse::Accepted
    );
    assert_eq!(
        handler.set_report(ReportId::Out(0), &bytes),
        OutResponse::Rejected
    );
    let packet = io.raw[0].rx.try_receive().ok().unwrap();
    assert_eq!(packet.bytes, bytes);
    assert_eq!(packet.interface, Interface::Via);
    assert_eq!(packet.generation, io.status().generation);
}

#[test]
fn raw_requests_wait_until_the_previous_reply_can_be_retained() {
    let io = Io::new();
    let mut loops = Loops::new(&io, 0);
    let mut s = shared(application(), support::Store::default());
    for queue in [&io.raw[0].rx, &io.raw[0].tx] {
        queue
            .try_send(RawPacket {
                generation: 0,
                interface: Interface::Via,
                bytes: [0; 32],
            })
            .ok()
            .unwrap();
    }
    loops.poll(&mut s, 0);
    assert!(io.raw[0].rx.is_full());
    assert!(io.raw[0].tx.is_full());
    io.raw[0].tx.try_receive().ok().unwrap();
    loops.poll(&mut s, 1);
    assert!(io.raw[0].rx.is_empty());
    assert!(io.raw[0].tx.is_full());
}

/// Both loops, on loaded storage with device 77 connected and forwarding input, profiles 1 to
/// `profiles` and saved devices 1 to `devices` that are disabled.
fn running(
    profiles: u64,
    devices: u64,
) -> (
    Owner<'static, support::Store, support::Radio>,
    &'static Io,
    LinkId,
) {
    use cordial_core::bluetooth::{Event, InputReport};
    let io = std::boxed::Box::leak(std::boxed::Box::new(Io::new()));
    let (mut app, mut store, mut radio) = profile_application();
    for _ in 3..=profiles {
        block_on(cordial_core::profiles::create(
            &mut store,
            "Map",
            &cordial_core::profiles::Rules::default(),
        ))
        .unwrap();
    }
    for id in 1..=devices {
        let n = id as u8 + 10;
        let mut policy = cordial_core::devices::Policy::paired(id, support::peer(n), b"Other");
        policy.enabled = false;
        policy.setup_pending = false;
        block_on(cordial_core::bonds::commit(
            &mut store,
            &policy,
            &support::bond(id, policy.peer),
        ))
        .unwrap();
    }
    let slot = app.manager.find(77).unwrap();
    let link = app
        .manager
        .connect(slot, true, 90_000, None, &mut radio)
        .unwrap()
        .unwrap();
    for event in [
        Event::Connected {
            link,
            descriptors: support::descriptor(),
            max_output: 255,
            layout: None,
        },
        Event::Input(InputReport::new(link, cordial_core::link::ServiceId(7), 0, &[1]).unwrap()),
    ] {
        block_on(app.event(event, &mut store, &mut radio, NOW));
    }
    app.manager.usb_budget = io.budget;
    io.set_topology(app.manager.usb_allocation().topology);
    BusHandler(io).configured(true);
    io.dtr(true);
    (Owner::new(io, app, store, radio), io, link)
}
const NOW: u64 = 2000;
/// A host on the other end of USB: takes every HID report and serial packet, and collects the
/// serial messages.
struct Host {
    decoder: cordial_protocol::frame::Decoder,
    messages: std::vec::Vec<cordial_protocol::message::Kind>,
    reports: usize,
    sent_hello: core::cell::Cell<Option<u64>>,
    received_hello: Option<u64>,
    secondary: Option<owner::Secondary<'static>>,
}
impl Host {
    fn new() -> Self {
        Self {
            decoder: cordial_protocol::frame::Decoder::new(None),
            messages: std::vec::Vec::new(),
            reports: 0,
            sent_hello: core::cell::Cell::new(None),
            received_hello: None,
            secondary: None,
        }
    }
    fn serve(&mut self, io: &Io) {
        use prost::Message;
        self.hello(io);
        if let Ok(tx) = io.hid_tx.try_receive() {
            self.reports += 1;
            io.hid_done
                .try_send(HidDone {
                    generation: tx.generation,
                    sequence: tx.sequence,
                    success: true,
                })
                .ok()
                .unwrap();
        }
        if let Ok(tx) = io.serial_tx.try_receive() {
            let current = tx.session == io.status().session;
            for &byte in &tx.bytes[..if current { tx.length } else { 0 }] {
                if let Some(frame) = self.decoder.push(byte) {
                    let frame = frame.unwrap();
                    if self.received_hello != Some(tx.session) {
                        let Ok(hello) = cordial_protocol::handshake::decode_adapter_hello(frame)
                        else {
                            continue;
                        };
                        assert_eq!(
                            hello.protocol_version,
                            Some(cordial_protocol::CURRENT_PROTOCOL_VERSION)
                        );
                        self.received_hello = Some(tx.session);
                    } else {
                        let message = cordial_protocol::Message::decode(frame).unwrap();
                        self.messages.push(message.kind.unwrap());
                    }
                }
            }
            io.serial_done
                .try_send(SerialDone {
                    session: tx.session,
                    token: tx.token,
                    length: if current { tx.length } else { 0 },
                })
                .ok()
                .unwrap();
        }
    }
    fn hello(&self, io: &Io) {
        let status = io.status();
        if !status.serial_open() || self.sent_hello.get() == Some(status.session) {
            return;
        }
        send_bytes(io, status.session, &client_hello());
        self.sent_hello.set(Some(status.session));
    }
    fn send(&self, io: &Io, command: cordial_protocol::request::Command) {
        assert_eq!(self.received_hello, Some(io.status().session));
        let mut bytes = std::vec::Vec::new();
        cordial_protocol::frame::encode(
            &cordial_protocol::Request {
                command: Some(command),
            },
            &mut bytes,
        );
        send_bytes(io, io.status().session, &bytes);
    }
    fn response(&self) -> Option<&cordial_protocol::Response> {
        self.messages.iter().find_map(|m| match m {
            cordial_protocol::message::Kind::Response(r) => Some(r),
            _ => None,
        })
    }
}
pub(super) fn client_hello() -> std::vec::Vec<u8> {
    let mut bytes = std::vec::Vec::new();
    cordial_protocol::frame::encode_bytes(
        &cordial_protocol::handshake::encode_client_hello(&cordial_protocol::ClientHello {})
            .unwrap(),
        &mut bytes,
    );
    bytes
}

fn send_bytes(io: &Io, session: u64, bytes: &[u8]) {
    for chunk in bytes.chunks(64) {
        let mut packet = SerialRx {
            session,
            bytes: [0; 64],
            length: chunk.len(),
        };
        packet.bytes[..chunk.len()].copy_from_slice(chunk);
        io.serial_rx.try_send(packet).ok().unwrap();
    }
}

fn negotiate<S: RecordStore>(
    loops: &mut Loops<'_>,
    shared: &mut Shared<S, support::Radio>,
    host: &mut Host,
    io: &Io,
    now: u64,
) {
    loops.poll(shared, now);
    for _ in 0..100 {
        host.serve(io);
        loops.poll(shared, now);
        if host.received_hello == Some(io.status().session) {
            return;
        }
    }
    panic!("the adapter did not negotiate");
}

/// Runs both loops, with the host and `drive` taking turns with them, until `drive` returns true.
fn run(
    owner: &Owner<'static, support::Store, support::Radio>,
    io: &'static Io,
    host: &mut Host,
    mut drive: impl FnMut(&mut Shared<support::Store, support::Radio>, &mut Host) -> bool,
) {
    use embassy_futures::{join::join, select::select, yield_now};
    let mut secondary = host
        .secondary
        .take()
        .unwrap_or_else(|| owner::Secondary::new(io, 0));
    block_on(async {
        let loops = join(
            owner.priority(
                async |_: &mut Shared<support::Store, support::Radio>| {},
                async || yield_now().await,
                || NOW,
            ),
            owner.run_secondary(&mut secondary, || NOW),
        );
        let driver = async {
            for _ in 0..100_000 {
                yield_now().await;
                host.serve(io);
                let mut shared = owner.lock().await;
                if drive(&mut shared, host) {
                    return;
                }
            }
            panic!("the loops did not finish");
        };
        select(loops, driver).await;
    });
    host.secondary = Some(secondary);
}
/// Record reads of `kind` so far.
fn reads(store: &support::Store, kind: u8) -> usize {
    store.reads.iter().filter(|key| key[0] == kind).count()
}
/// Runs until the session has started, the connection's reports have been sent and background
/// work has nothing left to read.
fn settle(
    owner: &Owner<'static, support::Store, support::Radio>,
    io: &'static Io,
    host: &mut Host,
) {
    let mut quiet = 0;
    let mut last = usize::MAX;
    run(owner, io, host, |shared, host| {
        let total = shared.store.reads.len();
        quiet = if total == last { quiet + 1 } else { 0 };
        last = total;
        quiet > 50
            && host.reports > 0
            && shared.app.manager.forward.pending() == 0
            && shared.app.serial.open()
    });
}
/// Sends `command` and, each time it reads a record of `kind`, sends an input report from
/// `link`. Checks that the command reads one record per step and that each input was handled and
/// taken by USB before the next read. Returns the response and the number of records read.
fn input_goes_first(
    owner: &Owner<'static, support::Store, support::Radio>,
    io: &'static Io,
    link: LinkId,
    command: cordial_protocol::request::Command,
    kind: u8,
) -> (cordial_protocol::Response, usize) {
    use cordial_core::bluetooth::{Event, InputReport};
    let mut host = Host::new();
    settle(owner, io, &mut host);
    let base = owner_reads(owner, kind);
    host.send(io, command);
    let mut seen = 0;
    let mut sent = None;
    // The connection's first input held the key; each input changes it.
    let mut press = true;
    run(owner, io, &mut host, |shared, host| {
        let count = reads(&shared.store, kind) - base;
        if count != seen {
            assert_eq!(count, seen + 1, "one record per step");
            assert!(shared.radio.events.is_empty());
            assert_eq!(shared.app.manager.forward.pending(), 0);
            if let Some(sent) = sent {
                assert!(host.reports > sent);
            }
            seen = count;
            press = !press;
            shared.radio.events.push_back(Event::Input(
                InputReport::new(
                    link,
                    cordial_core::link::ServiceId(7),
                    0,
                    &[u8::from(press)],
                )
                .unwrap(),
            ));
            sent = Some(host.reports);
        }
        host.response().is_some()
    });
    (host.response().unwrap().clone(), seen)
}
fn owner_reads(owner: &Owner<'static, support::Store, support::Radio>, kind: u8) -> usize {
    reads(&block_on(owner.lock()).store, kind)
}

#[test]
fn a_listing_reads_one_record_per_step_and_input_goes_first() {
    use cordial_protocol::{request::Command, response::Result as R};
    let (owner, io, link) = running(120, 0);
    let (response, read) = input_goes_first(
        &owner,
        io,
        link,
        Command::ListProfiles(cordial_protocol::ListProfiles {}),
        cordial_core::profiles::METADATA,
    );
    match response.result {
        Some(R::Profiles(list)) => assert_eq!(list.entries.len(), 120),
        other => panic!("{other:?}"),
    }
    assert_eq!(read, 120);
}

#[test]
fn a_reference_scan_reads_one_device_per_step_and_input_goes_first() {
    use cordial_protocol::request::Command;
    let (owner, io, link) = running(2, 6);
    let (response, read) = input_goes_first(
        &owner,
        io,
        link,
        Command::DeleteProfile(cordial_protocol::DeleteProfile { profile: 1 }),
        2,
    );
    assert_eq!(response, ok());
    assert_eq!(read, 7);
    let mut shared = block_on(owner.lock());
    assert!(
        block_on(cordial_core::profiles::metadata(&mut shared.store, 1)).is_err(),
        "the profile is deleted"
    );
}

#[test]
fn background_work_waits_for_a_request() {
    use cordial_protocol::request::Command;
    let (owner, io, _) = running(2, 12);
    let mut host = Host::new();
    settle(&owner, io, &mut host);
    // Filling the stack reads every saved device that is not resident, one per step.
    let base = owner_reads(&owner, 2);
    block_on(owner.lock()).app.manager.vacated = true;
    host.messages.clear();
    let mut sent = None;
    let mut answered = None;
    run(&owner, io, &mut host, |shared, host| {
        let count = reads(&shared.store, 2) - base;
        match sent {
            None if count >= 3 && shared.app.serial.queued() == 0 => {
                host.send(io, Command::GetStatus(cordial_protocol::GetStatus {}));
                sent = Some(count);
            }
            // The next step answers the request; the fill waits.
            Some(at) if answered.is_none() => {
                assert_eq!(count, at);
                assert!(shared.app.serial.queued() > 0);
                answered = Some(count);
            }
            _ => {}
        }
        count == 12 && host.response().is_some()
    });
    assert!(answered.is_some());
}

#[test]
fn continuous_input_leaves_the_secondary_loop_a_step() {
    use cordial_core::bluetooth::{Event, InputReport};
    use cordial_protocol::request::Command;
    use core::cell::Cell;
    use embassy_futures::{join::join, select::select, yield_now};
    std::thread_local! {
        static CLOCK: Cell<u64> = const { Cell::new(NOW) };
    }
    fn clock() -> u64 {
        CLOCK.with(Cell::get)
    }
    let (owner, io, link) = running(2, 0);
    let mut host = Host::new();
    settle(&owner, io, &mut host);
    host.messages.clear();
    let reports = host.reports;
    let mut secondary = host.secondary.take().unwrap();
    let mut answered = false;
    block_on(async {
        let loops = join(
            owner.priority(
                async |_: &mut Shared<support::Store, support::Radio>| {},
                async || yield_now().await,
                clock,
            ),
            owner.run_secondary(&mut secondary, clock),
        );
        let driver = async {
            let mut press = true;
            // Simulated milliseconds, each with several turns of the loops.
            for ms in 0..1000 {
                for turn in 0..4 {
                    yield_now().await;
                    host.serve(io);
                    let mut shared = owner.lock().await;
                    // A new report arrives before every pass and the host takes each report at
                    // once, so no pass leaves the forwarder empty.
                    if ms + turn != 0 {
                        assert_ne!(shared.app.manager.forward.pending(), 0);
                    }
                    press = !press;
                    shared.radio.events.push_back(Event::Input(
                        InputReport::new(
                            link,
                            cordial_core::link::ServiceId(7),
                            0,
                            &[u8::from(press)],
                        )
                        .unwrap(),
                    ));
                    if ms + turn == 0 {
                        drop(shared);
                        host.send(io, Command::GetStatus(cordial_protocol::GetStatus {}));
                    }
                }
                if host.response().is_some() {
                    answered = true;
                    return;
                }
                CLOCK.with(|c| c.set(c.get() + 1));
            }
        };
        select(loops, driver).await;
    });
    assert!(answered, "the request was not answered");
    assert!(host.reports > reports + 10);
}

#[test]
fn a_step_that_writes_is_followed_by_a_rest_as_long_as_it_took() {
    use cordial_protocol::request::Command;
    use core::cell::{Cell, RefCell};
    use embassy_futures::{join::join, select::select, yield_now};
    std::thread_local! {
        static CLOCK: Cell<u64> = const { Cell::new(NOW) };
        static SAVED: RefCell<std::vec::Vec<u64>> = const { RefCell::new(std::vec::Vec::new()) };
    }
    fn clock() -> u64 {
        CLOCK.with(Cell::get)
    }
    /// Each save takes 20 ms.
    fn slow_save() {
        CLOCK.with(|c| c.set(c.get() + 20));
        SAVED.with(|s| s.borrow_mut().push(clock()));
    }
    let (owner, io, _) = running(2, 0);
    let mut host = Host::new();
    settle(&owner, io, &mut host);
    block_on(owner.lock()).store.on_save = Some(slow_save);
    let rename = |host: &Host, n: usize| {
        host.send(
            io,
            Command::SetAdapter(cordial_protocol::SetAdapter {
                name: Some(std::format!("Adapter {n}")),
                ..Default::default()
            }),
        )
    };
    let mut secondary = host.secondary.take().unwrap();
    host.messages.clear();
    rename(&host, 0);
    let mut sent = 1;
    block_on(async {
        let loops = join(
            owner.priority(
                async |_: &mut Shared<support::Store, support::Radio>| {},
                async || yield_now().await,
                clock,
            ),
            owner.run_secondary(&mut secondary, clock),
        );
        let driver = async {
            for _ in 0..1000 {
                for _ in 0..4 {
                    yield_now().await;
                    host.serve(io);
                }
                // Each rename follows the response to the last.
                if host.response().is_some() {
                    if sent == 3 {
                        return;
                    }
                    host.messages.clear();
                    rename(&host, sent);
                    sent += 1;
                }
                CLOCK.with(|c| c.set(c.get() + 1));
            }
            panic!("the renames were not answered");
        };
        select(loops, driver).await;
    });
    let saved = SAVED.with(|s| s.borrow().clone());
    assert_eq!(saved.len(), 3);
    for pair in saved.windows(2) {
        // The next save starts no sooner than 20 ms after the last one ended.
        assert!(pair[1] - 20 >= pair[0] + 20, "{saved:?}");
    }
}

#[cfg(feature = "gamepad")]
#[test]
fn startup_pools_match_core_validation_including_nonresident_pairs() {
    use cordial_core::{
        devices::Policy,
        outputs::{Plan, Preference},
    };
    use cordial_protocol::{OutputMode, Role};
    let (mut manager, mut store, mut radio) = support::setup();
    for (id, address, mode) in [
        (1, 10, OutputMode::Hid),
        (2, 11, OutputMode::Hid),
        (3, 12, OutputMode::Xinput),
        (4, 10, OutputMode::Xinput),
        (5, 13, OutputMode::Disabled),
    ] {
        let mut policy = Policy::paired(id, support::peer(address), b"Controller");
        policy.enabled = false;
        policy.output_preferences.push(Preference {
            role: Role::Gamepad,
            mode,
        });
        block_on(cordial_core::bonds::commit(
            &mut store,
            &policy,
            &support::bond(id, policy.peer),
        ))
        .unwrap();
    }
    store.records.insert(
        cordial_core::storage::record_key(2, 6),
        b"invalid pair".to_vec(),
    );
    let startup = block_on(outputs::saved_plan(&mut store));
    assert_eq!(
        startup,
        Plan {
            hid_gamepads: 2,
            xinput_gamepads: 1
        }
    );
    block_on(manager.load(&mut store, &mut radio)).unwrap();
    assert_eq!(manager.outputs.plan(), startup);
    assert!(manager.find(1).is_none());
    store.fail_load = Some(cordial_core::storage::record_key(2, 3));
    assert_eq!(block_on(outputs::saved_plan(&mut store)), Plan::default());
}

#[cfg(feature = "gamepad")]
#[test]
fn saved_pool_change_waits_for_successful_persistence_and_physical_response_completion() {
    use cordial_protocol::{OutputMode, Role, request::Command};
    for fail in [true, false] {
        let io = Io::with_budget(Budget {
            config_bytes: 121,
            ..Budget::PICO
        });
        let (mut app, mut store, radio) = profile_application();
        app.manager.output_support = outputs::SUPPORT;
        app.manager.usb_budget = io.budget;
        io.set_topology(app.manager.usb_allocation().topology);
        if fail {
            store.fail_save = Some((cordial_core::storage::record_key(2, 77), false));
        }
        let mut shared = Shared { app, store, radio };
        let mut loops = Loops::new(&io, 0);
        let mut host = Host::new();
        BusHandler(&io).configured(true);
        io.dtr(true);
        negotiate(&mut loops, &mut shared, &mut host, &io, 1);
        host.send(
            &io,
            Command::SetDevice(cordial_protocol::SetDevice {
                device: 77,
                output_preferences: std::vec![cordial_protocol::OutputPreference {
                    role: Role::Gamepad as i32,
                    mode: OutputMode::Xinput as i32,
                }],
                ..Default::default()
            }),
        );
        for at in 2..8 {
            loops.poll(&mut shared, at);
        }
        assert!(!io.reconnect.signaled());
        assert_eq!(
            shared.app.manager.outputs.plan().xinput_gamepads,
            u8::from(!fail)
        );
        let saved = block_on(outputs::saved_plan(&mut shared.store));
        assert_eq!(saved, shared.app.manager.outputs.plan());
        assert!(!io.serial_tx.is_empty(), "the response is waiting for USB");
        for at in 8..40 {
            host.serve(&io);
            loops.poll(&mut shared, at);
            if io.reconnect.signaled() {
                break;
            }
        }
        assert!(host.response().is_some());
        if fail {
            assert!(!io.reconnect.signaled());
            assert!(!host.messages.iter().any(|message| matches!(
                message,
                cordial_protocol::message::Kind::Event(cordial_protocol::Event {
                    kind: Some(cordial_protocol::event::Kind::SessionEnding(_)),
                })
            )));
        } else {
            assert_eq!(
                host.messages.last(),
                Some(&session_ending(
                    cordial_protocol::SessionEndingReason::ConfigurationChange
                ))
            );
            assert_eq!(io.reconnect.try_take().unwrap().gamepads, saved);
        }
    }
}

#[cfg(feature = "gamepad")]
#[test]
fn saturated_saved_counts_do_not_reconnect_until_the_visible_pool_changes() {
    use cordial_protocol::OutputMode;
    let io = Io::new();
    let (mut app, store, radio) = profile_application();
    for id in 1..=5 {
        app.manager.outputs.prepare(id, OutputMode::Hid).unwrap();
        app.manager.outputs.set(id, OutputMode::Hid);
    }
    app.manager.output_support = outputs::SUPPORT;
    app.manager.usb_budget = io.budget;
    io.set_topology(app.manager.usb_allocation().topology);
    let mut shared = Shared { app, store, radio };
    let mut loops = Loops::new(&io, 0);
    loops.poll(&mut shared, 0);
    assert!(!io.reconnect.signaled());
    shared.app.manager.outputs.remove(5);
    loops.poll(&mut shared, 1);
    assert!(!io.reconnect.signaled());
    shared.app.manager.outputs.remove(4);
    loops.poll(&mut shared, 2);
    assert!(!io.reconnect.signaled());
}

#[cfg(feature = "gamepad")]
#[test]
fn bus_reset_and_suspend_preserve_the_allocation_generation() {
    let io = Io::new();
    let topology = Topology {
        desktop: true,
        interfaces: 0,
        gamepads: cordial_core::outputs::Plan {
            hid_gamepads: 2,
            xinput_gamepads: 1,
        },
    };
    io.set_topology(topology);
    let allocation = io.outputs.enumeration(false);
    let mut bus = BusHandler(&io);
    bus.configured(true);
    bus.suspended(true);
    bus.suspended(false);
    bus.reset();
    bus.configured(true);
    assert_eq!(io.outputs.enumeration(false), allocation);
    assert_ne!(io.status().generation, allocation.generation);
    io.set_topology(topology);
    assert_ne!(
        io.outputs.enumeration(false).generation,
        allocation.generation
    );
}

// These fixtures poll futures explicitly. Timers use a fixed clock unless a test
// advances it, so endpoint scheduling does not depend on wall-clock timing.
struct Clock;
std::thread_local! { pub(super) static TICKS: core::cell::Cell<u64> = const { core::cell::Cell::new(0) }; }
impl embassy_time_driver::Driver for Clock {
    fn now(&self) -> u64 {
        TICKS.with(core::cell::Cell::get)
    }
    fn schedule_wake(&self, _: u64, _: &core::task::Waker) {}
}
embassy_time_driver::time_driver_impl!(static CLOCK: Clock = Clock);

#[cfg(feature = "gamepad")]
#[test]
fn allocation_change_during_a_long_command_gives_its_response_a_fresh_drain_deadline() {
    use cordial_core::bluetooth::Event;
    use cordial_protocol::request::Command;
    for stalled in [false, true] {
        let io = Io::with_budget(Budget {
            config_bytes: 121,
            ..Budget::PICO
        });
        let (mut app, mut store, radio) = profile_application();
        for id in 1..=3 {
            let mut policy = cordial_core::devices::Policy::paired(
                id,
                support::peer(id as u8 + 10),
                b"Controller",
            );
            policy.enabled = false;
            block_on(cordial_core::bonds::commit(
                &mut store,
                &policy,
                &support::bond(id, policy.peer),
            ))
            .unwrap();
        }
        app.manager.output_support = outputs::SUPPORT;
        app.manager.usb_budget = io.budget;
        io.set_topology(app.manager.usb_allocation().topology);
        assert!(!io.topology().desktop);
        let mut shared = Shared { app, store, radio };
        let mut loops = Loops::new(&io, 0);
        let mut host = Host::new();
        BusHandler(&io).configured(true);
        io.dtr(true);
        negotiate(&mut loops, &mut shared, &mut host, &io, 1);
        host.send(
            &io,
            Command::DeleteProfile(cordial_protocol::DeleteProfile { profile: 1 }),
        );
        loops.poll(&mut shared, 2);
        assert!(shared.app.command_pending());
        let slot = shared.app.manager.find(77).unwrap();
        let link = shared
            .app
            .manager
            .connect(slot, true, 30_000, None, &mut shared.radio)
            .unwrap()
            .unwrap();
        shared.radio.events.push_back(Event::Connected {
            link,
            descriptors: support::descriptor(),
            max_output: 255,
            layout: None,
        });
        loops.poll(&mut shared, 3);
        assert!(shared.app.command_pending());
        assert!(shared.app.manager.usb_allocation().topology.desktop);
        let mut completed = 3 + owner::RESPONSE_DRAIN_MS;
        for _ in 0..20 {
            loops.poll(&mut shared, completed);
            assert!(!io.reconnect.signaled());
            if !shared.app.command_pending() {
                break;
            }
            completed += 1;
        }
        assert!(!shared.app.command_pending());
        assert!(shared.app.serial.queued() > 0);
        assert!(block_on(cordial_core::profiles::metadata(&mut shared.store, 1)).is_err());
        loops.poll(&mut shared, completed + 1);
        assert!(
            !io.reconnect.signaled(),
            "the durable response gets a fresh deadline"
        );
        if stalled {
            loops.poll(&mut shared, completed + owner::RESPONSE_DRAIN_MS);
            assert!(!io.reconnect.signaled());
            loops.poll(&mut shared, completed + 1 + owner::RESPONSE_DRAIN_MS);
        } else {
            for now in completed + 2..completed + owner::RESPONSE_DRAIN_MS {
                host.serve(&io);
                loops.poll(&mut shared, now);
                if io.reconnect.signaled() {
                    break;
                }
            }
            assert_eq!(host.response(), Some(&ok()));
            assert_eq!(
                host.messages.last(),
                Some(&session_ending(
                    cordial_protocol::SessionEndingReason::DeviceConnection
                ))
            );
        }
        assert!(io.reconnect.try_take().unwrap().desktop);
        assert!(!shared.app.serial.open());
    }
}

#[cfg(feature = "gamepad")]
#[test]
fn stalled_response_detaches_at_a_fixed_deadline_despite_new_host_traffic() {
    use cordial_protocol::{OutputMode, Role, request::Command};
    let io = Io::with_budget(Budget {
        config_bytes: 121,
        ..Budget::PICO
    });
    let (mut app, store, radio) = profile_application();
    app.manager.output_support = outputs::SUPPORT;
    app.manager.usb_budget = io.budget;
    io.set_topology(app.manager.usb_allocation().topology);
    let mut shared = Shared { app, store, radio };
    let mut loops = Loops::new(&io, 0);
    let mut host = Host::new();
    BusHandler(&io).configured(true);
    io.dtr(true);
    negotiate(&mut loops, &mut shared, &mut host, &io, 1);
    host.send(
        &io,
        Command::SetDevice(cordial_protocol::SetDevice {
            device: 77,
            output_preferences: std::vec![cordial_protocol::OutputPreference {
                role: Role::Gamepad as i32,
                mode: OutputMode::Xinput as i32
            }],
            ..Default::default()
        }),
    );
    loops.poll(&mut shared, 2);
    assert_eq!(shared.app.manager.outputs.plan().xinput_gamepads, 1);
    let session = io.status().session;
    let writes = shared.store.writes.len();
    let mut request = std::vec::Vec::new();
    cordial_protocol::frame::encode(
        &cordial_protocol::Request {
            command: Some(Command::GetStatus(cordial_protocol::GetStatus {})),
        },
        &mut request,
    );
    for now in 3..3 + owner::RESPONSE_DRAIN_MS {
        let mut bytes = [0; 64];
        bytes[..request.len()].copy_from_slice(&request);
        let _ = io.serial_rx.try_send(SerialRx {
            session,
            bytes,
            length: request.len(),
        });
        loops.poll(&mut shared, now);
        assert!(!io.reconnect.signaled());
    }
    assert!(host.response().is_none());
    loops.poll(&mut shared, 3 + owner::RESPONSE_DRAIN_MS);
    assert_eq!(io.reconnect.try_take().unwrap().gamepads.xinput_gamepads, 1);
    assert!(!shared.app.serial.open());
    assert_eq!(shared.app.serial.queued(), 0);
    assert_ne!(io.status().session, session);
    assert_eq!(shared.store.writes.len(), writes);
    assert_eq!(
        block_on(outputs::saved_plan(&mut shared.store)).xinput_gamepads,
        1
    );
}

#[test]
fn stalled_list_keeps_hid_running_and_reopen_cancels_the_read() {
    use cordial_core::bluetooth::{Event, InputReport};
    use cordial_protocol::{request::Command, response::Result as R};
    let (owner, io, link) = running(80, 0);
    let mut shared = block_on(owner.lock());
    let mut loops = Loops::new(io, 0);
    let mut host = Host::new();
    for now in 0..100 {
        loops.poll(&mut shared, NOW + now);
        host.serve(io);
    }
    host.messages.clear();
    host.send(io, Command::ListProfiles(cordial_protocol::ListProfiles {}));
    for now in 100..200 {
        loops.poll(&mut shared, NOW + now);
        if io.serial_tx.is_full() {
            break;
        }
    }
    assert!(shared.app.streaming());
    assert!(io.serial_tx.is_full());
    let initial_reads = reads(&shared.store, cordial_core::profiles::METADATA);
    let queued = shared.app.serial.queued();
    let mut reports = 0;
    for n in 0..1000 {
        shared.radio.events.push_back(Event::Input(
            InputReport::new(link, cordial_core::link::ServiceId(7), 0, &[(n % 2) as u8]).unwrap(),
        ));
        loops.poll(&mut shared, NOW + 200 + n);
        if let Ok(tx) = io.hid_tx.try_receive() {
            reports += 1;
            io.hid_done
                .try_send(HidDone {
                    generation: tx.generation,
                    sequence: tx.sequence,
                    success: true,
                })
                .ok()
                .unwrap();
        }
        assert_eq!(
            reads(&shared.store, cordial_core::profiles::METADATA),
            initial_reads
        );
        assert_eq!(shared.app.serial.queued(), queued);
    }
    assert!(
        reports >= 999,
        "{reports} HID reports during stalled serial output"
    );
    let stale = io.serial_tx.try_receive().ok().unwrap();
    io.dtr(false);
    io.dtr(true);
    loops.poll(&mut shared, NOW + 2000);
    assert!(!shared.app.command_pending());
    io.serial_done
        .try_send(SerialDone {
            session: stale.session,
            token: stale.token,
            length: stale.length,
        })
        .ok()
        .unwrap();
    host = Host::new();
    negotiate(&mut loops, &mut shared, &mut host, io, NOW + 2000);
    host.send(io, Command::GetStatus(cordial_protocol::GetStatus {}));
    for now in 2001..2100 {
        loops.poll(&mut shared, NOW + now);
        host.serve(io);
        if host.response().is_some() {
            break;
        }
    }
    assert!(matches!(
        host.response().and_then(|r| r.result.as_ref()),
        Some(R::Status(_))
    ));
    assert_eq!(
        host.messages
            .iter()
            .filter(|m| matches!(m, cordial_protocol::message::Kind::Response(_)))
            .count(),
        1
    );
}

#[test]
fn stalled_list_allows_raw_editor_writes_and_queries() {
    use cordial_protocol::{message::Kind, request::Command, response::Result as R};
    for interface in [Interface::Via, Interface::Vial] {
        let io = Io::new();
        let (mut app, mut store, _) = profile_application();
        app.manager.preference.configuration_interfaces = std::vec![enabled(interface, 1)];
        for _ in 2..80 {
            block_on(cordial_core::profiles::create(
                &mut store,
                "Editor streaming profile",
                &cordial_core::profiles::Rules::default(),
            ))
            .unwrap();
        }
        let mut shared = shared(app, store);
        let mut loops = Loops::new(&io, interface.bit());
        BusHandler(&io).configured(true);
        io.dtr(true);
        let mut host = Host::new();
        for now in 0..100 {
            loops.poll(&mut shared, NOW + now);
            host.serve(&io);
        }
        host.messages.clear();
        host.send(
            &io,
            Command::ListProfiles(cordial_protocol::ListProfiles {}),
        );
        for now in 100..200 {
            loops.poll(&mut shared, NOW + now);
            if io.serial_tx.is_full() {
                break;
            }
        }
        assert!(shared.app.streaming() && !shared.app.serial.stream_ready());
        assert!(io.serial_tx.is_full());
        let queued = shared.app.serial.queued();
        host.send(&io, Command::GetStatus(cordial_protocol::GetStatus {}));
        for (step, command) in [5, 4].into_iter().enumerate() {
            let mut bytes = [0; 32];
            bytes[0] = command;
            if command == 5 {
                bytes = set_keycode();
            }
            io.raw[0]
                .rx
                .try_send(RawPacket {
                    generation: io.status().generation,
                    interface,
                    bytes,
                })
                .ok()
                .unwrap();
            loops.poll(&mut shared, NOW + 200 + step as u64);
            let reply = io.raw[0].tx.try_receive().expect("raw editor reply");
            assert_eq!(reply.interface, interface);
            assert_eq!(&reply.bytes[..6], &[command, 0, 0, 0, 0, 28]);
            assert_eq!(loaded_rules(&shared.app, 1), 1);
            assert!(shared.app.streaming());
            assert_eq!(shared.app.serial.queued(), queued);
        }
        assert_eq!(rules(&mut shared.store, 1), 0);
        for now in 206..2200 {
            loops.poll(&mut shared, NOW + now);
        }
        assert_eq!(rules(&mut shared.store, 1), 1);
        assert!(shared.app.streaming() && io.serial_tx.is_full());
        assert_eq!(shared.app.serial.queued(), queued);
        assert!(host.messages.is_empty());
        for now in 2200..4200 {
            loops.poll(&mut shared, NOW + now);
            host.serve(&io);
        }
        assert!(matches!(
            host.messages.first(),
            Some(Kind::Response(response)) if matches!(
                &response.result, Some(R::Profiles(profiles)) if profiles.entries.len() == 80
            )
        ));
        let responses: std::vec::Vec<_> = host
            .messages
            .iter()
            .filter_map(|message| match message {
                Kind::Response(response) => response.result.as_ref(),
                _ => None,
            })
            .collect();
        assert_eq!(responses.len(), 2);
        assert!(matches!(responses[1], R::Status(_)));
        assert!(host.messages.iter().skip(1).any(|message| matches!(
            message,
            Kind::Event(event) if matches!(
                &event.kind, Some(cordial_protocol::event::Kind::Profile(profile)) if profile.id == 1
            )
        )));
    }
}

#[test]
fn stalled_list_allows_reconnect_settings_jobs_and_deferred_saves() {
    stalled_list_reconnect(true);
}

#[test]
fn stalled_list_allocation_change_aborts_without_losing_reconnect_settings_or_saves() {
    stalled_list_reconnect(false);
}

fn stalled_list_reconnect(keep_allocation: bool) {
    use cordial_core::{
        bluetooth::{Event, InputReport},
        compact::{Metadata, Preference},
        devices::{Policies, Roles},
        model::{
            hidpp::{FeatureId, FeatureRevision},
            settings::{SettingKey, SettingScope},
        },
        settings::PreferenceStore,
        storage::Preferences,
    };
    use cordial_protocol::request::Command;
    let (owner, io, old_link) = running(80, 0);
    let mut shared = block_on(owner.lock());
    let mut loops = Loops::new(io, 0);
    let mut host = Host::new();
    for now in 0..100 {
        loops.poll(&mut shared, NOW + now);
        host.serve(io);
    }
    let mut policy = block_on(
        Policies {
            store: &mut shared.store,
        }
        .load(77),
    )
    .unwrap();
    policy.name = "Reconnected keyboard".into();
    policy.roles = Roles(if keep_allocation {
        cordial_core::hid::MOUSE
    } else {
        0
    });
    block_on(
        Policies {
            store: &mut shared.store,
        }
        .save(&policy),
    )
    .unwrap();
    shared.app.manager.outputs.roles_changed(77, policy.roles.0);
    let preference = Preference {
        metadata: Metadata {
            key: SettingKey::BacklightEnabled,
            feature: FeatureId::BACKLIGHT,
            revision: FeatureRevision(3),
            scope: SettingScope::Device,
            choices: std::boxed::Box::new([]),
            range: None,
        },
        value: 1,
    };
    block_on(
        Preferences {
            store: &mut shared.store,
            device: 77,
        }
        .save(&preference),
    )
    .unwrap();
    host.messages.clear();
    host.send(io, Command::ListProfiles(cordial_protocol::ListProfiles {}));
    for now in 100..200 {
        loops.poll(&mut shared, NOW + now);
        if io.serial_tx.is_full() {
            break;
        }
    }
    assert!(shared.app.streaming() && io.serial_tx.is_full());
    let profile_reads = reads(&shared.store, cordial_core::profiles::METADATA);
    let session = io.status().session;
    shared.radio.events.push_back(Event::Disconnected {
        link: old_link,
        error: None,
    });
    loops.poll(&mut shared, NOW + 200);
    assert_eq!(
        shared.app.manager.usb_allocation().topology.desktop,
        keep_allocation
    );
    assert_eq!(shared.app.streaming(), keep_allocation);
    assert_eq!(shared.app.serial.ending(), !keep_allocation);
    if !keep_allocation {
        assert_eq!(
            shared.app.usb_change_reason(),
            cordial_protocol::SessionEndingReason::DeviceDisconnection
        );
    }
    assert!(!io.reconnect.signaled());
    let Shared {
        app,
        store: _,
        radio,
    } = &mut *shared;
    let slot = app.manager.find(77).unwrap();
    let link = app.manager.link_for(slot).unwrap_or_else(|| {
        app.manager
            .connect(slot, true, 90_000, None, radio)
            .unwrap()
            .unwrap()
    });
    shared.radio.events.push_back(Event::Connected {
        link,
        descriptors: support::descriptor(),
        max_output: 255,
        layout: None,
    });
    loops.poll(&mut shared, NOW + 201);
    assert!(
        shared.app.manager.devices[slot]
            .as_ref()
            .unwrap()
            .live
            .as_ref()
            .unwrap()
            .policy
            .is_none()
    );
    shared.radio.events.push_back(Event::Input(
        InputReport::new(link, cordial_core::link::ServiceId(7), 0, &[1]).unwrap(),
    ));
    let mut settings_started = false;
    for now in 202..2200 {
        loops.poll(&mut shared, NOW + now);
        assert_eq!(
            io.reconnect.signaled(),
            !keep_allocation && now >= 200 + owner::RESPONSE_DRAIN_MS
        );
        settings_started |= shared
            .app
            .manager
            .connection(link)
            .unwrap()
            .runtime
            .as_ref()
            .unwrap()
            .settings
            .busy();
        if let Ok(tx) = io.hid_tx.try_receive() {
            io.hid_done
                .try_send(HidDone {
                    generation: tx.generation,
                    sequence: tx.sequence,
                    success: true,
                })
                .ok()
                .unwrap();
        }
    }
    let live = shared.app.manager.devices[slot]
        .as_ref()
        .unwrap()
        .live
        .as_ref()
        .unwrap();
    assert_eq!(&*live.policy.as_ref().unwrap().name, "Reconnected keyboard");
    assert_eq!(
        live.catalog.preferences().collect::<std::vec::Vec<_>>(),
        [&preference]
    );
    assert!(
        settings_started,
        "hydration must start the saved-settings job"
    );
    let saved = block_on(
        Policies {
            store: &mut shared.store,
        }
        .load(77),
    )
    .unwrap();
    assert_eq!(
        saved.roles,
        Roles(cordial_core::hid::KEYBOARD),
        "deferred roles save must finish"
    );
    assert_eq!(
        reads(&shared.store, cordial_core::profiles::METADATA),
        profile_reads
    );
    if keep_allocation {
        assert!(shared.app.streaming() && io.serial_tx.is_full());
        assert!(!shared.app.serial.ending());
        assert_eq!(io.status().session, session);
    } else {
        assert!(!shared.app.command_pending());
        assert!(!shared.app.serial.open());
        assert_eq!(shared.app.serial.queued(), 0);
        assert_ne!(io.status().session, session);
        assert!(io.reconnect.try_take().unwrap().desktop);
    }
    assert!(host.response().is_none());
}

#[test]
fn draining_list_gives_raw_editors_a_turn_before_completion() {
    use cordial_protocol::{message::Kind, request::Command, response::Result as R};
    for interface in [Interface::Via, Interface::Vial] {
        for corrupt in [false, true] {
            let io = Io::new();
            let (mut app, mut store, _) = profile_application();
            app.manager.preference.configuration_interfaces = std::vec![enabled(interface, 1)];
            for _ in 2..200 {
                block_on(cordial_core::profiles::create(
                    &mut store,
                    "Editor streaming profile",
                    &cordial_core::profiles::Rules::default(),
                ))
                .unwrap();
            }
            if corrupt {
                for id in 3..=200 {
                    store
                        .records
                        .insert(cordial_core::storage::record_key(8, id), std::vec![0xff]);
                }
            }
            let mut shared = shared(app, store);
            let mut loops = Loops::new(&io, interface.bit());
            BusHandler(&io).configured(true);
            io.dtr(true);
            let mut host = Host::new();
            for now in 0..100 {
                loops.poll(&mut shared, NOW + now);
                host.serve(&io);
            }
            host.messages.clear();
            host.send(
                &io,
                Command::ListProfiles(cordial_protocol::ListProfiles {}),
            );
            let mut now = NOW + 100;
            for _ in 0..40 {
                loops.poll(&mut shared, now);
                host.serve(&io);
                now += 1;
            }
            assert!(shared.app.streaming());
            host.send(&io, Command::GetStatus(cordial_protocol::GetStatus {}));
            for command in [5, 4, 4, 4] {
                let mut bytes = [0; 32];
                bytes[0] = command;
                if command == 5 {
                    bytes = set_keycode();
                }
                io.raw[0]
                    .rx
                    .try_send(RawPacket {
                        generation: io.status().generation,
                        interface,
                        bytes,
                    })
                    .ok()
                    .unwrap();
                let mut reply = None;
                for _ in 0..4 {
                    loops.poll(&mut shared, now);
                    host.serve(&io);
                    now += 1;
                    assert!(
                        shared.app.streaming(),
                        "editor reply must precede list completion"
                    );
                    assert!(host.messages.is_empty(), "no interleaved response or event");
                    reply = io.raw[0].tx.try_receive().ok();
                    if reply.is_some() {
                        break;
                    }
                }
                let reply = reply.expect(
                    "editor reply within four secondary steps while the host drains the list",
                );
                assert_eq!(&reply.bytes[..6], &[command, 0, 0, 0, 0, 28]);
                assert_eq!(loaded_rules(&shared.app, 1), 1);
            }
            for _ in 0..4000 {
                loops.poll(&mut shared, now);
                host.serve(&io);
                now += 1;
            }
            assert!(
                matches!(host.messages.first(), Some(Kind::Response(response)) if matches!(
                    &response.result, Some(R::Profiles(profiles)) if profiles.entries.len() == if corrupt { 2 } else { 200 }
                ))
            );
            let responses: std::vec::Vec<_> = host
                .messages
                .iter()
                .filter_map(|message| match message {
                    Kind::Response(response) => response.result.as_ref(),
                    _ => None,
                })
                .collect();
            assert_eq!(responses.len(), 2);
            assert!(matches!(responses[1], R::Status(_)));
            assert_eq!(rules(&mut shared.store, 1), 1);
        }
    }
}

fn take_serial(
    io: &Io,
    decoder: &mut cordial_protocol::frame::Decoder,
    frames: &mut std::vec::Vec<std::vec::Vec<u8>>,
) {
    if let Ok(tx) = io.serial_tx.try_receive() {
        for &byte in &tx.bytes[..tx.length] {
            if let Some(frame) = decoder.push(byte) {
                frames.push(frame.unwrap().to_vec());
            }
        }
        io.serial_done
            .try_send(SerialDone {
                session: tx.session,
                token: tx.token,
                length: tx.length,
            })
            .ok()
            .unwrap();
    }
}

#[test]
fn ordinary_commands_and_hid_work_without_hello_and_queries_work_mid_session() {
    use cordial_core::bluetooth::{Event, InputReport};
    use cordial_protocol::{self as p, message::Kind, response::Result as R};
    use prost::Message;
    let (owner, io, link) = running(2, 0);
    let mut shared = block_on(owner.lock());
    let mut loops = Loops::new(io, 0);
    let mut request = std::vec::Vec::new();
    p::frame::encode(
        &p::Request {
            command: Some(p::request::Command::GetStatus(p::GetStatus {})),
        },
        &mut request,
    );
    send_bytes(io, io.status().session, &request);
    let mut decoder = p::frame::Decoder::new(None);
    let mut frames = std::vec::Vec::new();
    let mut reports = 0;
    for now in 0..40 {
        shared.radio.events.push_back(Event::Input(
            InputReport::new(link, cordial_core::link::ServiceId(7), 0, &[now % 2]).unwrap(),
        ));
        loops.poll(&mut shared, NOW + u64::from(now));
        take_serial(io, &mut decoder, &mut frames);
        if let Ok(tx) = io.hid_tx.try_receive() {
            reports += 1;
            io.hid_done
                .try_send(HidDone {
                    generation: tx.generation,
                    sequence: tx.sequence,
                    success: true,
                })
                .ok()
                .unwrap();
        }
    }
    assert!(reports > 30);
    let status = |bytes: &[u8]| {
        matches!(
            p::Message::decode(bytes).unwrap().kind,
            Some(Kind::Response(p::Response {
                result: Some(R::Status(_))
            }))
        )
    };
    assert!(frames.iter().any(|bytes| status(bytes)));
    assert!(shared.app.serial.open());
    frames.clear();
    let mut input = client_hello();
    input.extend(request);
    send_bytes(io, io.status().session, &input);
    for now in 40..60 {
        loops.poll(&mut shared, NOW + now);
        take_serial(io, &mut decoder, &mut frames);
    }
    let identity = frames
        .iter()
        .position(|bytes| p::handshake::decode_adapter_hello(bytes).is_ok())
        .unwrap();
    let hello = p::handshake::decode_adapter_hello(&frames[identity]).unwrap();
    assert_eq!(hello.protocol_version, Some(p::CURRENT_PROTOCOL_VERSION));
    assert_eq!(hello.firmware_version.as_deref(), Some("test"));
    assert_eq!(hello.serial_number.as_deref(), Some("adapter"));
    assert!(frames[identity + 1..].iter().any(|bytes| status(bytes)));
    assert!(!shared.app.command_pending());
}

#[test]
fn suspend_preserves_the_session_and_usb_rebuild_opens_a_fresh_session() {
    let io = Io::new();
    let mut loops = Loops::new(&io, 0);
    let mut shared = shared(application(), support::Store::default());
    BusHandler(&io).configured(true);
    io.dtr(true);
    let mut host = Host::new();
    negotiate(&mut loops, &mut shared, &mut host, &io, 0);
    let session = io.status().session;
    BusHandler(&io).suspended(true);
    loops.poll(&mut shared, 1);
    BusHandler(&io).suspended(false);
    loops.poll(&mut shared, 2);
    assert_eq!(io.status().session, session);
    assert!(shared.app.serial.open());
    io.reset(false);
    loops.poll(&mut shared, 3);
    assert!(!shared.app.serial.open());
    BusHandler(&io).configured(true);
    io.dtr(true);
    loops.poll(&mut shared, 4);
    assert_ne!(io.status().session, session);
    assert!(shared.app.serial.open());
    assert_eq!(shared.app.serial.queued(), 0);
    host = Host::new();
    negotiate(&mut loops, &mut shared, &mut host, &io, 5);
}

#[test]
fn hello_write_retries_and_partial_completions_keep_pipelined_requests_waiting() {
    let io = Io::new();
    let mut loops = Loops::new(&io, 0);
    let mut shared = shared(
        Application::new(Build {
            development: false,
            version: "test",
            board: "test",
            default_adapter_name: "Test adapter",
            adapter_id: "a".repeat(100),
            profile_memory_budget: None,
            restart: || panic!("restarted"),
            bootloader: None,
        }),
        support::Store::default(),
    );
    BusHandler(&io).configured(true);
    io.dtr(true);
    let mut bytes = client_hello();
    cordial_protocol::frame::encode(
        &cordial_protocol::Request {
            command: Some(cordial_protocol::request::Command::GetStatus(
                cordial_protocol::GetStatus {},
            )),
        },
        &mut bytes,
    );
    send_bytes(&io, io.status().session, &bytes);
    loops.poll(&mut shared, 0);
    let separator = io.serial_tx.try_receive().unwrap();
    complete_serial(&io, &separator, separator.length);
    loops.poll(&mut shared, 1);
    let first = io.serial_tx.try_receive().unwrap();
    assert_eq!(first.length, 63);
    assert!(!first.bytes[..first.length].contains(&0));
    complete_serial(&io, &first, 0);
    loops.poll(&mut shared, 2);
    let retry = io.serial_tx.try_receive().unwrap();
    assert_eq!(retry.bytes, first.bytes);
    assert!(shared.app.serial.open());
    assert!(!shared.app.command_pending());
    complete_serial(&io, &retry, retry.length);
    loops.poll(&mut shared, 3);
    let last = io.serial_tx.try_receive().unwrap();
    assert_eq!(last.bytes[last.length - 1], 0);
    assert!(shared.app.serial.open());
    complete_serial(&io, &last, last.length - 1);
    loops.poll(&mut shared, 4);
    let delimiter = io.serial_tx.try_receive().unwrap();
    assert_eq!(&delimiter.bytes[..delimiter.length], &[0]);
    assert!(shared.app.serial.open());
    assert!(!shared.app.command_pending());
    assert_eq!(shared.app.serial.queued(), 1);
    complete_serial(&io, &delimiter, 1);
    loops.poll(&mut shared, 5);
    assert!(shared.app.serial.open());
    assert!(io.serial_tx.try_receive().is_ok());
}

pub(super) fn complete_serial(io: &Io, tx: &SerialTx, sent: usize) {
    io.serial_done
        .try_send(SerialDone {
            session: tx.session,
            token: tx.token,
            length: sent,
        })
        .ok()
        .unwrap();
}

fn session_ending(
    reason: cordial_protocol::SessionEndingReason,
) -> cordial_protocol::message::Kind {
    cordial_protocol::message::Kind::Event(cordial_protocol::Event {
        kind: Some(cordial_protocol::event::Kind::SessionEnding(
            cordial_protocol::SessionEnding {
                reason: reason as i32,
            },
        )),
    })
}

#[cfg(feature = "gamepad")]
#[test]
fn autonomous_allocation_changes_announce_the_trigger_and_wait_for_the_delimiter() {
    use cordial_core::bluetooth::Event;
    use cordial_protocol::{self as p, message::Kind};
    use prost::Message;
    for budget in [Budget::PICO, Budget::ESP] {
        for disconnect in [false, true] {
            let io = Io::with_budget(Budget {
                config_bytes: 121,
                ..budget
            });
            let (mut app, mut store, mut radio) = profile_application();
            app.manager.output_support = outputs::SUPPORT;
            app.manager.usb_budget = io.budget;
            let slot = app.manager.find(77).unwrap();
            let link = app
                .manager
                .connect(slot, true, 30_000, None, &mut radio)
                .unwrap()
                .unwrap();
            if disconnect {
                block_on(app.event(
                    Event::Connected {
                        link,
                        descriptors: support::descriptor(),
                        max_output: 255,
                        layout: None,
                    },
                    &mut store,
                    &mut radio,
                    0,
                ));
            }
            io.set_topology(app.manager.usb_allocation().topology);
            assert_eq!(io.topology().desktop, disconnect);
            let mut shared = Shared { app, store, radio };
            let mut loops = Loops::new(&io, 0);
            let mut host = Host::new();
            BusHandler(&io).configured(true);
            io.dtr(true);
            negotiate(&mut loops, &mut shared, &mut host, &io, 1);
            for now in 2..50 {
                host.serve(&io);
                loops.poll(&mut shared, now);
            }
            shared.radio.events.push_back(if disconnect {
                Event::Disconnected { link, error: None }
            } else {
                Event::Connected {
                    link,
                    descriptors: support::descriptor(),
                    max_output: 255,
                    layout: None,
                }
            });
            loops.poll(&mut shared, 50);
            assert!(!io.reconnect.signaled());
            let event = io.serial_tx.try_receive().unwrap();
            let mut decoder = p::frame::Decoder::new(None);
            let mut messages = Vec::new();
            for &byte in &event.bytes[..event.length] {
                if let Some(frame) = decoder.push(byte) {
                    messages.push(p::Message::decode(frame.unwrap()).unwrap().kind.unwrap());
                }
            }
            let reason = if disconnect {
                p::SessionEndingReason::DeviceDisconnection
            } else {
                p::SessionEndingReason::DeviceConnection
            };
            assert_eq!(messages, [session_ending(reason)]);
            assert!(
                matches!(messages[0], Kind::Event(_)),
                "no request is needed"
            );
            assert_eq!(event.bytes[event.length - 1], p::frame::DELIMITER);
            complete_serial(&io, &event, event.length - 1);
            loops.poll(&mut shared, 51);
            assert!(!io.reconnect.signaled());
            let delimiter = io.serial_tx.try_receive().unwrap();
            assert_eq!(&delimiter.bytes[..delimiter.length], &[p::frame::DELIMITER]);
            complete_serial(&io, &delimiter, 0);
            loops.poll(&mut shared, 52);
            assert!(!io.reconnect.signaled());
            let retry = io.serial_tx.try_receive().unwrap();
            assert_eq!(&retry.bytes[..retry.length], &[p::frame::DELIMITER]);
            complete_serial(&io, &retry, 1);
            loops.poll(&mut shared, 53);
            assert_eq!(io.reconnect.try_take().unwrap().desktop, !disconnect);
            assert_eq!(shared.app.serial.queued(), 0);
        }
    }
}

#[cfg(feature = "gamepad")]
#[test]
fn autonomous_allocation_without_a_session_reconnects_immediately() {
    use cordial_core::bluetooth::Event;
    let io = Io::with_budget(Budget {
        config_bytes: 121,
        ..Budget::PICO
    });
    let (mut app, store, mut radio) = profile_application();
    app.manager.output_support = outputs::SUPPORT;
    app.manager.usb_budget = io.budget;
    io.set_topology(app.manager.usb_allocation().topology);
    let slot = app.manager.find(77).unwrap();
    let link = app
        .manager
        .connect(slot, true, 30_000, None, &mut radio)
        .unwrap()
        .unwrap();
    radio.events.push_back(Event::Connected {
        link,
        descriptors: support::descriptor(),
        max_output: 255,
        layout: None,
    });
    let mut shared = Shared { app, store, radio };
    let mut loops = Loops::new(&io, 0);
    loops.poll(&mut shared, 0);
    assert!(io.reconnect.try_take().unwrap().desktop);
    assert!(io.serial_tx.is_empty());
    assert_eq!(shared.app.serial.queued(), 0);
}

#[test]
fn reconfiguration_stops_listing_entries_and_discards_queued_requests() {
    use cordial_protocol::{self as p, message::Kind, request::Command, response::Result as R};
    for stalled in [false, true] {
        let io = Io::new();
        let (app, store, radio) = profile_application();
        let mut shared = Shared { app, store, radio };
        let mut loops = Loops::new(&io, 0);
        let mut host = Host::new();
        BusHandler(&io).configured(true);
        io.dtr(true);
        negotiate(&mut loops, &mut shared, &mut host, &io, 1);
        host.send(&io, Command::ListProfiles(p::ListProfiles {}));
        loops.poll(&mut shared, 2);
        loops.poll(&mut shared, 3);
        assert!(shared.app.streaming());
        host.send(
            &io,
            Command::SetAdapter(p::SetAdapter {
                name: Some("Never saved".into()),
                ..Default::default()
            }),
        );
        shared.app.manager.preference.configuration_interfaces = vec![enabled(Interface::Via, 1)];
        let name = shared.app.manager.preference.name.clone();
        let reads_before = reads(&shared.store, cordial_core::profiles::METADATA);
        loops.poll(&mut shared, 4);
        assert!(!shared.app.command_pending());
        for now in 5..=4 + owner::RESPONSE_DRAIN_MS {
            if !stalled {
                host.serve(&io);
            }
            loops.poll(&mut shared, now);
            if io.reconnect.signaled() {
                break;
            }
        }
        assert_eq!(
            io.reconnect.try_take().unwrap().interfaces,
            Interface::Via.bit()
        );
        assert_eq!(
            reads(&shared.store, cordial_core::profiles::METADATA),
            reads_before
        );
        assert_eq!(shared.app.manager.preference.name, name);
        if stalled {
            assert_eq!(shared.app.serial.queued(), 0);
            assert!(host.response().is_none());
            continue;
        }
        let responses: Vec<_> = host
            .messages
            .iter()
            .filter_map(|message| match message {
                Kind::Response(response) => Some(response),
                _ => None,
            })
            .collect();
        assert_eq!(responses.len(), 1);
        assert!(
            matches!(&responses[0].result, Some(R::Error(error)) if error.code == p::ErrorCode::Busy as i32)
        );
        assert_eq!(
            host.messages.last(),
            Some(&session_ending(p::SessionEndingReason::Unknown))
        );
    }
}

#[test]
fn saved_name_change_keeps_the_session_when_allocation_is_unchanged() {
    use cordial_protocol::{self as p, request::Command};
    let io = Io::new();
    let (mut app, store, radio) = profile_application();
    app.manager.usb_budget = io.budget;
    io.set_topology(app.manager.usb_allocation().topology);
    let mut shared = Shared { app, store, radio };
    let mut loops = Loops::new(&io, 0);
    let mut host = Host::new();
    BusHandler(&io).configured(true);
    io.dtr(true);
    negotiate(&mut loops, &mut shared, &mut host, &io, 1);
    host.send(
        &io,
        Command::SetAdapter(p::SetAdapter {
            name: Some("Office".into()),
            ..Default::default()
        }),
    );
    for now in 2..200 {
        host.serve(&io);
        loops.poll(&mut shared, now);
    }
    assert_eq!(host.response(), Some(&ok()));
    assert!(!io.reconnect.signaled());
    assert!(!shared.app.serial.ending());
    assert!(!host.messages.iter().any(|message| matches!(
        message,
        p::message::Kind::Event(p::Event {
            kind: Some(p::event::Kind::SessionEnding(_))
        })
    )));
}
