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
    pub boot: String,
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
    let boot = format!("{:016x}", random_u64());
    Ok(Identity {
        unique,
        adapter,
        boot,
    })
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
