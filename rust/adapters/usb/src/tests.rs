use super::*;
use cordial_core::{
    application::{Application, Build},
    hid::{Held, Input},
};
use embassy_futures::block_on;
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
        bootloader: None,
    })
}
fn ok() -> cordial_protocol::Response {
    cordial_protocol::Response { result: None }
}
#[test]
fn serial_waits_for_completion_and_rejects_old_session_packets() {
    let io = Io::new();
    let mut owner = owner::Owner::new(&io);
    let mut app = application();
    let mut store = support::Store::default();
    let mut radio = support::Radio::default();
    BusHandler(&io).configured(true);
    io.dtr(true);
    block_on(owner.poll(&mut app, &mut store, &mut radio, 1));
    assert!(app.serial.active());
    let separator = io.serial_tx.try_receive().ok().unwrap();
    assert_eq!(
        &separator.bytes[..separator.length],
        &[cordial_protocol::frame::DELIMITER]
    );
    app.serial.respond(ok());
    block_on(owner.poll(&mut app, &mut store, &mut radio, 2));
    assert!(io.serial_tx.is_empty());
    io.serial_done
        .try_send(SerialDone {
            session: separator.session,
            token: None,
            length: 1,
        })
        .ok()
        .unwrap();
    block_on(owner.poll(&mut app, &mut store, &mut radio, 3));
    let packet = io.serial_tx.try_receive().ok().unwrap();
    assert!(packet.token.is_some());
    assert_eq!(app.serial.queued(), 1);
    block_on(owner.poll(&mut app, &mut store, &mut radio, 4));
    assert_eq!(app.serial.queued(), 1);
    io.dtr(false);
    io.dtr(true);
    block_on(owner.poll(&mut app, &mut store, &mut radio, 5));
    app.serial.respond(ok());
    io.serial_done
        .try_send(SerialDone {
            session: packet.session,
            token: packet.token,
            length: packet.length,
        })
        .ok()
        .unwrap();
    block_on(owner.poll(&mut app, &mut store, &mut radio, 6));
    assert_eq!(app.serial.queued(), 1);
    assert_eq!(io.serial_tx.len(), 1); // New session's separator, still pending.
}
#[test]
fn suspend_completion_cannot_consume_resumed_input_and_leds_use_standard_report() {
    let io = Io::new();
    let mut owner = owner::Owner::new(&io);
    let mut app = application();
    let mut store = support::Store::default();
    let mut radio = support::Radio::default();
    BusHandler(&io).configured(true);
    block_on(owner.poll(&mut app, &mut store, &mut radio, 1));
    let old = io.hid_tx.try_receive().ok().unwrap();
    BusHandler(&io).suspended(true);
    block_on(owner.poll(&mut app, &mut store, &mut radio, 2));
    let mut held = Held::default();
    held.keys[0] = 16;
    app.manager
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
    block_on(owner.poll(&mut app, &mut store, &mut radio, 3));
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
    block_on(owner.poll(&mut app, &mut store, &mut radio, 4));
    assert_eq!(app.manager.forward.packet().unwrap().id, 1);
    io.hid_done
        .try_send(HidDone {
            generation: new.generation,
            sequence: new.sequence,
            success: true,
        })
        .ok()
        .unwrap();
    block_on(owner.poll(&mut app, &mut store, &mut radio, 5));
    assert_eq!(app.manager.forward.packet().unwrap().id, 3);
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
    let mut owner = owner::Owner::new(&io);
    let mut app = application();
    app.manager.storage_ready = true;
    let mut store = ClosingStore { io: &io, writes: 0 };
    let mut radio = support::Radio::default();
    BusHandler(&io).configured(true);
    io.dtr(true);
    block_on(owner.poll(&mut app, &mut store, &mut radio, 1));
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
    assert_eq!(app.serial.feed(prefix).0, prefix.len());
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
        block_on(owner.poll(&mut app, &mut store, &mut radio, 2));
    }
    block_on(owner.poll(&mut app, &mut store, &mut radio, 2));
    assert!(!io.status().dtr);
    assert_eq!(
        store.writes, 1,
        "The second old-session command must not save after DTR falls"
    );
    assert_eq!(app.manager.preference.host_platform, HostPlatform::Mac);
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
