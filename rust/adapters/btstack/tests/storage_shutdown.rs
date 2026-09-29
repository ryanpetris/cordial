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
static NOW: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
fn now() -> u64 {
    NOW.load(std::sync::atomic::Ordering::Relaxed)
}
unsafe extern "C" {
    fn hci_get_state() -> i32;
}
fn fatal() -> ! {
    std::process::abort()
}
#[test]
fn storage_fault_disconnects_active_native_links_and_stays_unavailable() {
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
    assert_eq!(&create.data()[..2], &[5, 4]);
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
    let api = Storage::<support::Store>::API;
    assert_eq!(
        unsafe { (api.store)(storage.context(), 0x1234, std::ptr::null(), 513) },
        -1
    );
    let mut failures = 0;
    let mut disconnects = 0;
    for iteration in 0..80 {
        NOW.store(iteration * 100, std::sync::atomic::Ordering::Relaxed);
        radio.poll();
        while let Some(event) = radio.next_event() {
            match event {
                Event::Failed(ErrorCode::StorageFailed) => failures += 1,
                _ => panic!("application callback must be suppressed after storage failure"),
            }
        }
        if let Some(packet) = take(io) {
            io.finish_outbound(true);
            radio.poll();
            if packet.kind == 1 {
                let opcode = u16::from_le_bytes([packet.data()[0], packet.data()[1]]);

                if opcode == 0x0406 {
                    disconnects += 1;
                    receive(io, &[0x0f, 4, 0, 1, 0x06, 0x04]);
                    radio.poll();
                    receive(io, &[0x05, 4, 0, 0x40, 0, 0x16]);
                } else {
                    receive(io, &[0x0e, 4, 1, opcode as u8, (opcode >> 8) as u8, 0]);
                }
            }
        }
    }
    assert_eq!(failures, 1);
    assert_eq!(
        disconnects, 1,
        "shutdown must issue a native ACL disconnect"
    );
    assert_eq!(
        unsafe { hci_get_state() },
        0,
        "native HCI host must reach OFF"
    );
    assert_eq!(radio.scan(2, true, true), Err(ErrorCode::StorageFailed));
    assert!(storage.error().is_some());
}
