//! Owned HCI packets between BTstack's owner and the controller task.
use core::cell::Cell;
use embassy_sync::{
    blocking_mutex::{Mutex, raw::CriticalSectionRawMutex as Raw},
    channel::Channel,
    signal::Signal,
};

// Includes an ACL header but excludes the one-byte transport indicator.
pub const PACKET_BYTES: usize = 1023;
// BTstack builds receive events in place before the HCI packet. Its selected
// configuration is checked against this capacity in c/runtime.c.
const INCOMING_HEADROOM: usize = 8;
pub struct Packet {
    pub kind: u8,
    pub len: u16,
    bytes: [u8; INCOMING_HEADROOM + PACKET_BYTES],
}
impl Packet {
    pub const fn empty() -> Self {
        Self {
            kind: 0,
            len: 0,
            bytes: [0; INCOMING_HEADROOM + PACKET_BYTES],
        }
    }
    pub fn data(&self) -> &[u8] {
        &self.bytes[INCOMING_HEADROOM..][..usize::from(self.len)]
    }
    pub fn data_mut(&mut self) -> &mut [u8] {
        &mut self.bytes[INCOMING_HEADROOM..][..usize::from(self.len)]
    }
    /// Derived from the whole allocation: the C receiver may also write up to
    /// INCOMING_HEADROOM bytes before this pointer. Do not derive it from data_mut.
    #[cfg(any(feature = "ffi", test))]
    pub(crate) fn incoming_ptr(&mut self) -> *mut u8 {
        self.bytes.as_mut_ptr().wrapping_add(INCOMING_HEADROOM)
    }
}

pub struct Io {
    outbound: Channel<Raw, Packet, 1>,
    inbound: Channel<Raw, Packet, 2>,
    inflight: Mutex<Raw, Cell<bool>>,
    completed: Signal<Raw, bool>,
    failed: Signal<Raw, ()>,
    changed: Signal<Raw, ()>,
}
impl Default for Io {
    fn default() -> Self {
        Self::new()
    }
}
impl Io {
    pub const fn new() -> Self {
        Self {
            outbound: Channel::new(),
            inbound: Channel::new(),
            inflight: Mutex::new(Cell::new(false)),
            completed: Signal::new(),
            failed: Signal::new(),
            changed: Signal::new(),
        }
    }
    pub fn can_send(&self) -> bool {
        self.inflight.lock(|v| !v.get())
    }
    /// One owner submits commands/ACL data. A buffer release is published only
    /// after the selected controller adapter has accepted ownership of the bytes.
    pub fn send(&self, kind: u8, bytes: &[u8]) -> bool {
        if !matches!(kind, 1 | 2) || bytes.len() > PACKET_BYTES {
            return false;
        }
        if self.inflight.lock(|v| v.replace(true)) {
            return false;
        }
        let mut packet = Packet::empty();
        packet.kind = kind;
        packet.len = bytes.len() as u16;
        packet.data_mut().copy_from_slice(bytes);
        if self.outbound.try_send(packet).is_err() {
            self.inflight.lock(|v| v.set(false));
            return false;
        }
        true
    }
    pub async fn next_outbound(&self) -> Packet {
        self.outbound.receive().await
    }
    pub fn finish_outbound(&self, ok: bool) {
        self.completed.signal(ok);
        self.changed.signal(());
    }
    pub fn take_completion(&self) -> Option<bool> {
        let result = self.completed.try_take()?;
        self.inflight.lock(|v| v.set(false));
        Some(result)
    }
    pub async fn received(&self, packet: Packet) {
        self.inbound.send(packet).await;
        self.changed.signal(());
    }
    pub fn receive(&self) -> Option<Packet> {
        self.inbound.try_receive().ok()
    }
    pub fn fail(&self) {
        self.failed.signal(());
        self.changed.signal(());
    }
    pub fn take_failure(&self) -> bool {
        self.failed.try_take().is_some()
    }
    pub async fn wait(&self) {
        self.changed.wait().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn incoming_event_prefix_preserves_metadata_and_payload() {
        let mut packet = Packet::empty();
        packet.kind = 2;
        packet.len = 19;
        packet.data_mut().fill(0xa5);
        let base = packet.bytes.as_ptr() as usize;
        let pointer = packet.incoming_ptr();
        assert_eq!(pointer as usize - base, 8);
        // Model BTstack's event-header writes before the received packet.
        unsafe { pointer.sub(8).write_bytes(0x5a, 8) };
        assert_eq!(packet.kind, 2);
        assert_eq!(packet.len, 19);
        assert_eq!(packet.data(), &[0xa5; 19]);
    }
    #[test]
    fn send_reservation_lasts_until_owner_consumes_completion() {
        let io = Io::new();
        assert!(!io.send(4, &[0]));
        assert!(io.send(1, &[3, 12, 0]));
        let packet = io.outbound.try_receive().unwrap();
        assert_eq!(packet.data(), &[3, 12, 0]);
        assert!(!io.can_send());
        assert!(!io.send(1, &[4, 12, 0]));
        io.finish_outbound(true);
        assert!(!io.can_send());
        assert_eq!(io.take_completion(), Some(true));
        assert!(io.can_send());
        assert!(io.send(2, &[0, 0, 0, 0]));
        io.finish_outbound(false);
        assert_eq!(io.take_completion(), Some(false));
    }
}
