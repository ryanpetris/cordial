//! ESP-IDF owns PHY clocks and interrupt allocation; Embassy owns USB protocol
//! and endpoint state. There is no second USB device stack.
use cordial_usb::completion::{CompleteDriver, Completion};
use embassy_time::Timer;
use embassy_usb_driver::{EndpointAddress, EndpointAllocError, EndpointError, EndpointType};
use embassy_usb_synopsys_otg::{self as otg, otg_v1::Otg};
use esp_idf_sys::{self as sys, platform};
use std::{
    cell::UnsafeCell,
    ffi::c_void,
    sync::atomic::{AtomicBool, Ordering},
};

struct State(UnsafeCell<otg::StateStorage<6>>);
// Only the interrupt and the borrowed peripheral driver read this storage.
// Peripheral::driver replaces it with the interrupt disabled and no live driver.
// The USB task stays on the main task's pinned core, where it allocates the interrupt.
unsafe impl Sync for State {}
static STATE: State = State(UnsafeCell::new(otg::StateStorage::new()));
static TAKEN: AtomicBool = AtomicBool::new(false);

fn registers() -> Otg {
    unsafe { Otg::from_ptr(platform::cordial_esp_usb_registers().cast()) }
}
unsafe extern "C" fn interrupt(_: *mut c_void) {
    unsafe { otg::on_interrupt(registers(), &(*STATE.0.get()).as_state()) };
}

pub struct Driver<'d>(otg::Driver<'d>);
#[derive(Clone, Copy)]
pub struct TransferCompletion;
impl Completion for TransferCompletion {
    fn disconnect(&self, disconnected: bool) {
        registers().dctl().modify(|r| r.set_sdis(disconnected));
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
