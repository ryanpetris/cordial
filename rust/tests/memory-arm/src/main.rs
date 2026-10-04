//! Execute real 32-bit core allocations under QEMU, without radio/USB hardware.
#![no_std]
#![no_main]
extern crate alloc;
mod discovery;
use alloc::{boxed::Box, vec, vec::Vec};
use core::{
    alloc::{GlobalAlloc, Layout},
    cell::UnsafeCell,
    fmt::{self, Write},
};
use embassy_futures::block_on;
use cordial_core::{
    application::{Application, Build},
    bluetooth::{Bluetooth, Capabilities, Descriptor, Event, Layout as HidLayout, ReportType},
    compact::{Metadata, Observed, Range, Record, scalar},
    devices::{Device, Peer, Policy},
    link::{Link, LinkId, Profile, ServiceId, WriteId},
    manager::Connection,
    storage::{self, Preferences, RecordKey, RecordStore},
};
use cordial_core::model::{
    errors::ErrorCode as Error, hidpp::*, identifiers::Transport, link::PromptMethod,
    settings::*,
};
use cordial_protocol::{frame, request::Command};
use prost::Message;
use talc::{TalcCell, source::Manual};
struct Heap {
    talc: TalcCell<Manual>,
    peak: usize,
    reject_after: usize,
}
struct Allocator(UnsafeCell<Heap>);
// This executable has one thread, interrupts disabled, and no asynchronous IRQs.
unsafe impl Sync for Allocator {}
#[global_allocator]
static HEAP: Allocator = Allocator(UnsafeCell::new(Heap {
    talc: TalcCell::new(Manual),
    peak: 0,
    reject_after: 0,
}));
const ARENA_BYTES: usize = include!(concat!(env!("OUT_DIR"), "/budget.rs"));
static mut ARENA: [u8; ARENA_BYTES] = [0; ARENA_BYTES];
unsafe impl GlobalAlloc for Allocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let h = unsafe { &mut *self.0.get() };
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
        unsafe { (*self.0.get()).talc.dealloc(p, layout) };
    }
    unsafe fn realloc(&self, p: *mut u8, layout: Layout, n: usize) -> *mut u8 {
        let h = unsafe { &mut *self.0.get() };
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
struct Store;
impl RecordStore for Store {
    async fn keys(&mut self) -> Result<alloc::vec::Vec<RecordKey>, storage::Error> {
        Ok(alloc::vec::Vec::new())
    }
    async fn available(&mut self) -> Result<usize, storage::Error> {
        Ok(65536)
    }

    async fn load(&mut self, _: RecordKey, _: &mut [u8]) -> Result<Option<usize>, storage::Error> {
        Ok(None)
    }
    async fn save(&mut self, _: RecordKey, bytes: &[u8]) -> Result<(), storage::Error> {
        core::hint::black_box(bytes);
        Ok(())
    }
    async fn remove(&mut self, _: RecordKey) -> Result<(), storage::Error> {
        Ok(())
    }
}
struct Radio;
impl Bluetooth for Radio {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            classic: true,
            ble: true,
            ble_scan_and_connect: false,
        }
    }
    fn scan(&mut self, _: u64, _: bool, _: bool) -> Result<(), Error> {
        Ok(())
    }
    fn reconnect(&mut self, _: &[Peer]) -> Result<(), Error> { Ok(()) }
    fn connect(&mut self, _: LinkId, _: Peer, _: bool, _: Option<&HidLayout>) -> Result<(), Error> {
        Ok(())
    }
    fn incoming(&mut self, _: u32, _: Option<LinkId>, _: Option<&HidLayout>) -> Result<(), Error> {
        Ok(())
    }
    fn disconnect(&mut self, _: LinkId) {}
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
    async fn bonds(&mut self) -> Result<Vec<Peer>, Error> {
        Ok(Vec::new())
    }
    async fn forget(&mut self, _: Peer) -> Result<(), Error> {
        Ok(())
    }
}
fn records(maximum: bool) -> Vec<Record> {
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
fn populate(app: &mut Application, devices: usize, saved: usize, maximum: bool) {
    for n in 0..devices {
        let peer = Peer {
            address: [n as u8; 6],
            random: false,
            transport: Transport::Ble,
        };
        let mut d = Device::new(Policy::paired(n as u64 + 1, peer, &[b'x'; 128]));
        d.catalog.connection(true, true);
        if maximum {
            discovery::run(&mut d.catalog);
        } else {
            d.catalog
                .replace_discovery(records(false), features(false))
                .unwrap();
        }
        let mut writable: Vec<_> = d.catalog.records().iter().filter(|r| r.writable).collect();
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
            block_on(d.catalog.set(
                key,
                scalar(key, value),
                &mut Preferences {
                    store: &mut Store,
                    device: n as u64 + 1,
                },
            ))
            .unwrap();
        }
        if app.manager.devices.len() <= n {
            app.manager.devices.resize_with(n + 1, || None);
        }
        app.manager.devices[n] = Some(d);
    }
}
fn refresh(app: &mut Application, maximum: bool) {
    for d in app.manager.devices.iter_mut().flatten() {
        if maximum {
            discovery::run(&mut d.catalog);
        } else {
            d.catalog
                .replace_discovery(records(false), features(false))
                .unwrap();
        }
    }
}
fn observations(app: &mut Application, maximum: bool) {
    // Real read/event handlers mutate individual rows in the live catalog.
    // They do not rebuild every disconnected device's inventory at once.
    for d in app.manager.devices.iter_mut().take(4).flatten() {
        d.catalog.invalidate();
        if maximum {
            use cordial_core::model::info::InfoKey as I;
            d.catalog.info.battery.configure(cordial_core::model::identifiers::Transport::Ble, false);
            for instance in 0..4 {
                d.catalog.info.battery.gatt(0x2a19, instance, &[50]);
                d.catalog.info.battery.gatt(0x2bed, instance, &[0,0x21,0]);
                d.catalog.info.battery.gatt(0x2bf0, instance, &[0]);
                d.catalog.info.battery.gatt(0x2be9, instance, &[1]);
            }
            for key in I::ALL {
                for instance in 0..key.instances() {
                    let value = match key {
                        I::BatteryPercent => SettingValue::Integer(50),
                        I::BatteryCharging => SettingValue::Bool(true),
                        I::VendorIdNamespace => SettingValue::Text("usb".into()),
                        I::VendorId | I::ProductId | I::ProductVersion => SettingValue::Integer(65535),
                        I::Kind => SettingValue::Text("keyboard".into()),
                        _ => SettingValue::Text("\\".repeat(64)),
                    };
                    d.catalog.info.observe(false,key,instance,value.clone());
                    d.catalog.info.observe(true,key,instance,value);
                }
            }
            d.catalog.info.changes();
        }
        for key in if maximum {
            &SettingKey::ALL[..]
        } else {
            &[SettingKey::FnRowDefault, SettingKey::BacklightEnabled]
        } {
            let row = d
                .catalog
                .records()
                .iter()
                .find(|r| r.metadata.key == *key)
                .unwrap();
            let value = row.observed.wire(*key);
            d.catalog
                .observe(*key, value, 4, ObservationSource::Read)
                .unwrap();
        }
    }
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
fn reports(app: &mut Application, count: usize, maximum: bool) {
    for n in 0..count {
        let id = LinkId {
            slot: n as u8,
            generation: 1,
        };
        app.manager.connections[n] = Some(Connection {
            security: Some(cordial_core::bluetooth::ConnectionSecurity {
                encrypted: Some(true),
                authenticated: Some(false),
                secure_connections: Some(true),
                key_size: Some(16),
                bonded: Some(true),
            }),
            id,
            peer: app.manager.devices[n].as_ref().unwrap().policy.peer,
            device: Some(n),
            runtime: None,
            closing: false,
            setup_failed: false,
            maps: None,
            error: None,
            deadline: 0,
        });
        let raw = descriptor(maximum, 16);
        let mut parsed: [Option<Descriptor>; 3] = core::array::from_fn(|_| None);
        for (service, entry) in parsed.iter_mut().enumerate().take(if maximum { 3 } else { 1 }) {
            *entry = Some(Descriptor::from_slice(ServiceId(service as u16), &raw).unwrap());
        }
        let descriptors = parsed.into_iter().flatten().collect();
        drop(raw);
        app.manager.connected(id, descriptors, 512, 0).unwrap();
    }
    for d in app.manager.devices.iter_mut().skip(count).flatten() {
        d.catalog.connection(false, true);
    }
    for _ in 0..8 {
        block_on(app.poll(&mut Store, &mut Radio, 0, 0));
    }
}
fn send(app: &mut Application, command: Command) {
    let mut bytes = alloc::vec::Vec::new();
    frame::encode(
        &cordial_protocol::Request {
            command: Some(command),
        },
        &mut bytes,
    );
    let (_, request) = app.serial.feed(&bytes);
    block_on(app.dispatch(request.unwrap(), &mut Store, &mut Radio, 1));
}
/// Writes out everything queued, returning the number of scan candidates reported.
fn drain(app: &mut Application) -> usize {
    let mut decoder = frame::Decoder::new(None);
    let mut candidates = 0;
    while let Some((token, bytes)) = app.serial.output(64) {
        let len = bytes.len();
        for &b in bytes {
            if let Some(frame) = decoder.push(b) {
                let message = cordial_protocol::Message::decode(frame.unwrap()).unwrap();
                candidates += usize::from(matches!(
                    message.kind,
                    Some(cordial_protocol::message::Kind::Event(cordial_protocol::Event {
                        kind: Some(cordial_protocol::event::Kind::ScanFound(_))
                    }))
                ));
            }
        }
        app.serial.output_complete(token, len);
    }
    candidates
}
fn candidates(app: &mut Application, scan: u64) {
    send(
        app,
        Command::StartScan(cordial_protocol::StartScan {
            transports: alloc::vec![cordial_protocol::Transport::Ble as i32],
            seconds: 60,
        }),
    );
    drain(app);
    for n in 0..32 {
        block_on(app.event(
            Event::Found {
                kind: cordial_core::model::link::DeviceKind::Unknown,
                connectable: true,
                scan,
                address: Some(Peer {
                    address: [n; 6],
                    random: false,
                    transport: Transport::Ble,
                }),
                peer: Peer {
                    address: [n; 6],
                    random: false,
                    transport: Transport::Ble,
                },
                name: "\\".repeat(128).into(),
                rssi: Some(-127),
            },
            &mut Store,
            &mut Radio,
            1,
        ));
    }
    // One event goes out per poll, and saved devices may still have events waiting.
    let mut found = 0;
    for _ in 0..96 {
        block_on(app.poll(&mut Store, &mut Radio, 0, 2));
        found += drain(app);
    }
    assert_eq!(found, 32);
    send(app, Command::StopScan(cordial_protocol::StopScan {}));
    drain(app);
}

fn reconnect(app: &mut Application, maximum: bool, slot: usize) {
    if !maximum {
        return;
    }
    // The real C callback supplies its descriptor from static storage.
    let raw = unsafe { &*core::ptr::addr_of!(MAX_DESCRIPTOR) };
    let peer = app.manager.devices[slot].as_ref().unwrap().policy.peer;
    let id = LinkId {
        slot: slot as u8,
        generation: 2,
    };
    app.manager.connections[slot] = Some(Connection {
        security: Some(cordial_core::bluetooth::ConnectionSecurity {
            encrypted: Some(true),
            authenticated: Some(false),
            secure_connections: Some(true),
            key_size: Some(16),
            bonded: Some(true),
        }),
        id,
        peer,
        device: Some(slot),
        runtime: None,
        closing: false,
        setup_failed: false,
        maps: None,
        error: None,
        deadline: 0,
    });
    // The callback retains maps inline before forming the Connected event.
    let parsed: [Descriptor; 3] = core::array::from_fn(|service| {
        Descriptor::from_slice(ServiceId(service as u16), raw).unwrap()
    });
    let descriptors = parsed.into_iter().collect();
    app.manager.connected(id, descriptors, 512, 0).unwrap();
}
fn commands(app: &mut Application, devices: usize, maximum: bool, scan: u64) {
    reconnect(app, maximum, scan as usize % 4);
    if maximum {
        for _ in 0..8 {
            block_on(app.poll(&mut Store, &mut Radio, 0, 0));
        }
        refresh(app, maximum);
    }
    app.session(true, &mut Radio);
    send(app, Command::GetStatus(cordial_protocol::GetStatus {}));
    drain(app);
    // A frame that is not a request is answered without reaching the application.
    let invalid = [3, 0xff, 0xff, 0];
    assert_eq!(app.serial.feed(&invalid), (invalid.len(), None));
    drain(app);
    if maximum {
        candidates(app, scan);
    }
    for n in 0..devices.min(4) {
        send(
            app,
            Command::ListSettings(cordial_protocol::ListSettings {
                device: alloc::format!("d_{:016x}", n + 1),
            }),
        );
        drain(app);
    }
    for _ in 0..3 {
        send(app, Command::GetStatus(cordial_protocol::GetStatus {}));
        drain(app);
    }
    observations(app, maximum);
    // Retain a full output queue while observations change live records.
    block_on(app.poll(&mut Store, &mut Radio, 0, 2));
    observations(app, maximum);
    unsafe {
        (*HEAP.0.get()).reject_after = 1;
    }
    assert!(matches!(
        Descriptor::from_slice(ServiceId(0), unsafe { &*core::ptr::addr_of!(MAX_DESCRIPTOR) }),
        Err(Error::Capacity)
    ));
    reconnect(app, maximum, (scan as usize + 1) % 4);
    for _ in 0..128 {
        block_on(app.poll(&mut Store, &mut Radio, 0, 2));
        drain(app);
    }
    assert!(Descriptor::from_slice(ServiceId(0), unsafe { &*core::ptr::addr_of!(MAX_DESCRIPTOR) }).is_ok());
    app.session(false, &mut Radio);
}
static mut MAX_DESCRIPTOR: [u8; 2048] = [0; 2048];
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
    }
    let template = descriptor(true, 16);
    unsafe {
        (*core::ptr::addr_of_mut!(MAX_DESCRIPTOR)).copy_from_slice(&template);
    }
    drop(template);
    writeln!(
        Output,
        "sizes record={} metadata={} preference={} feature={} profile={} link={} application={} device={} policy={}",
        size_of::<Record>(),
        size_of::<Metadata>(),
        size_of::<cordial_core::compact::Preference>(),
        size_of::<cordial_core::compact::FeatureEntry>(),
        size_of::<Profile>(),
        size_of::<Link>(),
        size_of::<Application>(),
        size_of::<Device>(),
        size_of::<Policy>()
    )
    .unwrap();
    static RADIO_IO: cordial_btstack::transport::Io = cordial_btstack::transport::Io::new();
    let store = Box::leak(Box::new(cordial_btstack::storage::Storage::new(Store)));
    let backend =
        cordial_btstack::backend::State::new(store, &RADIO_IO, || 0, || exit(1)).unwrap();
    let backend = Box::new(backend);
    // Reserve more than the Pico's record-storage scratch/driver state plus
    // leaked chipset and USB identity. BTstack event storage above is real.
    let platform_storage = Box::new([0u8; 2048]);
    core::hint::black_box(&platform_storage);
    stats("backend and platform reserve");
    let mut app = Application::new(Build {
        development: true,
        version: "0.0.0",
        board: "pico_w",
        default_adapter_name: "Test adapter",
        adapter_id: "E6613008E35A4733".into(),
        bootloader: None,
    });
    block_on(app.event(Event::Ready, &mut Store, &mut Radio, 0));
    stats("empty core");
    for (label, devices, saved, maximum) in [
        ("one no preferences", 1, 0, false),
        ("one two preferences", 1, 2, false),
        ("eight with sixteen preferences", 8, 16, true),
    ] {
        populate(&mut app, devices, saved, maximum);
        stats("catalogs");
        reports(&mut app, devices.min(4), maximum);
        stats(label);
        refresh(&mut app, maximum);
        commands(&mut app, devices, maximum, 1);
        let steady = stats("warm steady state");
        for scan in 2..=101 {
            refresh(&mut app, maximum);
            commands(&mut app, devices, maximum, scan);
        }
        assert_eq!(stats("after refresh/commands"), steady);
        assert!(largest_block(unsafe { &*HEAP.0.get() }) >= 4096);
        for link in &mut app.manager.connections {
            *link = None;
        }
        for d in &mut app.manager.devices {
            *d = None;
        }
        stats("after disconnect/forget");
    }
    drop(app);
    drop(backend);
    drop(platform_storage);
    unsafe {
        drop(Box::from_raw(
            store as *const _ as *mut cordial_btstack::storage::Storage<Store>,
        ));
    }
    assert_eq!(stats("all allocations dropped"), 0);
    exit(0)
}
