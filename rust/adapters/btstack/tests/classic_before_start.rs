#![cfg(all(feature = "ffi", feature = "classic"))]
mod common;
use common::{reply, take};
use cordial_btstack::{
    backend::{Backend, State},
    storage::Storage,
    transport::Io,
};
use cordial_core::{bluetooth::Bluetooth, model::identifiers::Transport};
#[allow(dead_code)]
mod support {
    use std::vec::Vec;
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../crates/cordial-core/tests/support/mod.rs"
    ));
}
#[test]
fn classic_turned_on_before_start_is_connectable_at_first_power_on() {
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
    radio.set_transport(Transport::Classic, true).unwrap();
    radio.start(Some([2, 3, 4, 5, 6, 7])).unwrap();
    let mut written = Vec::new();
    for sequence in 0..160 {
        radio.poll();
        while radio.next_event().is_some() {}
        let Some(packet) = take(io) else { break };
        let opcode = u16::from_le_bytes([packet.data()[0], packet.data()[1]]);
        if opcode == 0x0c1a {
            written.push(packet.data()[3]);
        }
        io.finish_outbound(true);
        radio.poll();
        reply(io, opcode, sequence);
    }
    assert_eq!(written.last(), Some(&2), "{written:?}");
}
