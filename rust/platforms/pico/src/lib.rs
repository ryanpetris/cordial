#![no_std]

extern crate alloc;

pub mod allocator;
pub mod radio;
pub mod services;
pub mod usb;

embassy_rp::bind_interrupts!(pub struct DmaIrqs {
    DMA_IRQ_0 => embassy_rp::dma::InterruptHandler<embassy_rp::peripherals::DMA_CH0>,
        embassy_rp::dma::InterruptHandler<embassy_rp::peripherals::DMA_CH1>;
});

#[cfg(board_configured)]
pub mod board {
    include!(concat!(env!("OUT_DIR"), "/board.rs"));
}
