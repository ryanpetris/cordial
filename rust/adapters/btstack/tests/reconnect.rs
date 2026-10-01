#![cfg(feature = "ffi")]
mod common;
use common::{receive, reply, take};
use cordial_btstack::{
    backend::{Backend, State},
    storage::Storage,
    transport::Io,
};
use cordial_core::bluetooth::{Bluetooth, Event};
use cordial_core::devices::Peer;
use cordial_core::model::identifiers::Transport;
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
fn accept_list_admits_any_advertiser_and_serializes_cancellation() {
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

    let first = Peer {
        address: [1, 2, 3, 4, 5, 6],
        random: false,
        transport: Transport::Ble,
    };
    let second = Peer {
        address: [2, 2, 3, 4, 5, 6],
        ..first
    };
    radio.reconnect(&[first, second]).unwrap();
    let mut added = Vec::new();
    // Pump real BTstack list programming and inspect the controller request.
    for sequence in 0..30 {
        radio.poll();
        let packet = take(io).expect("accept-list controller command");
        let opcode = u16::from_le_bytes([packet.data()[0], packet.data()[1]]);
        if opcode == 0x2011 {
            added.push(packet.data()[4..10].to_vec());
        }
        if opcode == 0x200d {
            assert_eq!(packet.data()[7], 1, "initiator uses accept list");
            io.finish_outbound(true);
            radio.poll();
            // Cancel while Create status is still outstanding. Pairing waits.
            radio.scan(1, false, true).unwrap();
            radio.poll();
            assert!(take(io).is_none());
            receive(io, &[0x0f, 4, 0, 1, 0x0d, 0x20]);
            radio.poll();
            break;
        }
        io.finish_outbound(true);
        radio.poll();
        reply(io, opcode, sequence);
    }
    assert_eq!(added.len(), 2);
    let cancel = loop {
        let packet = take(io).expect("cancel automatic initiation before scan");
        let opcode = u16::from_le_bytes([packet.data()[0], packet.data()[1]]);
        if opcode == 0x200e {
            break packet;
        }
        assert_ne!(opcode, 0x200c, "scanning must wait for cancel completion");
        io.finish_outbound(true);
        radio.poll();
        reply(io, opcode, 0);
        radio.poll();
    };
    assert_eq!(&cancel.data()[..2], &[0x0e, 0x20]);
    io.finish_outbound(true);
    radio.poll();
    reply(io, 0x200e, 0);
    radio.poll();
    let mut ended = [0; 21];
    ended[..4].copy_from_slice(&[0x3e, 19, 1, 2]);
    receive(io, &ended);
    radio.poll();
    let mut scanning = false;
    for sequence in 0..20 {
        let Some(packet) = take(io) else { break };
        let opcode = u16::from_le_bytes([packet.data()[0], packet.data()[1]]);
        if opcode == 0x200c && packet.data()[3] == 1 {
            scanning = true;
        }
        assert_ne!(opcode, 0x200d);
        io.finish_outbound(true);
        radio.poll();
        reply(io, opcode, sequence);
        radio.poll();
    }
    assert!(scanning);
    assert!(
        radio.next_event().is_none(),
        "cancelling auto has no application link to retire"
    );
    radio.scan(1, false, false).unwrap();
    for sequence in 0..30 {
        radio.poll();
        let packet = take(io).expect("resume auto connection");
        let opcode = u16::from_le_bytes([packet.data()[0], packet.data()[1]]);
        io.finish_outbound(true);
        radio.poll();
        if opcode == 0x200d {
            receive(io, &[0x0f, 4, 0, 1, 0x0d, 0x20]);
            radio.poll();
            break;
        }
        reply(io, opcode, sequence);
    }
    // A controller list update cancels/restarts internally, without a GAP
    // completion. Pause after raw completion while its replacement is queued.
    unsafe extern "C" {
        fn gap_whitelist_remove(kind: u8, address: *const u8) -> u8;
    }
    assert_eq!(
        unsafe { gap_whitelist_remove(0, first.address.as_ptr()) },
        0
    );
    radio.poll();
    let packet = take(io).expect("internal accept-list cancellation");
    assert_eq!(&packet.data()[..2], &[0x0e, 0x20]);
    io.finish_outbound(true);
    radio.poll();
    reply(io, 0x200e, 0);
    radio.poll();
    receive(io, &ended);
    radio.poll();
    radio.reconnect(&[second]).unwrap();
    let mut creates = 0;
    for sequence in 0..60 {
        radio.poll();
        let packet = take(io).expect("list update must not strand the automatic initiator");
        let opcode = u16::from_le_bytes([packet.data()[0], packet.data()[1]]);
        io.finish_outbound(true);
        radio.poll();
        if opcode == 0x200d {
            creates += 1;
            receive(io, &[0x0f, 4, 0, 1, 0x0d, 0x20]);
            radio.poll();
            if creates == 2 {
                break;
            }
        } else {
            reply(io, opcode, sequence);
            radio.poll();
            if opcode == 0x200e {
                receive(io, &ended);
                radio.poll();
            }
        }
    }
    assert_eq!(
        creates, 2,
        "replacement Create must finish cancellation before the new list starts"
    );
    // The second peer wins; no application LinkId was reserved for either.
    receive(
        io,
        &[
            0x3e, 19, 1, 0, 0x40, 0, 0, 0, 6, 5, 4, 3, 2, 2, 0x18, 0, 0, 0, 0x48, 0, 0,
        ],
    );
    radio.poll();
    let attempt = match radio.next_event().unwrap() {
        Event::Incoming { attempt, peer } => {
            assert_eq!(peer, second);
            attempt
        }
        _ => panic!("expected saved-peer admission"),
    };
    radio.incoming(attempt, None).unwrap();
    // Rejecting admission disconnects the actual ACL rather than starting SMP.
    let mut disconnected = false;
    for sequence in 0..20 {
        radio.poll();
        let Some(packet) = take(io) else { break };
        let opcode = u16::from_le_bytes([packet.data()[0], packet.data()[1]]);
        if opcode == 0x0406 {
            disconnected = true;
            assert_eq!(&packet.data()[3..5], &[0x40, 0]);
            break;
        }
        io.finish_outbound(true);
        radio.poll();
        reply(io, opcode, sequence);
    }
    assert!(disconnected);
    io.finish_outbound(true);
    radio.poll();
    receive(io, &[0x0f, 4, 0, 1, 0x06, 0x04]);
    radio.poll();
    receive(io, &[0x05, 4, 0, 0x40, 0, 0x16]);
    radio.poll();
    let mut rearmed = false;
    for sequence in 0..30 {
        radio.poll();
        let Some(packet) = take(io) else { break };
        let opcode = u16::from_le_bytes([packet.data()[0], packet.data()[1]]);
        if opcode == 0x200d {
            rearmed = true;
            assert_eq!(packet.data()[7], 1);
            break;
        }
        io.finish_outbound(true);
        radio.poll();
        reply(io, opcode, sequence);
    }
    assert!(
        rearmed,
        "rejection must rearm even when the desired peers have not changed"
    );
    io.finish_outbound(true);
    radio.poll();
    receive(io, &[0x0f, 4, 0, 1, 0x0d, 0x20]);
    radio.poll();
    let replacement: Vec<_> = (10..18)
        .map(|n| Peer {
            address: [n; 6],
            ..first
        })
        .collect();
    radio.reconnect(&replacement).unwrap();
    let mut additions = Vec::new();
    let mut replaced = false;
    for sequence in 0..100 {
        radio.poll();
        assert!(
            radio.next_event().is_none(),
            "list replacement must not fail the radio"
        );
        let packet = take(io).expect("full list replacement makes progress as old entries retire");
        let opcode = u16::from_le_bytes([packet.data()[0], packet.data()[1]]);
        if opcode == 0x2011 {
            additions.push(packet.data()[4..10].to_vec());
        }
        if opcode == 0x200d {
            replaced = true;
            break;
        }
        io.finish_outbound(true);
        radio.poll();
        reply(io, opcode, sequence);
        radio.poll();
        if opcode == 0x200e {
            receive(io, &ended);
            radio.poll();
        }
    }
    assert!(replaced);
    assert_eq!(
        additions.len(),
        8,
        "every desired address must reach the controller"
    );

    // Manual Connect races a successful automatic connection for the same peer.
    let link = cordial_core::link::LinkId {
        slot: 0,
        generation: 1,
    };
    radio.connect(link, replacement[7], true).unwrap();
    io.finish_outbound(true);
    radio.poll();
    receive(io, &[0x0f, 4, 0, 1, 0x0d, 0x20]);
    radio.poll();
    let cancel = take(io).unwrap();
    assert_eq!(&cancel.data()[..2], &[0x0e, 0x20]);
    io.finish_outbound(true);
    radio.poll();
    receive(io, &[0x0e, 4, 1, 0x0e, 0x20, 0x0c]);
    radio.poll();
    receive(
        io,
        &[
            0x3e, 19, 1, 0, 0x41, 0, 0, 0, 17, 17, 17, 17, 17, 17, 0x18, 0, 0, 0, 0x48, 0, 0,
        ],
    );
    radio.poll();
    let attempt = match radio.next_event().unwrap() {
        Event::Incoming { attempt, .. } => attempt,
        _ => panic!("late automatic ACL"),
    };
    radio.incoming(attempt, None).unwrap();
    let mut retired = false;
    for sequence in 0..30 {
        radio.poll();
        let packet = take(io).expect("late automatic ACL teardown");
        assert_eq!(packet.kind, 1);
        let opcode = u16::from_le_bytes([packet.data()[0], packet.data()[1]]);
        assert_ne!(
            opcode, 0x200d,
            "manual initiation must wait for duplicate ACL retirement"
        );
        io.finish_outbound(true);
        radio.poll();
        if opcode == 0x0406 {
            receive(io, &[0x0f, 4, 0, 1, 0x06, 0x04]);
            radio.poll();
            receive(io, &[0x05, 4, 0, 0x41, 0, 0x16]);
            radio.poll();
            retired = true;
            break;
        }
        reply(io, opcode, sequence);
    }
    assert!(retired);
    let mut direct = false;
    for sequence in 0..30 {
        radio.poll();
        let packet = take(io).expect("manual connect resumes");
        let opcode = u16::from_le_bytes([packet.data()[0], packet.data()[1]]);
        if opcode == 0x200d {
            assert_eq!(packet.data()[7], 0);
            assert_eq!(&packet.data()[9..15], &[17; 6]);
            direct = true;
            break;
        }
        io.finish_outbound(true);
        radio.poll();
        reply(io, opcode, sequence);
    }
    assert!(direct);
}
