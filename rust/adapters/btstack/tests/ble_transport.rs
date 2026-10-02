#![cfg(feature = "ffi")]
mod common;
use common::{receive, reply, take};
use cordial_btstack::{
    backend::{Backend, State},
    storage::Storage,
    transport::Io,
};
use cordial_core::model::{errors::ErrorCode, identifiers::Transport};
use cordial_core::{
    bluetooth::{Bluetooth, Event},
    devices::Peer,
    link::LinkId,
};
#[allow(dead_code)]
mod support {
    use std::vec::Vec;
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../crates/cordial-core/tests/support/mod.rs"
    ));
}
type Radio = Backend<support::Store>;
const BLE: Peer = Peer {
    address: [6, 5, 4, 3, 2, 1],
    random: false,
    transport: Transport::Ble,
};
const CANCELLED: [u8; 21] = {
    let mut event = [0; 21];
    event[0] = 0x3e;
    event[1] = 19;
    event[2] = 1;
    event[3] = 2;
    event
};
const CONNECTED: [u8; 21] = [
    0x3e, 19, 1, 0, 0x40, 0, 0, 0, 6, 5, 4, 3, 2, 1, 0x18, 0, 0, 0, 0x48, 0, 0,
];
/// Answers ordinary commands until `opcode` is sent.
fn until(radio: &mut Radio, io: &Io, opcode: u16) {
    for sequence in 0..80 {
        radio.poll();
        let packet = take(io).unwrap_or_else(|| panic!("controller never received {opcode:04x}"));
        let sent = u16::from_le_bytes([packet.data()[0], packet.data()[1]]);
        io.finish_outbound(true);
        radio.poll();
        if sent == opcode {
            return;
        }
        reply(io, sent, sequence);
        radio.poll();
    }
    panic!("controller never received {opcode:04x}");
}
/// Answers ordinary commands; LE initiation and scanning must not start.
fn settle(radio: &mut Radio, io: &Io) {
    for sequence in 0..40 {
        radio.poll();
        let Some(packet) = take(io) else { return };
        let sent = u16::from_le_bytes([packet.data()[0], packet.data()[1]]);
        assert_ne!(sent, 0x200d, "LE initiation while BLE is disabled");
        if sent == 0x200c {
            assert_eq!(packet.data()[3], 0, "LE scanning while BLE is disabled");
        }
        io.finish_outbound(true);
        radio.poll();
        reply(io, sent, sequence);
    }
}
fn status(radio: &mut Radio, io: &Io, opcode: u16) {
    receive(io, &[0x0f, 4, 0, 1, opcode as u8, (opcode >> 8) as u8]);
    radio.poll();
}
fn cancel_completes(radio: &mut Radio, io: &Io) {
    until(radio, io, 0x200e);
    receive(io, &[0x0e, 4, 1, 0x0e, 0x20, 0]);
    radio.poll();
    receive(io, &CANCELLED);
    radio.poll();
}
#[test]
fn disabled_ble_neither_scans_nor_initiates_nor_admits() {
    let io = Box::leak(Box::new(Io::new()));
    let storage = Box::leak(Box::new(Storage::new(support::Store::default())));
    embassy_futures::block_on(cordial_core::identity::Identity::initialize(
        &mut storage.handle(),
        [2; 6],
        || 42,
    ))
    .unwrap();
    let state = Box::leak(Box::new(
        State::new(storage, io, || 0, || std::process::abort()).unwrap(),
    ));
    let mut radio = unsafe { Backend::new(state, None) };
    radio.start(Some([2, 3, 4, 5, 6, 7])).unwrap();
    settle(&mut radio, io);
    assert!(matches!(radio.next_event(), Some(Event::Ready)));
    // BLE starts enabled: accept-list initiation runs.
    radio.reconnect(&[BLE]).unwrap();
    until(&mut radio, io, 0x200d);
    status(&mut radio, io, 0x200d);
    // Disabled: accept-list initiation is cancelled and stays off.
    radio.set_transport(Transport::Ble, false).unwrap();
    cancel_completes(&mut radio, io);
    settle(&mut radio, io);
    assert!(radio.next_event().is_none());
    assert_eq!(
        radio.scan(1, false, true),
        Err(ErrorCode::UnsupportedTransport)
    );
    let link = |generation| LinkId {
        slot: 0,
        generation,
    };
    assert_eq!(
        radio.connect(link(1), BLE, true, None),
        Err(ErrorCode::UnsupportedTransport)
    );
    radio.reconnect(&[BLE]).unwrap();
    settle(&mut radio, io);
    // Enabled again: the kept list resumes.
    radio.set_transport(Transport::Ble, true).unwrap();
    until(&mut radio, io, 0x200d);
    status(&mut radio, io, 0x200d);
    // A connection that completes as BLE is disabled is dropped, not offered.
    radio.set_transport(Transport::Ble, false).unwrap();
    until(&mut radio, io, 0x200e);
    receive(io, &CONNECTED);
    radio.poll();
    receive(io, &[0x0e, 4, 1, 0x0e, 0x20, 0x0c]);
    until(&mut radio, io, 0x0406);
    receive(io, &[0x0f, 4, 0, 1, 0x06, 0x04]);
    radio.poll();
    receive(io, &[0x05, 4, 0, 0x40, 0, 0x16]);
    settle(&mut radio, io);
    assert!(radio.next_event().is_none(), "nothing is offered");
    // An explicit LE connection in progress ends when BLE is disabled.
    radio.set_transport(Transport::Ble, true).unwrap();
    radio.reconnect(&[]).unwrap();
    settle(&mut radio, io);
    radio.connect(link(2), BLE, true, None).unwrap();
    until(&mut radio, io, 0x200d);
    status(&mut radio, io, 0x200d);
    radio.set_transport(Transport::Ble, false).unwrap();
    cancel_completes(&mut radio, io);
    assert!(matches!(
        radio.next_event(),
        Some(Event::Disconnected { link: ended, .. }) if ended == link(2)
    ));
    settle(&mut radio, io);
}
