#![cfg(feature = "ffi")]
mod common;
use common::{receive, reply, take};
use cordial_btstack::{
    backend::{Backend, State},
    storage::Storage,
    transport::Io,
};
use cordial_core::model::identifiers::Transport;
use cordial_core::{
    bluetooth::{Bluetooth, Event},
    devices::Peer,
};
#[allow(dead_code)]
mod support {
    use std::vec::Vec;
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../crates/cordial-core/tests/support/mod.rs"
    ));
}

#[test]
fn controller_support_allows_discovery_and_accept_list_initiation_together() {
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
    assert!(!radio.capabilities().ble_scan_and_connect);
    radio.start(Some([2, 3, 4, 5, 6, 7])).unwrap();
    let mut ready = false;
    let mut queried = false;
    for sequence in 0..200 {
        radio.poll();
        while let Some(event) = radio.next_event() {
            assert!(matches!(event, Event::Ready));
            ready = true;
        }
        let Some(packet) = take(io) else { break };
        assert_eq!(packet.kind, 1);
        let opcode = u16::from_le_bytes([packet.data()[0], packet.data()[1]]);
        io.finish_outbound(true);
        radio.poll();
        if opcode == 0x201c {
            assert_eq!(packet.data(), &[0x1c, 0x20, 0], "query has no parameters");
            assert!(!ready && !radio.capabilities().ble_scan_and_connect);
            queried = true;
            // LE supported states bit 23: active scanning and initiating.
            receive(io, &[0x0e, 12, 1, 0x1c, 0x20, 0, 0, 0, 0x80, 0, 0, 0, 0, 0]);
        } else {
            reply(io, opcode, sequence as u8);
        }
    }
    assert!(queried && ready && radio.capabilities().ble_scan_and_connect);
    let peer = Peer {
        address: [1, 2, 3, 4, 5, 6],
        random: false,
        transport: Transport::Ble,
    };
    radio.scan(1, false, true).unwrap();
    radio.reconnect(&[peer]).unwrap();
    let (mut scanning, mut connecting) = (false, false);
    for sequence in 0..40 {
        radio.poll();
        let Some(packet) = take(io) else { break };
        let opcode = u16::from_le_bytes([packet.data()[0], packet.data()[1]]);
        assert_ne!(
            opcode, 0x200e,
            "discovery must not cancel a concurrent initiator"
        );
        if opcode == 0x200c {
            scanning = packet.data()[3] == 1;
        }
        io.finish_outbound(true);
        radio.poll();
        if opcode == 0x200d {
            assert_eq!(packet.data()[7], 1);
            connecting = true;
            receive(io, &[0x0f, 4, 0, 1, 0x0d, 0x20]);
        } else {
            reply(io, opcode, sequence);
        }
    }
    assert!(scanning && connecting);
    // Turning discovery off and on must leave the established initiator alone.
    for enabled in [false, true] {
        radio.scan(2, false, enabled).unwrap();
        for sequence in 0..20 {
            radio.poll();
            let Some(packet) = take(io) else { break };
            let opcode = u16::from_le_bytes([packet.data()[0], packet.data()[1]]);
            assert_ne!(opcode, 0x200e);
            assert_ne!(opcode, 0x200d);
            if opcode == 0x200c {
                scanning = packet.data()[3] == 1;
            }
            io.finish_outbound(true);
            radio.poll();
            reply(io, opcode, sequence);
        }
        assert_eq!(scanning, enabled);
    }
}
