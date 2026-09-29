#![cfg(feature = "ffi")]
mod common;
use common::{receive, reply, take};
use cordial_btstack::{
    backend::{Backend, State},
    storage::Storage,
    transport::Io,
};
use cordial_core::bluetooth::{Bluetooth, Event};
use cordial_core::{devices::Peer, link::LinkId};
use cordial_protocol::{errors::ErrorCode, identifiers::Transport};
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
fn selected_stack_initializes_through_public_profiles_and_record_callbacks() {
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
        transport: Transport::Ble,
    };
    radio.connect(link, peer, true).unwrap();
    assert_eq!(
        radio.connect(
            LinkId {
                slot: 1,
                generation: 2
            },
            Peer {
                address: [2; 6],
                ..peer
            },
            true
        ),
        Err(ErrorCode::Busy)
    );
    let create = take(io).expect("LE create connection command");
    assert_eq!(&create.data()[..2], &[0x0d, 0x20]);
    // Cancelling while the controller owns CREATE must retain the generation
    // until native cancel completion, then allow the slot to be used again.
    radio.disconnect(link);
    io.finish_outbound(true);
    radio.poll();
    receive(io, &[0x0f, 4, 0, 1, 0x0d, 0x20]);
    radio.poll();
    let cancel = take(io).expect("LE cancel connection command");
    assert_eq!(&cancel.data()[..2], &[0x0e, 0x20]);
    io.finish_outbound(true);
    radio.poll();
    receive(io, &[0x0e, 4, 1, 0x0e, 0x20, 0]);
    radio.poll();
    let mut cancelled = [0; 21];
    cancelled[..4].copy_from_slice(&[0x3e, 19, 1, 2]);
    receive(io, &cancelled);
    radio.poll();
    assert!(
        matches!(radio.next_event(), Some(Event::Disconnected { link: ended, error: None }) if ended == link)
    );
    assert!(block_on(radio.bonds()).unwrap().is_empty());
    radio
        .connect(
            LinkId {
                generation: 2,
                ..link
            },
            peer,
            true,
        )
        .unwrap();
}
