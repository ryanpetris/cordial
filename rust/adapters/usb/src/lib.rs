#![no_std]
#![allow(async_fn_in_trait)]

#[cfg(test)]
#[macro_use]
extern crate std;

pub mod completion;
mod configuration;
mod descriptor;
mod desktop;
pub use desktop::ReportHandler;
#[cfg(feature = "gamepad")]
pub mod gamepad;
#[cfg(feature = "gamepad")]
pub mod outputs;
extern crate alloc;
#[cfg(all(test, feature = "gamepad"))]
mod gamepad_tests;
pub mod owner;
mod serial;
#[cfg(test)]
mod tests;
mod wake;
#[cfg(test)]
mod wake_tests;
#[cfg(feature = "gamepad")]
pub mod xinput;

use cordial_core::{
    control::OutputToken,
    devices::Policies,
    interfaces::Interface,
    profiles,
    storage::{self as records, RecordStore},
};
use core::{
    cell::RefCell,
    sync::atomic::{AtomicBool, AtomicU8, Ordering},
};
#[cfg(feature = "gamepad")]
use embassy_futures::join::join;
use embassy_futures::{
    join::{join_array, join4},
    select::{Either, select},
};
use embassy_sync::{
    blocking_mutex::{Mutex, raw::CriticalSectionRawMutex as CS},
    channel::Channel,
    signal::Signal,
    watch::Watch,
};
use embassy_usb::{
    Builder, Config, Handler, UsbDevice,
    class::{cdc_acm, hid},
    control::{OutResponse, Recipient, Request, RequestType},
    driver::Driver,
};

#[derive(Clone, Copy, Default, Debug, Eq, PartialEq)]
pub struct Status {
    pub generation: u64,
    pub session: u64,
    pub configured: bool,
    pub suspended: bool,
    pub dtr: bool,
    pub leds: u8,
    pub boot_keyboard: bool,
}
impl Status {
    pub fn input_ready(self) -> bool {
        self.configured && !self.suspended
    }
    pub fn serial_open(self) -> bool {
        self.configured && self.dtr
    }
}
pub use cordial_core::usb::{Budget, Topology};

/// Match Embassy's compiled limits and this adapter's enabled classes.
pub const fn compiled_budget(mut budget: Budget) -> Budget {
    const fn number(value: Option<&str>, default: u8) -> u8 {
        let Some(value) = value else {
            return default;
        };
        let bytes = value.as_bytes();
        let mut result = 0u8;
        let mut i = 0;
        while i < bytes.len() {
            result = result.saturating_mul(10).saturating_add(bytes[i] - b'0');
            i += 1;
        }
        result
    }
    budget.interfaces = number(option_env!("EMBASSY_USB_MAX_INTERFACE_COUNT"), 4);
    budget.handlers = number(option_env!("EMBASSY_USB_MAX_HANDLER_COUNT"), 4);
    if !cfg!(feature = "gamepad") {
        budget.hid_slots = 0;
        budget.xinput_slots = 0;
    }
    budget
}

const STATUS_RECEIVERS: usize = 3 + SLOTS + if cfg!(feature = "gamepad") { 12 } else { 0 };

/// One 32-byte Raw HID report and the configuration interface it belongs to.
struct RawPacket {
    generation: u64,
    interface: Interface,
    bytes: [u8; 32],
}
/// Maximum number of configuration editors that can be enabled together.
const SLOTS: usize = 1;
/// The enabled interface at `slot` for an enabled set, if that slot is in use.
fn slot_interface(set: u8, slot: usize) -> Option<Interface> {
    Interface::ALL
        .into_iter()
        .filter(|i| set & i.bit() != 0)
        .nth(slot)
}
/// The number of slots an enabled set uses.
fn used_slots(set: u8) -> usize {
    Interface::ALL
        .into_iter()
        .filter(|i| set & i.bit() != 0)
        .count()
        .min(SLOTS)
}
/// Requests and replies of one Raw HID slot.
struct RawQueue {
    rx: Channel<CS, RawPacket, 1>,
    tx: Channel<CS, RawPacket, 1>,
    input: Mutex<CS, RefCell<Option<RawPacket>>>,
    interface_number: AtomicU8,
}

/// The configuration interfaces USB starts with: the saved enabled set, without interfaces whose
/// profile no longer exists. A board without profiles, or unreadable preferences, start with none.
pub async fn saved_interfaces<S: RecordStore>(store: &mut S, profiles_supported: bool) -> u8 {
    if !profiles_supported {
        return 0;
    }
    let Ok(preference) = Policies { store: &mut *store }.load_adapter().await else {
        return 0;
    };
    let mut set = 0;
    for entry in preference
        .configuration_interfaces
        .iter()
        .filter(|p| p.enabled)
    {
        if let Some(id) = entry.profile
            && !matches!(
                profiles::metadata(store, id).await,
                Err(records::Error::Missing)
            )
        {
            set |= entry.interface.bit();
        }
    }
    set
}
/// Scan validated saved policies before the first enumeration, using the runtime allocator.
pub async fn saved_topology<S: RecordStore>(
    store: &mut S,
    profiles_supported: bool,
    budget: Budget,
) -> Topology {
    let mut manager = cordial_core::manager::Manager::default();
    manager.usb_budget = compiled_budget(budget);
    manager.profile_budget = profiles_supported.then_some(0);
    #[cfg(feature = "gamepad")]
    {
        manager.output_support = outputs::SUPPORT;
    }
    if let Ok(preference) = (Policies { store: &mut *store }).load_adapter().await {
        manager.preference = preference;
    }
    let interfaces = saved_interfaces(store, profiles_supported).await;
    manager
        .preference
        .configuration_interfaces
        .retain(|p| interfaces & p.interface.bit() != 0);
    let mut peers = alloc::vec::Vec::new();
    let mut after = 0;
    loop {
        let Ok(ids) = store.record_ids(2, after, 8).await else {
            break;
        };
        for &id in &ids {
            after = id;
            let Ok(Some(bytes)) = store.load_owned(records::record_key(2, id)).await else {
                continue;
            };
            let Some((policy, _)) = cordial_core::bonds::decode(id, &bytes) else {
                continue;
            };
            if peers.contains(&policy.peer) {
                continue;
            }
            if peers.try_reserve(1).is_err() {
                break;
            }
            peers.push(policy.peer);
            let mode = cordial_core::outputs::gamepad(&policy.output_preferences);
            if manager.outputs.prepare(id, mode).is_err() {
                break;
            }
            let configured = cordial_core::outputs::Configured::load(&policy, store).await;
            manager.outputs.configure(configured);
        }
        if ids.len() < 8 {
            break;
        }
    }
    manager.usb_allocation().topology
}

struct HidTx {
    generation: u64,
    sequence: u64,
    bytes: [u8; 69],
    length: usize,
}
struct HidDone {
    generation: u64,
    sequence: u64,
    success: bool,
}
struct SerialTx {
    session: u64,
    token: Option<OutputToken>,
    bytes: [u8; 63],
    length: usize,
}
struct SerialDone {
    session: u64,
    token: Option<OutputToken>,
    length: usize,
}
struct SerialRx {
    session: u64,
    bytes: [u8; 64],
    length: usize,
}

const fn idle_reports() -> [[u8; 68]; 10] {
    let mut reports = [[0; 68]; 10];
    let mut i = 0;
    while i < 34 {
        reports[7][i] = 0xff;
        if i < 6 {
            reports[6][i] = 0xff;
        }
        i += 1;
    }
    reports[8][4] = 0x20;
    reports[9][0] = 4;
    reports
}

/// USB tasks exchange bounded owned packets with the application owner.
pub struct Io {
    // Input HID, CDC TX/RX, Raw HID, four gamepad writers and four receiver IN/OUT pairs.
    status: Watch<CS, Status, STATUS_RECEIVERS>,
    reports: Mutex<CS, RefCell<[[u8; 68]; 10]>>,
    hid_idle: Mutex<CS, core::cell::Cell<[u32; 10]>>,
    hid_settings: Signal<CS, ()>,
    changed: Signal<CS, ()>,
    hid_tx: Channel<CS, HidTx, 1>,
    hid_done: Channel<CS, HidDone, 1>,
    serial_tx: Channel<CS, SerialTx, 1>,
    serial_done: Channel<CS, SerialDone, 1>,
    serial_rx: Channel<CS, SerialRx, 1>,
    rx_drained: Watch<CS, u64, 1>,
    control: Mutex<CS, RefCell<serial::ControlState>>,
    raw: [RawQueue; SLOTS],
    /// The enabled configuration interfaces the current enumeration exposes.
    interfaces: AtomicU8,
    desktop: AtomicBool,
    budget: Budget,
    reconnect: Signal<CS, Topology>,
    wake: wake::Wake,
    #[cfg(feature = "gamepad")]
    outputs: outputs::Outputs,
}
impl Default for Io {
    fn default() -> Self {
        Self::new()
    }
}
impl Io {
    pub const fn new() -> Self {
        Self::with_budget(Budget::PICO)
    }
    pub const fn with_budget(budget: Budget) -> Self {
        Self {
            status: Watch::new_with(Status {
                generation: 0,
                session: 0,
                configured: false,
                suspended: false,
                dtr: false,
                leds: 0,
                boot_keyboard: false,
            }),
            reports: Mutex::new(RefCell::new(idle_reports())),
            hid_idle: Mutex::new(core::cell::Cell::new([u32::MAX; 10])),
            hid_settings: Signal::new(),
            changed: Signal::new(),
            hid_tx: Channel::new(),
            hid_done: Channel::new(),
            serial_tx: Channel::new(),
            serial_done: Channel::new(),
            serial_rx: Channel::new(),
            rx_drained: Watch::new_with(0),
            control: Mutex::new(RefCell::new(serial::ControlState::new())),
            raw: [const {
                RawQueue {
                    rx: Channel::new(),
                    tx: Channel::new(),
                    input: Mutex::new(RefCell::new(None)),
                    interface_number: AtomicU8::new(u8::MAX),
                }
            }; SLOTS],
            interfaces: AtomicU8::new(0),
            desktop: AtomicBool::new(false),
            budget: compiled_budget(budget),
            reconnect: Signal::new(),
            wake: wake::Wake::new(),
            #[cfg(feature = "gamepad")]
            outputs: outputs::Outputs::new(),
        }
    }
    /// Set the allocation before starting the owner and when rebuilding USB.
    pub fn set_topology(&self, topology: Topology) {
        self.wake.reset();
        self.desktop.store(topology.desktop, Ordering::Relaxed);
        self.set_interfaces(topology.interfaces);
        #[cfg(feature = "gamepad")]
        self.outputs.enumerate(topology.gamepads);
        #[cfg(not(feature = "gamepad"))]
        assert_eq!(topology.gamepads, Default::default());
    }
    pub fn topology(&self) -> Topology {
        Topology {
            desktop: self.desktop.load(Ordering::Relaxed),
            interfaces: self.interfaces(),
            #[cfg(feature = "gamepad")]
            gamepads: self.outputs.plan(),
            #[cfg(not(feature = "gamepad"))]
            gamepads: Default::default(),
        }
    }
    fn interfaces(&self) -> u8 {
        self.interfaces.load(Ordering::Relaxed)
    }
    fn set_interfaces(&self, set: u8) {
        self.interfaces.store(set, Ordering::Relaxed);
    }
    /// The configuration interface the current enumeration exposes at `slot`.
    fn slot(&self, slot: usize) -> Option<Interface> {
        slot_interface(self.interfaces(), slot)
    }
    pub fn status(&self) -> Status {
        self.status.try_get().unwrap()
    }
    pub async fn changed(&self) {
        self.changed.wait().await;
    }
    fn update(&self, change: impl FnOnce(&mut Status)) {
        let mut status = self.status();
        change(&mut status);
        self.status.sender().send(status);
        self.changed.signal(());
    }
    fn reset(&self, configured: bool) {
        self.wake.suspend(false);
        #[cfg(feature = "gamepad")]
        self.outputs.reset();
        self.reports.lock(|r| *r.borrow_mut() = idle_reports());
        for queue in &self.raw {
            queue.input.lock(|input| *input.borrow_mut() = None);
        }
        self.hid_idle.lock(|idle| idle.set([u32::MAX; 10]));
        self.hid_settings.signal(());
        self.update(|s| {
            s.generation = s.generation.wrapping_add(1);
            s.session = s.session.wrapping_add(1);
            s.configured = configured;
            s.suspended = false;
            s.dtr = false;
            s.leds = 0;
            s.boot_keyboard = false;
        });
    }
    fn dtr(&self, dtr: bool) {
        if self.status().dtr != dtr {
            self.update(|s| {
                s.dtr = dtr;
                s.session = s.session.wrapping_add(1);
            });
        }
    }
}

pub struct BusHandler<'a>(pub &'a Io);
impl Handler for BusHandler<'_> {
    fn control_out(&mut self, req: Request, _: &[u8]) -> Option<OutResponse> {
        // CDC is interface zero. Observe before its handler accepts the request,
        // so even consecutive close/open requests retain distinct sessions.
        if (req.request_type, req.recipient, req.index, req.request)
            == (RequestType::Class, Recipient::Interface, 0, 0x22)
        {
            self.0.dtr(req.value & 1 != 0);
        }
        None
    }
    fn enabled(&mut self, enabled: bool) {
        if !enabled {
            self.0.wake.arm(false);
            self.0.reset(false);
        }
    }
    fn reset(&mut self) {
        self.0.wake.arm(false);
        self.0.reset(false);
    }
    fn configured(&mut self, configured: bool) {
        self.0.reset(configured);
    }
    fn suspended(&mut self, suspended: bool) {
        self.0.wake.suspend(suspended);
        self.0.update(|s| {
            s.suspended = suspended;
            s.generation = s.generation.wrapping_add(1);
        });
    }
    fn remote_wakeup_enabled(&mut self, enabled: bool) {
        self.0.wake.arm(enabled);
    }
}
/// Borrowed descriptor/class storage has the same lifetime as the USB tasks.
pub struct Buffers<'a> {
    pub config: [u8; 512],
    pub msos: [u8; 64],
    #[cfg(feature = "gamepad")]
    gamepads: [gamepad::State<'a>; 4],
    #[cfg(feature = "gamepad")]
    receiver: xinput::Control,
    pub bos: [u8; 64],
    // USB strings use UTF-16; board default names allow up to 64 UTF-8 bytes.
    pub control: [u8; 256],
    pub cdc: cdc_acm::State<'a>,
    pub hid: hid::State<'a>,
    raw: [hid::State<'a>; SLOTS],
    pub bus_handler: BusHandler<'a>,
    pub report_handler: ReportHandler<'a>,
    raw_report_handlers: [configuration::ReportHandler<'a>; SLOTS],
    serial_number: configuration::SerialNumber<'a>,
}
impl<'a> Buffers<'a> {
    pub fn new(io: &'a Io) -> Self {
        Self {
            config: [0; 512],
            msos: [0; 64],
            #[cfg(feature = "gamepad")]
            gamepads: core::array::from_fn(|i| gamepad::State::new(io.outputs.reports(i))),
            #[cfg(feature = "gamepad")]
            receiver: xinput::Control::new([0; 7]),
            bos: [0; 64],
            control: [0; 256],
            cdc: cdc_acm::State::new(),
            hid: hid::State::new(),
            raw: core::array::from_fn(|_| hid::State::new()),
            bus_handler: BusHandler(io),
            report_handler: ReportHandler(io),
            raw_report_handlers: core::array::from_fn(|slot| configuration::ReportHandler {
                io,
                slot,
            }),
            serial_number: configuration::SerialNumber { io, bytes: [0; 30] },
        }
    }
}
pub struct Usb<'d, D: Driver<'d>> {
    device: UsbDevice<'d, serial::SessionDriver<'d, D>>,
    hid: Option<hid::HidWriter<'d, serial::SessionDriver<'d, D>, 69>>,
    tx: cdc_acm::Sender<'d, serial::SessionDriver<'d, D>>,
    rx: cdc_acm::Receiver<'d, serial::SessionDriver<'d, D>>,
    io: &'d Io,
    #[cfg(feature = "gamepad")]
    gamepads: [Option<gamepad::Gamepad<'d, serial::SessionDriver<'d, D>>>; 4],
    #[cfg(feature = "gamepad")]
    receiver: [Option<xinput::Channel<'d, serial::SessionDriver<'d, D>>>; 4],
    raw: [Option<hid::HidReaderWriter<'d, serial::SessionDriver<'d, D>, 32, 32>>; SLOTS],
}
impl<'d, D: Driver<'d>> Usb<'d, D> {
    /// D's IN writes must await physical completion, using CompleteDriver when
    /// needed. This preserves output deadlines and the last completed HID report.
    /// `interfaces` is the enabled configuration interface set to enumerate with.
    pub fn new(
        driver: D,
        serial: &'d str,
        default_adapter_name: &'d str,
        topology: Topology,
        io: &'d Io,
        buffers: &'d mut Buffers<'d>,
    ) -> Self {
        let mut config = Config::new(0x1209, 0xc0d1);
        config.manufacturer = Some("Cordial");
        config.product = Some(default_adapter_name);
        config.serial_number = Some(serial);
        config.device_release = 0x0100;
        config.device_class = 0xef;
        config.device_sub_class = 0x02;
        config.device_protocol = 0x01;
        config.composite_with_iads = true;
        config.max_power = 100;
        config.supports_remote_wakeup = true;
        buffers.serial_number.bytes[..16].copy_from_slice(serial.as_bytes());
        buffers.serial_number.bytes[16..].copy_from_slice(configuration::VIAL_SERIAL_SUFFIX);
        // A malformed startup allocation must still expose the management port.
        let topology = if topology.fits(io.budget) {
            topology
        } else {
            Topology::default()
        };
        io.set_topology(topology);
        io.control
            .lock(|state| *state.borrow_mut() = serial::ControlState::new());
        // Raw HID follows CDC's two interfaces, desktop HID and the gamepad interfaces.
        let raw_start = 2
            + u8::from(topology.desktop)
            + topology.gamepads.hid_gamepads
            + topology.gamepads.xinput_gamepads;
        for (index, queue) in io.raw.iter().enumerate() {
            queue.interface_number.store(
                if index < used_slots(topology.interfaces) {
                    raw_start + index as u8
                } else {
                    u8::MAX
                },
                Ordering::Relaxed,
            );
        }
        let mut builder = Builder::new(
            serial::SessionDriver { driver, io },
            config,
            &mut buffers.config,
            &mut buffers.bos,
            &mut buffers.msos,
            &mut buffers.control,
        );
        assert_eq!(u8::from(builder.string()), configuration::SERIAL_INDEX);
        builder.handler(&mut buffers.serial_number);
        builder.handler(&mut buffers.bus_handler);
        let cdc = cdc_acm::CdcAcmClass::new(&mut builder, &mut buffers.cdc, 64);
        let hid = topology.desktop.then(|| {
            hid::HidWriter::new(
                &mut builder,
                &mut buffers.hid,
                hid::Config {
                    report_descriptor: descriptor::REPORT_DESCRIPTOR,
                    request_handler: Some(&mut buffers.report_handler),
                    poll_ms: 1,
                    max_packet_size: 64,
                    hid_subclass: hid::HidSubclass::Boot,
                    hid_boot_protocol: hid::HidBootProtocol::Keyboard,
                },
            )
        });
        #[cfg(feature = "gamepad")]
        let (gamepads, receiver) = {
            assert!(topology.gamepads.hid_gamepads <= 4);
            let mut states = buffers.gamepads.iter_mut();
            let gamepads = core::array::from_fn(|i| {
                (i < topology.gamepads.hid_gamepads as usize)
                    .then(|| gamepad::Gamepad::new(&mut builder, states.next().unwrap()))
            });
            let receiver = xinput::new(
                &mut builder,
                &mut buffers.receiver,
                topology.gamepads.xinput_gamepads as usize,
            );
            (gamepads, receiver)
        };
        let mut slots = buffers.raw.iter_mut().zip(&mut buffers.raw_report_handlers);
        let raw = core::array::from_fn(|index| {
            let (state, handler) = slots.next().unwrap();
            (index < used_slots(topology.interfaces)).then(|| {
                hid::HidReaderWriter::new(
                    &mut builder,
                    state,
                    hid::Config {
                        report_descriptor: configuration::REPORT_DESCRIPTOR,
                        request_handler: Some(handler),
                        poll_ms: 1,
                        max_packet_size: 32,
                        hid_subclass: hid::HidSubclass::No,
                        hid_boot_protocol: hid::HidBootProtocol::None,
                    },
                )
            })
        });
        let (tx, rx) = cdc.split();
        Self {
            #[cfg(feature = "gamepad")]
            gamepads,
            #[cfg(feature = "gamepad")]
            receiver,
            raw,
            device: builder.build(),
            hid,
            tx,
            rx,
            io,
        }
    }
    /// Disable USB and drop all workers before returning the requested allocation.
    pub async fn run_until_rebuild(self) -> Topology {
        let Self {
            mut device,
            #[cfg(feature = "gamepad")]
            gamepads,
            #[cfg(feature = "gamepad")]
            receiver,
            raw,
            hid,
            mut tx,
            rx,
            io,
        } = self;
        let mut tx_status = io.status.receiver().unwrap();
        let bus = wake::run(&mut device, io);
        let workers = join4(
            {
                let mut slot = 0;
                join_array(raw.map(|raw| {
                    slot += 1;
                    let index = slot - 1;
                    async move {
                        if let Some(raw) = raw {
                            configuration::run(raw, io, index).await;
                        }
                    }
                }))
            },
            async {
                if let Some(hid) = hid {
                    desktop::run(hid, io).await;
                }
            },
            async {
                loop {
                    let packet = io.serial_tx.receive().await;
                    let status = io.status();
                    let success = status.serial_open()
                        && status.session == packet.session
                        && matches!(
                            select(
                                tx.write_packet(&packet.bytes[..packet.length]),
                                tx_status.changed_and(|s| s.session != packet.session)
                            )
                            .await,
                            Either::First(Ok(()))
                        );
                    io.serial_done
                        .send(SerialDone {
                            session: packet.session,
                            token: packet.token,
                            length: if success { packet.length } else { 0 },
                        })
                        .await;
                    io.changed.signal(());
                }
            },
            serial::receive(rx, io),
        );
        #[cfg(feature = "gamepad")]
        let workers = join(workers, outputs::run(gamepads, receiver, io));
        let topology = match select(bus, workers).await {
            Either::First(topology) => topology,
            Either::Second(_) => unreachable!(),
        };
        device.disable().await;
        topology
    }
}
