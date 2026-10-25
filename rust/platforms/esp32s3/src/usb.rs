//! ESP-IDF owns PHY clocks and interrupt allocation; Embassy owns USB protocol
//! and endpoint state. There is no second USB device stack.
use cordial_usb::completion::{
    CompleteDriver, Completion, OutHalt, reconfigure_endpoint, set_out_halt,
};
use embassy_time::Timer;
use embassy_usb_driver::{
    Bus, EndpointAddress, EndpointAllocError, EndpointError, EndpointInfo, EndpointType,
    Unsupported,
};
use embassy_usb_synopsys_otg::{self as otg, otg_v1::Otg};
use esp_idf_sys::{self as sys, platform};
use std::{
    cell::UnsafeCell,
    ffi::c_void,
    sync::atomic::{AtomicBool, AtomicU32, Ordering},
};

struct State(UnsafeCell<otg::StateStorage<6>>);
// Only the interrupt and the borrowed peripheral driver read this storage.
// Peripheral::driver replaces it with the interrupt disabled and no live driver.
// The USB task stays on the main task's pinned core, where it allocates the interrupt.
unsafe impl Sync for State {}
static STATE: State = State(UnsafeCell::new(otg::StateStorage::new()));
static TAKEN: AtomicBool = AtomicBool::new(false);
static EPOCHS: [[AtomicU32; 6]; 2] = [const { [const { AtomicU32::new(0) }; 6] }; 2];
static OUT_HALTS: [OutHalt; 6] = [const { OutHalt::new() }; 6];
static OUT_NEEDS_DRAIN: [AtomicBool; 6] = [const { AtomicBool::new(false) }; 6];
static RESET_PENDING: AtomicBool = AtomicBool::new(false);

fn epoch(address: EndpointAddress) -> &'static AtomicU32 {
    &EPOCHS[usize::from(address.is_in())][address.index()]
}

fn registers() -> Otg {
    unsafe { Otg::from_ptr(platform::cordial_esp_usb_registers().cast()) }
}

fn set_global_out_nak(r: Otg) {
    // A previous window must end before its effective indication can be reused.
    // USB interrupts stay enabled so Embassy drains preceding RX FIFO entries.
    while r.gintsts().read().goutnakeff() {}
    r.dctl().modify(|w| {
        w.set_cgonak(false);
        w.set_sgonak(true);
    });
    while !r.gintsts().read().goutnakeff() {}
}

fn clear_global_out_nak(r: Otg) {
    r.dctl().modify(|w| {
        w.set_sgonak(false);
        w.set_cgonak(true);
    });
    while r.gintsts().read().goutnakeff() {}
}

fn hold_out(r: Otg, index: usize) {
    r.doepctl(index).modify(|w| {
        w.set_epena(false);
        w.set_epdis(false);
        w.set_cnak(false);
        w.set_snak(true);
    });
    while !r.doepctl(index).read().naksts() {}
}

fn disable_out(r: Otg, index: usize, stall: bool) -> bool {
    let control = r.doepctl(index);
    if !control.read().epena() {
        return false;
    }
    control.modify(|w| {
        w.set_epena(false);
        w.set_epdis(true);
        w.set_snak(true);
        if stall {
            w.set_stall(true);
        }
    });
    while control.read().epena() {}
    true
}

fn reset_endpoints() {
    for halt in &OUT_HALTS {
        halt.reset();
    }
    // The driver resets receive registers without clearing its software slot.
    for dirty in &OUT_NEEDS_DRAIN {
        dirty.store(true, Ordering::Relaxed);
    }
    for direction in &EPOCHS {
        for epoch in direction {
            epoch.fetch_add(1, Ordering::Relaxed);
        }
    }
}

unsafe extern "C" fn interrupt(_: *mut c_void) {
    let r = registers();
    if r.gintsts().read().usbrst() && !RESET_PENDING.load(Ordering::Relaxed) {
        reset_endpoints();
        for index in 1..OUT_NEEDS_DRAIN.len() {
            r.doepctl(index).modify(|w| {
                w.set_cnak(false);
                w.set_snak(true);
            });
        }
        RESET_PENDING.store(true, Ordering::Release);
    }
    unsafe { otg::on_interrupt(r, &(*STATE.0.get()).as_state()) };
}

pub struct Driver<'d>(otg::Driver<'d>);
#[derive(Clone, Copy)]
pub struct TransferCompletion;
impl Completion for TransferCompletion {
    fn epoch(&self, address: EndpointAddress) -> u32 {
        epoch(address).load(Ordering::Relaxed)
    }
    fn reset_pending(&self) -> bool {
        RESET_PENDING.load(Ordering::Acquire) || registers().gintsts().read().usbrst()
    }
    fn cancel_out_read(&self, address: EndpointAddress) {
        registers().doepctl(address.index()).modify(|w| {
            w.set_cnak(false);
            w.set_snak(true);
        });
    }
    fn endpoint_set_enabled<B: Bus>(&self, bus: &mut B, address: EndpointAddress, enabled: bool) {
        if address.index() == 0 {
            bus.endpoint_set_enabled(address, enabled);
            return;
        }
        let r = registers();
        let index = address.index();
        if address.is_out() {
            OUT_HALTS[index].reset();
            OUT_NEEDS_DRAIN[index].store(true, Ordering::Relaxed);
        }
        // USBAEP can be restored before a pending write runs. Preserve the
        // cancellation so it cannot confirm a flushed packet or send another chunk.
        epoch(address).fetch_add(1, Ordering::Relaxed);
        let out_nak = address.is_out();
        reconfigure_endpoint(
            bus,
            address,
            enabled,
            || {
                // Stop transactions before the driver flushes IN or primes OUT. Poll
                // state bits because Embassy owns and acknowledges endpoint interrupts.
                if address.is_in() {
                    let control = r.diepctl(index);
                    if control.read().epena() {
                        control.modify(|w| {
                            w.set_epena(false);
                            w.set_epdis(false);
                            w.set_snak(true);
                        });
                        while control.read().epena() && !control.read().naksts() {}
                        if control.read().epena() {
                            control.modify(|w| {
                                w.set_epena(false);
                                w.set_epdis(true);
                                w.set_snak(true);
                            });
                            while control.read().epena() {}
                        }
                    }
                } else if out_nak {
                    // Global OUT NAK lets the IRQ drain the shared RX FIFO before
                    // an active OUT endpoint can be disabled.
                    set_global_out_nak(r);
                    disable_out(r, index, false);
                }
            },
            |bus| {
                let kind = if address.is_in() {
                    r.diepctl(index).read().eptyp()
                } else {
                    r.doepctl(index).read().eptyp()
                };
                if matches!(
                    kind,
                    otg::otg_v1::vals::Eptyp::BULK | otg::otg_v1::vals::Eptyp::INTERRUPT
                ) {
                    self.endpoint_set_stalled(bus, address, false);
                }
            },
        );
        if out_nak {
            hold_out(r, index);
            clear_global_out_nak(r);
        }
    }
    fn begin_out_drain(&self, address: EndpointAddress) -> bool {
        let index = address.index();
        if !OUT_NEEDS_DRAIN[index].load(Ordering::Relaxed) {
            return false;
        }
        let r = registers();
        set_global_out_nak(r);
        disable_out(r, index, false);
        true
    }
    fn finish_out_drain(
        &self,
        info: &EndpointInfo,
        mut result: Result<(), EndpointError>,
    ) -> Result<(), EndpointError> {
        let r = registers();
        let index = info.addr.index();
        if !r.doepctl(index).read().usbaep()
            || r.gintsts().read().usbrst()
            || RESET_PENDING.load(Ordering::Acquire)
        {
            result = Err(EndpointError::Disabled);
        }
        if result.is_ok() {
            // The public read empties Embassy's software slot and issues CNAK.
            // Global NAK contains that rearm until DATA0 and the receive are ready.
            r.doeptsiz(index).modify(|w| {
                w.set_xfrsiz(info.max_packet_size.into());
                w.set_pktcnt(1);
            });
            r.doepctl(index).modify(|w| {
                w.set_epena(false);
                w.set_epdis(false);
                w.set_cnak(false);
                w.set_snak(false);
                w.set_sd0pid_sevnfrm(true);
            });
            r.doepctl(index).modify(|w| {
                w.set_epdis(false);
                w.set_snak(false);
                w.set_sd0pid_sevnfrm(false);
                w.set_epena(true);
                w.set_cnak(true);
            });
            OUT_NEEDS_DRAIN[index].store(false, Ordering::Relaxed);
            // A reset interrupt can preempt the drain after the first check.
            if r.gintsts().read().usbrst() || RESET_PENDING.load(Ordering::Acquire) {
                OUT_NEEDS_DRAIN[index].store(true, Ordering::Relaxed);
                result = Err(EndpointError::Disabled);
            }
        }
        if result.is_err() {
            hold_out(r, index);
        }
        clear_global_out_nak(r);
        result
    }
    fn stalled(&self, address: EndpointAddress) -> bool {
        if address.is_in() {
            registers().diepctl(address.index()).read().stall()
        } else {
            registers().doepctl(address.index()).read().stall()
        }
    }
    fn disconnect(&self, disconnected: bool) {
        registers().dctl().modify(|r| {
            r.set_sdis(disconnected);
            if disconnected {
                r.set_rwusig(false);
            }
        });
    }
    fn endpoint_set_stalled<B: Bus>(&self, bus: &mut B, address: EndpointAddress, stalled: bool) {
        if address.index() == 0 {
            bus.endpoint_set_stalled(address, stalled);
            return;
        }
        let r = registers();
        if address.is_in() {
            // Clear STALL and reset DATA0 in one register write without flushing
            // the FIFO. Zero command bits cannot re-enable a completed transfer.
            r.diepctl(address.index()).modify(|r| {
                r.set_epena(false);
                r.set_epdis(false);
                r.set_stall(stalled);
                r.set_sd0pid_sevnfrm(!stalled);
            });
        } else {
            let index = address.index();
            if OUT_NEEDS_DRAIN[index].load(Ordering::Relaxed) {
                // Reconfiguration already holds this endpoint in NAK. HALT must
                // not prime over the unread software packet, even after reset.
                OUT_HALTS[index].reset();
                r.doepctl(index).modify(|w| {
                    w.set_epena(false);
                    w.set_epdis(false);
                    w.set_cnak(false);
                    w.set_snak(true);
                    w.set_stall(stalled);
                    w.set_sd0pid_sevnfrm(!stalled);
                });
                return;
            }
            set_out_halt(
                &OUT_HALTS[index],
                bus,
                address,
                stalled,
                || {
                    let control = r.doepctl(index);
                    if !control.read().epena() {
                        control.modify(|w| {
                            w.set_epena(false);
                            w.set_epdis(false);
                            w.set_stall(true);
                        });
                        return false;
                    }
                    set_global_out_nak(r);
                    // EPENA remains set after a one-packet receive completes.
                    // PKTCNT distinguishes that buffered packet from an active receive.
                    let stopped =
                        r.doeptsiz(index).read().pktcnt() != 0 && disable_out(r, index, true);
                    if !stopped {
                        control.modify(|w| {
                            w.set_epena(false);
                            w.set_epdis(false);
                            w.set_stall(true);
                        });
                    }
                    clear_global_out_nak(r);
                    stopped
                },
                || {
                    r.doepctl(index).modify(|w| {
                        w.set_epena(false);
                        w.set_epdis(false);
                        w.set_stall(false);
                        w.set_sd0pid_sevnfrm(true);
                    });
                },
            );
        }
    }
    fn reset(&self) {
        if !RESET_PENDING.swap(false, Ordering::AcqRel) {
            reset_endpoints();
        }
    }
    async fn remote_wakeup<B: Bus>(&self, bus: &mut B) -> Result<(), Unsupported> {
        let r = registers();
        if r.gintsts().read().usbrst() || r.dctl().read().sdis() {
            return Err(Unsupported);
        }
        if !r.dsts().read().suspsts() {
            return Ok(());
        }
        bus.remote_wakeup().await
    }
    async fn wait_resume(&self) {
        while !registers().gintsts().read().sof() {
            Timer::after_micros(125).await;
        }
        registers().gintsts().write(|r| r.set_sof(true));
    }
    fn suspended(&self) {
        registers().gintsts().write(|r| r.set_sof(true));
    }
    async fn wait(&self, endpoint: EndpointAddress) -> Result<(), EndpointError> {
        loop {
            let r = registers();
            let control = r.diepctl(endpoint.index()).read();
            if !control.usbaep() || r.gintsts().read().usbrst() {
                return Err(EndpointError::Disabled);
            }
            if !control.epena() {
                return Ok(());
            }
            Timer::after_micros(125).await;
        }
    }
}

/// Owns the USB peripheral and lends it to one endpoint allocation at a time.
pub struct Peripheral(());
impl Peripheral {
    pub fn new() -> Result<Self, sys::esp_err_t> {
        if TAKEN.swap(true, Ordering::AcqRel) {
            return Err(sys::ESP_ERR_INVALID_STATE);
        }
        let status = unsafe { platform::cordial_esp_usb_phy() };
        if status != sys::ESP_OK {
            return Err(status);
        }
        registers().gahbcfg().modify(|r| r.set_gint(false));
        let status =
            unsafe { platform::cordial_esp_usb_interrupt(Some(interrupt), std::ptr::null_mut()) };
        if status != sys::ESP_OK {
            return Err(status);
        }
        Ok(Self(()))
    }

    pub fn driver<'d>(
        &'d mut self,
        out: &'d mut [u8],
    ) -> CompleteDriver<Driver<'d>, TransferCompletion> {
        let regs = registers();
        regs.gahbcfg().modify(|r| r.set_gint(false));
        assert_eq!(
            unsafe { platform::cordial_esp_usb_interrupt_enable(false) },
            sys::ESP_OK
        );
        for halt in &OUT_HALTS {
            halt.reset();
        }
        for dirty in &OUT_NEEDS_DRAIN {
            dirty.store(false, Ordering::Relaxed);
        }
        RESET_PENDING.store(false, Ordering::Relaxed);
        // The borrow excludes all previous endpoints. Disabling the interrupt
        // prevents it from accessing endpoint buffers while the allocation resets.
        let state = unsafe {
            STATE.0.get().write(otg::StateStorage::new());
            &*STATE.0.get()
        };
        let instance = otg::OtgInstance {
            regs,
            state: state.as_state(),
            fifo_depth_words: 256,
            extra_rx_fifo_words: 30,
            phy_type: otg::PhyType::InternalFullSpeed,
            calculate_trdt_fn: |_| 5,
        };
        CompleteDriver {
            driver: Driver(otg::Driver::new(out, instance, otg::Config::default())),
            completion: TransferCompletion,
        }
    }
}

impl<'d> embassy_usb_driver::Driver<'d> for Driver<'d> {
    type EndpointOut = otg::Endpoint<'d, otg::Out>;
    type EndpointIn = otg::Endpoint<'d, otg::In>;
    type ControlPipe = otg::ControlPipe<'d>;
    type Bus = otg::Bus<'d>;
    fn alloc_endpoint_out(
        &mut self,
        kind: EndpointType,
        address: Option<EndpointAddress>,
        size: u16,
        interval: u8,
    ) -> Result<Self::EndpointOut, EndpointAllocError> {
        self.0.alloc_endpoint_out(kind, address, size, interval)
    }
    fn alloc_endpoint_in(
        &mut self,
        kind: EndpointType,
        address: Option<EndpointAddress>,
        size: u16,
        interval: u8,
    ) -> Result<Self::EndpointIn, EndpointAllocError> {
        self.0.alloc_endpoint_in(kind, address, size, interval)
    }
    fn start(self, size: u16) -> (Self::Bus, Self::ControlPipe) {
        let (mut bus, control) = self.0.start(size);
        bus.core_soft_reset();
        bus.configure_as_device();
        bus.config_v5();
        registers().pcgcctl().write(|r| r.0 = 0);
        assert_eq!(
            unsafe { platform::cordial_esp_usb_interrupt_enable(true) },
            sys::ESP_OK
        );
        (bus, control)
    }
}
