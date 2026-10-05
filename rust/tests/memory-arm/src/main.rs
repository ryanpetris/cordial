//! Execute real 32-bit core allocations under QEMU, without radio/USB hardware.
//!
//! The workloads run with one Pico board's configuration, against the heap span of its linked
//! development firmware. Saved records live in LittleFS over a static flash array outside the
//! measured heap, as they live in flash on the board. The heap holds what the firmware keeps in
//! RAM: its platform and Bluetooth state, the mounted filesystem, resident reconnection entries,
//! the state of connected devices, loaded profiles, and the transient buffers of reading and
//! writing records.
#![no_std]
#![no_main]
extern crate alloc;
mod discovery;
use alloc::{boxed::Box, format, vec, vec::Vec};
use cordial_core::model::{
    errors::ErrorCode as Error, hidpp::*, identifiers::Transport, link::PromptMethod, settings::*,
};
use cordial_core::{
    application::{Application, Build},
    bluetooth::{
        Bluetooth, Capabilities, Descriptor, Event, InputReport, Layout as HidLayout, LayoutReport,
        ReportMap, ReportType,
    },
    bonds::{Bond, Keys, Security},
    compact::{Metadata, Observed, Range, Record, scalar},
    devices::{AdapterPreference, Device, Live, Peer, Policies, Policy, Transports},
    interfaces::{Interface, InterfacePreference},
    link::{Link, LinkId, Profile, ServiceId, WriteId},
    manager::Connection,
    profiles::{self, Effect, KEYBOARD, KEYBOARD_PAGE, Output as RuleOutput, Rule, Rules},
    storage::{self, Preferences},
};
use cordial_protocol::{frame, request::Command};
use core::{
    alloc::{GlobalAlloc, Layout},
    cell::UnsafeCell,
    fmt::{self, Write},
};
use embassy_futures::block_on;
use embedded_storage_async::nor_flash::{ErrorType, NorFlash, NorFlashErrorKind, ReadNorFlash};
use prost::Message;
use talc::{TalcCell, source::Manual};
struct Heap {
    talc: TalcCell<Manual>,
    peak: usize,
    reject_after: usize,
    /// The host's side of the serial session, outside the measured heap.
    host: TalcCell<Manual>,
    hosting: bool,
}
struct Allocator(UnsafeCell<Heap>);
// This executable has one thread, interrupts disabled, and no asynchronous IRQs.
unsafe impl Sync for Allocator {}
#[global_allocator]
static HEAP: Allocator = Allocator(UnsafeCell::new(Heap {
    talc: TalcCell::new(Manual),
    peak: 0,
    reject_after: 0,
    host: TalcCell::new(Manual),
    hosting: false,
}));
const ARENA_BYTES: usize = board::HEAP_BYTES;
static mut ARENA: [u8; ARENA_BYTES] = [0; ARENA_BYTES];
const HOST_BYTES: usize = 64 * 1024;
static mut HOST: [u8; HOST_BYTES] = [0; HOST_BYTES];
fn hosted(p: *mut u8) -> bool {
    let start = core::ptr::addr_of!(HOST).addr();
    (start..start + HOST_BYTES).contains(&p.addr())
}
/// Runs `f` as the host: what it allocates is outside the measured heap.
fn host<R>(f: impl FnOnce() -> R) -> R {
    // Each write ends before `f` reaches the allocator.
    unsafe { (*HEAP.0.get()).hosting = true };
    let result = f();
    unsafe { (*HEAP.0.get()).hosting = false };
    result
}
unsafe impl GlobalAlloc for Allocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let h = unsafe { &mut *self.0.get() };
        if h.hosting {
            let p = unsafe { h.host.alloc(layout) };
            assert!(!p.is_null(), "the host arena is full");
            return p;
        }
        if h.reject_after != 0 {
            h.reject_after -= 1;
            if h.reject_after == 0 {
                return core::ptr::null_mut();
            }
        }
        let p = unsafe { h.talc.alloc(layout) };
        if p.is_null() {
            writeln!(
                Output,
                "allocation failed: {} bytes, live={}, overhead={}, available={}",
                layout.size(),
                h.talc.counters().allocated_bytes,
                h.talc.counters().overhead_bytes(),
                h.talc.counters().available_bytes
            )
            .ok();
            exit(1);
        }
        h.peak = h
            .peak
            .max(h.talc.counters().allocated_bytes + h.talc.counters().overhead_bytes());
        p
    }
    unsafe fn dealloc(&self, p: *mut u8, layout: Layout) {
        let h = unsafe { &mut *self.0.get() };
        if hosted(p) {
            unsafe { h.host.dealloc(p, layout) };
        } else {
            unsafe { h.talc.dealloc(p, layout) };
        }
    }
    unsafe fn realloc(&self, p: *mut u8, layout: Layout, n: usize) -> *mut u8 {
        let h = unsafe { &mut *self.0.get() };
        if hosted(p) {
            let next = unsafe { h.host.realloc(p, layout, n) };
            assert!(!next.is_null(), "the host arena is full");
            return next;
        }
        let next = unsafe { h.talc.realloc(p, layout, n) };
        if next.is_null() {
            writeln!(
                Output,
                "reallocation failed: {n} bytes, live={}, overhead={}, available={}",
                h.talc.counters().allocated_bytes,
                h.talc.counters().overhead_bytes(),
                h.talc.counters().available_bytes
            )
            .ok();
            exit(1);
        }
        // A moved realloc holds both blocks inside Talc. Include an upper
        // bound for the old block and its alignment/tag overhead in the peak.
        let extra = if next != p {
            layout.size() + layout.align() + 16
        } else {
            0
        };
        h.peak = h
            .peak
            .max(h.talc.counters().allocated_bytes + h.talc.counters().overhead_bytes() + extra);
        next
    }
}
fn call(number: u32, args: *const u32) {
    unsafe {
        core::arch::asm!("bkpt 0xab", inout("r0") number => _, in("r1") args, options(nostack));
    }
}
struct Output;
impl Write for Output {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for chunk in s.as_bytes().chunks(63) {
            let mut bytes = [0u8; 64];
            bytes[..chunk.len()].copy_from_slice(chunk);
            call(4, bytes.as_ptr().cast());
        }
        Ok(())
    }
}
fn largest_block(h: &Heap) -> usize {
    let mut low = 0;
    let mut high = h.talc.counters().available_bytes / 8;
    while low < high {
        let middle = (low + high).div_ceil(2);
        let layout = Layout::from_size_align(middle * 8, 8).unwrap();
        let ptr = unsafe { h.talc.alloc(layout) };
        if ptr.is_null() {
            high = middle - 1;
        } else {
            unsafe {
                h.talc.dealloc(ptr, layout);
            }
            low = middle;
        }
    }
    low * 8
}
fn stats(label: &str) -> usize {
    let h = unsafe { &*HEAP.0.get() };
    let largest = largest_block(h);
    let c = h.talc.counters();
    writeln!(
        Output,
        "{label}: live={} overhead={} peak_total={} available={} largest_block={largest}",
        c.allocated_bytes,
        c.overhead_bytes(),
        h.peak,
        c.available_bytes
    )
    .unwrap();
    c.allocated_bytes
}

fn exit(code: u32) -> ! {
    call(0x20, [0x20026, code].as_ptr());
    loop {
        core::hint::spin_loop();
    }
}
#[panic_handler]
fn panic(info: &core::panic::PanicInfo<'_>) -> ! {
    writeln!(Output, "{info}").ok();
    exit(1)
}

/// The board under test, from its generated configuration: `BOARD`, `HEAP_BYTES` and
/// `PROFILE_MEMORY_BUDGET`.
mod board {
    include!(concat!(env!("OUT_DIR"), "/board.rs"));
}
/// Saved devices: IDs 1..=8 are BLE and 9..=16 Classic. Each transport's stack holds eight bonds,
/// one kept free for pairing, so 1..=7 and 9..=15 are resident and 8 and 16 are not.
const DEVICES: u64 = 16;
const RESIDENT: usize = 14;
/// Saved profiles on a board with profiles, each a full VIA keymap: one remap rule for every
/// input the editor's matrix selects.
const PROFILES: u64 = 12;
/// Connected devices and their layers on a board with profiles; the VIA editor uses profile 1.
/// Together they load profiles 1..=7, within the budget. `OVER_BUDGET` gives device 9 layers that
/// need all twelve, which do not fit.
const CONNECTED: [(u64, [u64; 2]); 4] = [(1, [1, 2]), (2, [2, 3]), (3, [4, 5]), (9, [6, 7])];
const OVER_BUDGET: core::ops::RangeInclusive<u32> = 6..=12;
const EDITOR_PROFILE: u64 = 1;

/// The board's record storage: LittleFS over flash. The flash is a static array outside the
/// measured heap; the mounted filesystem's state and every transient buffer of reading and
/// writing records are on the heap, as on the board. A filesystem's heap use does not depend on
/// its size, so every board uses the Pico W's 1 MiB.
const STORAGE_BLOCKS: usize = 255;
const STORAGE_BYTES: usize = (STORAGE_BLOCKS + 1) * 4096;
static mut FLASH: [u8; STORAGE_BYTES] = [0; STORAGE_BYTES];
struct RamFlash;
impl RamFlash {
    fn bytes(&mut self) -> &'static mut [u8; STORAGE_BYTES] {
        // One thread, and each access ends before the next begins.
        unsafe { &mut *core::ptr::addr_of_mut!(FLASH) }
    }
}
impl ErrorType for RamFlash {
    type Error = NorFlashErrorKind;
}
impl ReadNorFlash for RamFlash {
    const READ_SIZE: usize = 1;
    async fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Self::Error> {
        let at = offset as usize;
        bytes.copy_from_slice(&self.bytes()[at..at + bytes.len()]);
        Ok(())
    }
    fn capacity(&self) -> usize {
        STORAGE_BYTES
    }
}
impl NorFlash for RamFlash {
    const WRITE_SIZE: usize = 1;
    const ERASE_SIZE: usize = 4096;
    async fn erase(&mut self, from: u32, to: u32) -> Result<(), Self::Error> {
        self.bytes()[from as usize..to as usize].fill(0xff);
        Ok(())
    }
    async fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Self::Error> {
        let at = offset as usize;
        for (to, from) in self.bytes()[at..at + bytes.len()].iter_mut().zip(bytes) {
            *to &= *from;
        }
        Ok(())
    }
}
/// The board's store: a bad layout leaves storage unavailable.
type Store = Result<cordial_record_storage::Storage<RamFlash, STORAGE_BLOCKS>, storage::Error>;
/// The application reaches storage through the BTstack adapter's handle, as on the board.
type Disk = cordial_btstack::storage::Handle<'static, Store>;

#[derive(Default)]
struct Radio {
    /// The link the last incoming connection was admitted as.
    accepted: Option<LinkId>,
    /// Whether that connection was given a saved layout.
    supplied: bool,
    scan: u64,
    /// Links whose Disconnected event the backend has yet to deliver.
    ended: [Option<(LinkId, Option<Error>)>; 8],
    /// The copy of its saved layout the backend keeps for each link, until it has verified the
    /// layout against the device or the link ends.
    layouts: [Option<HidLayout>; cordial_core::devices::ACTIVE_CONNECTIONS],
}
impl Radio {
    /// The backend keeps a copy of a saved layout it starts a link with.
    fn keep(&mut self, link: LinkId, layout: Option<&HidLayout>) {
        self.layouts[usize::from(link.slot)] = layout.cloned();
    }
    /// The backend verifies a link's saved layout: it reads each report map and the report table
    /// again, compares them with its copy and releases both. BLE links verify one at a time.
    fn verify(&mut self, link: LinkId) {
        let Some(supplied) = self.layouts[usize::from(link.slot)].take() else {
            return;
        };
        let mut read: [Option<Vec<u8>>; cordial_core::bluetooth::LAYOUT_SERVICES] =
            Default::default();
        for (to, map) in read.iter_mut().zip(&supplied.maps) {
            *to = Some(map.0.clone());
        }
        let layout = HidLayout {
            maps: read
                .iter_mut()
                .map_while(Option::take)
                .map(ReportMap)
                .collect(),
            reports: supplied.reports.clone(),
            hash: supplied.hash,
        };
        assert!(layout == supplied);
    }
    fn end(&mut self, link: LinkId, error: Option<Error>) {
        let free = self
            .ended
            .iter_mut()
            .find(|e| e.is_none())
            .expect("ended links");
        *free = Some((link, error));
    }
}
impl Bluetooth for Radio {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            classic: true,
            ble: true,
            ble_scan_and_connect: false,
        }
    }
    /// The Pico BTstack configuration's eight Classic link keys and eight LE device entries.
    fn bond_capacity(&self, _: Transport) -> usize {
        8
    }
    fn scan(&mut self, id: u64, _: bool, _: bool) -> Result<(), Error> {
        self.scan = id;
        Ok(())
    }
    fn reconnect(&mut self, _: &[Peer]) -> Result<(), Error> {
        Ok(())
    }
    /// Background paging finds no device: the page fails.
    fn connect(
        &mut self,
        link: LinkId,
        _: Peer,
        pairing: bool,
        layout: Option<&HidLayout>,
    ) -> Result<(), Error> {
        self.keep(link, layout.filter(|_| !pairing));
        self.end(link, Some(Error::ConnectionFailed));
        Ok(())
    }
    fn incoming(
        &mut self,
        _: u32,
        accept: Option<LinkId>,
        layout: Option<&HidLayout>,
    ) -> Result<(), Error> {
        self.accepted = accept;
        self.supplied = layout.is_some();
        if let Some(link) = accept {
            self.keep(link, layout);
        }
        Ok(())
    }
    fn disconnect(&mut self, link: LinkId) {
        self.end(link, None);
    }
    fn adopt(&mut self, _: LinkId) -> Result<(), Error> {
        Ok(())
    }
    fn pair_reply(
        &mut self,
        _: LinkId,
        _: PromptMethod,
        _: bool,
        _: Option<&str>,
    ) -> Result<(), Error> {
        Ok(())
    }
    fn can_write(&self, _: LinkId) -> bool {
        true
    }
    fn write(
        &mut self,
        _: WriteId,
        _: ServiceId,
        _: ReportType,
        _: Option<u8>,
        _: &[u8],
    ) -> Result<(), Error> {
        Ok(())
    }
    fn read(
        &mut self,
        _: WriteId,
        _: ServiceId,
        _: ReportType,
        _: Option<u8>,
    ) -> Result<(), Error> {
        Ok(())
    }
    async fn import_bond(&mut self, _: &Bond) -> Result<(), Error> {
        Ok(())
    }
    async fn bonds(&mut self) -> Result<Vec<Peer>, Error> {
        Ok(Vec::new())
    }
    async fn forget(&mut self, _: Peer) -> Result<(), Error> {
        Ok(())
    }
}

fn peer(id: u64) -> Peer {
    let ble = id <= DEVICES / 2;
    Peer {
        address: [if ble { id as u8 } else { 0x80 | id as u8 }; 6],
        random: false,
        transport: if ble {
            Transport::Ble
        } else {
            Transport::Classic
        },
    }
}
fn bond(id: u64) -> Bond {
    let identity = peer(id);
    Bond {
        owner: id,
        identity,
        complete: true,
        keys: if identity.transport == Transport::Classic {
            Keys::Classic {
                key: [42; 16],
                kind: 4,
            }
        } else {
            Keys::Ble {
                local: Security::default(),
                peer: Security {
                    flags: 7,
                    key_size: 16,
                    ltk: [42; 16],
                    irk: [43; 16],
                    csrk: [44; 16],
                    ..Security::default()
                },
            }
        },
    }
}
/// Whether the board supports profiles.
const PROFILES_SUPPORTED: bool = board::PROFILE_MEMORY_BUDGET.is_some();
/// The rules of a full VIA keymap: every input the editor's matrix selects remapped to a key,
/// every fourth with Left Control held too. The matrix's unused positions select no input.
fn via_rules(profile: u64) -> Rules {
    let key = |id: u16| RuleOutput {
        usage: profiles::usage(KEYBOARD_PAGE, id),
        collection: KEYBOARD,
    };
    let rules = (0..cordial_core::configurator::KEYS)
        .filter_map(cordial_core::configurator::input)
        .enumerate()
        .map(|(index, input)| {
            let mut outputs = vec![key(4 + ((index as u16 + profile as u16) % 161))];
            if index % 4 == 0 {
                outputs.push(key(0xe0));
            }
            Rule {
                input,
                effect: Effect::Remap(outputs),
            }
            .normalized()
            .unwrap()
        })
        .collect();
    Rules::new(rules).unwrap()
}
/// Saves what a long-used adapter holds: devices and, on a board with profiles, profiles the
/// devices use and the VIA interface's preference.
fn provision(store: &mut Disk) {
    block_on(storage::open(store)).unwrap();
    block_on(storage::initialized(store)).unwrap();
    if PROFILES_SUPPORTED {
        for profile in 1..=PROFILES {
            let (id, _) = block_on(profiles::create(
                store,
                &format!("VIA keymap {profile}"),
                &via_rules(profile),
            ))
            .unwrap();
            assert_eq!(id, profile);
        }
    }
    let mut transports = Transports::NONE;
    for transport in Transports::ALL {
        transports.set(transport, true);
    }
    block_on(Policies { store }.save_adapter(&AdapterPreference {
        transports,
        configuration_interfaces: if PROFILES_SUPPORTED {
            vec![InterfacePreference {
                interface: Interface::Via,
                enabled: true,
                profile: Some(EDITOR_PROFILE),
            }]
        } else {
            Vec::new()
        },
        ..AdapterPreference::default()
    }))
    .unwrap();
    for id in 1..=DEVICES {
        let mut policy = Policy::paired(id, peer(id), &[b'x'; 128]);
        policy.setup_pending = false;
        policy.bond = id;
        // Devices that stay disconnected hold the most layers a device can have.
        if PROFILES_SUPPORTED {
            policy.profiles = match CONNECTED.iter().find(|(device, _)| *device == id) {
                Some((_, layers)) => layers.to_vec(),
                None => (1..=profiles::MAX_LAYERS as u64).collect(),
            };
        }
        block_on(cordial_core::bonds::commit(store, &policy, &bond(id))).unwrap();
    }
}

fn records_for(maximum: bool) -> Vec<Record> {
    let keys: &[SettingKey] = if maximum {
        &SettingKey::ALL
    } else {
        &[SettingKey::FnRowDefault, SettingKey::BacklightEnabled]
    };
    keys.iter()
        .map(|&key| {
            use SettingKey::*;
            let feature = match key {
                FnRowDefault => FeatureId::FN_INVERSION,
                KeyboardPlatform => FeatureId::DUAL_PLATFORM,
                PowerAutoOff => FeatureId::ADC_MEASUREMENT,
                PointerDpi0 | PointerDpi1 => FeatureId::ADJUSTABLE_DPI,
                WheelMode | WheelThreshold => FeatureId::SMART_SHIFT,
                WheelInvert => FeatureId::HIRES_WHEEL,
                ThumbwheelInvert => FeatureId::THUMBWHEEL,
                _ => FeatureId::BACKLIGHT,
            };
            let mut row = Record::new(
                Metadata {
                    key,
                    feature,
                    revision: FeatureRevision(3),
                    scope: SettingScope::Device,
                    choices: match key.kind() {
                        SettingType::Enum => key
                            .enum_values()
                            .iter()
                            .enumerate()
                            .filter(|(_, v)| {
                                key != BacklightMode
                                    || ["automatic", "permanent_manual"].contains(v)
                            })
                            .map(|(n, _)| n as u16 + u16::from(key == WheelMode))
                            .collect(),
                        SettingType::Integer if matches!(key, PointerDpi0 | PointerDpi1) => {
                            (1..=6).collect()
                        }
                        _ => Box::new([]),
                    },
                    range: None,
                },
                key.writable_feature(feature, FeatureRevision(3)),
            );
            if key.kind() == SettingType::Integer && row.metadata.choices.is_empty() {
                let (min, max, step) = match key {
                    BacklightLevel => (0, 7, 1),
                    BacklightDelayHandsIn | BacklightDelayHandsOut | BacklightDelayPowered => {
                        (5, 7200, 5)
                    }
                    WheelThreshold => (1, 255, 1),
                    PowerAutoOff => (0, 15300, 60),
                    BacklightCurrentLevel => (0, 254, 1),
                    _ => unreachable!(),
                };
                row.metadata.range = Some(Range { min, max, step });
            }
            row.observed = if key.kind() == SettingType::Text {
                Observed::Text("\\".repeat(64).into())
            } else {
                Observed::Number(row.metadata.choices.first().copied().unwrap_or(1))
            };
            row.fresh = true;
            row
        })
        .collect()
}
fn features(maximum: bool) -> Vec<Feature> {
    (0..if maximum { 256 } else { 35 })
        .map(|i| Feature {
            index: FeatureIndex(i as u8),
            id: FeatureId(i),
            version: FeatureRevision(3),
            flags: FeatureFlags(0),
            supported: false,
        })
        .collect()
}
fn descriptor(maximum: bool, reports: usize) -> Vec<u8> {
    let mut descriptor = vec![
        5, 1, 9, 6, 0xa1, 1, 5, 7, 0x15, 0, 0x25, 1, 0x75, 1, 0x95, 1,
    ];
    for field in 0..if maximum { 96 } else { 1 } {
        if maximum && field % (96 / reports) == 0 {
            descriptor.extend_from_slice(&[0x85, (field / (96 / reports) + 1) as u8]);
        }
        if maximum && field == 0 {
            descriptor.extend_from_slice(&[0x95, 2]);
        }
        if maximum && field == 32 {
            descriptor.extend_from_slice(&[0x95, 1]);
        }
        if maximum && field < 32 {
            descriptor.extend_from_slice(&[9, 5]);
        }
        descriptor.extend_from_slice(&[9, 4, 0x81, 2]);
    }
    if maximum {
        while descriptor.len() + 3 <= 2048 {
            descriptor.extend_from_slice(&[5, 7]);
        }
    }
    if maximum && descriptor.len() == 2046 {
        descriptor.push(0x04);
    }
    assert!(descriptor.len() < 2048);
    descriptor.push(0xc0);
    descriptor
}
static mut MAX_DESCRIPTOR: [u8; 2048] = [0; 2048];
/// The real C callback supplies its descriptors from static storage.
fn max_descriptor() -> &'static [u8] {
    unsafe { &*core::ptr::addr_of!(MAX_DESCRIPTOR) }
}
/// The services of a device: BLE devices expose three HID services, Classic devices one.
fn services(transport: Transport) -> u16 {
    if transport == Transport::Ble { 3 } else { 1 }
}
/// The layout a backend discovers, the largest a saved layout file holds.
fn discovered(transport: Transport) -> HidLayout {
    let services = services(transport);
    HidLayout {
        maps: (0..services)
            .map(|_| ReportMap(max_descriptor().to_vec()))
            .collect(),
        reports: if transport == Transport::Ble {
            (0..services)
                .flat_map(|service| {
                    (1..=10u8).map(move |id| LayoutReport {
                        service,
                        kind: ReportType::Input,
                        id,
                        value: 0x100 * (service + 1) + u16::from(id) * 4,
                        properties: 0x12,
                        cccd: 0x100 * (service + 1) + u16::from(id) * 4 + 1,
                    })
                })
                .collect()
        } else {
            Vec::new()
        },
        hash: (transport == Transport::Ble)
            .then_some(cordial_core::bluetooth::DatabaseHash([7; 16])),
    }
}

struct Fixture {
    /// The firmware allocates the application, shared by its two loops, on the heap.
    app: Box<Application>,
    store: Disk,
    radio: Radio,
    now: u64,
    /// The request for the next page of the last list the host read.
    next: Option<Command>,
}
impl Fixture {
    fn event(&mut self, event: Event) {
        block_on(
            self.app
                .event(event, &mut self.store, &mut self.radio, self.now),
        );
    }
    fn poll(&mut self, count: usize) {
        for _ in 0..count {
            block_on(self.app.poll(&mut self.store, &mut self.radio, 0, self.now));
            self.ended();
            self.usb();
            self.drain();
        }
    }
    /// The USB host takes every HID report the adapter has ready.
    fn usb(&mut self) {
        while self.app.manager.forward.packet().is_some() {
            self.app.manager.forward.complete();
        }
    }
    /// The device sends a key press and its release on its first service.
    fn type_key(&mut self, id: u64) {
        let slot = self.slot(id);
        let link = self.app.manager.link_for(slot).expect("connected device");
        for payload in [[1, 0], [0, 0]] {
            let report = InputReport::new(link, ServiceId(0), 1, &payload).unwrap();
            self.event(Event::Input(report));
        }
        assert!(
            self.app.manager.link_for(slot).is_some(),
            "device {id} kept its link"
        );
    }
    /// Delivers the Disconnected events of links the backend has ended.
    fn ended(&mut self) {
        while let Some((link, error)) = self.radio.ended.iter_mut().find_map(Option::take) {
            self.radio.layouts[usize::from(link.slot)] = None;
            self.event(Event::Disconnected { link, error });
        }
    }
    /// The host sends one request; the adapter reads and answers it.
    fn send(&mut self, command: Command) {
        let bytes = host(|| {
            let mut bytes = Vec::new();
            frame::encode(
                &cordial_protocol::Request {
                    command: Some(command),
                },
                &mut bytes,
            );
            bytes
        });
        let (_, request) = self.app.serial.feed(&bytes);
        drop(bytes);
        block_on(
            self.app
                .dispatch(request.unwrap(), &mut self.store, &mut self.radio, self.now),
        );
        self.drain();
    }
    /// The host sends a list request and then one for each further page.
    fn list(&mut self, command: Command) {
        self.send(command);
        while let Some(next) = self.next.take() {
            self.send(next);
        }
    }
    /// Writes out everything queued, returning the number of scan candidates reported. The host
    /// notes the request for the next page of a list response that does not end.
    fn drain(&mut self) -> usize {
        use cordial_protocol::{event, message::Kind, response};
        let mut decoder = host(|| frame::Decoder::new(None));
        let mut candidates = 0;
        let next = &mut self.next;
        while let Some((token, bytes)) = self.app.serial.output(64) {
            let len = bytes.len();
            host(|| {
                for &b in bytes {
                    let Some(frame) = decoder.push(b) else {
                        continue;
                    };
                    let message = cordial_protocol::Message::decode(frame.unwrap()).unwrap();
                    match message.kind {
                        Some(Kind::Event(cordial_protocol::Event {
                            kind: Some(event::Kind::ScanFound(_)),
                        })) => candidates += 1,
                        Some(Kind::Response(cordial_protocol::Response {
                            result: Some(response::Result::Settings(page)),
                        })) if !page.end => {
                            let last = page.settings.last().expect("a page that ends early");
                            *next = Some(Command::ListSettings(cordial_protocol::ListSettings {
                                device: page.device,
                                after: Some(cordial_protocol::SettingRef {
                                    integration: last.integration,
                                    key: last.key.clone(),
                                }),
                            }));
                        }
                        Some(Kind::Response(cordial_protocol::Response {
                            result: Some(response::Result::ProfileRules(page)),
                        })) if !page.end => {
                            let last = page.rules.last().expect("a page that ends early");
                            *next = Some(Command::ListProfileRules(
                                cordial_protocol::ListProfileRules {
                                    profile: page.profile,
                                    after: Some(last.input.expect("a rule's input")),
                                },
                            ));
                        }
                        _ => {}
                    }
                }
            });
            self.app.serial.output_complete(token, len);
        }
        candidates
    }
    fn slot(&self, id: u64) -> usize {
        self.app.manager.find(id).expect("resident device")
    }
    fn live(&mut self, id: u64) -> &mut Live {
        let slot = self.slot(id);
        self.app.manager.devices[slot]
            .as_mut()
            .unwrap()
            .live
            .as_deref_mut()
            .expect("connected device")
    }
    /// The device connects to the adapter, which admits it with its saved layout when it has
    /// one. Its profiles load before its first input. A device that is `typing` sends input at
    /// once; another sends none before the adapter stops waiting for it.
    fn connect(&mut self, id: u64, typing: bool) {
        let peer = peer(id);
        self.radio.accepted = None;
        self.event(Event::Incoming {
            attempt: id as u32,
            peer,
        });
        let link = self.radio.accepted.take().expect("admitted connection");
        let descriptors = (0..services(peer.transport))
            .map(|service| Descriptor::from_slice(ServiceId(service), max_descriptor()).unwrap())
            .collect();
        let layout = (!self.radio.supplied).then(|| discovered(peer.transport));
        self.event(Event::Connected {
            link,
            descriptors,
            max_output: 512,
            layout,
        });
        assert!(self.live(id).profile_error.is_none());
        // Background storage work waits for the connection's first input, which forwards at
        // once; then the connection reads the policy and saved preferences.
        self.poll(2);
        assert!(self.live(id).policy.is_none());
        if typing {
            self.type_key(id);
        } else {
            self.now += cordial_core::manager::FIRST_INPUT_WAIT_MS;
        }
        self.poll(8);
        assert!(self.live(id).policy.is_some());
    }
    fn disconnect(&mut self, id: u64) {
        let slot = self.slot(id);
        let link = self.app.manager.link_for(slot).expect("connected device");
        self.radio.layouts[usize::from(link.slot)] = None;
        self.event(Event::Disconnected { link, error: None });
        assert!(
            self.app.manager.devices[slot]
                .as_ref()
                .unwrap()
                .live
                .is_none()
        );
    }
    /// The backend verifies the saved layouts of connected devices, after their connections
    /// have started up.
    fn verify(&mut self, ids: &[u64]) {
        for &id in ids {
            let slot = self.slot(id);
            let link = self.app.manager.link_for(slot).expect("connected device");
            self.radio.verify(link);
        }
    }
    /// One VIA packet; returns the reply.
    fn via(&mut self, packet: &[u8]) -> [u8; 32] {
        let mut data = [0; 32];
        data[..packet.len()].copy_from_slice(packet);
        block_on(
            self.app
                .configure(Interface::Via, data, &mut self.store, self.now),
        )
    }
    fn discover(&mut self, ids: &[u64], maximum: bool) {
        for &id in ids {
            let catalog = &mut self.live(id).catalog;
            if maximum {
                discovery::run(catalog);
            } else {
                catalog
                    .replace_discovery(records_for(false), features(false))
                    .unwrap();
            }
        }
    }
    /// Saves `saved` preferences of each connected device, as SetSettings does.
    fn save_preferences(&mut self, ids: &[u64], saved: usize) {
        for &id in ids {
            let live = self.live(id);
            let mut writable: Vec<_> = live
                .catalog
                .records()
                .iter()
                .filter(|r| r.writable)
                .collect();
            writable.sort_by_key(|r| core::cmp::Reverse(r.metadata.choices.len()));
            let chosen: Vec<_> = writable
                .into_iter()
                .take(saved)
                .map(|r| {
                    (
                        r.metadata.key,
                        r.metadata
                            .choices
                            .first()
                            .copied()
                            .unwrap_or_else(|| r.metadata.range.map_or(1, |v| v.min)),
                    )
                })
                .collect();
            assert_eq!(chosen.len(), saved);
            for (key, value) in chosen {
                let slot = self.slot(id);
                let live = self.app.manager.devices[slot]
                    .as_mut()
                    .unwrap()
                    .live
                    .as_deref_mut()
                    .unwrap();
                block_on(live.catalog.set(
                    key,
                    scalar(key, value),
                    &mut Preferences {
                        store: &mut self.store,
                        device: id,
                    },
                ))
                .unwrap();
            }
        }
    }
    fn observations(&mut self, ids: &[u64], maximum: bool) {
        // Real read/event handlers mutate individual rows in the live catalog.
        for &id in ids {
            let catalog = &mut self.live(id).catalog;
            catalog.invalidate();
            if maximum {
                use cordial_core::model::info::InfoKey as I;
                catalog.info.battery.configure(Transport::Ble, false);
                for instance in 0..4 {
                    catalog.info.battery.gatt(0x2a19, instance, &[50]);
                    catalog.info.battery.gatt(0x2bed, instance, &[0, 0x21, 0]);
                    catalog.info.battery.gatt(0x2bf0, instance, &[0]);
                    catalog.info.battery.gatt(0x2be9, instance, &[1]);
                }
                for key in I::ALL {
                    for instance in 0..key.instances() {
                        let value = match key {
                            I::BatteryPercent => SettingValue::Integer(50),
                            I::BatteryCharging => SettingValue::Bool(true),
                            I::VendorIdNamespace => SettingValue::Text("usb".into()),
                            I::VendorId | I::ProductId | I::ProductVersion => {
                                SettingValue::Integer(65535)
                            }
                            I::Kind => SettingValue::Text("keyboard".into()),
                            _ => SettingValue::Text("\\".repeat(64)),
                        };
                        catalog.info.observe(false, key, instance, value.clone());
                        catalog.info.observe(true, key, instance, value);
                    }
                }
                catalog.info.changes();
            }
            for key in if maximum {
                &SettingKey::ALL[..]
            } else {
                &[SettingKey::FnRowDefault, SettingKey::BacklightEnabled]
            } {
                let row = catalog
                    .records()
                    .iter()
                    .find(|r| r.metadata.key == *key)
                    .unwrap();
                let value = row.observed.wire(*key);
                catalog
                    .observe(*key, value, 4, ObservationSource::Read)
                    .unwrap();
            }
        }
    }
    fn candidates(&mut self) {
        self.send(Command::StartScan(cordial_protocol::StartScan {
            transports: vec![cordial_protocol::Transport::Ble as i32],
            seconds: 60,
        }));
        for n in 0..32 {
            let peer = Peer {
                address: [0x40 | n; 6],
                random: false,
                transport: Transport::Ble,
            };
            self.event(Event::Found {
                kind: cordial_core::model::link::DeviceKind::Unknown,
                connectable: true,
                scan: self.radio.scan,
                address: Some(peer),
                peer,
                name: "\\".repeat(128).into(),
                rssi: Some(-127),
            });
        }
        // One event goes out per poll, and saved devices may still have events waiting.
        let mut found = 0;
        for _ in 0..96 {
            block_on(self.app.poll(&mut self.store, &mut self.radio, 0, self.now));
            self.ended();
            found += self.drain();
        }
        assert_eq!(found, 32);
        self.send(Command::StopScan(cordial_protocol::StopScan {}));
    }
    fn layers(&mut self, id: u64, profiles: impl IntoIterator<Item = u32>) {
        self.send(Command::SetDevice(cordial_protocol::SetDevice {
            device: id as u32,
            profiles: Some(cordial_protocol::ProfileLayers {
                profiles: profiles.into_iter().collect(),
            }),
            ..Default::default()
        }));
    }
    /// Loaded profiles stay within the budget, and a device whose layers do not fit passes its
    /// input through until they do.
    fn budget(&mut self) {
        let budget = board::PROFILE_MEMORY_BUDGET.unwrap() as usize;
        assert!(self.app.manager.profiles.used() <= budget);
        self.app.session(true, &mut self.radio);
        self.layers(9, OVER_BUDGET);
        assert_eq!(self.live(9).profile_error, Some(Error::Capacity));
        assert!(self.app.manager.profiles.used() <= budget);
        stats("layers over budget");
        self.layers(9, CONNECTED[3].1.map(|id| id as u32));
        assert_eq!(self.live(9).profile_error, None);
        assert!(self.app.manager.profiles.used() <= budget);
        self.app.session(false, &mut self.radio);
    }
    /// Reconnects connected devices: Classic device 9 and one BLE device in turn. A BLE link
    /// starts only while one connection stays free for pairing, so device 9 leaves first.
    fn cycle(&mut self, ids: &[u64], turn: usize) {
        let ble = ids[turn % ids.len().min(3)];
        let classic = ids.contains(&9);
        if classic {
            self.disconnect(9);
        }
        self.disconnect(ble);
        self.connect(ble, true);
        if classic {
            self.connect(9, false);
        }
    }
    fn commands(&mut self, ids: &[u64], maximum: bool, turn: usize) {
        // Long enough for the idle editor to release its profile, and for no drop to be rapid.
        self.now += 10_000;
        self.poll(4);
        self.cycle(ids, turn);
        // A reconnected catalog holds only its saved preferences until discovery runs again.
        self.discover(ids, maximum);
        self.app.session(true, &mut self.radio);
        self.send(Command::GetStatus(cordial_protocol::GetStatus {}));
        // A frame that is not a request is answered without reaching the application.
        let invalid = [3, 0xff, 0xff, 0];
        assert_eq!(self.app.serial.feed(&invalid), (invalid.len(), None));
        self.drain();
        if maximum {
            self.candidates();
        }
        for after in [0, 8] {
            self.send(Command::ListDevices(cordial_protocol::ListDevices {
                after,
            }));
        }
        self.send(Command::GetDevice(cordial_protocol::GetDevice {
            device: DEVICES as u32,
        }));
        for &id in ids.iter().chain(&[4]) {
            self.list(Command::ListSettings(cordial_protocol::ListSettings {
                device: id as u32,
                after: None,
            }));
        }
        if PROFILES_SUPPORTED {
            self.send(Command::ListProfiles(cordial_protocol::ListProfiles {
                after: 0,
            }));
            self.send(Command::GetProfile(cordial_protocol::GetProfile {
                profile: 1,
            }));
            self.list(Command::ListProfileRules(
                cordial_protocol::ListProfileRules {
                    profile: PROFILES as u32,
                    after: None,
                },
            ));
            // The VIA editor reads a key, changes one and reads a block of the keymap.
            assert_ne!(self.via(&[0x04, 0, 0, 0])[0], 0xff);
            let code = 4 + (turn % 2) as u8;
            assert_ne!(self.via(&[0x05, 0, 0, 1, 0, code])[0], 0xff);
            assert_ne!(self.via(&[0x12, 0, 0, 28])[0], 0xff);
        } else {
            // Without profiles, the configuration interfaces answer nothing.
            assert_eq!(self.via(&[0x04, 0, 0, 0])[0], 0xff);
        }
        for _ in 0..3 {
            self.send(Command::GetStatus(cordial_protocol::GetStatus {}));
        }
        self.observations(ids, maximum);
        // Retain a full output queue while observations change live records.
        block_on(self.app.poll(&mut self.store, &mut self.radio, 0, self.now));
        self.ended();
        self.observations(ids, maximum);
        unsafe {
            (*HEAP.0.get()).reject_after = 1;
        }
        assert!(matches!(
            Descriptor::from_slice(ServiceId(0), max_descriptor()),
            Err(Error::Capacity)
        ));
        self.poll(128);
        assert!(Descriptor::from_slice(ServiceId(0), max_descriptor()).is_ok());
        self.verify(ids);
        self.app.session(false, &mut self.radio);
    }
}

#[cortex_m_rt::entry]
fn main() -> ! {
    unsafe {
        core::arch::asm!("cpsid i");
        assert!(
            (*HEAP.0.get())
                .talc
                .claim(core::ptr::addr_of_mut!(ARENA).cast(), ARENA_BYTES)
                .is_some()
        );
        assert!(
            (*HEAP.0.get())
                .host
                .claim(core::ptr::addr_of_mut!(HOST).cast(), HOST_BYTES)
                .is_some()
        );
    }
    let template = descriptor(true, 16);
    unsafe {
        (*core::ptr::addr_of_mut!(MAX_DESCRIPTOR)).copy_from_slice(&template);
    }
    drop(template);
    let profile = via_rules(1).memory();
    writeln!(
        Output,
        "sizes record={} metadata={} preference={} feature={} profile={} link={} application={} device={} live={} policy={} connection={} via_profile={profile}",
        size_of::<Record>(),
        size_of::<Metadata>(),
        size_of::<cordial_core::compact::Preference>(),
        size_of::<cordial_core::compact::FeatureEntry>(),
        size_of::<Profile>(),
        size_of::<Link>(),
        size_of::<Application>(),
        size_of::<Device>(),
        size_of::<Live>(),
        size_of::<Policy>(),
        size_of::<Connection>(),
    )
    .unwrap();
    writeln!(
        Output,
        "board {} heap={ARENA_BYTES} profile budget {:?}",
        board::BOARD,
        board::PROFILE_MEMORY_BUDGET
    )
    .unwrap();
    if let Some(budget) = board::PROFILE_MEMORY_BUDGET {
        // Profiles 1..=7 fit in the budget together and all twelve do not.
        let budget = budget as usize;
        assert!(
            7 * profile <= budget && budget < PROFILES as usize * profile,
            "a full VIA profile takes {profile} bytes; the budget scenario needs another layout"
        );
    }
    // What the firmware allocates at startup and keeps: the USB serial number, the mounted
    // filesystem, the storage and Bluetooth state, the chipset and the application's owner.
    let serial: *mut str = Box::into_raw("E6613008E35A4733".into());
    unsafe {
        core::ptr::addr_of_mut!(FLASH)
            .cast::<u8>()
            .write_bytes(0xff, STORAGE_BYTES);
    }
    let records = block_on(cordial_record_storage::Storage::open_or_provision_blank(
        RamFlash,
        0..STORAGE_BYTES as u32,
        [0x5a; cordial_record_storage::MARKER_BYTES],
    ));
    let storage = Box::into_raw(Box::new(cordial_btstack::storage::Storage::new(records)));
    // The firmware never frees its storage; the fixture frees it once nothing uses it.
    let mut store = unsafe { &*storage }.handle();
    // The records a long-used adapter boots with.
    provision(&mut store);
    static RADIO_IO: cordial_btstack::transport::Io = cordial_btstack::transport::Io::new();
    let backend =
        cordial_btstack::backend::State::new(unsafe { &*storage }, &RADIO_IO, || 0, || exit(1))
            .unwrap();
    let backend = Box::new(backend);
    let chipset = Box::new(cordial_btstack::ffi::Chipset {
        name: core::ptr::null(),
        init: None,
        next_command: None,
        set_baudrate: None,
        set_address: None,
    });
    core::hint::black_box(&chipset);
    let mut fixture = Fixture {
        app: Box::new(Application::new(Build {
            development: true,
            version: "0.0.0",
            board: board::BOARD,
            default_adapter_name: "Test adapter",
            adapter_id: unsafe { &*serial }.into(),
            profile_memory_budget: board::PROFILE_MEMORY_BUDGET,
            bootloader: None,
        })),
        store,
        radio: Radio::default(),
        now: 1,
        next: None,
    };
    // The firmware's two loops share the application, its store and the Bluetooth backend in one
    // heap allocation; this holds the rest of it beside the application's own.
    type Owner = cordial_usb::owner::Owner<'static, Disk, cordial_btstack::backend::Backend<Store>>;
    let owner = vec![0u8; size_of::<Owner>() - size_of::<Application>()];
    core::hint::black_box(&owner);
    stats("platform, storage, Bluetooth state and application");
    // USB is configured and the host reads its input reports.
    fixture.app.manager.forward.enable(true);
    fixture.event(Event::Ready);
    assert!(fixture.app.manager.storage_ready && fixture.app.manager.radio_ready);
    let resident: Vec<u64> = fixture
        .app
        .manager
        .devices
        .iter()
        .flatten()
        .map(|d| d.id)
        .collect();
    assert_eq!(resident.len(), RESIDENT);
    assert!(!resident.contains(&(DEVICES / 2)) && !resident.contains(&DEVICES));
    drop(resident);
    assert!(
        fixture
            .app
            .manager
            .devices
            .iter()
            .flatten()
            .all(|d| d.live.is_none())
    );
    assert_eq!(fixture.app.manager.profiles.used(), 0);
    stats("resident entries");
    for (label, count, saved, maximum) in [
        ("one device, no preferences", 1, 0, false),
        ("one device, two preferences", 1, 2, false),
        ("four devices, sixteen preferences", 4, 16, true),
    ] {
        let ids: Vec<u64> = CONNECTED[..count].iter().map(|(id, _)| *id).collect();
        for &id in &ids {
            fixture.connect(id, true);
        }
        stats("connected");
        fixture.discover(&ids, maximum);
        fixture.save_preferences(&ids, saved);
        if maximum {
            // Every device reconnects at once, as after the adapter restarts: each starts with its
            // saved layout, which the backend keeps until it verifies it.
            for &id in &ids {
                fixture.disconnect(id);
            }
            for &id in &ids {
                fixture.connect(id, true);
            }
            fixture.discover(&ids, maximum);
        }
        stats("catalogs");
        if let Some(budget) = board::PROFILE_MEMORY_BUDGET {
            assert_ne!(fixture.via(&[0x04, 0, 0, 0])[0], 0xff);
            for (index, &id) in ids.iter().enumerate() {
                let slot = fixture.slot(id);
                let link = fixture.app.manager.link_for(slot).unwrap();
                assert_eq!(
                    fixture.app.manager.forward.layers(link.slot as usize).len(),
                    2
                );
                assert_eq!(fixture.live(id).profile_error, None, "device {index}");
            }
            assert!(fixture.app.manager.profiles.used() <= budget as usize);
            stats("profiles loaded");
            if maximum {
                fixture.budget();
            }
        }
        fixture.verify(&ids);
        // Reconnecting reads the saved layout, policy, preferences and profiles back.
        fixture.commands(&ids, maximum, 0);
        stats(label);
        fixture.commands(&ids, maximum, 1);
        let steady = stats("warm steady state");
        for turn in 2..=101 {
            fixture.commands(&ids, maximum, turn);
        }
        assert_eq!(stats("after refresh/commands"), steady);
        assert!(largest_block(unsafe { &*HEAP.0.get() }) >= 4096);
        for &id in &ids {
            fixture.disconnect(id);
        }
        // The idle editor releases its profile; nothing else holds one.
        fixture.now += 10_000;
        fixture.poll(4);
        assert_eq!(fixture.app.manager.profiles.used(), 0);
        stats("after disconnect");
    }
    drop(fixture);
    drop(owner);
    drop(chipset);
    drop(backend);
    unsafe {
        drop(Box::from_raw(storage));
        drop(Box::from_raw(serial));
    }
    assert_eq!(stats("all allocations dropped"), 0);
    exit(0)
}
