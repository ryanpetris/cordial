//! ESP-IDF owns PHY clocks and interrupt allocation; Embassy owns USB protocol
//! and endpoint state. There is no second USB device stack.
use cordial_usb::completion::{
    CompleteDriver, Completion, OutHalt, reconfigure_endpoint, set_out_halt,
};
use embassy_time::Timer;
use embassy_usb_driver::{
    Bus, EndpointAddress, EndpointAllocError, EndpointError, EndpointType, Unsupported,
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

fn epoch(address: EndpointAddress) -> &'static AtomicU32 {
    &EPOCHS[usize::from(address.is_in())][address.index()]
}

fn registers() -> Otg {
    unsafe { Otg::from_ptr(platform::cordial_esp_usb_registers().cast()) }
}

fn set_global_out_nak(r: Otg) {
    r.dctl().modify(|w| w.set_sgonak(true));
    while !r.gintsts().read().goutnakeff() {}
}

fn clear_global_out_nak(r: Otg) {
    r.dctl().modify(|w| w.set_cgonak(true));
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

unsafe extern "C" fn interrupt(_: *mut c_void) {
    unsafe { otg::on_interrupt(registers(), &(*STATE.0.get()).as_state()) };
}

pub struct Driver<'d>(otg::Driver<'d>);
#[derive(Clone, Copy)]
pub struct TransferCompletion;
impl Completion for TransferCompletion {
    fn epoch(&self, address: EndpointAddress) -> u32 {
        epoch(address).load(Ordering::Relaxed)
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
        }
        // USBAEP can be restored before a pending write runs. Preserve the
        // cancellation so it cannot confirm a flushed packet or send another chunk.
        epoch(address).fetch_add(1, Ordering::Relaxed);
        let out_nak = address.is_out() && r.doepctl(index).read().epena();
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
            clear_global_out_nak(r);
        }
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
                    let stopped = r.doeptsiz(index).read().pktcnt() != 0
                        && disable_out(r, index, true);
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
        for halt in &OUT_HALTS {
            halt.reset();
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
