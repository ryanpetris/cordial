#![cfg(all(feature = "ffi", feature = "classic"))]
mod common;
use common::{receive, reply, take};
use cordial_btstack::{
    backend::{Backend, State},
    storage::Storage,
    transport::Io,
};
use cordial_core::bluetooth::{Bluetooth, Event};
use cordial_core::{devices::Peer, link::LinkId};
use cordial_protocol::identifiers::Transport;
use embassy_futures::block_on;
#[allow(dead_code)]
mod support {
    use std::vec::Vec;
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../crates/cordial-core/tests/support/mod.rs"
    ));
}
fn now() -> u64 {
    0
}
fn fatal() -> ! {
    std::process::abort()
}
#[test]
fn cancellation_removes_a_key_persisted_before_authentication_completes() {
    let io = Box::leak(Box::new(Io::new()));
    let storage = Box::leak(Box::new(Storage::new(support::Store::default())));
    embassy_futures::block_on(cordial_core::identity::Identity::initialize(
        &mut storage.handle(),
        [2; 6],
        || 42,
    ))
    .unwrap();
    let state = Box::leak(Box::new(State::new(storage, io, now, fatal).unwrap()));
    let mut radio = unsafe { Backend::new(state, None) };
    radio.start(Some([2, 3, 4, 5, 6, 7])).unwrap();
    let mut ready = false;
    let mut commands = Vec::new();
    for sequence in 0..160 {
        radio.poll();
        while let Some(event) = radio.next_event() {
            match event {
                Event::Ready => ready = true,
                Event::Failed(error) => panic!("startup failed: {error:?}"),
                _ => panic!("unexpected startup event"),
            }
        }
        let Some(packet) = take(io) else {
            break;
        };
        assert_eq!(packet.kind, 1);
        let opcode = u16::from_le_bytes([packet.data()[0], packet.data()[1]]);
        commands.push(opcode);
        assert_eq!(packet.len as usize, usize::from(packet.data()[2]) + 3);
        io.finish_outbound(true);
        radio.poll();
        reply(io, opcode, sequence as u8);
    }
    assert!(
        ready,
        "initialization did not reach Ready; opcodes {commands:04x?}"
    );
    assert!(block_on(radio.bonds()).unwrap().is_empty());
    assert!(storage.take_error().is_none());

    let link = LinkId {
        slot: 0,
        generation: 1,
    };
    let peer = Peer {
        address: [1, 2, 3, 4, 5, 6],
        random: false,
        transport: Transport::Classic,
    };
    radio.connect(link, peer, true).unwrap();
    radio.poll();
    let create = take(io).unwrap();
    eprintln!("Create opcode {:02x?}", &create.data()[..2]);
    io.finish_outbound(true);
    radio.poll();
    receive(io, &[0x0f, 4, 0, 1, 5, 4]);
    radio.poll();
    receive(io, &[0x03, 0x0b, 0, 0x40, 0, 6, 5, 4, 3, 2, 1, 1, 0]);
    radio.poll();
    let mut key = vec![0x18, 0x17, 6, 5, 4, 3, 2, 1];
    key.extend([0x55; 16]);
    key.push(0);
    receive(io, &key);
    radio.poll();
    assert_eq!(
        block_on(radio.bonds()).unwrap(),
        vec![peer],
        "link key stored before authentication complete"
    );
    radio.disconnect(link);
    receive(io, &[0x05, 4, 0, 0x40, 0, 0x16]);
    radio.poll();
    while let Some(event) = radio.next_event() {
        if let Event::Disconnected { link, error } = event {
            eprintln!("Disconnected {link:?}: {error:?}");
        }
    }
    let bonds = block_on(radio.bonds()).unwrap();
    eprintln!("Bonds after pairing cancellation: {bonds:?}");
    assert!(
        bonds.is_empty(),
        "Cancelled Classic pairing must remove its unadopted key"
    );
}
