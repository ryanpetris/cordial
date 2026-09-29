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
fn application(input: &mut [u8]) -> Application<'_> {
    Application::new(
        input,
        Build {
            profile: cordial_protocol::messages::BuildProfile::Development,
            version: "test",
            hardware: "test",
            default_adapter_name: "Test adapter",
            radio_backend: "pico-sdk-cyw43",
            digest: "test",
            adapter_id: "adapter".into(),
            boot_id: "boot".into(),
            bootloader: None,
        },
    )
}
#[test]
fn serial_waits_for_completion_and_rejects_old_session_packets() {
    let io = Io::new();
    let mut owner = owner::Owner::new(&io);
    let mut input = [0; 4095];
    let mut app = application(&mut input);
    let mut store = support::Store::default();
    let mut radio = support::Radio::default();
    BusHandler(&io).configured(true);
    io.dtr(true);
    block_on(owner.poll(&mut app, &mut store, &mut radio, 1));
    assert!(app.serial.active());
    let separator = io.serial_tx.try_receive().ok().unwrap();
    assert_eq!(&separator.bytes[..separator.length], b"\n");
    app.serial
        .response(1.try_into().unwrap(), true, true, 2)
        .unwrap();
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
    app.serial
        .response(1.try_into().unwrap(), true, false, 5)
        .unwrap();
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
    let mut input = [0; 4095];
    let mut app = application(&mut input);
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
    use cordial_core::storage::{Error, RecordKey, RecordStore};
    use cordial_protocol::identifiers::HostPlatform;
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
    let mut input = [0; 4095];
    let mut app = application(&mut input);
    app.manager.storage_ready = true;
    let mut store = ClosingStore { io: &io, writes: 0 };
    let mut radio = support::Radio::default();
    BusHandler(&io).configured(true);
    io.dtr(true);
    block_on(owner.poll(&mut app, &mut store, &mut radio, 1));
    let prefix = br#"{"v":1,"id":1,"cmd":"adapter.platform.set","args":{"platform":"mac"}}"#;
    assert_eq!(app.serial.feed(prefix, 2).0, prefix.len());
    let tail = b"\n{\"v\":1,\"id\":2,\"cmd\":\"adapter.platform.set\",\"args\":{\"platform\":\"windows\"}}\n";
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
