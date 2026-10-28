use cordial_usb::completion::CompleteDriver;
use cordial_usb::completion::{Completion, PollDelay, rp_data0_pid};
use embassy_rp::pac;
use embassy_rp::peripherals::USB;
use embassy_rp::usb::Driver;
use embassy_rp::{Peri, bind_interrupts};
use embassy_time::Timer;
use embassy_usb_driver::{Bus, EndpointAddress, EndpointError, Unsupported};
use portable_atomic::{AtomicU32, Ordering};

static EPOCHS: [[AtomicU32; 16]; 2] = [const { [const { AtomicU32::new(0) }; 16] }; 2];

fn epoch(address: EndpointAddress) -> &'static AtomicU32 {
    &EPOCHS[usize::from(address.is_in())][address.index()]
}

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
    fn epoch(&self, address: EndpointAddress) -> u32 {
        epoch(address).load(Ordering::Relaxed)
    }
    fn endpoint_set_enabled<B: Bus>(&self, bus: &mut B, address: EndpointAddress, enabled: bool) {
        if address.index() != 0 {
            // Reinitializing buffer control clears AVAILABLE without delivering
            // the packet. Preserve cancellation across immediate re-enabling.
            epoch(address).fetch_add(1, Ordering::Relaxed);
        }
        bus.endpoint_set_enabled(address, enabled);
    }
    fn stalled(&self, address: EndpointAddress) -> bool {
        if address.is_in() {
            pac::USB_DPRAM
                .ep_in_buffer_control(address.index())
                .read()
                .stall()
        } else {
            pac::USB_DPRAM
                .ep_out_buffer_control(address.index())
                .read()
                .stall()
        }
    }
    fn disconnect(&self, disconnected: bool) {
        pac::USB.sie_ctrl().modify(|r| {
            r.set_pullup_en(!disconnected);
            if disconnected {
                r.set_resume(false);
            }
        });
    }
    fn endpoint_set_stalled<B: Bus>(&self, bus: &mut B, address: EndpointAddress, stalled: bool) {
        if address.index() == 0 {
            bus.endpoint_set_stalled(address, stalled);
            return;
        }
        // RP2040/RP2350 USB EP_ABORT/EP_ABORT_DONE grant software ownership
        // without consuming the packet. The IRQ still owns completion status.
        let mask = 1 << (address.index() * 2 + usize::from(address.is_out()));
        pac::USB.ep_abort().modify(|r| r.0 |= mask);
        while pac::USB.ep_abort_done().read().0 & mask == 0 {
            core::hint::spin_loop();
        }
        if !stalled {
            let buffer = if address.is_in() {
                pac::USB_DPRAM.ep_in_buffer_control(address.index())
            } else {
                pac::USB_DPRAM.ep_out_buffer_control(address.index())
            };
            // Keep FULL, AVAILABLE, length, and payload intact. An unread OUT
            // packet remains readable; its later rearm must expect DATA0.
            buffer.modify(|r| r.set_pid(0, rp_data0_pid(r.available(0))));
        }
        bus.endpoint_set_stalled(address, stalled);
        pac::USB.ep_abort_done().write(|r| r.0 = mask);
        pac::USB.ep_abort().modify(|r| r.0 &= !mask);
    }
    async fn remote_wakeup<B: Bus>(&self, _: &mut B) -> Result<(), Unsupported> {
        if pac::USB.sie_status().read().bus_reset() || !pac::USB.sie_ctrl().read().pullup_en() {
            return Err(Unsupported);
        }
        if pac::USB.intr().read().dev_sof() || pac::USB.sie_status().read().resume() {
            return Ok(());
        }
        pac::USB.sie_ctrl().modify(|r| r.set_resume(true));
        Timer::after_millis(10).await;
        pac::USB.sie_ctrl().modify(|r| r.set_resume(false));
        Ok(())
    }
    async fn wait_resume(&self) {
        while !pac::USB.intr().read().dev_sof() {
            Timer::after_micros(125).await;
        }
        let _ = pac::USB.sof_rd().read();
    }
    fn suspended(&self) {
        // Reading SOF_RD clears DEV_SOF. A fresh host frame confirms resume,
        // including device-initiated resume that produces no host-resume IRQ.
        let _ = pac::USB.sof_rd().read();
    }
    async fn wait(&self, endpoint: EndpointAddress) -> Result<(), EndpointError> {
        let index = endpoint.index();
        let mut delay = PollDelay::default();
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
            Timer::after_micros(delay.micros()).await;
        }
    }
}
pub fn driver(usb: Peri<'_, USB>) -> CompleteDriver<Driver<'_, USB>, TransferCompletion> {
    CompleteDriver {
        driver: Driver::new(usb, Irqs),
        completion: TransferCompletion,
    }
}
