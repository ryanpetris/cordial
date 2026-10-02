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
unsafe extern "C" {
    fn gap_store_link_key_for_bd_addr(address: *const u8, key: *const u8, kind: i32);
}
const CLASSIC: Peer = Peer {
    address: [0x11, 0x22, 0x33, 0x44, 0x55, 0x66],
    random: false,
    transport: Transport::Classic,
};
/// Answers ordinary commands and returns the scan-enable values written.
fn pump(radio: &mut Backend<support::Store>, io: &Io) -> Vec<u8> {
    let mut written = Vec::new();
    for sequence in 0..160 {
        radio.poll();
        let Some(packet) = take(io) else { break };
        let opcode = u16::from_le_bytes([packet.data()[0], packet.data()[1]]);
        assert_ne!(opcode, 0x0405, "no page while Classic is off");
        assert_ne!(opcode, 0x0401, "no inquiry while Classic is off");
        if opcode == 0x0c1a {
            written.push(packet.data()[3]);
        }
        io.finish_outbound(true);
        radio.poll();
        reply(io, opcode, sequence);
    }
    written
}
#[test]
fn classic_starts_off_and_follows_the_setting() {
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
    let written = pump(&mut radio, io);
    assert!(
        written.iter().all(|value| value & 3 == 0),
        "neither connectable nor discoverable after start: {written:?}"
    );
    assert!(matches!(radio.next_event(), Some(Event::Ready)));
    assert!(radio.capabilities().classic, "support is not masked");
    unsafe { gap_store_link_key_for_bd_addr(CLASSIC.address.as_ptr(), [0x5a; 16].as_ptr(), 4) };
    let link = |generation| LinkId {
        slot: 0,
        generation,
    };
    // Off: Classic is refused like an unsupported transport.
    assert_eq!(
        radio.connect(link(1), CLASSIC, false, None),
        Err(ErrorCode::UnsupportedTransport)
    );
    assert_eq!(
        radio.scan(1, true, false),
        Err(ErrorCode::UnsupportedTransport)
    );
    assert!(pump(&mut radio, io).is_empty());
    // On: connectable, and pages start.
    common::enable_classic(&mut radio, io);
    radio.connect(link(2), CLASSIC, false, None).unwrap();
    page(&mut radio, io);
    // Off during the page: not connectable at once, and the controller's page
    // is cancelled.
    radio.set_transport(Transport::Classic, false).unwrap();
    let mut written = Vec::new();
    let mut cancelled = false;
    for sequence in 0..40 {
        radio.poll();
        let Some(packet) = take(io) else { break };
        let opcode = u16::from_le_bytes([packet.data()[0], packet.data()[1]]);
        io.finish_outbound(true);
        radio.poll();
        if opcode == 0x0c1a {
            written.push(packet.data()[3]);
        }
        if opcode == 0x0408 {
            let mut address = CLASSIC.address;
            address.reverse();
            assert_eq!(&packet.data()[3..9], &address);
            let mut complete = vec![0x0e, 10, 1, 0x08, 0x04, 0];
            complete.extend(address);
            receive(io, &complete);
            radio.poll();
            let mut ended = vec![0x03, 11, 0x02, 0, 0];
            ended.extend(address);
            ended.extend([1, 0]);
            receive(io, &ended);
            cancelled = true;
            continue;
        }
        reply(io, opcode, sequence);
    }
    assert!(cancelled);
    assert_eq!(written, [0]);
    pump(&mut radio, io);
    assert!(matches!(
        radio.next_event(),
        Some(Event::Disconnected { link: ended, .. }) if ended == link(2)
    ));
    assert_eq!(
        radio.connect(link(3), CLASSIC, false, None),
        Err(ErrorCode::UnsupportedTransport)
    );
    assert!(pump(&mut radio, io).is_empty());
    // A page that succeeds after Classic is turned off is disconnected.
    common::enable_classic(&mut radio, io);
    radio.connect(link(4), CLASSIC, false, None).unwrap();
    page(&mut radio, io);
    radio.set_transport(Transport::Classic, false).unwrap();
    assert_eq!(pump(&mut radio, io), [0]);
    let mut connected = vec![0x03, 11, 0, 0x41, 0];
    connected.extend(CLASSIC.address.iter().rev());
    connected.extend([1, 0]);
    receive(io, &connected);
    let mut disconnected = false;
    for sequence in 0..40 {
        radio.poll();
        let Some(packet) = take(io) else { break };
        let opcode = u16::from_le_bytes([packet.data()[0], packet.data()[1]]);
        io.finish_outbound(true);
        radio.poll();
        if opcode == 0x0406 {
            assert_eq!(&packet.data()[3..5], &[0x41, 0]);
            receive(io, &[0x0f, 4, 0, 1, 0x06, 0x04]);
            radio.poll();
            receive(io, &[0x05, 4, 0, 0x41, 0, 0x16]);
            disconnected = true;
            break;
        }
        reply(io, opcode, sequence);
    }
    assert!(disconnected);
    pump(&mut radio, io);
    let mut ended = false;
    while let Some(event) = radio.next_event() {
        assert!(!matches!(event, Event::Connected { .. }));
        ended |= matches!(event, Event::Disconnected { link: l, .. } if l == link(4));
    }
    assert!(ended);
}
fn page(radio: &mut Backend<support::Store>, io: &Io) {
    for sequence in 0..40 {
        radio.poll();
        let packet = take(io).expect("page");
        let opcode = u16::from_le_bytes([packet.data()[0], packet.data()[1]]);
        io.finish_outbound(true);
        radio.poll();
        if opcode == 0x0405 {
            receive(io, &[0x0f, 4, 0, 1, 5, 4]);
            radio.poll();
            return;
        }
        reply(io, opcode, sequence);
    }
    panic!("no page");
}
