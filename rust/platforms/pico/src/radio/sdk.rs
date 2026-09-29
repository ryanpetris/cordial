use alloc::alloc::{alloc, dealloc};
use cordial_btstack::transport::{Io, PACKET_BYTES, Packet};
use core::alloc::Layout;
use embassy_rp::{
    Peri,
    gpio::Pin,
    peripherals::{DMA_CH0, DMA_CH2, PIO0},
    pio::PioPin,
};

unsafe extern "C" {
    fn cordial_radio_init(mac: *mut u8, fallback: *const u8, sys_hz: u32) -> i32;
    fn cordial_radio_poll();
    fn cordial_radio_led(pin: u32, value: bool) -> i32;
    fn cyw43_bluetooth_hci_read(data: *mut u8, size: u32, len: *mut u32) -> i32;
    fn cyw43_bluetooth_hci_write(data: *mut u8, size: usize) -> i32;
}

// The SDK shared-bus downloader has temporary allocations. Keep them in the
// same Talc heap as Rust, storing the layout for its C free callback.
#[unsafe(no_mangle)]
unsafe extern "C" fn cordial_radio_alloc(size: usize) -> *mut u8 {
    let Some(total) = size.checked_add(4) else {
        return core::ptr::null_mut();
    };
    let Ok(layout) = Layout::from_size_align(total, 4) else {
        return core::ptr::null_mut();
    };
    let ptr = unsafe { alloc(layout) };
    if ptr.is_null() {
        return ptr;
    }
    unsafe {
        ptr.cast::<usize>().write(total);
        ptr.add(4)
    }
}
#[unsafe(no_mangle)]
unsafe extern "C" fn cordial_radio_free(ptr: *mut u8) {
    if !ptr.is_null() {
        let base = unsafe { ptr.sub(4) };
        let size = unsafe { base.cast::<usize>().read() };
        unsafe {
            dealloc(base, Layout::from_size_align_unchecked(size, 4));
        }
    }
}

pub struct Control;
impl Control {
    pub async fn led(&mut self, pin: u8, value: bool) {
        // Synchronous calls cannot overlap on the single-core executor.
        unsafe {
            cordial_radio_led(pin.into(), value);
        }
    }
}

#[cfg(feature = "firmware")]
pub async fn init(
    spawner: embassy_executor::Spawner,
    _pio: Peri<'static, PIO0>,
    _dma: Peri<'static, DMA_CH0>,
    _spare_dma: Peri<'static, DMA_CH2>,
    _pins: (
        Peri<'static, impl Pin>,
        Peri<'static, impl PioPin>,
        Peri<'static, impl PioPin>,
        Peri<'static, impl Pin>,
    ),
    io: &'static Io,
    unique: [u8; 8],
) -> Option<(Control, [u8; 6])> {
    let mut mac = [0; 6];
    // The consumed tokens reserve these resources for the C driver's lifetime.
    let fallback = crate::services::fallback_wifi_address(unique);
    if unsafe {
        cordial_radio_init(
            mac.as_mut_ptr(),
            fallback.as_ptr(),
            embassy_rp::clocks::clk_sys_freq(),
        )
    } != 0
    {
        return None;
    }
    spawner.spawn(transport_task(io).unwrap());
    Some((Control, mac))
}

// CYW43 requires a four-byte header and word-aligned, padded transfers.
#[repr(align(4))]
struct Buffer([u8; (PACKET_BYTES + 4).next_multiple_of(4)]);

#[cfg(feature = "firmware")]
#[embassy_executor::task]
async fn transport_task(io: &'static Io) {
    use embassy_futures::select::{Either, select};
    use embassy_time::Timer;
    let mut buffer = Buffer([0; (PACKET_BYTES + 4).next_multiple_of(4)]);
    loop {
        if let Either::First(packet) = select(io.next_outbound(), Timer::after_millis(1)).await {
            buffer.0.fill(0);
            buffer.0[3] = packet.kind;
            let len = packet.data().len();
            buffer.0[4..4 + len].copy_from_slice(packet.data());
            let ok = unsafe { cyw43_bluetooth_hci_write(buffer.0.as_mut_ptr(), len + 4) } == 0;
            io.finish_outbound(ok);
            if !ok {
                io.fail();
                return;
            }
        }
        unsafe {
            cordial_radio_poll();
        }
        // Drain the controller before waiting again. A timer between queued
        // packets would cap receive throughput at 1,000 packets per second.
        loop {
            let mut len = 0;
            let result = unsafe {
                cyw43_bluetooth_hci_read(buffer.0.as_mut_ptr(), buffer.0.len() as u32, &mut len)
            };
            if result != 0 || len != 0 && !(4..=PACKET_BYTES + 4).contains(&(len as usize)) {
                io.fail();
                return;
            }
            if len == 0 {
                break;
            }
            let mut packet = Packet::empty();
            packet.kind = buffer.0[3];
            packet.len = (len - 4) as u16;
            packet
                .data_mut()
                .copy_from_slice(&buffer.0[4..len as usize]);
            io.received(packet).await;
        }
    }
}
