#![cfg(all(feature = "ffi", feature = "classic"))]
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
fn command(radio: &mut Backend<support::Store>, io: &Io, expected: u16, status: u8) {
    radio.poll();
    let packet = take(io).expect("native controller command");
    assert_eq!(packet.kind, 1);
    assert_eq!(
        u16::from_le_bytes([packet.data()[0], packet.data()[1]]),
        expected
    );
    io.finish_outbound(true);
    radio.poll();
    receive(
        io,
        &[0x0f, 4, status, 1, expected as u8, (expected >> 8) as u8],
    );
    radio.poll();
}
#[test]
fn serialized_initiation_keeps_classic_command_failure_out_of_le_attempts() {
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
    let mut ready = false;
    for sequence in 0..160 {
        radio.poll();
        while let Some(event) = radio.next_event() {
            match event {
                Event::Ready => ready = true,
                _ => panic!("unexpected startup event"),
            }
        }
        let Some(packet) = take(io) else { break };
        let opcode = u16::from_le_bytes([packet.data()[0], packet.data()[1]]);
        io.finish_outbound(true);
        radio.poll();
        reply(io, opcode, sequence);
    }
    assert!(ready);
    let classic = Peer {
        address: [1, 2, 3, 4, 5, 6],
        random: false,
        transport: Transport::Classic,
    };
    let ble = Peer {
        address: [6, 5, 4, 3, 2, 1],
        random: false,
        transport: Transport::Ble,
    };
    let id = LinkId {
        slot: 0,
        generation: 1,
    };
    radio.connect(id, classic, true).unwrap();
    assert_eq!(
        radio.connect(
            LinkId {
                slot: 1,
                generation: 1
            },
            ble,
            false
        ),
        Err(ErrorCode::Busy)
    );
    command(&mut radio, io, 0x0405, 0x0c);
    assert!(matches!(radio.next_event(), Some(Event::Disconnected { link, .. }) if link == id));
    assert!(radio.next_event().is_none());
    let next = LinkId {
        slot: 0,
        generation: 2,
    };
    radio.connect(next, ble, true).unwrap();
    assert_eq!(
        radio.connect(
            LinkId {
                slot: 1,
                generation: 2
            },
            classic,
            false
        ),
        Err(ErrorCode::Busy)
    );
    command(&mut radio, io, 0x200d, 0);
    // User cancellation retains the native non-reset path.
    radio.disconnect(next);
    radio.poll();
    let cancel = take(io).unwrap();
    assert_eq!(&cancel.data()[..2], &[0x0e, 0x20]);
    io.finish_outbound(true);
    radio.poll();
    receive(io, &[0x0e, 4, 1, 0x0e, 0x20, 0]);
    radio.poll();
    let mut complete = [0; 21];
    complete[..4].copy_from_slice(&[0x3e, 19, 1, 2]);
    receive(io, &complete);
    radio.poll();
    assert!(
        matches!(radio.next_event(), Some(Event::Disconnected { link, error: None }) if link == next)
    );
    assert!(radio.next_event().is_none());
    for sequence in 0..32 {
        let Some(packet) = take(io) else { break };
        let opcode = u16::from_le_bytes([packet.data()[0], packet.data()[1]]);
        assert_ne!(
            opcode, 0x0c03,
            "ordinary cancellation must not reset Bluetooth"
        );
        io.finish_outbound(true);
        radio.poll();
        reply(io, opcode, sequence);
        radio.poll();
        assert!(radio.next_event().is_none());
    }
    radio
        .connect(
            LinkId {
                slot: 0,
                generation: 3,
            },
            ble,
            true,
        )
        .unwrap();
    command(&mut radio, io, 0x200d, 0x0c);
    assert!(matches!(
        radio.next_event(),
        Some(Event::Disconnected {
            link: LinkId {
                slot: 0,
                generation: 3
            },
            error: Some(ErrorCode::ConnectionFailed),
        })
    ));
    assert!(radio.next_event().is_none());
    radio
        .connect(
            LinkId {
                slot: 0,
                generation: 4,
            },
            ble,
            true,
        )
        .unwrap();
    // Reconnecting can first refresh the native resolving list.
    let mut retried = false;
    for sequence in 0..40 {
        radio.poll();
        let packet = take(io).expect("retry controller command");
        let opcode = u16::from_le_bytes([packet.data()[0], packet.data()[1]]);
        io.finish_outbound(true);
        radio.poll();
        if opcode == 0x200d {
            receive(io, &[0x0f, 4, 0, 1, 0x0d, 0x20]);
            radio.poll();
            retried = true;
            break;
        }
        reply(io, opcode, sequence);
        radio.poll();
    }
    assert!(retried);
}
