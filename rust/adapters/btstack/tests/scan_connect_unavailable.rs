#![cfg(feature = "ffi")]
mod common;
use common::{receive, reply, take};
use cordial_btstack::{
    backend::{Backend, State},
    storage::Storage,
    transport::Io,
};
use cordial_core::bluetooth::{Bluetooth, Event};
#[allow(dead_code)]
mod support {
    use std::vec::Vec;
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../crates/cordial-core/tests/support/mod.rs"
    ));
}

#[test]
fn unsupported_controller_query_reports_exclusive_before_ready() {
    // Separate processes keep BTstack global state isolated for both HCI error forms.
    if std::env::var_os("CORDIAL_QUERY_STATUS").is_none() {
        assert!(
            std::process::Command::new(std::env::current_exe().unwrap())
                .env("CORDIAL_QUERY_STATUS", "1")
                .status()
                .unwrap()
                .success()
        );
    }
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
            // Unsupported command must still finish startup with exclusive scheduling.
            if std::env::var_os("CORDIAL_QUERY_STATUS").is_some() {
                receive(io, &[0x0f, 4, 1, 1, 0x1c, 0x20]);
            } else {
                receive(io, &[0x0e, 4, 1, 0x1c, 0x20, 1]);
            }
        } else {
            reply(io, opcode, sequence as u8);
        }
    }
    assert!(queried && ready && !radio.capabilities().ble_scan_and_connect);
}
