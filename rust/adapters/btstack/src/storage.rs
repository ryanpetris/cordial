//! The public BTstack TLV callbacks map each opaque tag to one record.
use alloc::vec::Vec;
use cordial_core::storage::{Error, RecordKey, RecordStore};
use core::{
    cell::Cell,
    ffi::{c_int, c_void},
};
use embassy_futures::block_on;
use embassy_sync::{blocking_mutex::raw::NoopRawMutex, mutex::Mutex};

#[repr(C)]
pub struct Tlv {
    pub get: unsafe extern "C" fn(*mut c_void, u32, *mut u8, u32) -> c_int,
    pub store: unsafe extern "C" fn(*mut c_void, u32, *const u8, u32) -> c_int,
    pub delete: unsafe extern "C" fn(*mut c_void, u32),
}

/// Application commands and C callbacks run on the same owner. The platform
/// store must make progress without dispatching another owner task, since the
/// public TLV API is synchronous. Pico flash waits only for DMA interrupts.
pub struct Storage<S> {
    store: Mutex<NoopRawMutex, S>,
    error: Cell<Option<Error>>,
    reported: Cell<bool>,
    roots: Cell<Option<[u8; 56]>>,
}
pub struct Handle<'a, S>(&'a Storage<S>);

impl<S: RecordStore> Storage<S> {
    pub const API: Tlv = Tlv {
        get: Self::get,
        store: Self::save,
        delete: Self::remove,
    };
    pub fn new(store: S) -> Self {
        Self {
            store: Mutex::new(store),
            error: Cell::new(None),
            reported: Cell::new(false),
            roots: Cell::new(None),
        }
    }
    pub fn cache_identity(&self) -> Result<(), Error> {
        let identity = self.fault(
            block_on(cordial_core::identity::Identity::load(&mut self.handle()))
                .and_then(|id| id.ok_or(Error::Corrupt)),
        )?;
        self.roots.set(Some(identity.0));
        Ok(())
    }
    pub fn handle(&self) -> Handle<'_, S> {
        Handle(self)
    }
    pub fn context(&self) -> *mut c_void {
        core::ptr::from_ref(self).cast_mut().cast()
    }
    /// Consume after vendor calls, including APIs whose signature discards
    /// storage errors. Bond adoption/removal must not succeed after such a fault.
    pub fn take_error(&self) -> Option<Error> {
        if self.reported.replace(true) {
            None
        } else {
            self.error.get()
        }
    }
    /// A failed native access leaves key state uncertain until the next boot.
    /// Consuming its notification must never re-enable security reads or mutations.
    /// Development filesystem inspection remains available for diagnosis.
    pub fn error(&self) -> Option<Error> {
        self.error.get()
    }
    fn fault<T>(&self, result: Result<T, Error>) -> Result<T, Error> {
        if let Err(error) = result
            && self.error.get().is_none()
        {
            self.error.set(Some(error));
            self.reported.set(false);
        }
        result
    }
    // These pointers are registered together with context() by the adapter and
    // remain valid until the vendor host has stopped. C supplies valid buffers.
    unsafe extern "C" fn get(context: *mut c_void, tag: u32, buffer: *mut u8, size: u32) -> c_int {
        let this = unsafe { &*context.cast::<Self>() };
        if !buffer.is_null() {
            // Some native consumers inspect their destination despite a missing
            // record. Failed and short reads must leave initialized bytes.
            unsafe {
                core::ptr::write_bytes(buffer, 0, size as usize);
            }
        }
        let result = this
            .roots
            .get()
            .map(cordial_core::identity::Identity)
            .map(Some)
            .ok_or(Error::Corrupt);
        match this.fault(result) {
            Ok(Some(identity)) => {
                let offset = match tag {
                    0x534d4952 => 8,
                    0x534d4552 => 24,
                    _ => return 0,
                };
                if buffer.is_null() {
                    return 16;
                }
                let n = (size as usize).min(16);
                unsafe {
                    core::ptr::copy_nonoverlapping(identity.0[offset..].as_ptr(), buffer, n);
                }
                n as c_int
            }
            _ => 0,
        }
    }

    unsafe extern "C" fn save(context: *mut c_void, tag: u32, data: *const u8, size: u32) -> c_int {
        let this = unsafe { &*context.cast::<Self>() };
        if size != 0 && data.is_null() {
            let _ = this.fault::<()>(Err(Error::TooLarge));
            return -1;
        }
        // Initialization must provide roots before starting the stack. Missing
        // or unreadable roots fail closed instead of generating a second identity.
        let _ = (tag, data);
        let _ = this.fault::<()>(Err(Error::Corrupt));
        -1
    }
    unsafe extern "C" fn remove(context: *mut c_void, _tag: u32) {
        let this = unsafe { &*context.cast::<Self>() };
        let _ = this.fault::<()>(Err(Error::Corrupt));
    }
}

impl<S: RecordStore> RecordStore for Handle<'_, S> {
    // Development file inspection remains available after a native storage fault.
    async fn generation(&mut self) -> Result<u64, Error> {
        self.0
            .store
            .try_lock()
            .map_err(|_| Error::Unavailable)?
            .generation()
            .await
    }
    async fn file_entry(
        &mut self,
        path: &str,
        index: usize,
    ) -> Result<Option<cordial_core::storage::FileEntry>, Error> {
        self.0
            .store
            .try_lock()
            .map_err(|_| Error::Unavailable)?
            .file_entry(path, index)
            .await
    }
    async fn file_read(
        &mut self,
        path: &str,
        offset: u32,
        bytes: &mut [u8],
    ) -> Result<usize, Error> {
        self.0
            .store
            .try_lock()
            .map_err(|_| Error::Unavailable)?
            .file_read(path, offset, bytes)
            .await
    }
    async fn next_key(&mut self, after: Option<RecordKey>) -> Result<Option<RecordKey>, Error> {
        if let Some(error) = self.0.error() {
            return Err(error);
        }
        self.0
            .store
            .try_lock()
            .map_err(|_| Error::Unavailable)?
            .next_key(after)
            .await
    }
    async fn keys(&mut self) -> Result<Vec<RecordKey>, Error> {
        if let Some(error) = self.0.error() {
            return Err(error);
        }
        self.0
            .store
            .try_lock()
            .map_err(|_| Error::Unavailable)?
            .keys()
            .await
    }
    async fn available(&mut self) -> Result<usize, Error> {
        if let Some(error) = self.0.error() {
            return Err(error);
        }
        self.0
            .store
            .try_lock()
            .map_err(|_| Error::Unavailable)?
            .available()
            .await
    }

    async fn load(&mut self, key: RecordKey, value: &mut [u8]) -> Result<Option<usize>, Error> {
        if let Some(error) = self.0.error() {
            return Err(error);
        }
        let mut store = self.0.store.try_lock().map_err(|_| Error::Unavailable)?;
        store.load(key, value).await
    }
    async fn save(&mut self, key: RecordKey, value: &[u8]) -> Result<(), Error> {
        if let Some(error) = self.0.error() {
            return Err(error);
        }
        let mut store = self.0.store.try_lock().map_err(|_| Error::Unavailable)?;
        store.save(key, value).await
    }
    async fn remove(&mut self, key: RecordKey) -> Result<(), Error> {
        if let Some(error) = self.0.error() {
            return Err(error);
        }
        let mut store = self.0.store.try_lock().map_err(|_| Error::Unavailable)?;
        store.remove(key).await
    }
}

#[cfg(test)]
mod tests;
