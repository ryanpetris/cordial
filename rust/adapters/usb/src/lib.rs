#![no_std]
#![allow(async_fn_in_trait)]

#[cfg(test)]
#[macro_use]
extern crate std;

pub mod completion;
mod descriptor;
pub mod owner;
mod serial;
#[cfg(test)]
mod tests;

use cordial_core::control::OutputToken;
use core::cell::RefCell;
use embassy_futures::{
    join::join4,
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
}
impl Status {
    pub fn input_ready(self) -> bool {
        self.configured && !self.suspended
    }
    pub fn serial_open(self) -> bool {
        self.configured && self.dtr
    }
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
    status: Watch<CS, Status, 3>,
    reports: Mutex<CS, RefCell<[[u8; 68]; 10]>>,
    changed: Signal<CS, ()>,
    hid_tx: Channel<CS, HidTx, 1>,
    hid_done: Channel<CS, HidDone, 1>,
    serial_tx: Channel<CS, SerialTx, 1>,
    serial_done: Channel<CS, SerialDone, 1>,
    serial_rx: Channel<CS, SerialRx, 1>,
    rx_drained: Watch<CS, u64, 1>,
}
impl Default for Io {
    fn default() -> Self {
        Self::new()
    }
}
impl Io {
    pub const fn new() -> Self {
        Self {
            status: Watch::new_with(Status {
                generation: 0,
                session: 0,
                configured: false,
                suspended: false,
                dtr: false,
                leds: 0,
            }),
            reports: Mutex::new(RefCell::new(idle_reports())),
            changed: Signal::new(),
            hid_tx: Channel::new(),
            hid_done: Channel::new(),
            serial_tx: Channel::new(),
            serial_done: Channel::new(),
            serial_rx: Channel::new(),
            rx_drained: Watch::new_with(0),
        }
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
        self.reports.lock(|r| *r.borrow_mut() = idle_reports());
        self.update(|s| {
            s.generation = s.generation.wrapping_add(1);
            s.session = s.session.wrapping_add(1);
            s.configured = configured;
            s.suspended = false;
            s.dtr = false;
            s.leds = 0;
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
            self.0.reset(false);
        }
    }
    fn reset(&mut self) {
        self.0.reset(false);
    }
    fn configured(&mut self, configured: bool) {
        self.0.reset(configured);
    }
    fn suspended(&mut self, suspended: bool) {
        self.0.update(|s| {
            s.suspended = suspended;
            s.generation = s.generation.wrapping_add(1);
        });
    }
}
pub struct ReportHandler<'a>(pub &'a Io);
impl hid::RequestHandler for ReportHandler<'_> {
    fn get_report(&mut self, id: hid::ReportId, buf: &mut [u8]) -> Option<usize> {
        match id {
            hid::ReportId::Out(1) if !buf.is_empty() => {
                let size = buf.len().min(2);
                buf[..size].copy_from_slice(&[1, self.0.status().leds][..size]);
                Some(size)
            }
            hid::ReportId::In(id @ 1..=10) if !buf.is_empty() => {
                buf[0] = id;
                let size = [32, 10, 16, 68, 5, 9, 6, 34, 8, 1][id as usize - 1].min(buf.len() - 1);
                self.0.reports.lock(|r| {
                    buf[1..1 + size].copy_from_slice(&r.borrow()[id as usize - 1][..size])
                });
                if id == 2 && size > 2 {
                    buf[3..1 + size].fill(0);
                } else if (4..=6).contains(&id) {
                    buf[1..1 + size].fill(0);
                }
                Some(1 + size)
            }
            _ => None,
        }
    }
    fn set_report(&mut self, id: hid::ReportId, data: &[u8]) -> OutResponse {
        if id == hid::ReportId::Out(1) && data.len() == 2 && data[0] == 1 {
            self.0.update(|s| s.leds = data[1] & 0x1f);
            OutResponse::Accepted
        } else {
            OutResponse::Rejected
        }
    }
}

/// Borrowed descriptor/class storage has the same lifetime as the USB tasks.
pub struct Buffers<'a> {
    pub config: [u8; 256],
    pub bos: [u8; 64],
    // USB strings use UTF-16; board default names allow up to 64 UTF-8 bytes.
    pub control: [u8; 256],
    pub cdc: cdc_acm::State<'a>,
    pub hid: hid::State<'a>,
    pub bus_handler: BusHandler<'a>,
    pub report_handler: ReportHandler<'a>,
}
impl<'a> Buffers<'a> {
    pub fn new(io: &'a Io) -> Self {
        Self {
            config: [0; 256],
            bos: [0; 64],
            control: [0; 256],
            cdc: cdc_acm::State::new(),
            hid: hid::State::new(),
            bus_handler: BusHandler(io),
            report_handler: ReportHandler(io),
        }
    }
}
pub struct Usb<'d, D: Driver<'d>> {
    device: UsbDevice<'d, serial::SessionDriver<'d, D>>,
    hid: hid::HidWriter<'d, serial::SessionDriver<'d, D>, 69>,
    tx: cdc_acm::Sender<'d, serial::SessionDriver<'d, D>>,
    rx: cdc_acm::Receiver<'d, serial::SessionDriver<'d, D>>,
    io: &'d Io,
}
impl<'d, D: Driver<'d>> Usb<'d, D> {
    /// D's IN writes must await physical completion, using CompleteDriver when
    /// needed. This preserves output deadlines and the last completed HID report.
    pub fn new(
        driver: D,
        serial: &'d str,
        default_adapter_name: &'d str,
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
        let mut builder = Builder::new(
            serial::SessionDriver { driver, io },
            config,
            &mut buffers.config,
            &mut buffers.bos,
            &mut [],
            &mut buffers.control,
        );
        builder.handler(&mut buffers.bus_handler);
        let cdc = cdc_acm::CdcAcmClass::new(&mut builder, &mut buffers.cdc, 64);
        let hid = hid::HidWriter::new(
            &mut builder,
            &mut buffers.hid,
            hid::Config {
                report_descriptor: descriptor::REPORT_DESCRIPTOR,
                request_handler: Some(&mut buffers.report_handler),
                poll_ms: 1,
                max_packet_size: 64,
                hid_subclass: hid::HidSubclass::No,
                hid_boot_protocol: hid::HidBootProtocol::None,
            },
        );
        let (tx, rx) = cdc.split();
        Self {
            device: builder.build(),
            hid,
            tx,
            rx,
            io,
        }
    }
    pub async fn run(self) -> ! {
        let Self {
            mut device,
            mut hid,
            mut tx,
            rx,
            io,
        } = self;
        let mut hid_status = io.status.receiver().unwrap();
        let mut tx_status = io.status.receiver().unwrap();
        join4(
            device.run(),
            async {
                loop {
                    let packet = io.hid_tx.receive().await;
                    let status = io.status();
                    let success = status.input_ready()
                        && status.generation == packet.generation
                        && matches!(
                            select(
                                hid.write(&packet.bytes[..packet.length]),
                                hid_status.changed_and(|s| s.generation != packet.generation)
                            )
                            .await,
                            Either::First(Ok(()))
                        );
                    if success && io.status().generation == packet.generation {
                        io.reports.lock(|r| {
                            let mut reports = r.borrow_mut();
                            let target = &mut reports[packet.bytes[0] as usize - 1];
                            let data = &packet.bytes[1..packet.length];
                            if matches!(packet.bytes[0], 7 | 8) {
                                for (old, value) in target[..data.len()]
                                    .as_chunks_mut::<2>()
                                    .0
                                    .iter_mut()
                                    .zip(data.as_chunks::<2>().0.iter())
                                {
                                    if *value != [0xff, 0xff] {
                                        old.copy_from_slice(value);
                                    }
                                }
                            } else {
                                target[..data.len()].copy_from_slice(data);
                            }
                        });
                    }
                    io.hid_done
                        .send(HidDone {
                            generation: packet.generation,
                            sequence: packet.sequence,
                            success,
                        })
                        .await;
                    io.changed.signal(());
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
        )
        .await;
        unreachable!()
    }
}
