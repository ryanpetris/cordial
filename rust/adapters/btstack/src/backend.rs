//! BTstack callbacks own their bytes before returning to the vendor stack.
use crate::{ffi, storage::Storage, transport::Io};
use alloc::{boxed::Box, collections::VecDeque, format, vec::Vec};
use cordial_core::model::{errors::ErrorCode as Error, identifiers::Transport, link::PromptMethod};
use cordial_core::{
    bluetooth::{
        Bluetooth, Capabilities, ConnectionSecurity, DatabaseHash, Descriptor, Event, InputReport,
        LAYOUT_SERVICES, Layout, LayoutReport, ReportMap, ReportType,
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
type Maps = [Option<Vec<u8>>; LAYOUT_SERVICES];
/// A usable saved layout with its compiled report maps.
type Cached = (Layout, heapless::Vec<Descriptor, LAYOUT_SERVICES>);
struct Pending {
    events: VecDeque<Event>,
    descriptors: [heapless::Vec<Descriptor, LAYOUT_SERVICES>; 4],
    /// Raw report maps by service: discovered maps until Connected, or the
    /// maps a verification found until its result.
    maps: [Maps; 4],
    /// The saved layout a link uses until its verification ends.
    layouts: [Option<Layout>; 4],
    generations: [u64; 4],
}
/// A saved layout lent to C for one call.
struct Native {
    _reports: Vec<ffi::LayoutReport>,
    // A heap copy: the supplied layout moves into its slot before the call.
    _hash: Option<Box<[u8; 16]>>,
    raw: ffi::Layout,
}
impl Native {
    fn new(layout: &Layout) -> Self {
        let reports: Vec<_> = layout
            .reports
            .iter()
            .map(|r| ffi::LayoutReport {
                value: r.value,
                cccd: r.cccd,
                properties: r.properties,
                service: r.service as u8,
                id: r.id,
                kind: raw_type(r.kind),
            })
            .collect();
        let hash = layout.hash.map(|h| Box::new(h.0));
        let map = if reports.is_empty() {
            layout.maps.first().map_or(&[][..], |m| &m.0)
        } else {
            &[]
        };
        let raw = ffi::Layout {
            reports: if reports.is_empty() {
                core::ptr::null()
            } else {
                reports.as_ptr()
            },
            descriptor: if map.is_empty() {
                core::ptr::null()
            } else {
                map.as_ptr()
            },
            hash: hash.as_ref().map_or(core::ptr::null(), |h| h.as_ptr()),
            count: reports.len() as u16,
            length: map.len() as u16,
        };
        Self {
            _reports: reports,
            _hash: hash,
            raw,
        }
    }
}
/// A saved layout is used when it is valid for the link and its maps compile.
fn cached(layout: Option<&Layout>, transport: Option<Transport>) -> Option<Cached> {
    let layout = layout?;
    let valid = match transport {
        Some(transport) => layout.valid(transport),
        None => layout.valid(Transport::Ble) || layout.valid(Transport::Classic),
    };
    if !valid {
        return None;
    }
    let mut descriptors = heapless::Vec::new();
    for (service, map) in layout.maps.iter().enumerate() {
        let descriptor = Descriptor::from_slice(ServiceId(service as u16), &map.0).ok()?;
        descriptors.push(descriptor).ok()?;
    }
    Some((layout.clone(), descriptors))
}
/// The Database Hash C reports with a layout.
fn hash(raw: &ffi::Event) -> Option<DatabaseHash> {
    // SAFETY: C passes either null or 16 readable bytes that outlive the callback.
    unsafe { raw.hash.cast::<[u8; 16]>().as_ref() }.map(|h| DatabaseHash(*h))
}
fn compile(maps: &[ReportMap]) -> Result<Vec<Descriptor>, Error> {
    let mut descriptors = Vec::new();
    descriptors
        .try_reserve_exact(maps.len())
        .map_err(|_| Error::Capacity)?;
    for (service, map) in maps.iter().enumerate() {
        descriptors.push(Descriptor::from_slice(ServiceId(service as u16), &map.0)?);
    }
    Ok(descriptors)
}
/// Decodes C's packed report table.
fn layout_reports(bytes: &[u8]) -> Option<Vec<LayoutReport>> {
    let (reports, rest) = bytes.as_chunks::<8>();
    if !rest.is_empty() {
        return None;
    }
    let mut decoded = Vec::new();
    decoded.try_reserve_exact(reports.len()).ok()?;
    for r in reports {
        decoded.push(LayoutReport {
            value: u16::from_ne_bytes([r[0], r[1]]),
            cccd: u16::from_ne_bytes([r[2], r[3]]),
            properties: r[4],
            service: r[5].into(),
            id: r[6],
            kind: match r[7] {
                1 => ReportType::Input,
                2 => ReportType::Output,
                3 => ReportType::Feature,
                _ => return None,
            },
        });
    }
    Some(decoded)
}
/// Moves the first `count` maps out of a slot.
fn take_maps(maps: &mut Maps, count: usize) -> Option<Vec<ReportMap>> {
    if count == 0 || count > maps.len() || maps[..count].iter().any(Option::is_none) {
        return None;
    }
    let mut taken = Vec::new();
    taken.try_reserve_exact(count).ok()?;
    taken.extend(
        maps[..count]
            .iter_mut()
            .map(|m| ReportMap(m.take().unwrap_or_default())),
    );
    Some(taken)
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
    /// Enabled transports, indexed by `transport_index`. Classic starts
    /// disabled and BLE enabled; both survive restarts.
    enabled: [Cell<bool>; 2],
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
            enabled: [Cell::new(false), Cell::new(true)],
            pairing: Cell::new(None),
            pending: RefCell::new(Pending {
                events,
                descriptors: core::array::from_fn(|_| heapless::Vec::new()),
                maps: Default::default(),
                layouts: Default::default(),
                generations: [0; 4],
            }),
        })
    }
    pub async fn changed(&self) {
        embassy_futures::select::select(self.wake.wait(), self.io.wait()).await;
    }
    /// Owns a slot for `link` before C can report it. Returns the generation
    /// it replaces so a refused call can restore it.
    fn activate(&self, link: LinkId, cached: Option<Cached>) -> Option<u64> {
        let mut pending = self.pending.borrow_mut();
        let slot = usize::from(link.slot);
        if slot >= pending.generations.len() {
            return None;
        }
        let previous = core::mem::replace(&mut pending.generations[slot], link.generation);
        let (layout, descriptors) =
            cached.map_or((None, heapless::Vec::new()), |(l, d)| (Some(l), d));
        pending.descriptors[slot] = descriptors;
        pending.layouts[slot] = layout;
        pending.maps[slot] = Default::default();
        Some(previous)
    }
    fn restore(&self, link: LinkId, previous: Option<u64>) {
        let Some(previous) = previous else { return };
        let mut pending = self.pending.borrow_mut();
        let slot = usize::from(link.slot);
        pending.generations[slot] = previous;
        pending.descriptors[slot].clear();
        pending.layouts[slot] = None;
        pending.maps[slot] = Default::default();
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
        if matches!(raw.kind, 5..=12 | 14..=16) {
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
            let service = usize::from(raw.service);
            if bytes.is_empty() || bytes.len() > hid::DESCRIPTOR_BYTES || service >= LAYOUT_SERVICES
            {
                return 0;
            }
            let mut map = Vec::new();
            if map.try_reserve_exact(bytes.len()).is_err() {
                return -2;
            }
            map.extend_from_slice(bytes);
            if raw.code != 0 {
                // Verification compares raw maps; only a changed layout compiles.
                pending.maps[slot][service] = Some(map);
                return 1;
            }
            // Discovery means C could not use a supplied layout.
            if pending.layouts[slot].take().is_some() {
                pending.descriptors[slot].clear();
            }
            let descriptors = &mut pending.descriptors[slot];
            if descriptors.len() >= LAYOUT_SERVICES
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
            pending.maps[slot][service] = Some(map);
            return 1;
        }
        if raw.kind == 16 && raw.code != 0 {
            // Verification ended without a result; C keeps the supplied layout.
            pending.layouts[slot] = None;
            pending.maps[slot] = Default::default();
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
                // C reports whether the supplied layout is in use or delivers
                // the discovered report table for the maps it reported.
                let layout = if raw.code != 0 {
                    None
                } else {
                    let count = pending.descriptors[slot].len();
                    let (Some(maps), Some(reports)) = (
                        take_maps(&mut pending.maps[slot], count),
                        layout_reports(bytes),
                    ) else {
                        return -5;
                    };
                    Some(Layout {
                        maps,
                        reports,
                        hash: hash(raw),
                    })
                };
                descriptors.extend(core::mem::take(&mut pending.descriptors[slot]));
                Event::Connected {
                    link,
                    descriptors,
                    max_output: raw.number as usize,
                    layout,
                }
            }
            16 => {
                // Verification ended. An equal layout needs no event. A
                // refusal reports that the device's layout cannot be used,
                // with the error its discovery would report.
                let Some(supplied) = pending.layouts[slot].take() else {
                    return -5;
                };
                let maps = take_maps(&mut pending.maps[slot], raw.service.into());
                pending.maps[slot] = Default::default();
                let (Some(maps), Some(reports)) = (maps, layout_reports(bytes)) else {
                    return -5;
                };
                let layout = Layout {
                    maps,
                    reports,
                    hash: hash(raw),
                };
                if layout == supplied {
                    return 1;
                }
                drop(supplied);
                match compile(&layout.maps) {
                    Ok(descriptors) => Event::Layout {
                        link,
                        descriptors,
                        layout,
                    },
                    Err(Error::Capacity) => return -2,
                    Err(_) => return -5,
                }
            }
            9 => {
                pending.descriptors[slot].clear();
                pending.layouts[slot] = None;
                pending.maps[slot] = Default::default();
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
    /// The transport is supported and enabled.
    fn enabled(&self, transport: Transport) -> bool {
        self.capabilities().supports(transport)
            && self.state.enabled[transport_index(transport)].get()
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
/// C's transport numbering: 0 Classic, 1 BLE.
fn transport_index(transport: Transport) -> usize {
    usize::from(transport == Transport::Ble)
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
        11 => Error::UnsupportedTransport,
        12 => Error::InvalidArgs,
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
    fn set_transport(&mut self, transport: Transport, enabled: bool) -> Result<(), Error> {
        if enabled && !self.capabilities().supports(transport) {
            return Err(Error::UnsupportedTransport);
        }
        let index = transport_index(transport);
        self.checked(|| unsafe { ffi::cordial_profiles_set_transport(index as u8, enabled) })?;
        self.state.enabled[index].set(enabled);
        Ok(())
    }
    fn scan(&mut self, id: u64, classic: bool, ble: bool) -> Result<(), Error> {
        if (classic && !self.enabled(Transport::Classic)) || (ble && !self.enabled(Transport::Ble))
        {
            return Err(Error::UnsupportedTransport);
        }
        self.checked(|| unsafe { ffi::cordial_profiles_scan(id, classic, ble) })
    }
    fn connect(
        &mut self,
        link: LinkId,
        peer: Peer,
        pairing: bool,
        layout: Option<&Layout>,
    ) -> Result<(), Error> {
        if !self.enabled(peer.transport) {
            return Err(Error::UnsupportedTransport);
        }
        if pairing {
            if self.state.pairing.get().is_some() {
                return Err(Error::Busy);
            }
            self.state.pairing.set(Some(link));
        }
        let cached = cached(layout.filter(|_| !pairing), Some(peer.transport));
        let native = cached.as_ref().map(|(layout, _)| Native::new(layout));
        let previous = self.state.activate(link, cached);
        let result = self.checked(|| unsafe {
            ffi::cordial_profiles_connect(
                raw_link(link),
                raw_peer(peer),
                pairing,
                native.as_ref().map_or(core::ptr::null(), |n| &n.raw),
            )
        });
        if result.is_err() {
            self.state.restore(link, previous);
            if pairing {
                self.pairing_ended(link);
            }
        }
        if result == Err(Error::StorageFailed) {
            self.disconnect(link);
        }
        result
    }
    fn incoming(
        &mut self,
        attempt: u32,
        accept: Option<LinkId>,
        layout: Option<&Layout>,
    ) -> Result<(), Error> {
        let id = accept.map(raw_link);
        // C reports a link it admits while this call runs: its slot and any
        // supplied layout must already be owned. C picks the layout matching
        // the attempt's transport.
        let cached = accept.and_then(|_| cached(layout, None));
        let native = cached.as_ref().map(|(layout, _)| Native::new(layout));
        let previous = accept.map(|link| self.state.activate(link, cached));
        let result = self.checked(|| unsafe {
            ffi::cordial_profiles_incoming(
                attempt,
                id.as_ref().map_or(core::ptr::null(), core::ptr::from_ref),
                native.as_ref().map_or(core::ptr::null(), |n| &n.raw),
            )
        });
        if result.is_err()
            && let (Some(link), Some(previous)) = (accept, previous)
        {
            self.state.restore(link, previous);
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
            hash: core::ptr::null(),
        }
    }
    const KEYBOARD: [u8; 21] = [
        5, 1, 9, 6, 0xa1, 1, 5, 7, 9, 4, 0x15, 0, 0x25, 1, 0x75, 1, 0x95, 1, 0x81, 2, 0xc0,
    ];
    const MOUSE: [u8; 21] = [
        5, 1, 9, 2, 0xa1, 1, 5, 9, 9, 1, 0x15, 0, 0x25, 1, 0x75, 1, 0x95, 1, 0x81, 2, 0xc0,
    ];
    fn map_event(generation: u64, service: u16, code: u8) -> ffi::Event {
        ffi::Event {
            service,
            code,
            ..raw(7, generation)
        }
    }
    fn table(reports: &[ffi::LayoutReport]) -> Vec<u8> {
        reports
            .iter()
            .flat_map(|r| {
                let mut bytes = [0; 8];
                bytes[..2].copy_from_slice(&r.value.to_ne_bytes());
                bytes[2..4].copy_from_slice(&r.cccd.to_ne_bytes());
                bytes[4..].copy_from_slice(&[r.properties, r.service, r.id, r.kind]);
                bytes
            })
            .collect()
    }
    const INPUT: ffi::LayoutReport = ffi::LayoutReport {
        value: 5,
        cccd: 7,
        properties: 0x12,
        service: 0,
        id: 1,
        kind: 1,
    };
    fn state() -> State<support::Store> {
        let storage = Box::leak(Box::new(Storage::new(support::Store::default())));
        let io = Box::leak(Box::new(Io::new()));
        State::new(storage, io, || 0, || panic!("fatal")).unwrap()
    }
    const LINK: LinkId = LinkId {
        slot: 0,
        generation: 1,
    };
    fn saved(maps: &[&[u8]]) -> Layout {
        Layout {
            maps: maps.iter().map(|m| ReportMap(m.to_vec())).collect(),
            reports: layout_reports(&table(&[INPUT])).unwrap(),
            hash: None,
        }
    }
    fn pop(state: &State<support::Store>) -> Option<Event> {
        state.pending.borrow_mut().events.pop_front()
    }
    #[test]
    fn native_link_errors_reach_the_application_unchanged() {
        let state = state();
        for (code, expected) in [
            (2, Error::Capacity),
            (3, Error::ConnectionFailed),
            (4, Error::AuthenticationFailed),
            (5, Error::UnsupportedHid),
            (11, Error::UnsupportedTransport),
            (12, Error::InvalidArgs),
        ] {
            state.activate(LINK, None);
            let ended = ffi::Event { code, ..raw(9, 1) };
            assert_eq!(state.copy_event(&ended, &[]), 1);
            assert!(matches!(
                pop(&state),
                Some(Event::Disconnected { error: Some(error), .. }) if error == expected
            ));
        }
    }
    #[test]
    fn native_report_capacity_matches_the_application_limit() {
        assert_eq!(
            usize::from(unsafe { ffi::cordial_layout_report_capacity }),
            cordial_core::bluetooth::LAYOUT_REPORTS
        );
    }
    #[test]
    fn discovered_links_report_their_layout() {
        let state = state();
        state.activate(LINK, None);
        assert_eq!(state.copy_event(&map_event(1, 0, 0), &KEYBOARD), 1);
        assert_eq!(state.copy_event(&map_event(1, 1, 0), &MOUSE), 1);
        let output = ffi::LayoutReport {
            value: 9,
            cccd: 0,
            properties: 0x08,
            service: 1,
            id: 2,
            kind: 2,
        };
        let digest = [9; 16];
        let connected = ffi::Event {
            number: 512,
            hash: digest.as_ptr(),
            ..raw(8, 1)
        };
        assert_eq!(state.copy_event(&connected, &table(&[INPUT, output])), 1);
        let Some(Event::Connected {
            descriptors,
            layout: Some(layout),
            ..
        }) = pop(&state)
        else {
            panic!("missing discovered layout");
        };
        assert_eq!(descriptors.len(), 2);
        assert_eq!(
            layout.maps,
            [ReportMap(KEYBOARD.to_vec()), ReportMap(MOUSE.to_vec())]
        );
        assert_eq!(
            layout.reports,
            [
                LayoutReport {
                    service: 0,
                    kind: ReportType::Input,
                    id: 1,
                    value: 5,
                    properties: 0x12,
                    cccd: 7,
                },
                LayoutReport {
                    service: 1,
                    kind: ReportType::Output,
                    id: 2,
                    value: 9,
                    properties: 0x08,
                    cccd: 0,
                },
            ]
        );
        assert!(layout.valid(Transport::Ble));
        assert_eq!(
            layout.hash,
            Some(cordial_core::bluetooth::DatabaseHash(digest))
        );
        // A Classic link's layout is its one report map.
        state.activate(LINK, None);
        assert_eq!(state.copy_event(&map_event(1, 0, 0), &KEYBOARD), 1);
        assert_eq!(state.copy_event(&raw(8, 1), &[]), 1);
        let Some(Event::Connected {
            layout: Some(layout),
            ..
        }) = pop(&state)
        else {
            panic!("missing Classic layout");
        };
        assert!(layout.valid(Transport::Classic));
        // A table that does not match the reported maps is refused.
        state.activate(LINK, None);
        assert_eq!(state.copy_event(&map_event(1, 1, 0), &KEYBOARD), 1);
        assert_eq!(state.copy_event(&raw(8, 1), &[]), -5);
        state.activate(LINK, None);
        assert_eq!(state.copy_event(&map_event(1, 0, 0), &KEYBOARD), 1);
        assert_eq!(state.copy_event(&raw(8, 1), &[0; 7]), -5);
    }
    #[test]
    fn saved_layouts_connect_at_once_and_report_verified_changes() {
        let state = state();
        let supplied = saved(&[&KEYBOARD]);
        // Unusable layouts leave discovery on.
        assert!(cached(Some(&supplied), Some(Transport::Classic)).is_none());
        assert!(cached(Some(&saved(&[&[0xc0]])), Some(Transport::Ble)).is_none());
        assert!(cached(Some(&Layout::default()), None).is_none());
        let classic = Layout {
            maps: supplied.maps.clone(),
            reports: Vec::new(),
            hash: None,
        };
        assert!(cached(Some(&classic), None).is_some());
        let native = Native::new(&classic);
        assert!(native.raw.reports.is_null() && native.raw.length == KEYBOARD.len() as u16);
        let native = Native::new(&supplied);
        assert!(native.raw.descriptor.is_null() && native.raw.count == 1);
        assert_eq!(unsafe { *native.raw.reports }, INPUT);

        state.activate(LINK, cached(Some(&supplied), Some(Transport::Ble)));
        let connected = ffi::Event {
            code: 1,
            ..raw(8, 1)
        };
        assert_eq!(state.copy_event(&connected, &[]), 1);
        let Some(Event::Connected {
            descriptors,
            layout: None,
            ..
        }) = pop(&state)
        else {
            panic!("missing Connected");
        };
        assert_eq!(descriptors[0].map.roles, hid::KEYBOARD);
        // An equal verification result needs no event.
        assert_eq!(state.copy_event(&map_event(1, 0, 1), &KEYBOARD), 1);
        let verified = ffi::Event {
            service: 1,
            ..raw(16, 1)
        };
        assert_eq!(state.copy_event(&verified, &table(&[INPUT])), 1);
        assert!(pop(&state).is_none());
        assert!(state.pending.borrow().layouts[0].is_none());

        // The Database Hash is part of the layout: the same hash needs no
        // event, and a hash-only change is reported.
        let digest = [7; 16];
        let hashed = Layout {
            hash: Some(cordial_core::bluetooth::DatabaseHash(digest)),
            ..supplied.clone()
        };
        let with_hash = ffi::Event {
            hash: digest.as_ptr(),
            ..verified
        };
        state.activate(LINK, cached(Some(&hashed), Some(Transport::Ble)));
        assert_eq!(state.copy_event(&connected, &[]), 1);
        pop(&state);
        assert_eq!(state.copy_event(&map_event(1, 0, 1), &KEYBOARD), 1);
        assert_eq!(state.copy_event(&with_hash, &table(&[INPUT])), 1);
        assert!(pop(&state).is_none());
        state.activate(LINK, cached(Some(&hashed), Some(Transport::Ble)));
        assert_eq!(state.copy_event(&connected, &[]), 1);
        pop(&state);
        assert_eq!(state.copy_event(&map_event(1, 0, 1), &KEYBOARD), 1);
        assert_eq!(state.copy_event(&verified, &table(&[INPUT])), 1);
        assert!(matches!(
            pop(&state),
            Some(Event::Layout { layout, .. }) if layout == supplied
        ));
        let native = {
            let moved = hashed.clone();
            Native::new(&moved)
        };
        assert_eq!(
            unsafe { *native.raw.hash.cast::<[u8; 16]>() },
            digest,
            "C receives the saved hash"
        );

        // A changed layout waits for queue room, then precedes later input.
        state.activate(LINK, cached(Some(&supplied), Some(Transport::Ble)));
        assert_eq!(state.copy_event(&connected, &[]), 1);
        assert_eq!(state.copy_event(&map_event(1, 0, 1), &MOUSE), 1);
        for _ in 0..EVENTS - 1 {
            assert_eq!(state.copy_event(&raw(10, 1), &[1]), 1);
        }
        assert_eq!(state.copy_event(&verified, &table(&[INPUT])), 0);
        state.pending.borrow_mut().events.clear();
        assert_eq!(state.copy_event(&verified, &table(&[INPUT])), 1);
        assert_eq!(state.copy_event(&raw(10, 1), &[2]), 1);
        let Some(Event::Layout {
            descriptors,
            layout,
            ..
        }) = pop(&state)
        else {
            panic!("missing Layout");
        };
        assert_eq!(descriptors[0].map.roles, hid::MOUSE);
        assert_eq!(layout, saved(&[&MOUSE]));
        assert!(matches!(pop(&state), Some(Event::Input(report)) if report.payload() == [2]));

        // A changed layout Cordial cannot use is refused with its error.
        state.activate(LINK, cached(Some(&supplied), Some(Transport::Ble)));
        assert_eq!(state.copy_event(&connected, &[]), 1);
        pop(&state);
        assert_eq!(state.copy_event(&map_event(1, 0, 1), &[0xc0]), 1);
        assert_eq!(state.copy_event(&verified, &table(&[INPUT])), -5);
        assert!(pop(&state).is_none());

        // Abandoned verification releases the supplied layout, even under
        // queue pressure.
        state.activate(LINK, cached(Some(&supplied), Some(Transport::Ble)));
        assert_eq!(state.copy_event(&connected, &[]), 1);
        assert_eq!(state.copy_event(&map_event(1, 0, 1), &KEYBOARD), 1);
        for _ in 0..EVENTS - 1 {
            assert_eq!(state.copy_event(&raw(10, 1), &[1]), 1);
        }
        let abandoned = ffi::Event {
            code: 1,
            ..raw(16, 1)
        };
        assert_eq!(state.copy_event(&abandoned, &[]), 1);
        assert!(state.pending.borrow().layouts[0].is_none());
        assert!(state.pending.borrow().maps[0].iter().all(Option::is_none));
        assert_eq!(state.pending.borrow().events.len(), EVENTS);
        state.pending.borrow_mut().events.clear();

        // Discovery replaces a supplied layout C did not use.
        state.activate(LINK, cached(Some(&supplied), None));
        assert_eq!(state.copy_event(&map_event(1, 0, 0), &MOUSE), 1);
        assert_eq!(state.copy_event(&raw(8, 1), &table(&[INPUT])), 1);
        assert!(matches!(
            pop(&state),
            Some(Event::Connected { layout: Some(layout), .. }) if layout == saved(&[&MOUSE])
        ));
        assert!(state.pending.borrow().layouts[0].is_none());
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
        state.activate(
            LinkId {
                slot: 0,
                generation: 2,
            },
            None,
        );
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
        state.activate(
            LinkId {
                slot: 0,
                generation: 1,
            },
            None,
        );
        assert_eq!(state.copy_event(&map_event(1, 0, 0), &KEYBOARD), 1);
        assert_eq!(state.copy_event(&map_event(1, 0, 0), &KEYBOARD), 0); // Duplicate service.
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
                assert_eq!(descriptors[0].service, ServiceId(0));
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
        state.activate(
            LinkId {
                slot: 0,
                generation: 2,
            },
            None,
        );
        assert_eq!(state.copy_event(&map_event(2, 0, 0), &MOUSE), 1);
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
