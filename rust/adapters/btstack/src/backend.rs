//! BTstack callbacks own their bytes before returning to the vendor stack.
use crate::{ffi, storage::Storage, transport::Io};
use alloc::{collections::VecDeque, format, vec::Vec};
use cordial_core::model::{errors::ErrorCode as Error, identifiers::Transport, link::PromptMethod};
use cordial_core::{
    bluetooth::{
        Bluetooth, Capabilities, ConnectionSecurity, Descriptor, Event, InputReport, ReportType,
    },
    devices::{Peer, display_name},
    hid,
    link::{LinkId, ServiceId, WriteId},
    storage::RecordStore,
};
use core::{
    cell::{Cell, RefCell},
    ffi::{c_int, c_void},
};
use embassy_sync::{blocking_mutex::raw::NoopRawMutex, signal::Signal};

const EVENTS: usize = 8;
struct Pending {
    events: VecDeque<Event>,
    descriptors: [heapless::Vec<Descriptor, 3>; 4],
    generations: [u64; 4],
}
/// Only the application owner calls C. The controller task exchanges packets
/// through Io and never invokes BTstack or touches this callback state.
pub struct State<S: 'static> {
    storage: &'static Storage<S>,
    io: &'static Io,
    now: fn() -> u64,
    fatal: fn() -> !,
    wake: Signal<NoopRawMutex, ()>,
    fault: Cell<Option<Error>>,
    stopping: Cell<bool>,
    pairing: Cell<Option<LinkId>>,
    pending: RefCell<Pending>,
}
pub struct Backend<S: 'static> {
    state: &'static State<S>,
}
impl<S: RecordStore + 'static> State<S> {
    pub fn new(
        storage: &'static Storage<S>,
        io: &'static Io,
        now: fn() -> u64,
        fatal: fn() -> !,
    ) -> Result<Self, Error> {
        let mut events = VecDeque::new();
        events
            .try_reserve_exact(EVENTS)
            .map_err(|_| Error::Capacity)?;
        Ok(Self {
            storage,
            io,
            now,
            fatal,
            wake: Signal::new(),
            fault: Cell::new(None),
            stopping: Cell::new(false),
            pairing: Cell::new(None),
            pending: RefCell::new(Pending {
                events,
                descriptors: core::array::from_fn(|_| heapless::Vec::new()),
                generations: [0; 4],
            }),
        })
    }
    pub async fn changed(&self) {
        embassy_futures::select::select(self.wake.wait(), self.io.wait()).await;
    }
    fn activate(&self, link: LinkId) {
        let mut pending = self.pending.borrow_mut();
        let slot = usize::from(link.slot);
        pending.generations[slot] = link.generation;
        pending.descriptors[slot].clear();
    }
    unsafe extern "C" fn time(context: *mut c_void) -> u32 {
        let state = unsafe { &*context.cast::<Self>() };
        (state.now)() as u32
    }
    unsafe extern "C" fn wake(context: *mut c_void) {
        unsafe { &*context.cast::<Self>() }.wake.signal(());
    }
    unsafe extern "C" fn can_send(context: *mut c_void) -> c_int {
        i32::from(unsafe { &*context.cast::<Self>() }.io.can_send())
    }
    unsafe extern "C" fn send(
        context: *mut c_void,
        kind: u8,
        data: *const u8,
        length: u16,
    ) -> c_int {
        let state = unsafe { &*context.cast::<Self>() };
        let bytes = unsafe { core::slice::from_raw_parts(data, length.into()) };
        if state.io.send(kind, bytes) { 0 } else { -1 }
    }
    unsafe extern "C" fn fatal(context: *mut c_void) {
        (unsafe { &*context.cast::<Self>() }.fatal)()
    }
    unsafe extern "C" fn event(context: *mut c_void, event: *const ffi::Event) -> c_int {
        let state = unsafe { &*context.cast::<Self>() };
        let raw = unsafe { &*event };
        let data = if raw.length == 0 {
            &[]
        } else {
            unsafe { core::slice::from_raw_parts(raw.data, raw.length.into()) }
        };
        state.copy_event(raw, data)
    }
    fn copy_event(&self, raw: &ffi::Event, bytes: &[u8]) -> c_int {
        if self.stopping.get() {
            return 1;
        }
        let link = LinkId {
            slot: raw.link.slot,
            generation: raw.link.generation,
        };
        let slot = usize::from(link.slot);
        let mut pending = self.pending.borrow_mut();
        if matches!(raw.kind, 5..=12 | 14 | 15) {
            if slot >= 4 || link.generation == 0 {
                return 0;
            }
            if pending.generations[slot] != link.generation {
                // Only an accepted owner operation can replace a generation.
                // Late callbacks cannot erase a new connection's descriptors.
                return 1;
            }
        }
        if raw.kind == 7 {
            let descriptors = &mut pending.descriptors[slot];
            if bytes.is_empty()
                || bytes.len() > hid::DESCRIPTOR_BYTES
                || descriptors.len() >= 3
                || descriptors.iter().any(|d| d.service.0 == raw.service)
            {
                return 0;
            }
            let descriptor = match Descriptor::from_slice(ServiceId(raw.service), bytes) {
                Ok(descriptor) => descriptor,
                Err(Error::Capacity) => return -2,
                Err(_) => return -5,
            };
            if descriptors.push(descriptor).is_err() {
                return -2;
            }
            return 1;
        }
        // C retries security observations after pressure subsides. Keep room
        // for the Connected event that follows the initial observation.
        if raw.kind == 14 && pending.events.len() >= EVENTS - 1 {
            return 0;
        }
        if pending.events.len() == EVENTS {
            // Discovery updates are optional. Link callbacks return failure to
            // C, which tears down that source and retries its terminal event.
            if matches!(raw.kind, 1 | 2 | 13) {
                self.fault.set(Some(Error::Capacity));
            }
            return i32::from(matches!(raw.kind, 1..=3 | 13));
        }
        let id = WriteId {
            link,
            sequence: raw.number,
        };
        let report = || InputReport::new(link, ServiceId(raw.service), raw.report_id, bytes);
        let event = match raw.kind {
            1 => Event::Ready,
            2 => Event::Failed(error(raw.code)),
            13 => Event::Restarting(error(raw.code)),
            14 => {
                let flag = |bit: u32| {
                    (raw.number & (1 << (16 + bit)) != 0).then_some(raw.number & (1 << bit) != 0)
                };
                Event::Security {
                    link,
                    security: ConnectionSecurity {
                        encrypted: flag(0),
                        authenticated: flag(1),
                        secure_connections: flag(2),
                        bonded: flag(3),
                        key_size: match ((raw.number >> 8) & 0xff) as u8 {
                            7..=16 => Some((raw.number >> 8) as u8),
                            _ => None,
                        },
                    },
                }
            }
            15 => Event::Information {
                success: raw.code == 0,
                link,
                uuid: raw.service,
                instance: raw.report_id,
                bytes: bytes.into(),
            },
            3 => Event::Found {
                scan: raw.operation,
                address: (raw.address.transport != 0xff).then(|| peer(raw.address)),
                peer: peer(raw.peer),
                connectable: raw.code != 0,
                kind: cordial_core::bluetooth::discovery_kind(peer(raw.peer).transport, raw.number),
                name: display_name(bytes),
                rssi: (raw.rssi != 127).then_some(raw.rssi),
            },
            4 => Event::Incoming {
                attempt: raw.number,
                peer: peer(raw.peer),
            },
            5 => {
                let method = match raw.code {
                    0 => PromptMethod::ConfirmPasskey,
                    1 => PromptMethod::EnterPasskey,
                    2 => PromptMethod::EnterPin,
                    3 => PromptMethod::DisplayPasskey,
                    _ => return 0,
                };
                Event::Prompt {
                    link,
                    method,
                    value: matches!(raw.code, 0 | 3)
                        .then(|| format!("{:06}", raw.number).into_boxed_str()),
                }
            }
            6 => Event::Bonded {
                link,
                identity: peer(raw.peer),
            },
            8 => {
                if pending.descriptors[slot].is_empty() {
                    return 0;
                }
                let mut descriptors = Vec::new();
                if descriptors
                    .try_reserve_exact(pending.descriptors[slot].len())
                    .is_err()
                {
                    return -2;
                }
                descriptors.extend(core::mem::take(&mut pending.descriptors[slot]));
                Event::Connected {
                    link,
                    descriptors,
                    max_output: raw.number as usize,
                }
            }
            9 => {
                pending.descriptors[slot].clear();
                Event::Disconnected {
                    link,
                    error: (raw.code != 0).then(|| error(raw.code)),
                }
            }
            10 => match report() {
                Ok(report) => Event::Input(report),
                Err(_) => return 0,
            },
            11 => Event::Written {
                id,
                result: result(raw.code.into()),
            },
            12 => match raw.report_type {
                1..=3 => Event::Read {
                    id,
                    report_type: report_type(raw.report_type),
                    result: if raw.code == 0 {
                        report()
                    } else {
                        Err(error(raw.code))
                    },
                },
                _ => return 0,
            },
            _ => return 0,
        };
        pending.events.push_back(event);
        self.wake.signal(());
        1
    }
}
impl<S: RecordStore + 'static> Backend<S> {
    /// # Safety
    /// BTstack is process-global. Call once per boot, on its sole owner; keep
    /// the selected chipset table and all callbacks valid for that boot.
    pub unsafe fn new(state: &'static State<S>, chipset: Option<&'static ffi::Chipset>) -> Self {
        let context = core::ptr::from_ref(state).cast_mut().cast();
        let callbacks = ffi::RuntimeCallbacks {
            context,
            time_ms: State::<S>::time,
            wake: State::<S>::wake,
            can_send: State::<S>::can_send,
            send: State::<S>::send,
            fatal: State::<S>::fatal,
        };
        unsafe {
            ffi::cordial_runtime_init(
                &callbacks,
                &Storage::<S>::API,
                state.storage.context(),
                chipset.map_or(core::ptr::null(), core::ptr::from_ref),
            );
            ffi::cordial_profiles_init(context, State::<S>::event);
        }
        Self { state }
    }
    /// Construction leaves the management interface available even when native
    /// storage initialization fails. The owner publishes this error to the app.
    pub fn start(&mut self, address: Option<[u8; 6]>) -> Result<(), Error> {
        self.state
            .storage
            .cache_identity()
            .map_err(|_| Error::StorageFailed)?;
        self.checked(|| unsafe {
            ffi::cordial_profiles_start(address.as_ref().map_or(core::ptr::null(), |a| a.as_ptr()))
        })
    }
    pub fn poll(&mut self) {
        let _ = self.healthy();
        if self.state.io.take_failure() {
            self.state.fault.set(Some(Error::RadioUnavailable));
        }
        if let Some(ok) = self.state.io.take_completion() {
            if ok {
                unsafe { ffi::cordial_runtime_sent() };
            } else {
                self.state.fault.set(Some(Error::RadioUnavailable));
            }
        }
        while let Some(mut packet) = self.state.io.receive() {
            unsafe { ffi::cordial_runtime_receive(packet.kind, packet.incoming_ptr(), packet.len) };
            let _ = self.healthy();
        }
        unsafe {
            ffi::cordial_runtime_poll();
            let _ = self.healthy();
            ffi::cordial_profiles_poll((self.state.now)());
        }
        let _ = self.healthy();
    }
    pub fn next_event(&mut self) -> Option<Event> {
        if let Some(error) = self.state.fault.take() {
            self.shutdown();
            self.state.pending.borrow_mut().events.clear();
            return Some(Event::Failed(error));
        }
        let event = self.state.pending.borrow_mut().events.pop_front();
        if let Some(Event::Disconnected { link, .. }) = &event {
            self.pairing_ended(*link);
        }
        if matches!(event, Some(Event::Failed(_))) {
            self.shutdown();
            self.state.pending.borrow_mut().events.clear();
        }
        event
    }
    pub fn timeout_ms(&self) -> u32 {
        let timeout = unsafe { ffi::cordial_runtime_timeout() };
        if timeout < 0 {
            1000
        } else {
            (timeout as u32).min(1000)
        }
    }
    fn healthy(&self) -> Result<(), Error> {
        if self.state.storage.take_error().is_some() {
            self.state.fault.set(Some(Error::StorageFailed));
        }
        if self.state.storage.error().is_some() {
            self.shutdown();
            Err(Error::StorageFailed)
        } else if self.state.stopping.get() {
            Err(Error::RadioUnavailable)
        } else {
            Ok(())
        }
    }
    fn shutdown(&self) {
        if !self.state.stopping.replace(true) {
            // Keep servicing transport/run-loop completion while the public
            // power-off path disconnects native links. Storage stays latched.
            unsafe {
                ffi::cordial_profiles_stop();
            }
        }
    }
    fn pairing_ended(&self, link: LinkId) {
        if self.state.pairing.get() == Some(link) {
            self.state.pairing.set(None);
        }
    }
    fn checked(&self, call: impl FnOnce() -> c_int) -> Result<(), Error> {
        self.healthy()?;
        let status = call();
        self.healthy()?;
        result(status)
    }
}
fn peer(raw: ffi::Peer) -> Peer {
    Peer {
        address: raw.address,
        random: raw.random != 0,
        transport: if raw.transport == 0 {
            Transport::Classic
        } else {
            Transport::Ble
        },
    }
}
fn raw_peer(peer: Peer) -> ffi::Peer {
    ffi::Peer {
        address: peer.address,
        random: u8::from(peer.random),
        transport: u8::from(peer.transport == Transport::Ble),
    }
}
fn raw_link(link: LinkId) -> ffi::Link {
    ffi::Link {
        generation: link.generation,
        slot: link.slot,
    }
}
fn error(code: u8) -> Error {
    match code {
        1 => Error::Busy,
        2 => Error::Capacity,
        3 => Error::ConnectionFailed,
        4 => Error::AuthenticationFailed,
        5 => Error::UnsupportedHid,
        6 => Error::StorageFailed,
        7 => Error::InputOverflow,
        8 => Error::Timeout,
        10 => Error::HidReportTooLarge,
        _ => Error::RadioUnavailable,
    }
}
fn result(code: c_int) -> Result<(), Error> {
    if code == 0 {
        Ok(())
    } else {
        Err(error(code as u8))
    }
}
fn raw_type(kind: ReportType) -> u8 {
    match kind {
        ReportType::Input => 1,
        ReportType::Output => 2,
        ReportType::Feature => 3,
    }
}
fn report_type(kind: u8) -> ReportType {
    match kind {
        1 => ReportType::Input,
        2 => ReportType::Output,
        _ => ReportType::Feature,
    }
}
impl<S: RecordStore + 'static> Bluetooth for Backend<S> {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            classic: cfg!(feature = "classic"),
            ble: true,
            ble_scan_and_connect: unsafe { ffi::cordial_profiles_scan_and_connect() },
        }
    }
    fn bond_capacity(&self, transport: Transport) -> usize {
        unsafe {
            ffi::cordial_bond_capacity(if transport == Transport::Classic {
                1
            } else {
                2
            }) as usize
        }
    }
    fn reconnect(&mut self, peers: &[Peer]) -> Result<(), Error> {
        let peers: Vec<_> = peers.iter().copied().map(raw_peer).collect();
        self.checked(|| unsafe { ffi::cordial_profiles_reconnect(peers.as_ptr(), peers.len()) })
    }
    fn scan(&mut self, id: u64, classic: bool, ble: bool) -> Result<(), Error> {
        if classic && !self.capabilities().classic {
            return Err(Error::UnsupportedTransport);
        }
        self.checked(|| unsafe { ffi::cordial_profiles_scan(id, classic, ble) })
    }
    fn connect(&mut self, link: LinkId, peer: Peer, pairing: bool) -> Result<(), Error> {
        if !self.capabilities().supports(peer.transport) {
            return Err(Error::UnsupportedTransport);
        }
        if pairing {
            if self.state.pairing.get().is_some() {
                return Err(Error::Busy);
            }
            self.state.pairing.set(Some(link));
        }
        let result = self.checked(|| unsafe {
            ffi::cordial_profiles_connect(raw_link(link), raw_peer(peer), pairing)
        });
        if result.is_ok() {
            self.state.activate(link);
        } else if pairing {
            self.pairing_ended(link);
        }
        if result == Err(Error::StorageFailed) {
            self.disconnect(link);
        }
        result
    }
    fn incoming(&mut self, attempt: u32, accept: Option<LinkId>) -> Result<(), Error> {
        let id = accept.map(raw_link);
        let result = self.checked(|| unsafe {
            ffi::cordial_profiles_incoming(
                attempt,
                id.as_ref().map_or(core::ptr::null(), core::ptr::from_ref),
            )
        });
        if result.is_ok()
            && let Some(link) = accept
        {
            self.state.activate(link);
        }
        result
    }
    fn disconnect(&mut self, link: LinkId) {
        if self.healthy().is_err() {
            return;
        }
        unsafe {
            ffi::cordial_profiles_disconnect(raw_link(link));
        }
    }
    fn adopt(&mut self, link: LinkId) -> Result<(), Error> {
        self.checked(|| unsafe { ffi::cordial_profiles_adopt(raw_link(link)) })?;
        self.pairing_ended(link);
        Ok(())
    }
    fn pair_reply(
        &mut self,
        link: LinkId,
        method: PromptMethod,
        accept: bool,
        value: Option<&str>,
    ) -> Result<(), Error> {
        let method = match method {
            PromptMethod::ConfirmPasskey => 0,
            PromptMethod::EnterPasskey => 1,
            PromptMethod::EnterPin => 2,
            _ => return Err(Error::StalePrompt),
        };
        let mut bytes = [0; 17];
        if let Some(value) = value {
            if value.len() > 16 || value.bytes().any(|b| b == 0) {
                return Err(Error::InvalidArgs);
            }
            bytes[..value.len()].copy_from_slice(value.as_bytes());
        }
        self.checked(|| unsafe {
            ffi::cordial_profiles_reply(raw_link(link), method, accept, bytes.as_ptr().cast())
        })
    }
    fn refresh_info(&mut self, link: LinkId) -> Result<(), Error> {
        result(unsafe { ffi::cordial_profiles_info_refresh(raw_link(link)) })
    }
    fn info_busy(&self, link: LinkId) -> bool {
        unsafe { ffi::cordial_profiles_info_busy(raw_link(link)) != 0 }
    }
    fn can_write(&self, link: LinkId) -> bool {
        if self.healthy().is_err() {
            return false;
        }
        unsafe { ffi::cordial_profiles_can_write(raw_link(link)) != 0 }
    }
    fn write(
        &mut self,
        id: WriteId,
        service: ServiceId,
        kind: ReportType,
        report_id: Option<u8>,
        payload: &[u8],
    ) -> Result<(), Error> {
        if payload.len() > hid::REPORT_BYTES {
            return Err(Error::UnsupportedHid);
        }
        self.checked(|| unsafe {
            ffi::cordial_profiles_write(
                raw_link(id.link),
                id.sequence,
                service.0,
                raw_type(kind),
                report_id.map_or(0xffff, u16::from),
                payload.as_ptr(),
                payload.len() as u16,
            )
        })
    }
    fn read(
        &mut self,
        id: WriteId,
        service: ServiceId,
        kind: ReportType,
        report_id: Option<u8>,
    ) -> Result<(), Error> {
        self.checked(|| unsafe {
            ffi::cordial_profiles_read(
                raw_link(id.link),
                id.sequence,
                service.0,
                raw_type(kind),
                report_id.map_or(0xffff, u16::from),
            )
        })
    }
    async fn import_bond(&mut self, bond: &cordial_core::bonds::Bond) -> Result<(), Error> {
        self.healthy()?;
        let bytes = bond.encode().map_err(|_| Error::StorageFailed)?;
        if unsafe { ffi::cordial_bond_import(bytes.as_ptr(), bytes.len() as u32) } < 0 {
            return Err(Error::Capacity);
        }
        Ok(())
    }
    async fn export_bond(&mut self, peer: Peer) -> Result<cordial_core::bonds::Bond, Error> {
        self.healthy()?;
        let mut bytes = [0; 145];
        let n = unsafe {
            ffi::cordial_bond_export(
                if peer.transport == Transport::Classic {
                    1
                } else {
                    2
                },
                u32::from(peer.random),
                peer.address.as_ptr(),
                bytes.as_mut_ptr(),
            )
        };
        if n < 0 {
            return Err(Error::AuthenticationFailed);
        }
        cordial_core::bonds::Bond::decode(&bytes[..n as usize]).map_err(|_| Error::StorageFailed)
    }
    async fn bonds(&mut self) -> Result<Vec<Peer>, Error> {
        self.healthy()?;
        let mut peers = [ffi::Peer::default(); 16];
        let count = unsafe { ffi::cordial_profiles_bonds(peers.as_mut_ptr(), peers.len() as u32) };
        self.healthy()?;
        if count < 0 {
            return Err(error((-count) as u8));
        }
        Ok(peers[..count as usize].iter().copied().map(peer).collect())
    }
    async fn forget(&mut self, peer: Peer) -> Result<(), Error> {
        self.checked(|| unsafe { ffi::cordial_profiles_forget(raw_peer(peer)) })
    }
}

impl<S: RecordStore + 'static> cordial_core::bluetooth::EventSource for Backend<S> {
    async fn poll(&mut self) {
        Backend::poll(self);
    }
    fn next_event(&mut self) -> Option<Event> {
        Backend::next_event(self)
    }
    async fn changed(&self) {
        self.state.changed().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::boxed::Box;
    #[allow(dead_code)]
    mod support {
        use alloc::vec::Vec;
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../crates/cordial-core/tests/support/mod.rs"
        ));
    }
    fn raw(kind: u8, generation: u64) -> ffi::Event {
        ffi::Event {
            link: ffi::Link {
                slot: 0,
                generation,
            },
            peer: ffi::Peer::default(),
            address: ffi::Peer::default(),
            operation: 0,
            number: 0,
            service: 2,
            length: 0,
            rssi: 0,
            kind,
            code: 0,
            report_id: 0x11,
            report_type: 2,
            data: core::ptr::null(),
        }
    }
    #[test]
    fn discovery_preserves_classic_ble_and_identity_only_addresses() {
        let storage = Box::leak(Box::new(Storage::new(support::Store::default())));
        let io = Box::leak(Box::new(Io::new()));
        let state = State::new(storage, io, || 0, || panic!("fatal")).unwrap();
        for (transport, address_transport) in [(0, 0), (1, 1), (1, 0xff)] {
            let mut event = raw(3, 0);
            event.peer.transport = transport;
            event.number = if transport == 0 { 0x0540 } else { 0x03c1 };
            event.peer.address = [1; 6];
            event.address.transport = address_transport;
            event.address.address = [2; 6];
            assert_eq!(state.copy_event(&event, &[]), 1);
            let Event::Found {
                peer,
                address,
                kind,
                ..
            } = state.pending.borrow_mut().events.pop_front().unwrap()
            else {
                panic!("missing discovery");
            };
            assert_eq!(kind, cordial_core::bluetooth::DeviceKind::Keyboard);
            assert_eq!(
                peer.transport,
                if transport == 0 {
                    Transport::Classic
                } else {
                    Transport::Ble
                }
            );
            if address_transport == 0xff {
                assert_eq!(address, None);
            } else {
                assert_eq!(address.unwrap().transport, peer.transport);
                assert_eq!(address.unwrap().address, [2; 6]);
            }
        }
    }
    #[test]
    fn security_callbacks_decode_negotiated_properties_and_reject_old_generations() {
        let storage = Box::leak(Box::new(Storage::new(support::Store::default())));
        let io = Box::leak(Box::new(Io::new()));
        let state = State::new(storage, io, || 0, || panic!("fatal")).unwrap();
        state.activate(LinkId {
            slot: 0,
            generation: 2,
        });
        let mut event = raw(14, 2);
        event.number = 1 | 4 | 8 | (16 << 8) | (15 << 16); // Encrypted SC Just Works bond.
        assert_eq!(state.copy_event(&event, &[]), 1);
        match state.pending.borrow_mut().events.pop_front().unwrap() {
            Event::Security { security, .. } => {
                assert_eq!(security.encrypted, Some(true));
                assert_eq!(security.authenticated, Some(false));
                assert_eq!(security.secure_connections, Some(true));
                assert_eq!(security.bonded, Some(true));
                assert_eq!(security.key_size, Some(16));
            }
            _ => panic!("missing security"),
        }
        event.link.generation = 1;
        assert_eq!(state.copy_event(&event, &[]), 1);
        assert!(state.pending.borrow_mut().events.pop_front().is_none());
        event.link.generation = 2;
        event.number = 0;
        assert_eq!(state.copy_event(&event, &[]), 1);
        assert!(
            matches!(state.pending.borrow_mut().events.pop_front(), Some(Event::Security { security, .. })
            if security == ConnectionSecurity::default())
        );
    }
    #[test]
    fn callbacks_copy_service_payloads_and_preserve_terminal_events_under_pressure() {
        let storage = Box::leak(Box::new(Storage::new(support::Store::default())));
        let io = Box::leak(Box::new(Io::new()));
        let state = State::new(storage, io, || 0, || panic!("fatal")).unwrap();
        state.activate(LinkId {
            slot: 0,
            generation: 1,
        });
        assert_eq!(
            state.copy_event(
                &raw(7, 1),
                &[
                    5, 1, 9, 6, 0xa1, 1, 5, 7, 9, 4, 0x15, 0, 0x25, 1, 0x75, 1, 0x95, 1, 0x81, 2,
                    0xc0
                ]
            ),
            1
        );
        assert_eq!(
            state.copy_event(
                &raw(7, 1),
                &[
                    5, 1, 9, 6, 0xa1, 1, 5, 7, 9, 4, 0x15, 0, 0x25, 1, 0x75, 1, 0x95, 1, 0x81, 2,
                    0xc0
                ]
            ),
            0
        ); // Duplicate service.
        let mut connected = raw(8, 1);
        connected.number = 512;
        assert_eq!(state.copy_event(&connected, &[]), 1);
        match state.pending.borrow_mut().events.pop_front().unwrap() {
            Event::Connected {
                descriptors,
                max_output,
                ..
            } => {
                assert_eq!(max_output, 512);
                assert_eq!(descriptors[0].service, ServiceId(2));
                assert_eq!(descriptors[0].map.roles, hid::KEYBOARD);
            }
            _ => panic!("missing Connected"),
        }
        let mut input = [0xff, 3, 0x10];
        for _ in 0..EVENTS {
            assert_eq!(state.copy_event(&raw(10, 1), &input), 1);
        }
        input.fill(0);
        assert_eq!(state.copy_event(&raw(10, 1), &input), 0);
        assert_eq!(state.copy_event(&raw(9, 1), &[]), 0);
        assert_eq!(state.copy_event(&raw(14, 1), &[]), 0);
        match state.pending.borrow_mut().events.pop_front().unwrap() {
            Event::Input(report) => {
                assert_eq!(report.service, ServiceId(2));
                assert_eq!(report.report_id, 0x11);
                assert_eq!(report.payload(), &[0xff, 3, 0x10]);
            }
            _ => panic!("missing owned Input"),
        }
        assert_eq!(state.copy_event(&raw(14, 1), &[]), 0); // Reserve the last slot.
        assert_eq!(state.copy_event(&raw(9, 1), &[]), 1);
        assert!(matches!(
            state.pending.borrow_mut().events.pop_back(),
            Some(Event::Disconnected { .. })
        ));
        // A new generation owns a fresh service catalog.
        state.pending.borrow_mut().events.clear();
        state.activate(LinkId {
            slot: 0,
            generation: 2,
        });
        assert_eq!(
            state.copy_event(
                &raw(7, 2),
                &[
                    5, 1, 9, 6, 0xa1, 1, 5, 7, 9, 5, 0x15, 0, 0x25, 1, 0x75, 1, 0x95, 1, 0x81, 2,
                    0xc0
                ]
            ),
            1
        );
        assert_eq!(state.copy_event(&raw(9, 1), &[]), 1);
        assert_eq!(state.pending.borrow().descriptors[0].len(), 1);
        assert_eq!(state.copy_event(&raw(8, 2), &[]), 1);
        let mut read = raw(12, 2);
        read.code = 3;
        assert_eq!(state.copy_event(&read, &[]), 1);
        assert!(matches!(
            state.pending.borrow_mut().events.pop_back(),
            Some(Event::Read {
                result: Err(Error::ConnectionFailed),
                ..
            })
        ));
    }
}
