use super::{receive, reply, take};
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
    fn gap_store_link_key_for_bd_addr(address: *const u8, key: *const u8, kind: i32);
}
const CLASSIC: [u8; 6] = [0x11, 0x22, 0x33, 0x44, 0x55, 0x66];
fn pump(
    radio: &mut Backend<support::Store>,
    io: &Io,
    commands: &mut Vec<(u16, Vec<u8>)>,
    failures: &mut usize,
) {
    for sequence in 0..160 {
        radio.poll();
        while let Some(event) = radio.next_event() {
            if let Event::Failed(error) = event {
                assert_eq!(error, ErrorCode::StorageFailed);
                *failures += 1;
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
        commands.push((opcode, params.clone()));
        if opcode == 0x0406 {
            assert_eq!(
                &params[..2],
                &[0x40, 0],
                "never disconnect an invalid handle"
            );
            receive(io, &[0x0f, 4, 0, 1, 0x06, 0x04]);
            radio.poll();
            receive(io, &[0x05, 4, 0, 0x40, 0, 0x16]);
        } else if opcode == 0x200e {
            // Create wins the cancellation race. The late physical connection
            // must still be disconnected while ordinary callbacks are blocked.
            receive(io, &[0x0e, 4, 1, 0x0e, 0x20, 0x0c]);
            radio.poll();
            le_connected(io);
        } else if matches!(
            opcode,
            0x200d | 0x0405 | 0x041b | 0x041c | 0x0419 | 0x0411 | 0x0413 | 0x2013 | 0x2019 | 0x0408
        ) {
            receive(io, &[0x0f, 4, 0, 1, opcode as u8, (opcode >> 8) as u8]);
        } else {
            reply(io, opcode, sequence);
        }
    }
}
fn le_connected(io: &Io) {
    receive(
        io,
        &[
            0x3e, 19, 1, 0, 0x40, 0, 0, 0, 6, 5, 4, 3, 2, 1, 0x18, 0, 0, 0, 0x48, 0, 0,
        ],
    );
}
pub fn pending_create(classic_page: bool) {
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
    let mut commands = Vec::new();
    let mut failures = 0;
    pump(&mut radio, io, &mut commands, &mut failures);
    assert_eq!(unsafe { hci_get_state() }, 2);
    if classic_page {
        radio.set_transport(Transport::Classic, true).unwrap();
        unsafe {
            gap_store_link_key_for_bd_addr(CLASSIC.as_ptr(), [0x5a; 16].as_ptr(), 4);
        }
    }
    radio
        .connect(
            LinkId {
                slot: 0,
                generation: 1,
            },
            Peer {
                address: [1, 2, 3, 4, 5, 6],
                random: false,
                transport: Transport::Ble,
            },
            true,
            None,
        )
        .unwrap();
    pump(&mut radio, io, &mut commands, &mut failures);
    assert!(commands.iter().any(|(opcode, _)| *opcode == 0x200d));
    if classic_page {
        le_connected(io);
        pump(&mut radio, io, &mut commands, &mut failures);
        radio
            .connect(
                LinkId {
                    slot: 1,
                    generation: 2,
                },
                Peer {
                    address: CLASSIC,
                    random: false,
                    transport: Transport::Classic,
                },
                false,
                None,
            )
            .unwrap();
        pump(&mut radio, io, &mut commands, &mut failures);
        assert!(commands.iter().any(|(opcode, _)| *opcode == 0x0405));
    }
    let mark = commands.len();
    assert_eq!(
        unsafe {
            (Storage::<support::Store>::API.store)(storage.context(), 0x1234, std::ptr::null(), 513)
        },
        -1
    );
    pump(&mut radio, io, &mut commands, &mut failures);
    assert!(
        commands[mark..].iter().any(|(opcode, _)| *opcode == 0x0406),
        "active/late link must disconnect"
    );
    if classic_page {
        NOW.store(3000, Relaxed);
        pump(&mut radio, io, &mut commands, &mut failures);
        assert_eq!(
            unsafe { hci_get_state() },
            2,
            "wait for the pending Classic page before halting"
        );
        NOW.store(5200, Relaxed);
        receive(
            io,
            &[
                0x03, 11, 0x04, 0, 0, 0x66, 0x55, 0x44, 0x33, 0x22, 0x11, 1, 0,
            ],
        );
        pump(&mut radio, io, &mut commands, &mut failures);
    } else {
        assert!(commands[mark..].iter().any(|(opcode, _)| *opcode == 0x200e));
    }
    NOW.store(10000, Relaxed);
    pump(&mut radio, io, &mut commands, &mut failures);
    assert_eq!(unsafe { hci_get_state() }, 0);
    assert_eq!(failures, 1);
    assert_eq!(radio.scan(2, false, true), Err(ErrorCode::StorageFailed));
}
