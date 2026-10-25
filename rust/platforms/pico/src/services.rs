use alloc::{format, string::String};
use embassy_rp::{
    clocks::RoscRng,
    flash::{Flash, Instance, Mode},
};

/// Startup calls this once, before allocating application or driver state.
///
/// # Safety
/// No other code may use or have initialized the linker-reserved heap.
pub unsafe fn init_heap() {
    unsafe extern "C" {
        static mut __sheap: u8;
        static mut _heap_end: u8;
    }
    let start = core::ptr::addr_of_mut!(__sheap);
    let end = core::ptr::addr_of_mut!(_heap_end);
    assert!(unsafe { crate::allocator::init(start, end.addr() - start.addr()) });
}

pub fn random_u64() -> u64 {
    RoscRng.next_u64()
}

pub struct Identity {
    pub unique: [u8; 8],
    pub adapter: String,
}
pub fn identity<T: Instance, M: Mode, const N: usize>(
    flash: &mut Flash<'_, T, M, N>,
) -> Result<Identity, embassy_rp::flash::Error> {
    #[cfg(feature = "rp2040")]
    let mut unique = [0; 8];
    #[cfg(feature = "rp2040")]
    flash.blocking_unique_id(&mut unique)?;
    #[cfg(any(feature = "rp235xa", feature = "rp235xb"))]
    let unique = {
        let _ = flash;
        embassy_rp::otp::get_chipid()
            .map_err(|_| embassy_rp::flash::Error::Other)?
            .to_be_bytes()
    };
    let adapter = format!("{:016X}", u64::from_be_bytes(unique));
    Ok(Identity { unique, adapter })
}

/// The controller address follows the Wi-Fi OTP MAC plus one. If OTP is unset,
/// use the board's flash ID as the SDK did, preserving this adapter's identity.
pub fn bluetooth_address(mut wifi: [u8; 6], unique: [u8; 8]) -> [u8; 6] {
    if wifi == [0x00, 0xa0, 0x50, 0xb5, 0x59, 0x5e] {
        wifi = fallback_wifi_address(unique);
    }
    wifi[5] = wifi[5].wrapping_add(1);
    wifi
}

pub fn fallback_wifi_address(unique: [u8; 8]) -> [u8; 6] {
    let mut wifi: [u8; 6] = unique[2..].try_into().unwrap();
    wifi[0] = (wifi[0] & !1) | 2;
    wifi
}

#[cfg(feature = "development")]
pub fn bootloader() -> ! {
    embassy_rp::rom_data::reset_to_usb_boot(0, 0);
    loop {
        core::hint::spin_loop();
    }
}

/// Detects the physical flash through JEDEC RDID before other flash users start.
fn detect_flash_capacity<T: Instance, M: Mode, const N: usize>(
    flash: &mut Flash<'_, T, M, N>,
) -> Result<u32, embassy_rp::flash::Error> {
    #[cfg(feature = "rp2040")]
    let jedec = flash.blocking_jedec_id()?;
    #[cfg(any(feature = "rp235xa", feature = "rp235xb"))]
    let jedec = {
        let _ = flash;
        unsafe extern "C" {
            fn cordial_flash_jedec_id() -> u32;
        }
        unsafe { cordial_flash_jedec_id() }
    };
    // JEDEC's capacity byte is the base-2 exponent of the capacity in bytes.
    let exponent = jedec & 0xff;
    if jedec >> 16 == 0 || jedec >> 16 == 0xff || !(1..32).contains(&exponent) {
        return Err(embassy_rp::flash::Error::Other);
    }
    // Embassy's flash commands and the XIP window address at most 16 MiB.
    let capacity = 1u32 << exponent;
    if capacity as usize > N {
        return Err(embassy_rp::flash::Error::OutOfBounds);
    }
    Ok(capacity)
}

/// Retries missing JEDEC responses without inventing a capacity measurement.
pub async fn flash_capacity<T: Instance, M: Mode, const N: usize>(
    flash: &mut Flash<'_, T, M, N>,
) -> Result<Option<u32>, embassy_rp::flash::Error> {
    for attempt in 0..3 {
        match detect_flash_capacity(flash) {
            Ok(capacity) => return Ok(Some(capacity)),
            Err(embassy_rp::flash::Error::OutOfBounds) => {
                return Err(embassy_rp::flash::Error::OutOfBounds);
            }
            Err(_) if attempt < 2 => embassy_time::Timer::after_millis(10).await,
            Err(_) => (),
        }
    }
    Ok(None)
}
