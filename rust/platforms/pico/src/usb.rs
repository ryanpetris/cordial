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

#[derive(Clone, Copy)]
pub struct TransferCompletion;
impl Completion for TransferCompletion {
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
