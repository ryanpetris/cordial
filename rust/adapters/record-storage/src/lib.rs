#![no_std]
extern crate alloc;

use alloc::{boxed::Box, format, vec::Vec};
use cordial_core::storage::{Error, RecordKey, RecordStore};
use core::{ops::Range, ptr::NonNull};
use embassy_futures::block_on;
use embedded_storage_async::nor_flash::NorFlash;
use littlefs2::{
    driver,
    fs::{Allocation, Filesystem},
    io::{Error as LfsError, SeekFrom, Write},
    path,
    path::PathBuf,
};

pub const MARKER_BYTES: usize = 32;
pub const BLOCK_BYTES: usize = 4096;
/// LittleFS's smallest read. Directory and metadata reads ask for a few bytes at a time, and on
/// the RP2040 and RP2350 each flash read is a separate DMA transfer with a fixed setup cost, so
/// reading whole 128-byte units cuts a record listing's flash reads about tenfold for about the
/// same number of bytes.
const READ_BYTES: usize = 128;

/// The owner serializes all filesystem access. Operations complete their flash
/// writes before returning; an async caller does not make erase nonblocking.
///
/// The filesystem stays mounted between operations, since a LittleFS mount
/// reads every metadata pair.
pub struct Storage<F: NorFlash + 'static, const BLOCKS: usize> {
    mount: Mount<F, BLOCKS>,
    generation: u64,
    /// Free space as of the last traversal, cleared by every write attempt.
    available: Option<usize>,
}

/// LittleFS state, the driver it reads through and the filesystem mounted over
/// them. LittleFS keeps pointers to the allocation and driver while mounted, so
/// they stay at one heap address for the mount's life.
struct Volume<F: NorFlash + 'static, const BLOCKS: usize> {
    alloc: Allocation<Flash<F, BLOCKS>>,
    driver: Flash<F, BLOCKS>,
    fs: Option<Filesystem<'static, Flash<F, BLOCKS>>>,
}

/// A filesystem mounted on demand over a `Volume` this value owns.
///
/// Safety: `volume` comes from a leaked `Box` and is freed only by `Drop` or
/// `into_driver`, after `fs` is cleared. Every access goes through the raw
/// pointer. `fs` holds the only references to `alloc` and `driver`, created as
/// disjoint field borrows when it is mounted, and it never leaves the heap, so
/// moving or dropping a `Mount` moves only the pointer. The `'static` lifetime is
/// never exposed: `with` lends the filesystem for one call, and no closure
/// returns a borrow of it.
struct Mount<F: NorFlash + 'static, const BLOCKS: usize> {
    volume: NonNull<Volume<F, BLOCKS>>,
}
impl<F: NorFlash + 'static, const BLOCKS: usize> Mount<F, BLOCKS> {
    fn new(driver: Flash<F, BLOCKS>) -> Self {
        let volume = Box::new(Volume {
            alloc: Allocation::new(),
            driver,
            fs: None,
        });
        Self {
            volume: NonNull::from(Box::leak(volume)),
        }
    }
    /// Run `f` on the mounted filesystem, mounting it first if needed. An error
    /// that may leave the in-memory state behind the flash unmounts it, so the
    /// next call reads the flash again.
    fn with<R>(
        &mut self,
        f: impl FnOnce(&Filesystem<'static, Flash<F, BLOCKS>>) -> littlefs2::io::Result<R>,
    ) -> littlefs2::io::Result<R> {
        let volume = self.volume.as_ptr();
        // SAFETY: see `Mount`. `fs` is a field disjoint from `alloc` and `driver`.
        let fs = match unsafe { &mut (*volume).fs } {
            Some(fs) => fs,
            empty => {
                // SAFETY: see `Mount`. No reference to `alloc` or `driver`
                // exists while `fs` is empty.
                let (alloc, driver) = unsafe { (&mut (*volume).alloc, &mut (*volume).driver) };
                // A mount reinitializes all LittleFS state in the allocation.
                empty.insert(Filesystem::mount(alloc, driver)?)
            }
        };
        let result = f(fs);
        if let Err(e) = result
            && !matches!(
                e,
                LfsError::NO_SUCH_ENTRY
                    | LfsError::PATH_NOT_DIR
                    | LfsError::PATH_IS_DIR
                    | LfsError::FILENAME_TOO_LONG
            )
        {
            self.unmount();
        }
        result
    }
    fn unmount(&mut self) {
        // LittleFS buffers are caller-provided, so there is nothing to release.
        // SAFETY: see `Mount`. No borrow of `fs` outlives a `with` call.
        unsafe { (*self.volume.as_ptr()).fs = None };
    }
    fn into_driver(mut self) -> Flash<F, BLOCKS> {
        self.unmount();
        // SAFETY: see `Mount`. The filesystem's borrows ended above, and
        // forgetting `self` keeps `Drop` from freeing the volume again.
        let volume = unsafe { Box::from_raw(self.volume.as_ptr()) };
        core::mem::forget(self);
        volume.driver
    }
}
impl<F: NorFlash + 'static, const BLOCKS: usize> Drop for Mount<F, BLOCKS> {
    fn drop(&mut self) {
        self.unmount();
        // SAFETY: see `Mount`. The filesystem's borrows ended above.
        drop(unsafe { Box::from_raw(self.volume.as_ptr()) });
    }
}
struct Flash<F, const BLOCKS: usize> {
    flash: F,
    start: u32,
}
impl<F: NorFlash, const BLOCKS: usize> driver::Storage for Flash<F, BLOCKS> {
    const READ_SIZE: usize = READ_BYTES;
    const WRITE_SIZE: usize = F::WRITE_SIZE;
    const BLOCK_SIZE: usize = BLOCK_BYTES;
    const BLOCK_COUNT: usize = BLOCKS;
    const BLOCK_CYCLES: isize = 500;
    type CACHE_SIZE = littlefs2::consts::U512;
    type LOOKAHEAD_SIZE = littlefs2::consts::U16;
    fn read(&mut self, off: usize, buf: &mut [u8]) -> littlefs2::io::Result<usize> {
        self.bounds(off, buf.len())?;
        block_on(self.flash.read(self.start + off as u32, buf)).map_err(|_| LfsError::IO)?;
        Ok(buf.len())
    }
    fn write(&mut self, off: usize, buf: &[u8]) -> littlefs2::io::Result<usize> {
        self.bounds(off, buf.len())?;
        block_on(self.flash.write(self.start + off as u32, buf)).map_err(|_| LfsError::IO)?;
        Ok(buf.len())
    }
    fn erase(&mut self, off: usize, len: usize) -> littlefs2::io::Result<usize> {
        self.bounds(off, len)?;
        block_on(
            self.flash
                .erase(self.start + off as u32, self.start + (off + len) as u32),
        )
        .map_err(|_| LfsError::IO)?;
        Ok(len)
    }
}
impl<F, const BLOCKS: usize> Flash<F, BLOCKS> {
    fn bounds(&self, off: usize, len: usize) -> littlefs2::io::Result<()> {
        if off
            .checked_add(len)
            .is_none_or(|end| end > BLOCKS * BLOCK_BYTES)
        {
            Err(LfsError::INVALID)
        } else {
            Ok(())
        }
    }
}
fn error(e: LfsError) -> Error {
    match e {
        LfsError::NO_SPACE => Error::Full,
        LfsError::NO_MEMORY => Error::Unavailable,
        LfsError::INVALID | LfsError::FILENAME_TOO_LONG => Error::Bounds,
        LfsError::FILE_TOO_BIG => Error::TooLarge,
        // A failed metadata checksum is a read error: the data may read back on a later try.
        LfsError::NO_SUCH_ENTRY | LfsError::PATH_NOT_DIR => Error::Missing,
        _ => Error::Io,
    }
}
fn checked_path(value: &str) -> Result<PathBuf, Error> {
    if !value.starts_with('/') || value.split('/').any(|part| part == ".." || part == ".") {
        return Err(Error::Bounds);
    }
    PathBuf::try_from(value).map_err(|_| Error::Bounds)
}
impl<F: NorFlash + 'static, const BLOCKS: usize> Storage<F, BLOCKS> {
    fn config(flash: &F, range: &Range<u32>) -> Result<(), Error> {
        if BLOCKS < 2
            || F::ERASE_SIZE != BLOCK_BYTES
            || range.end as usize > flash.capacity()
            || range.end.checked_sub(range.start).map(|n| n as usize)
                != Some((BLOCKS + 1) * BLOCK_BYTES)
            || !(range.start as usize).is_multiple_of(BLOCK_BYTES)
            || F::READ_SIZE == 0
            || F::WRITE_SIZE == 0
            || !READ_BYTES.is_multiple_of(F::READ_SIZE)
            || !512usize.is_multiple_of(F::WRITE_SIZE)
        {
            return Err(Error::Bounds);
        }
        Ok(())
    }
    pub async fn open_or_provision_blank(
        mut flash: F,
        range: Range<u32>,
        identity: [u8; MARKER_BYTES],
    ) -> Result<Self, Error> {
        Self::config(&flash, &range)?;
        let mut page = [0; 512];
        flash
            .read(range.start, &mut page)
            .await
            .map_err(|_| Error::Io)?;
        if page[..MARKER_BYTES] == [0xff; MARKER_BYTES] {
            Self::provision_blank(flash, range, identity).await
        } else {
            Self::open(flash, range, identity).await
        }
    }
    pub async fn open(
        mut flash: F,
        range: Range<u32>,
        identity: [u8; MARKER_BYTES],
    ) -> Result<Self, Error> {
        Self::config(&flash, &range)?;
        let mut page = [0; 512];
        flash
            .read(range.start, &mut page)
            .await
            .map_err(|_| Error::Io)?;
        if page[..MARKER_BYTES] == [0xff; MARKER_BYTES] {
            return Err(Error::Unprovisioned);
        }
        if page[..MARKER_BYTES] != identity {
            return Err(Error::Layout);
        }
        let mut store = Self {
            mount: Mount::new(Flash {
                flash,
                start: range.start + BLOCK_BYTES as u32,
            }),
            generation: 0,
            available: None,
        };
        // Never format on mount failure. An interrupted provisioning attempt
        // requires explicit recovery, just like unknown or damaged storage.
        store.mount.with(|_| Ok(())).map_err(error)?;
        // Cleanup is best effort, and its first failure ends it: a stale temporary file is
        // truncated by the next replacement, listings skip a record directory without its
        // record file, and record IDs are never reused. A failure that may leave the mounted
        // state behind the flash unmounts it, so the next operation mounts again.
        let _ = store.mount.with(|fs| {
            for name in [
                "/identity.json.tmp",
                "/adapter.json.tmp",
                "/sequence.json.tmp",
                "/format.json.tmp",
            ] {
                let path = PathBuf::try_from(name).map_err(|_| LfsError::INVALID)?;
                match fs.remove(&path) {
                    Ok(()) | Err(LfsError::NO_SUCH_ENTRY) => (),
                    Err(e) => return Err(e),
                }
            }
            for (directory, record, temps) in [
                (
                    path!("/devices"),
                    path!("device.json"),
                    &[
                        path!("device.json.tmp"),
                        path!("settings.json.tmp"),
                        path!("layout.json.tmp"),
                    ][..],
                ),
                (
                    path!("/profiles"),
                    path!("profile.json"),
                    &[path!("profile.json.tmp"), path!("rules.json.tmp")][..],
                ),
            ] {
                match fs.read_dir_and_then(directory, |iter| {
                    for entry in iter {
                        let entry = entry?;
                        if !entry.metadata().is_dir()
                            || cordial_core::storage::parse_id(entry.file_name().as_str()).is_none()
                        {
                            continue;
                        }
                        match fs.metadata(&entry.path().join(record)) {
                            // A directory in the commit file's place holds no record.
                            Ok(meta) if !meta.is_file() => fs.remove_dir_all(entry.path())?,
                            Err(LfsError::NO_SUCH_ENTRY) => fs.remove_dir_all(entry.path())?,
                            Err(e) => return Err(e),
                            Ok(_) => {
                                for temp in temps {
                                    match fs.remove(&entry.path().join(temp)) {
                                        Ok(()) | Err(LfsError::NO_SUCH_ENTRY) => (),
                                        Err(e) => return Err(e),
                                    }
                                }
                            }
                        }
                    }
                    Ok(())
                }) {
                    Ok(()) | Err(LfsError::NO_SUCH_ENTRY) => (),
                    Err(e) => return Err(e),
                }
            }
            Ok(())
        });
        Ok(store)
    }
    pub async fn provision_blank(
        mut flash: F,
        range: Range<u32>,
        identity: [u8; MARKER_BYTES],
    ) -> Result<Self, Error> {
        Self::config(&flash, &range)?;
        if identity == [0xff; MARKER_BYTES] {
            return Err(Error::Layout);
        }
        let mut page = [0; 512];
        for at in (range.start..range.end).step_by(page.len()) {
            flash.read(at, &mut page).await.map_err(|_| Error::Io)?;
            if page.iter().any(|&v| v != 0xff) {
                return Err(Error::Layout);
            }
        }
        page.fill(0xff);
        page[..MARKER_BYTES].copy_from_slice(&identity);
        // Claim the layout before formatting. Any interruption leaves either a
        // mountable filesystem or a failure, never permission to format again.
        flash
            .write(range.start, &page)
            .await
            .map_err(|_| Error::Io)?;
        flash
            .read(range.start, &mut page)
            .await
            .map_err(|_| Error::Io)?;
        if page[..MARKER_BYTES] != identity {
            return Err(Error::Io);
        }
        let mut driver = Flash {
            flash,
            start: range.start + BLOCK_BYTES as u32,
        };
        Filesystem::format(&mut driver).map_err(error)?;
        let mut store = Self {
            mount: Mount::new(driver),
            generation: 0,
            available: None,
        };
        store.mount.with(|_| Ok(())).map_err(error)?;
        Ok(store)
    }
    pub fn into_flash(self) -> F {
        self.mount.into_driver().flash
    }
    pub fn generation(&self) -> u64 {
        self.generation
    }
    pub fn read_file(&mut self, name: &str, offset: u32, bytes: &mut [u8]) -> Result<usize, Error> {
        let path = checked_path(name)?;
        self.mount
            .with(|fs| {
                fs.open_file_and_then(&path, |file| {
                    file.seek(SeekFrom::Start(offset))?;
                    file.read(bytes)
                })
            })
            .map_err(error)
    }
    pub fn file_size(&mut self, name: &str) -> Result<Option<usize>, Error> {
        let path = checked_path(name)?;
        self.mount
            .with(|fs| match fs.metadata(&path) {
                Ok(m) if m.is_file() => Ok(Some(m.len())),
                Ok(_) => Err(LfsError::PATH_IS_DIR),
                Err(LfsError::NO_SUCH_ENTRY) => Ok(None),
                Err(e) => Err(e),
            })
            .map_err(error)
    }
    pub fn entry(
        &mut self,
        name: &str,
        index: usize,
    ) -> Result<Option<littlefs2::fs::DirEntry>, Error> {
        let path = checked_path(name)?;
        // ponytail: reopening and skipping uses constant RAM but is quadratic
        // for a full listing. Use a pinned open iterator if measured latency warrants it.
        self.mount
            .with(|fs| {
                fs.read_dir_and_then(&path, |iter| {
                    let mut remaining = index;
                    for entry in iter {
                        let entry = entry?;
                        if matches!(entry.file_name().as_str(), "." | "..") {
                            continue;
                        }
                        if remaining == 0 {
                            return Ok(Some(entry));
                        }
                        remaining -= 1;
                    }
                    Ok(None)
                })
            })
            .map_err(error)
    }
    pub fn replace_file(&mut self, name: &str, bytes: &[u8]) -> Result<(), Error> {
        let path = checked_path(name)?;
        let temp = checked_path(&format!("{name}.tmp"))?;
        let same = self.mount.with(|fs| equal(fs, &path, bytes));
        if same == Ok(true) {
            return Ok(());
        }
        same.map_err(error)?;
        // Even a failed attempt can change temporary files visible to debug readers.
        self.generation = self.generation.checked_add(1).ok_or(Error::Unavailable)?;
        self.available = None;
        let result = self.mount.with(|fs| {
            if let Some(parent) = path.parent() {
                fs.create_dir_all(&parent)?;
            }
            fs.create_file_and_then(&temp, |file| file.write_all(bytes))?;
            fs.rename(&temp, &path)
        });
        match result {
            Ok(()) => Ok(()),
            Err(e) => {
                // Resolve against a fresh mount, never the mutation's caches.
                self.mount.unmount();
                match self.mount.with(|fs| equal(fs, &path, bytes)) {
                    Ok(true) => Ok(()),
                    Ok(false) => {
                        // The old destination is authoritative. Reclaim the failed
                        // replacement now; startup retries cleanup after I/O failure.
                        let _ = self.mount.with(|fs| fs.remove(&temp));
                        Err(error(e))
                    }
                    Err(_) => Err(Error::Unknown),
                }
            }
        }
    }
    pub fn remove_file(&mut self, name: &str) -> Result<(), Error> {
        let path = checked_path(name)?;
        self.generation = self.generation.checked_add(1).ok_or(Error::Unavailable)?;
        self.available = None;
        let result = self.mount.with(|fs| {
            match fs.remove(&path) {
                Ok(()) | Err(LfsError::NO_SUCH_ENTRY) => (),
                Err(e) => return Err(e),
            }
            if (name.starts_with("/devices/") || name.starts_with("/profiles/"))
                && let Some(parent) = path.parent()
            {
                // Empty record directories consume a metadata pair. Their
                // removal is cleanup, after the authoritative file deletion.
                return Ok(matches!(
                    fs.remove_dir(&parent),
                    Ok(()) | Err(LfsError::DIR_NOT_EMPTY | LfsError::NO_SUCH_ENTRY)
                ));
            }
            Ok(true)
        });
        match result {
            Ok(true) => Ok(()),
            Ok(false) => {
                // A failed cleanup commit leaves the next operation a fresh mount.
                self.mount.unmount();
                Ok(())
            }
            Err(LfsError::NO_SUCH_ENTRY) => Ok(()),
            Err(e) => {
                // Resolve against a fresh mount, never the mutation's caches.
                self.mount.unmount();
                match self.file_size(name) {
                    Ok(None) => Ok(()),
                    Ok(Some(_)) => Err(error(e)),
                    Err(_) => Err(Error::Unknown),
                }
            }
        }
    }
}
fn equal<F: driver::Storage>(
    fs: &Filesystem<'_, F>,
    path: &littlefs2::path::Path,
    bytes: &[u8],
) -> littlefs2::io::Result<bool> {
    match fs.metadata(path) {
        Err(LfsError::NO_SUCH_ENTRY) => return Ok(false),
        Err(e) => return Err(e),
        Ok(m) if m.len() != bytes.len() || !m.is_file() => return Ok(false),
        _ => (),
    }
    fs.open_file_and_then(path, |file| {
        let mut buffer = [0; 512];
        let mut at = 0;
        while at < bytes.len() {
            let len = buffer.len().min(bytes.len() - at);
            let n = file.read(&mut buffer[..len])?;
            if n == 0 || buffer[..n] != bytes[at..at + n] {
                return Ok(false);
            }
            at += n;
        }
        Ok(true)
    })
}
fn record_path(key: RecordKey) -> Result<alloc::string::String, Error> {
    let id = u64::from_be_bytes(key[1..].try_into().unwrap());
    Ok(match (key[0], id) {
        (0, 0) => "/format.json".into(),
        (1, 0) => "/adapter.json".into(),
        (3, 0) => "/identity.json".into(),
        (7, 0) => "/sequence.json".into(),
        (8, id) if id != 0 => format!("/profiles/{id}/profile.json"),
        (9, id) if id != 0 => format!("/profiles/{id}/rules.json"),
        (2, id) if id != 0 => format!("/devices/{id}/device.json"),
        (4, id) if id != 0 => format!("/devices/{id}/settings.json"),
        (5, id) if id != 0 => format!("/devices/{id}/layout.json"),
        _ => return Err(Error::Bounds),
    })
}
impl<F: NorFlash + 'static, const BLOCKS: usize> RecordStore for Storage<F, BLOCKS> {
    async fn generation(&mut self) -> Result<u64, Error> {
        Ok(self.generation)
    }
    async fn file_entry(
        &mut self,
        path: &str,
        index: usize,
    ) -> Result<Option<cordial_core::storage::FileEntry>, Error> {
        Ok(self
            .entry(path, index)?
            .map(|e| cordial_core::storage::FileEntry {
                name: e.file_name().as_str().into(),
                kind: if e.metadata().is_dir() {
                    cordial_core::storage::FileType::Directory
                } else {
                    cordial_core::storage::FileType::File
                },
                size: e.metadata().len(),
            }))
    }
    async fn file_read(
        &mut self,
        path: &str,
        offset: u32,
        bytes: &mut [u8],
    ) -> Result<usize, Error> {
        self.read_file(path, offset, bytes)
    }
    async fn keys(&mut self) -> Result<Vec<RecordKey>, Error> {
        let mut result = Vec::new();
        let mut previous = None;
        while let Some(key) = self.next_key(previous).await? {
            previous = Some(key);
            result.try_reserve(1).map_err(|_| Error::Unavailable)?;
            result.push(key);
        }
        Ok(result)
    }
    async fn record_ids(&mut self, kind: u8, after: u64, limit: usize) -> Result<Vec<u64>, Error> {
        // Only the directory is read. Checking each directory for its record file would cost
        // about as much as reading the record, which the caller does anyway.
        let directory = match kind {
            2 => path!("/devices"),
            8 => path!("/profiles"),
            _ => return Err(Error::Bounds),
        };
        let mut ids = Vec::new();
        if limit == 0 {
            return Ok(ids);
        }
        ids.try_reserve_exact(limit)
            .map_err(|_| Error::Unavailable)?;
        self.mount
            .with(|fs| {
                match fs.read_dir_and_then(directory, |iter| {
                    for entry in iter {
                        let entry = entry?;
                        if !entry.metadata().is_dir() {
                            continue;
                        }
                        let Some(id) = cordial_core::storage::parse_id(entry.file_name().as_str())
                            .filter(|id| *id > after)
                        else {
                            continue;
                        };
                        let index = ids.partition_point(|key| *key < id);
                        if index >= limit {
                            continue;
                        }
                        if ids.len() == limit {
                            ids.pop();
                        }
                        ids.insert(index, id);
                    }
                    Ok(())
                }) {
                    Ok(()) | Err(LfsError::NO_SUCH_ENTRY) => Ok(()),
                    Err(e) => Err(e),
                }
            })
            .map_err(error)?;
        Ok(ids)
    }
    async fn next_key(&mut self, after: Option<RecordKey>) -> Result<Option<RecordKey>, Error> {
        use cordial_core::storage::record_key;
        let mut next = None;
        self.mount
            .with(|fs| {
                fn consider(
                    next: &mut Option<RecordKey>,
                    after: Option<RecordKey>,
                    key: RecordKey,
                ) {
                    if after.is_none_or(|a| key > a) && next.is_none_or(|n| key < n) {
                        *next = Some(key);
                    }
                }
                for (kind, name) in [
                    (0, path!("/format.json")),
                    (1, path!("/adapter.json")),
                    (3, path!("/identity.json")),
                    (7, path!("/sequence.json")),
                ] {
                    match fs.metadata(name) {
                        Ok(_) => consider(&mut next, after, record_key(kind, 0)),
                        Err(LfsError::NO_SUCH_ENTRY) => (),
                        Err(e) => return Err(e),
                    }
                }
                for (directory, records) in [
                    (
                        path!("/devices"),
                        &[(2, "device.json"), (4, "settings.json")][..],
                    ),
                    (path!("/profiles"), &[(8, "profile.json")][..]),
                ] {
                    if after.is_some_and(|key| key[0] > records.last().unwrap().0)
                        || next.is_some_and(|key| key[0] < records[0].0)
                    {
                        continue;
                    }
                    match fs.read_dir_and_then(directory, |iter| {
                        for entry in iter {
                            let entry = entry?;
                            let Some(id) =
                                cordial_core::storage::parse_id(entry.file_name().as_str())
                            else {
                                continue;
                            };
                            if !entry.metadata().is_dir() {
                                continue;
                            }
                            for &(kind, suffix) in records {
                                let suffix =
                                    PathBuf::try_from(suffix).map_err(|_| LfsError::INVALID)?;
                                match fs.metadata(&entry.path().join(&suffix)) {
                                    Ok(meta) if meta.is_file() => {
                                        consider(&mut next, after, record_key(kind, id))
                                    }
                                    // A directory in a record's place holds no record.
                                    Ok(_) | Err(LfsError::NO_SUCH_ENTRY) => (),
                                    Err(e) => return Err(e),
                                }
                            }
                        }
                        Ok(())
                    }) {
                        Ok(()) | Err(LfsError::NO_SUCH_ENTRY) => (),
                        Err(e) => return Err(e),
                    }
                }
                Ok(next)
            })
            .map_err(error)
    }
    async fn available(&mut self) -> Result<usize, Error> {
        // LittleFS counts used blocks by traversing every metadata pair and file.
        if let Some(bytes) = self.available {
            return Ok(bytes);
        }
        let bytes = self.mount.with(|fs| fs.available_space()).map_err(error)?;
        self.available = Some(bytes);
        Ok(bytes)
    }
    async fn load(&mut self, key: RecordKey, bytes: &mut [u8]) -> Result<Option<usize>, Error> {
        let path = checked_path(&record_path(key)?)?;
        let read = self.mount.with(|fs| {
            fs.open_file_and_then(&path, |file| {
                let n = file.len()?;
                let Some(bytes) = bytes.get_mut(..n) else {
                    return Ok(Err(Error::TooLarge));
                };
                Ok(if file.read(bytes)? == n {
                    Ok(Some(n))
                } else {
                    Err(Error::Io)
                })
            })
        });
        match read {
            Ok(result) => result,
            Err(LfsError::NO_SUCH_ENTRY) => Ok(None),
            // A directory in the record's place.
            Err(LfsError::PATH_IS_DIR) => Err(Error::Io),
            Err(e) => Err(error(e)),
        }
    }
    async fn save(&mut self, key: RecordKey, bytes: &[u8]) -> Result<(), Error> {
        self.replace_file(&record_path(key)?, bytes)
    }
    async fn remove(&mut self, key: RecordKey) -> Result<(), Error> {
        self.remove_file(&record_path(key)?)
    }
}
