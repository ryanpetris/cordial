//! Connection setup across transports against the pinned host with a
//! scripted controller. Each scenario runs in its own process because a
//! controller refusal changes the session's initiation policy.
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
#[allow(dead_code)]
mod support {
    use std::vec::Vec;
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../crates/cordial-core/tests/support/mod.rs"
    ));
}
type Radio = Backend<support::Store>;
unsafe extern "C" {
    fn gap_store_link_key_for_bd_addr(address: *const u8, key: *const u8, kind: i32);
}
const COMMAND_DISALLOWED: u8 = 0x0c;
const PAGE_TIMEOUT: u8 = 0x04;
const LIMITED_RESOURCES: u8 = 0x0d;
pub const CLASSIC: Peer = Peer {
    address: [0x11, 0x22, 0x33, 0x44, 0x55, 0x66],
    random: false,
    transport: Transport::Classic,
};
const OTHER_CLASSIC: Peer = Peer {
    address: [0x11, 0x22, 0x33, 0x44, 0x55, 0x67],
    ..CLASSIC
};
pub const BLE: Peer = Peer {
    address: [6, 5, 4, 3, 2, 1],
    random: false,
    transport: Transport::Ble,
};
const OTHER_BLE: Peer = Peer {
    address: [7, 5, 4, 3, 2, 1],
    ..BLE
};
fn link(slot: u8) -> LinkId {
    LinkId {
        slot,
        generation: u64::from(slot) + 1,
    }
}
fn start() -> (Radio, &'static Io) {
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
            assert!(matches!(event, Event::Ready), "unexpected startup event");
            ready = true;
        }
        let Some(packet) = take(io) else { break };
        let opcode = u16::from_le_bytes([packet.data()[0], packet.data()[1]]);
        io.finish_outbound(true);
        radio.poll();
        reply(io, opcode, sequence);
    }
    assert!(ready);
    radio.set_transport(Transport::Classic, true).unwrap();
    // Saved Classic devices are paged without pairing.
    for peer in [CLASSIC, OTHER_CLASSIC] {
        unsafe { gap_store_link_key_for_bd_addr(peer.address.as_ptr(), [0x5a; 16].as_ptr(), 4) };
    }
    (radio, io)
}
/// Answers ordinary commands until `opcode` is sent and returns its parameters.
/// A connection command other than the expected one, or a cancellation, fails.
fn until(radio: &mut Radio, io: &Io, opcode: u16) -> Vec<u8> {
    for sequence in 0..80 {
        radio.poll();
        let packet = take(io).unwrap_or_else(|| panic!("controller never received {opcode:04x}"));
        let sent = u16::from_le_bytes([packet.data()[0], packet.data()[1]]);
        let parameters = packet.data()[3..].to_vec();
        io.finish_outbound(true);
        radio.poll();
        if sent == opcode {
            return parameters;
        }
        assert!(
            !matches!(sent, 0x0405 | 0x200d | 0x200e),
            "{sent:04x} sent while waiting for {opcode:04x}"
        );
        reply(io, sent, sequence);
        radio.poll();
    }
    panic!("controller never received {opcode:04x}");
}
/// Answers ordinary commands; a connection command must not be sent.
fn settle(radio: &mut Radio, io: &Io) {
    for sequence in 0..40 {
        radio.poll();
        let Some(packet) = take(io) else { return };
        let sent = u16::from_le_bytes([packet.data()[0], packet.data()[1]]);
        assert!(
            !matches!(sent, 0x0405 | 0x200d),
            "{sent:04x} sent before the other initiator finished"
        );
        io.finish_outbound(true);
        radio.poll();
        reply(io, sent, sequence);
    }
}
fn status(radio: &mut Radio, io: &Io, opcode: u16, status: u8) {
    receive(io, &[0x0f, 4, status, 1, opcode as u8, (opcode >> 8) as u8]);
    radio.poll();
}
fn page_timeout(radio: &mut Radio, io: &Io, expected: LinkId) {
    page_failure(radio, io, expected, PAGE_TIMEOUT);
}
fn page_failure(radio: &mut Radio, io: &Io, expected: LinkId, status: u8) {
    let mut complete = vec![0x03, 11, status, 0, 0];
    complete.extend(CLASSIC.address.iter().rev());
    complete.extend([1, 0]);
    receive(io, &complete);
    radio.poll();
    assert!(matches!(
        radio.next_event(),
        Some(Event::Disconnected { link: ended, error: Some(ErrorCode::ConnectionFailed) }) if ended == expected
    ));
}
fn quiet(radio: &mut Radio) {
    radio.poll();
    let event = radio.next_event();
    assert!(event.is_none(), "unexpected {:?}", event.map(|_| ()));
}

/// A page starts while an LE connection is being created. When the controller
/// refuses it, the LE attempt is cancelled, the page runs first, and setup is
/// serialized from then on.
pub fn page_during_le_creation() {
    let (mut radio, io) = start();
    radio.connect(link(0), BLE, true, None).unwrap();
    until(&mut radio, io, 0x200d);
    status(&mut radio, io, 0x200d, 0);
    radio.connect(link(1), CLASSIC, false, None).unwrap();
    // One explicit connection per transport.
    assert_eq!(
        radio.connect(link(2), OTHER_BLE, false, None),
        Err(ErrorCode::Busy)
    );
    assert_eq!(
        radio.connect(link(2), OTHER_CLASSIC, false, None),
        Err(ErrorCode::Busy)
    );
    until(&mut radio, io, 0x0405);
    status(&mut radio, io, 0x0405, COMMAND_DISALLOWED);
    quiet(&mut radio);
    // The host forgot the LE initiation; it is cancelled directly and kept.
    until(&mut radio, io, 0x200e);
    receive(io, &[0x0e, 4, 1, 0x0e, 0x20, 0]);
    radio.poll();
    let mut cancelled = [0; 21];
    cancelled[..4].copy_from_slice(&[0x3e, 19, 1, 2]);
    receive(io, &cancelled);
    quiet(&mut radio);
    until(&mut radio, io, 0x0405);
    status(&mut radio, io, 0x0405, 0);
    // Serialized: the other transport waits for the page.
    assert_eq!(
        radio.connect(link(2), OTHER_BLE, false, None),
        Err(ErrorCode::Busy)
    );
    settle(&mut radio, io);
    page_timeout(&mut radio, io, link(1));
    until(&mut radio, io, 0x200d);
    status(&mut radio, io, 0x200d, 0);
    quiet(&mut radio);
    // Cancelling the requeued attempt still ends it.
    radio.disconnect(link(0));
    until(&mut radio, io, 0x200e);
    receive(io, &[0x0e, 4, 1, 0x0e, 0x20, 0]);
    radio.poll();
    receive(io, &cancelled);
    radio.poll();
    assert!(matches!(
        radio.next_event(),
        Some(Event::Disconnected { link: ended, error: None }) if ended == link(0)
    ));
}
/// An LE connection is created during a page. When the controller refuses
/// it, the LE attempt waits for the page instead of failing.
pub fn le_creation_during_page() {
    let (mut radio, io) = start();
    radio.connect(link(0), CLASSIC, false, None).unwrap();
    until(&mut radio, io, 0x0405);
    radio.connect(link(1), BLE, true, None).unwrap();
    // LE initiation waits until the controller acknowledges the page.
    settle(&mut radio, io);
    status(&mut radio, io, 0x0405, 0);
    until(&mut radio, io, 0x200d);
    status(&mut radio, io, 0x200d, COMMAND_DISALLOWED);
    quiet(&mut radio);
    settle(&mut radio, io);
    page_timeout(&mut radio, io, link(0));
    until(&mut radio, io, 0x200d);
    status(&mut radio, io, 0x200d, 0);
    quiet(&mut radio);
}
/// Accept-list initiation keeps running during a page, uses the HID
/// connection parameters, and waits for the page when the controller refuses.
pub fn accept_list_during_page() {
    let (mut radio, io) = start();
    radio.reconnect(&[BLE]).unwrap();
    let create = until(&mut radio, io, 0x200d);
    assert_eq!(create[4], 1, "initiator uses the accept list");
    assert_eq!(
        &create[..4],
        &[0x10, 0, 0x10, 0],
        "connection scan interval and window"
    );
    assert_eq!(
        &create[13..25],
        &[6, 0, 6, 0, 0, 0, 0, 1, 0, 0, 0, 0],
        "7.5 ms interval, no latency, 2.56 s supervision"
    );
    status(&mut radio, io, 0x200d, 0);
    // The page starts without cancelling accept-list initiation.
    radio.connect(link(0), CLASSIC, false, None).unwrap();
    until(&mut radio, io, 0x0405);
    status(&mut radio, io, 0x0405, 0);
    settle(&mut radio, io);
    // A page the device refuses ends only that attempt; setup stays concurrent.
    page_failure(&mut radio, io, link(0), LIMITED_RESOURCES);
    quiet(&mut radio);
    // A refused accept-list initiation during a page does not fail the radio.
    radio.connect(link(1), CLASSIC, false, None).unwrap();
    until(&mut radio, io, 0x0405);
    status(&mut radio, io, 0x0405, 0);
    // Changing the list restarts accept-list initiation during the page.
    radio.reconnect(&[BLE, OTHER_BLE]).unwrap();
    until(&mut radio, io, 0x200e);
    receive(io, &[0x0e, 4, 1, 0x0e, 0x20, 0]);
    radio.poll();
    let mut cancelled = [0; 21];
    cancelled[..4].copy_from_slice(&[0x3e, 19, 1, 2]);
    receive(io, &cancelled);
    radio.poll();
    until(&mut radio, io, 0x200d);
    status(&mut radio, io, 0x200d, COMMAND_DISALLOWED);
    quiet(&mut radio);
    settle(&mut radio, io);
    page_timeout(&mut radio, io, link(1));
    let create = until(&mut radio, io, 0x200d);
    assert_eq!(create[4], 1);
    status(&mut radio, io, 0x200d, 0);
    quiet(&mut radio);
}
/// A page starts during accept-list initiation. When the controller refuses
/// it, accept-list initiation is cancelled until the page ends.
pub fn page_during_accept_list() {
    let (mut radio, io) = start();
    radio.reconnect(&[BLE]).unwrap();
    until(&mut radio, io, 0x200d);
    status(&mut radio, io, 0x200d, 0);
    radio.connect(link(0), CLASSIC, false, None).unwrap();
    until(&mut radio, io, 0x0405);
    status(&mut radio, io, 0x0405, COMMAND_DISALLOWED);
    quiet(&mut radio);
    until(&mut radio, io, 0x200e);
    receive(io, &[0x0e, 4, 1, 0x0e, 0x20, 0]);
    radio.poll();
    let mut cancelled = [0; 21];
    cancelled[..4].copy_from_slice(&[0x3e, 19, 1, 2]);
    receive(io, &cancelled);
    quiet(&mut radio);
    until(&mut radio, io, 0x0405);
    status(&mut radio, io, 0x0405, 0);
    settle(&mut radio, io);
    page_timeout(&mut radio, io, link(0));
    let create = until(&mut radio, io, 0x200d);
    assert_eq!(create[4], 1);
    status(&mut radio, io, 0x200d, 0);
    quiet(&mut radio);
}
/// When the direct cancel after a refused page finds nothing to cancel, the
/// host's LE state cannot recover without its power cycle.
pub fn failed_resync_restarts() {
    let (mut radio, io) = start();
    radio.reconnect(&[BLE]).unwrap();
    until(&mut radio, io, 0x200d);
    status(&mut radio, io, 0x200d, 0);
    radio.connect(link(0), CLASSIC, false, None).unwrap();
    until(&mut radio, io, 0x0405);
    status(&mut radio, io, 0x0405, COMMAND_DISALLOWED);
    quiet(&mut radio);
    until(&mut radio, io, 0x200e);
    receive(io, &[0x0e, 4, 1, 0x0e, 0x20, COMMAND_DISALLOWED]);
    let mut restarting = false;
    for _ in 0..20 {
        radio.poll();
        while let Some(event) = radio.next_event() {
            match event {
                Event::Restarting(_) => restarting = true,
                Event::Disconnected { link: ended, .. } => assert_eq!(ended, link(0)),
                _ => panic!("unexpected event"),
            }
        }
        if restarting {
            break;
        }
    }
    assert!(restarting);
}
/// A connection that completes before the direct cancel makes the cancel
/// fail harmlessly; the new link stays.
pub fn resync_race_keeps_the_link() {
    let (mut radio, io) = start();
    radio.connect(link(0), BLE, true, None).unwrap();
    until(&mut radio, io, 0x200d);
    status(&mut radio, io, 0x200d, 0);
    radio.connect(link(1), CLASSIC, false, None).unwrap();
    until(&mut radio, io, 0x0405);
    status(&mut radio, io, 0x0405, COMMAND_DISALLOWED);
    quiet(&mut radio);
    until(&mut radio, io, 0x200e);
    receive(
        io,
        &[
            0x3e, 19, 1, 0, 0x40, 0, 0, 0, 6, 5, 4, 3, 2, 1, 0x18, 0, 0, 0, 0x48, 0, 0,
        ],
    );
    radio.poll();
    receive(io, &[0x0e, 4, 1, 0x0e, 0x20, COMMAND_DISALLOWED]);
    for _ in 0..10 {
        radio.poll();
        if let Some(event) = radio.next_event() {
            assert!(
                !matches!(event, Event::Restarting(_) | Event::Disconnected { .. }),
                "the connected link must stay"
            );
        }
    }
}
