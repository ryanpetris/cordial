//! Raw application flash in the `cordial_layout` and `cordial_app` partitions.
use cordial_core::storage::Error;
use embedded_storage_async::nor_flash::{ErrorType, NorFlash, NorFlashErrorKind, ReadNorFlash};
use esp_idf_sys as sys;
use std::ops::Range;

pub type Storage = cordial_record_storage::Storage<
    Flash,
    { ((crate::board::STORAGE_END - crate::board::STORAGE_START) / 4096 - 1) as usize },
>;
pub struct Flash {
    guard: &'static sys::esp_partition_t,
    app: &'static sys::esp_partition_t,
}
fn partition(
    subtype: sys::esp_partition_subtype_t,
    range: Range<u32>,
) -> Result<&'static sys::esp_partition_t, Error> {
    let p = unsafe {
        sys::esp_partition_find_first(
            sys::esp_partition_type_t_ESP_PARTITION_TYPE_DATA,
            subtype,
            std::ptr::null(),
        )
        .as_ref()
    }
    .ok_or(Error::Layout)?;
    if p.address != range.start || Some(p.size) != range.end.checked_sub(range.start) {
        return Err(Error::Layout);
    }
    Ok(p)
}
pub fn open(identity: [u8; 32], range: Range<u32>) -> Result<Storage, Error> {
    let start = range.start.checked_add(4096).ok_or(Error::Bounds)?;
    let flash = Flash {
        guard: partition(0x40, range.start..start)?,
        app: partition(0x83, start..range.end)?,
    };
    embassy_futures::block_on(Storage::open_or_provision_blank(flash, range, identity))
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
