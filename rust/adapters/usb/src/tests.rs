use super::*;
use cordial_core::link::LinkId;
use cordial_core::storage::RecordStore;
use cordial_core::{
    application::{Application, Build},
    hid::{Held, Input},
    interfaces::InterfacePreference,
};
use embassy_futures::block_on;
use owner::{Owner, Shared};
#[allow(dead_code)]
mod support {
    use std::vec::Vec;
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../crates/cordial-core/tests/support/mod.rs"
    ));
}
fn application() -> Application {
    Application::new(Build {
        development: true,
        version: "test",
        board: "test",
        default_adapter_name: "Test adapter",
        adapter_id: "adapter".into(),
        profile_memory_budget: None,
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
    assert!(s.app.serial.active());
    let separator = io.serial_tx.try_receive().ok().unwrap();
    assert_eq!(
        &separator.bytes[..separator.length],
        &[cordial_protocol::frame::DELIMITER]
    );
    s.app.serial.respond(ok());
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
    assert_eq!(s.app.serial.queued(), 1);
    loops.poll(&mut s, 4);
    assert_eq!(s.app.serial.queued(), 1);
    io.dtr(false);
    io.dtr(true);
    loops.poll(&mut s, 5);
    s.app.serial.respond(ok());
    io.serial_done
        .try_send(SerialDone {
            session: packet.session,
            token: packet.token,
            length: packet.length,
        })
        .ok()
        .unwrap();
    loops.poll(&mut s, 6);
    assert_eq!(s.app.serial.queued(), 1);
    assert_eq!(io.serial_tx.len(), 1); // New session's separator, still pending.
}
#[test]
fn suspend_completion_cannot_consume_resumed_input_and_leds_use_standard_report() {
    let io = Io::new();
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
    loops.poll(&mut s, 1);
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
fn disabled_interfaces_are_omitted_from_both_descriptor_reads() {
    use crate::configuration::{DescriptorFilter, INTERFACE_BYTES};
    assert_eq!(DescriptorFilter::hidden(0), 1);
    for interface in Interface::ALL {
        assert_eq!(DescriptorFilter::hidden(interface.bit()), 0);
        let mut filter = DescriptorFilter::default();
        let mut bytes = [9, 2, 173, 0, 4, 1, 0, 0x80, 50];
        assert_eq!(filter.packet(&mut bytes, true, true), Some((9, true)));
        assert_eq!(bytes, [9, 2, 173, 0, 4, 1, 0, 0x80, 50]);
    }
    // The class builder's final Raw HID function consists of these descriptors.
    let total = 173usize;
    let visible = total - INTERFACE_BYTES;
    for requested in [9, 255] {
        let mut descriptor = std::vec![0; total];
        descriptor[..9].copy_from_slice(&[9, 2, total as u8, 0, 4, 1, 0, 0x80, 50]);
        let mut filter = DescriptorFilter::default();
        filter.hide = DescriptorFilter::hidden(0);
        let mut response = std::vec::Vec::new();
        let size = requested.min(total);
        for (i, data) in descriptor[..size].chunks(64).enumerate() {
            let mut data = data.to_vec();
            if let Some((len, _)) = filter.packet(&mut data, i == 0, (i + 1) * 64 >= size) {
                response.extend_from_slice(&data[..len]);
            }
        }
        assert_eq!(response.len(), requested.min(visible));
        assert_eq!(
            u16::from_le_bytes([response[2], response[3]]) as usize,
            visible
        );
        assert_eq!(response[4], 3);
    }
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
    s.app.usb_reconnect = true;
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
    assert_eq!(io.reconnect.try_take(), Some(Interface::Via.bit()));
    BusHandler(&io).configured(true);
    loops.poll(&mut s, 2);
    assert_eq!(io.raw[0].tx.try_receive().ok().unwrap().bytes[0], 255);
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
    // A packet tagged for an interface the slot no longer exposes is refused.
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
        assert_eq!(rules(&mut s.store, 1), edited);
        assert_eq!(rules(&mut s.store, 2), 0);
    }
    assert!(!io.reconnect.signaled());
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
    assert_eq!(io.reconnect.try_take(), Some(Interface::Vial.bit()));
    loops.poll(&mut s, 3);
    assert!(!io.reconnect.signaled());
    // The application requests a reconnect when an enabled interface's profile changes.
    s.app.manager.preference.configuration_interfaces = std::vec![enabled(Interface::Vial, 2)];
    s.app.usb_reconnect = true;
    loops.poll(&mut s, 4);
    assert_eq!(io.reconnect.try_take(), Some(Interface::Vial.bit()));
    assert!(!s.app.usb_reconnect);
    // Switching interfaces changes the set.
    s.app.manager.preference.configuration_interfaces = std::vec![enabled(Interface::Via, 2)];
    loops.poll(&mut s, 5);
    assert_eq!(io.reconnect.try_take(), Some(Interface::Via.bit()));
    s.app.manager.preference.configuration_interfaces.clear();
    loops.poll(&mut s, 6);
    assert_eq!(io.reconnect.try_take(), Some(0));
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
fn hidden_interface_descriptor_ends_full_packets_with_a_zlp() {
    let mut filter = crate::configuration::DescriptorFilter::default();
    filter.hide = 1;
    let mut first = [0; 64];
    first[..9].copy_from_slice(&[9, 2, 168, 0, 4, 1, 0, 0x80, 50]);
    assert_eq!(filter.packet(&mut first, true, false), Some((64, false)));
    assert_eq!(filter.packet(&mut [0; 64], false, false), Some((64, false)));
    assert_eq!(filter.packet(&mut [0; 40], false, true), Some((0, true)));
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
}
impl Host {
    fn new() -> Self {
        Self {
            decoder: cordial_protocol::frame::Decoder::new(None),
            messages: std::vec::Vec::new(),
            reports: 0,
        }
    }
    fn serve(&mut self, io: &Io) {
        use prost::Message;
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
            for &byte in &tx.bytes[..tx.length] {
                if let Some(frame) = self.decoder.push(byte) {
                    let message = cordial_protocol::Message::decode(frame.unwrap()).unwrap();
                    self.messages.push(message.kind.unwrap());
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
    fn send(&self, io: &Io, command: cordial_protocol::request::Command) {
        let mut bytes = std::vec::Vec::new();
        cordial_protocol::frame::encode(
            &cordial_protocol::Request {
                command: Some(command),
            },
            &mut bytes,
        );
        for chunk in bytes.chunks(64) {
            let mut packet = SerialRx {
                session: io.status().session,
                bytes: [0; 64],
                length: chunk.len(),
            };
            packet.bytes[..chunk.len()].copy_from_slice(chunk);
            io.serial_rx.try_send(packet).ok().unwrap();
        }
    }
    fn response(&self) -> Option<&cordial_protocol::Response> {
        self.messages.iter().find_map(|m| match m {
            cordial_protocol::message::Kind::Response(r) => Some(r),
            _ => None,
        })
    }
}
/// Runs both loops, with the host and `drive` taking turns with them, until `drive` returns true.
fn run(
    owner: &Owner<'static, support::Store, support::Radio>,
    io: &Io,
    host: &mut Host,
    mut drive: impl FnMut(&mut Shared<support::Store, support::Radio>, &mut Host) -> bool,
) {
    use embassy_futures::{join::join, select::select, yield_now};
    block_on(async {
        let loops = join(
            owner.priority(
                async |_: &mut Shared<support::Store, support::Radio>| {},
                async || yield_now().await,
                || NOW,
            ),
            owner.secondary(0, || NOW),
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
}
/// Record reads of `kind` so far.
fn reads(store: &support::Store, kind: u8) -> usize {
    store.reads.iter().filter(|key| key[0] == kind).count()
}
/// Runs until the session has started, the connection's reports have been sent and background
/// work has nothing left to read.
fn settle(owner: &Owner<'static, support::Store, support::Radio>, io: &Io, host: &mut Host) {
    let mut quiet = 0;
    let mut last = usize::MAX;
    run(owner, io, host, |shared, host| {
        let total = shared.store.reads.len();
        quiet = if total == last { quiet + 1 } else { 0 };
        last = total;
        quiet > 50 && host.reports > 0 && shared.app.manager.forward.pending() == 0
    });
}
/// Sends `command` and, each time it reads a record of `kind`, sends an input report from
/// `link`. Checks that the command reads one record per step and that each input was handled and
/// taken by USB before the next read. Returns the response and the number of records read.
fn input_goes_first(
    owner: &Owner<'static, support::Store, support::Radio>,
    io: &Io,
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
    let (owner, io, link) = running(6, 0);
    let (response, read) = input_goes_first(
        &owner,
        io,
        link,
        Command::ListProfiles(cordial_protocol::ListProfiles { after: 0 }),
        cordial_core::profiles::METADATA,
    );
    match response.result {
        Some(R::Profiles(list)) => assert_eq!(list.entries.len(), 6),
        other => panic!("{other:?}"),
    }
    assert_eq!(read, 6);
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
    let mut answered = false;
    block_on(async {
        let loops = join(
            owner.priority(
                async |_: &mut Shared<support::Store, support::Radio>| {},
                async || yield_now().await,
                clock,
            ),
            owner.secondary(0, clock),
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
