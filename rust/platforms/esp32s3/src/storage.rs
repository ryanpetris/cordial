//! Runtime partitions on the detected main flash, outside the firmware reservation.
use cordial_core::storage::Error;
use embedded_storage_async::nor_flash::{ErrorType, NorFlash, NorFlashErrorKind, ReadNorFlash};
use esp_idf_sys as sys;
use std::ops::Range;

pub type Storage = cordial_record_storage::Storage<Flash>;
pub struct Flash {
    guard: &'static sys::esp_partition_t,
    app: &'static sys::esp_partition_t,
}
fn partition(
    subtype: sys::esp_partition_subtype_t,
    range: Range<u32>,
) -> Result<&'static sys::esp_partition_t, Error> {
    let mut p = std::ptr::null();
    let label = if subtype == 0x40 {
        c"cordial_layout"
    } else {
        c"cordial_app"
    };
    let code = unsafe {
        sys::esp_partition_register_external(
            std::ptr::null_mut(),
            range.start as usize,
            (range.end - range.start) as usize,
            label.as_ptr(),
            sys::esp_partition_type_t_ESP_PARTITION_TYPE_DATA,
            subtype,
            &mut p,
        )
    };
    if code != sys::ESP_OK {
        return Err(Error::Layout);
    }
    unsafe { p.as_ref() }.ok_or(Error::Layout)
}
pub fn open(identity: [u8; 32]) -> Result<Storage, Error> {
    let mut capacity = 0;
    let mut addressable = 0;
    if unsafe {
        sys::platform::cordial_esp_storage_capacity(
            crate::board::MINIMUM_FLASH_BYTES,
            &mut capacity,
            &mut addressable,
        )
    } != sys::ESP_OK
    {
        return Err(Error::Io);
    }
    let range = cordial_record_storage::flash_range(addressable)?;
    if range.start != crate::board::STORAGE_START {
        return Err(Error::Bounds);
    }
    let start = range.start.checked_add(4096).ok_or(Error::Bounds)?;
    let guard = partition(0x40, range.start..start)?;
    let app = match partition(0x83, start..range.end) {
        Ok(p) => p,
        Err(e) => {
            unsafe {
                sys::esp_partition_deregister_external(guard);
            }
            return Err(e);
        }
    };
    let flash = Flash { guard, app };
    embassy_futures::block_on(Storage::open_with_limits(
        flash,
        range,
        (capacity == 0 || capacity >= crate::board::MINIMUM_FLASH_BYTES)
            .then_some(capacity.max(crate::board::MINIMUM_FLASH_BYTES)),
        (capacity != 0).then_some(capacity),
        identity,
    ))
}
impl Drop for Flash {
    fn drop(&mut self) {
        unsafe {
            sys::esp_partition_deregister_external(self.app);
            sys::esp_partition_deregister_external(self.guard);
        }
    }
}
impl Flash {
    fn region(
        &self,
        offset: u32,
        size: usize,
    ) -> Result<(&sys::esp_partition_t, usize), NorFlashErrorKind> {
        let end = offset
            .checked_add(u32::try_from(size).map_err(|_| NorFlashErrorKind::OutOfBounds)?)
            .ok_or(NorFlashErrorKind::OutOfBounds)?;
        for p in [self.guard, self.app] {
            if offset >= p.address && end <= p.address + p.size {
                return Ok((p, (offset - p.address) as usize));
            }
        }
        Err(NorFlashErrorKind::OutOfBounds)
    }
}
fn result(code: sys::esp_err_t) -> Result<(), NorFlashErrorKind> {
    if code == sys::ESP_OK {
        Ok(())
    } else {
        Err(NorFlashErrorKind::Other)
    }
}
impl ErrorType for Flash {
    type Error = NorFlashErrorKind;
}
impl ReadNorFlash for Flash {
    const READ_SIZE: usize = 1;
    fn capacity(&self) -> usize {
        (self.app.address + self.app.size) as usize
    }
    async fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Self::Error> {
        let (p, off) = self.region(offset, bytes.len())?;
        result(unsafe { sys::esp_partition_read(p, off, bytes.as_mut_ptr().cast(), bytes.len()) })
    }
}
impl NorFlash for Flash {
    const WRITE_SIZE: usize = 4;
    const ERASE_SIZE: usize = 4096;
    async fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Self::Error> {
        let (p, off) = self.region(offset, bytes.len())?;
        result(unsafe { sys::esp_partition_write(p, off, bytes.as_ptr().cast(), bytes.len()) })
    }
    async fn erase(&mut self, from: u32, to: u32) -> Result<(), Self::Error> {
        let len = to.checked_sub(from).ok_or(NorFlashErrorKind::OutOfBounds)? as usize;
        let (p, off) = self.region(from, len)?;
        result(unsafe { sys::esp_partition_erase_range(p, off, len) })
    }
}
