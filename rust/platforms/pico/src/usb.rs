use cordial_usb::completion::CompleteDriver;
use cordial_usb::completion::Completion;
use embassy_rp::pac;
use embassy_rp::peripherals::USB;
use embassy_rp::usb::Driver;
use embassy_rp::{Peri, bind_interrupts};
use embassy_time::Timer;
use embassy_usb_driver::{EndpointAddress, EndpointError};

bind_interrupts!(pub struct Irqs {
    USBCTRL_IRQ => embassy_rp::usb::InterruptHandler<USB>;
});

/// Copies USB DPRAM with byte accesses, since unaligned wider accesses fault.
///
/// # Safety
/// Source and destination are valid for `len` bytes and do not overlap.
#[cfg(all(feature = "firmware", any(feature = "rp235xa", feature = "rp235xb")))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __wrap_memcpy(dst: *mut u8, src: *const u8, len: usize) -> *mut u8 {
    let base = pac::USB_DPRAM.as_ptr() as usize;
    let dpram = base..base + 4096;
    if dpram.contains(&(dst as usize)) || dpram.contains(&(src as usize)) {
        for offset in 0..len {
            unsafe {
                dst.add(offset)
                    .write_volatile(src.add(offset).read_volatile())
            };
        }
        dst
    } else {
        unsafe extern "C" {
            fn __real_memcpy(dst: *mut u8, src: *const u8, len: usize) -> *mut u8;
        }
        unsafe { __real_memcpy(dst, src, len) }
    }
}

#[derive(Clone, Copy)]
pub struct TransferCompletion;
impl Completion for TransferCompletion {
    fn disconnect(&self, disconnected: bool) {
        pac::USB.sie_ctrl().modify(|r| r.set_pullup_en(!disconnected));
    }
    async fn wait(&self, endpoint: EndpointAddress) -> Result<(), EndpointError> {
        let index = endpoint.index();
        loop {
            if !pac::USB_DPRAM.ep_in_control(index - 1).read().enable()
                || pac::USB.sie_status().read().bus_reset()
            {
                return Err(EndpointError::Disabled);
            }
            if !pac::USB_DPRAM
                .ep_in_buffer_control(index)
                .read()
                .available(0)
            {
                return Ok(());
            }
            // Embassy owns the IRQ. Observe its endpoint buffer ownership rather
            // than registering a competing completion interrupt handler.
            Timer::after_micros(125).await;
        }
    }
}
pub fn driver(usb: Peri<'static, USB>) -> CompleteDriver<Driver<'static, USB>, TransferCompletion> {
    CompleteDriver {
        driver: Driver::new(usb, Irqs),
        completion: TransferCompletion,
    }
}
