//! Closing links whose end the host never reports.
use super::{receive, reply, take};
use cordial_btstack::{
    backend::{Backend, State},
    storage::Storage,
    transport::Io,
};
use cordial_core::model::identifiers::Transport;
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
type Radio = Backend<support::Store>;
static NOW: AtomicU64 = AtomicU64::new(0);
unsafe extern "C" {
    fn gap_store_link_key_for_bd_addr(address: *const u8, key: *const u8, kind: i32);
}
const CLASSIC: Peer = Peer {
    address: [0x11, 0x22, 0x33, 0x44, 0x55, 0x66],
    random: false,
    transport: Transport::Classic,
};
const BLE: Peer = Peer {
    address: [6, 5, 4, 3, 2, 1],
    random: false,
    transport: Transport::Ble,
};
fn link(slot: u8, generation: u64) -> LinkId {
    LinkId { slot, generation }
}
/// Answers ordinary commands until `opcode` is sent.
fn until(radio: &mut Radio, io: &Io, opcode: u16) {
    for sequence in 0..80 {
        radio.poll();
        let packet = take(io).unwrap_or_else(|| panic!("controller never received {opcode:04x}"));
        let sent = u16::from_le_bytes([packet.data()[0], packet.data()[1]]);
        io.finish_outbound(true);
        radio.poll();
        if sent == opcode {
            return;
        }
        reply(io, sent, sequence);
        radio.poll();
    }
    panic!("controller never received {opcode:04x}");
}
fn settle(radio: &mut Radio, io: &Io) {
    for sequence in 0..40 {
        radio.poll();
        let Some(packet) = take(io) else { return };
        if packet.kind != 1 {
            io.finish_outbound(true);
            continue;
        }
        let sent = u16::from_le_bytes([packet.data()[0], packet.data()[1]]);
        io.finish_outbound(true);
        radio.poll();
        reply(io, sent, sequence);
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
        State::new(storage, io, || NOW.load(Relaxed), || std::process::abort()).unwrap(),
    ));
    let mut radio = unsafe { Backend::new(state, None) };
    radio.start(Some([2, 3, 4, 5, 6, 7])).unwrap();
    settle(&mut radio, io);
    assert!(matches!(radio.next_event(), Some(Event::Ready)));
    (radio, io)
}
/// Advances past the closing deadline and expects the link to end with a
/// restart of Bluetooth.
/// A link closed at time zero while connecting outlives its 10 s bound:
/// Bluetooth restarts, the link ends once the host is off, and the host
/// becomes ready again.
fn expire(radio: &mut Radio, io: &Io, ended_link: LinkId) {
    NOW.store(9_999, Relaxed);
    settle(radio, io);
    assert!(radio.next_event().is_none(), "the host may still end it");
    NOW.store(10_000, Relaxed);
    let (mut ended, mut restarting, mut ready) = (false, false, false);
    for now in [10_000, 60_000] {
        NOW.store(now, Relaxed);
        for _ in 0..10 {
            settle(radio, io);
            while let Some(event) = radio.next_event() {
                match event {
                    Event::Disconnected { link, .. } if link == ended_link => ended = true,
                    Event::Restarting(_) => restarting = true,
                    Event::Ready => ready = true,
                    _ => {}
                }
            }
        }
    }
    assert!(ended && restarting && ready);
}
/// A page whose cancel finds nothing and whose completion never arrives.
pub fn stuck_page() {
    let (mut radio, io) = start();
    super::enable_classic(&mut radio, io);
    unsafe { gap_store_link_key_for_bd_addr(CLASSIC.address.as_ptr(), [0x5a; 16].as_ptr(), 4) };
    radio.connect(link(0, 1), CLASSIC, false, None).unwrap();
    until(&mut radio, io, 0x0405);
    receive(io, &[0x0f, 4, 0, 1, 0x05, 0x04]);
    radio.poll();
    radio.disconnect(link(0, 1));
    until(&mut radio, io, 0x0408);
    receive(io, &[0x0e, 10, 1, 0x08, 0x04, 0x02, 0, 0, 0, 0, 0, 0]);
    expire(&mut radio, io, link(0, 1));
    // The restarted host pages again.
    radio.connect(link(0, 2), CLASSIC, false, None).unwrap();
    until(&mut radio, io, 0x0405);
}
fn restarted(radio: &mut Radio, io: &Io) {
    NOW.store(60_000, Relaxed);
    let mut ready = false;
    for _ in 0..10 {
        settle(radio, io);
        while let Some(event) = radio.next_event() {
            ready |= matches!(event, Event::Ready);
        }
    }
    assert!(ready);
}
/// An LE cancel that never completes.
pub fn stuck_le_cancel() {
    let (mut radio, io) = start();
    radio.connect(link(1, 3), BLE, true, None).unwrap();
    until(&mut radio, io, 0x200d);
    receive(io, &[0x0f, 4, 0, 1, 0x0d, 0x20]);
    radio.poll();
    radio.disconnect(link(1, 3));
    until(&mut radio, io, 0x200e);
    receive(io, &[0x0e, 4, 1, 0x0e, 0x20, 0]);
    expire(&mut radio, io, link(1, 3));
    radio.connect(link(1, 4), BLE, true, None).unwrap();
    until(&mut radio, io, 0x200d);
}
/// An accept-list initiation whose cancel never completes.
pub fn stuck_accept_list_cancel() {
    let (mut radio, io) = start();
    radio.reconnect(&[BLE]).unwrap();
    until(&mut radio, io, 0x200d);
    receive(io, &[0x0f, 4, 0, 1, 0x0d, 0x20]);
    radio.poll();
    // Disabling BLE cancels accept-list initiation.
    radio.set_transport(Transport::Ble, false).unwrap();
    until(&mut radio, io, 0x200e);
    receive(io, &[0x0e, 4, 1, 0x0e, 0x20, 0]);
    NOW.store(9_999, Relaxed);
    settle(&mut radio, io);
    assert!(
        radio.next_event().is_none(),
        "the controller may still answer"
    );
    NOW.store(10_000, Relaxed);
    let mut restarting = false;
    for _ in 0..10 {
        settle(&mut radio, io);
        while let Some(event) = radio.next_event() {
            restarting |= matches!(event, Event::Restarting(_));
        }
    }
    assert!(restarting);
    restarted(&mut radio, io);
    // The restarted host initiates again once BLE is enabled and the
    // application supplies its list, as it does after every Ready.
    radio.set_transport(Transport::Ble, true).unwrap();
    radio.reconnect(&[BLE]).unwrap();
    until(&mut radio, io, 0x200d);
}
/// A page closed before the controller acknowledged it is cancelled once it
/// is.
pub fn cancel_before_acknowledgement() {
    let (mut radio, io) = start();
    super::enable_classic(&mut radio, io);
    unsafe { gap_store_link_key_for_bd_addr(CLASSIC.address.as_ptr(), [0x5a; 16].as_ptr(), 4) };
    radio.connect(link(0, 1), CLASSIC, false, None).unwrap();
    until(&mut radio, io, 0x0405);
    radio.disconnect(link(0, 1));
    radio.poll();
    assert!(
        take(io).is_none(),
        "nothing to cancel before the controller runs the page"
    );
    receive(io, &[0x0f, 4, 0, 1, 0x05, 0x04]);
    until(&mut radio, io, 0x0408);
    let mut address = CLASSIC.address;
    address.reverse();
    let mut cancelled = vec![0x0e, 10, 1, 0x08, 0x04, 0];
    cancelled.extend(address);
    receive(io, &cancelled);
    radio.poll();
    let mut complete = vec![0x03, 11, 0x02, 0, 0];
    complete.extend(address);
    complete.extend([1, 0]);
    receive(io, &complete);
    settle(&mut radio, io);
    radio.poll();
    assert!(matches!(
        radio.next_event(),
        Some(Event::Disconnected { link: ended, .. }) if ended == link(0, 1)
    ));
}
