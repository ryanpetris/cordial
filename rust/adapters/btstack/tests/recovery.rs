#![cfg(feature = "ffi")]
mod common;
use common::{receive, reply, take};
use cordial_btstack::{
    backend::{Backend, State},
    storage::Storage,
    transport::Io,
};
use cordial_core::{
    bluetooth::{Bluetooth, Event},
    devices::Peer,
    link::LinkId,
};
use cordial_protocol::{errors::ErrorCode, identifiers::Transport};
use embassy_futures::block_on;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
#[allow(dead_code)]
mod support {
    use std::vec::Vec;
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../crates/cordial-core/tests/support/mod.rs"
    ));
}
static NOW: AtomicU64 = AtomicU64::new(0);
unsafe extern "C" {
    fn hci_get_state() -> i32;
    #[cfg(feature = "classic")]
    fn gap_store_link_key_for_bd_addr(address: *const u8, key: *const u8, kind: i32);
}
#[derive(Default)]
struct Trace {
    ready: usize,
    restarting: usize,
    failures: usize,
    commands: Vec<(u16, Vec<u8>)>,
}
fn pump(radio: &mut Backend<support::Store>, io: &Io, trace: &mut Trace) {
    for sequence in 0..160 {
        radio.poll();
        while let Some(event) = radio.next_event() {
            match event {
                Event::Ready => trace.ready += 1,
                Event::Disconnected {
                    error: Some(ErrorCode::ConnectionFailed),
                    ..
                } => trace.failures += 1,
                Event::Restarting(ErrorCode::RadioUnavailable) => {
                    trace.restarting += 1;
                    assert_eq!(
                        trace.failures, trace.restarting,
                        "restart hid the connection failure"
                    );
                }
                Event::Failed(error) => panic!("recovery became terminal: {error:?}"),
                _ => {}
            }
        }
        let Some(packet) = take(io) else { break };
        io.finish_outbound(true);
        radio.poll();
        if packet.kind != 1 {
            continue;
        }
        let opcode = u16::from_le_bytes([packet.data()[0], packet.data()[1]]);
        let params = packet.data()[3..].to_vec();
        trace.commands.push((opcode, params.clone()));
        if opcode == 0x0406 {
            let handle = u16::from_le_bytes([params[0], params[1]]);
            receive(io, &[0x0f, 4, if handle == 0x41 { 0 } else { 2 }, 1, 6, 4]);
            if handle == 0x41 {
                radio.poll();
                receive(io, &[0x05, 4, 0, 0x41, 0, 0x16]);
            }
        } else if matches!(
            opcode,
            0x200d | 0x0405 | 0x041b | 0x041c | 0x0419 | 0x0411 | 0x0413 | 0x2013 | 0x2019
        ) {
            receive(io, &[0x0f, 4, 0, 1, opcode as u8, (opcode >> 8) as u8]);
        } else {
            reply(io, opcode, sequence);
        }
    }
}
#[test]
fn failed_le_connection_recovers_through_public_restart_and_preserves_bonds() {
    let io = Box::leak(Box::new(Io::new()));
    let storage = Box::leak(Box::new(Storage::new(support::Store::default())));
    embassy_futures::block_on(cordial_core::identity::Identity::initialize(
        &mut storage.handle(),
        [2; 6],
        || 42,
    ))
    .unwrap();
    let state = Box::leak(Box::new(
        State::new(storage, io, || NOW.load(Relaxed), || std::process::abort()).unwrap(),
    ));
    let mut radio = unsafe { Backend::new(state, None) };
    radio.start(Some([2, 3, 4, 5, 6, 7])).unwrap();
    let mut trace = Trace::default();
    pump(&mut radio, io, &mut trace);
    assert_eq!(trace.ready, 1);
    let roots = |storage: &Storage<support::Store>| {
        [*b"SMER", *b"SMIR"].map(|tag| {
            let mut bytes = [0; 16];
            assert_eq!(
                unsafe {
                    (Storage::<support::Store>::API.get)(
                        storage.context(),
                        u32::from_be_bytes(tag),
                        bytes.as_mut_ptr(),
                        16,
                    )
                },
                16
            );
            bytes
        })
    };
    let original_roots = roots(storage);
    #[cfg(feature = "classic")]
    {
        let saved = Peer {
            address: [9, 8, 7, 6, 5, 4],
            random: false,
            transport: Transport::Classic,
        };
        unsafe {
            gap_store_link_key_for_bd_addr(saved.address.as_ptr(), [0x5a; 16].as_ptr(), 4);
        }
        radio
            .connect(
                LinkId {
                    slot: 1,
                    generation: 1,
                },
                saved,
                false,
            )
            .unwrap();
        pump(&mut radio, io, &mut trace);
        receive(io, &[0x03, 11, 0, 0x41, 0, 4, 5, 6, 7, 8, 9, 1, 0]);
        pump(&mut radio, io, &mut trace);
    }
    let original_bonds = block_on(radio.bonds()).unwrap();
    let peer = Peer {
        address: [1, 2, 3, 4, 5, 6],
        random: false,
        transport: Transport::Ble,
    };
    for (index, status) in [0x3e, 0x08].into_iter().enumerate() {
        let link = LinkId {
            slot: 0,
            generation: index as u64 + 1,
        };
        radio.connect(link, peer, true).unwrap();
        let mark = trace.commands.len();
        pump(&mut radio, io, &mut trace);
        assert!(
            trace.commands[mark..]
                .iter()
                .any(|(opcode, _)| *opcode == 0x200d)
        );
        let mut failed = [0; 21];
        failed[..4].copy_from_slice(&[0x3e, 19, 1, status]);
        failed[8..14].copy_from_slice(&[6, 5, 4, 3, 2, 1]);
        receive(io, &failed);
        pump(&mut radio, io, &mut trace);
        assert_eq!(trace.restarting, index + 1);
        assert_eq!(radio.scan(7, false, true), Err(ErrorCode::RadioUnavailable));
        // The public OFF path times out the native stale, handle-less entry.
        NOW.fetch_add(2000, Relaxed);
        pump(&mut radio, io, &mut trace);
        assert_eq!(trace.ready, index + 2);
        assert_eq!(unsafe { hci_get_state() }, 2);
        assert_eq!(block_on(radio.bonds()).unwrap(), original_bonds);
        assert_eq!(roots(storage), original_roots);
        assert!(storage.error().is_none());
    }
    #[cfg(feature = "classic")]
    assert!(
        trace
            .commands
            .iter()
            .any(|(opcode, params)| *opcode == 0x0406 && params[..2] == [0x41, 0]),
        "other physical links drain before restart"
    );
    radio
        .connect(
            LinkId {
                slot: 0,
                generation: 3,
            },
            peer,
            true,
        )
        .unwrap();
    let mark = trace.commands.len();
    pump(&mut radio, io, &mut trace);
    assert!(
        trace.commands[mark..]
            .iter()
            .any(|(opcode, _)| *opcode == 0x200d)
    );
}
