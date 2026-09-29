//! ESP-IDF owns PHY clocks and interrupt allocation; Embassy owns USB protocol
//! and endpoint state. There is no second USB device stack.
use cordial_usb::completion::{CompleteDriver, Completion};
use embassy_time::Timer;
use embassy_usb_driver::{EndpointAddress, EndpointAllocError, EndpointError, EndpointType};
use embassy_usb_synopsys_otg::{self as otg, otg_v1::Otg};
use esp_idf_sys::{self as sys, platform};
use std::{
    ffi::c_void,
    sync::atomic::{AtomicBool, Ordering},
};

static STATE: otg::StateStorage<6> = otg::StateStorage::new();
static TAKEN: AtomicBool = AtomicBool::new(false);

fn registers() -> Otg {
    unsafe { Otg::from_ptr(platform::cordial_esp_usb_registers().cast()) }
}
unsafe extern "C" fn interrupt(_: *mut c_void) {
    unsafe { otg::on_interrupt(registers(), &STATE.as_state()) };
}

pub struct Driver<'d>(otg::Driver<'d>);
#[derive(Clone, Copy)]
pub struct TransferCompletion;
impl Completion for TransferCompletion {
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

/// Takes the SoC's internal USB peripheral for the application's lifetime.
pub fn driver(
    out: &mut [u8],
) -> Result<CompleteDriver<Driver<'_>, TransferCompletion>, sys::esp_err_t> {
    if TAKEN.swap(true, Ordering::AcqRel) {
        return Err(sys::ESP_ERR_INVALID_STATE);
    }
    let status = unsafe { platform::cordial_esp_usb_phy() };
    if status != sys::ESP_OK {
        return Err(status);
    }
    let regs = registers();
    regs.gahbcfg().modify(|r| r.set_gint(false));
    let status =
        unsafe { platform::cordial_esp_usb_interrupt(Some(interrupt), std::ptr::null_mut()) };
    if status != sys::ESP_OK {
        return Err(status);
    }
    let instance = otg::OtgInstance {
        regs,
        state: STATE.as_state(),
        fifo_depth_words: 256,
        extra_rx_fifo_words: 30,
        phy_type: otg::PhyType::InternalFullSpeed,
        calculate_trdt_fn: |_| 5,
    };
    Ok(CompleteDriver {
        driver: Driver(otg::Driver::new(out, instance, otg::Config::default())),
        completion: TransferCompletion,
    })
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
        (bus, control)
    }
}
