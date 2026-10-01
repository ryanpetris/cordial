#![no_std]
extern crate alloc;

use alloc::{format, vec::Vec};
use cordial_core::storage::{Error, RecordKey, RecordStore};
use core::ops::Range;
use embassy_futures::block_on;
use embedded_storage_async::nor_flash::NorFlash;
use littlefs2::{
    driver,
    fs::Filesystem,
    io::{Error as LfsError, SeekFrom, Write},
    path,
    path::PathBuf,
};

pub const MARKER_BYTES: usize = 32;
pub const BLOCK_BYTES: usize = 4096;

/// The owner serializes all filesystem access. Operations complete their flash
/// writes before returning; an async caller does not make erase nonblocking.
pub struct Storage<F: NorFlash, const BLOCKS: usize> {
    driver: Flash<F, BLOCKS>,
    generation: u64,
}
struct Flash<F, const BLOCKS: usize> {
    flash: F,
    start: u32,
}
impl<F: NorFlash, const BLOCKS: usize> driver::Storage for Flash<F, BLOCKS> {
    const READ_SIZE: usize = F::READ_SIZE;
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
impl<F: NorFlash, const BLOCKS: usize> Storage<F, BLOCKS> {
    fn config(flash: &F, range: &Range<u32>) -> Result<(), Error> {
        if BLOCKS < 2
            || F::ERASE_SIZE != BLOCK_BYTES
            || range.end as usize > flash.capacity()
            || range.end.checked_sub(range.start).map(|n| n as usize)
                != Some((BLOCKS + 1) * BLOCK_BYTES)
            || !(range.start as usize).is_multiple_of(BLOCK_BYTES)
            || F::READ_SIZE == 0
            || F::WRITE_SIZE == 0
            || !512usize.is_multiple_of(F::READ_SIZE)
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
            driver: Flash {
                flash,
                start: range.start + BLOCK_BYTES as u32,
            },
            generation: 0,
        };
        // Never format on mount failure. An interrupted provisioning attempt
        // requires explicit recovery, just like unknown or damaged storage.
        Filesystem::mount_and_then(&mut store.driver, |fs| {
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
            match fs.read_dir_and_then(path!("/devices"), |iter| {
                for entry in iter {
                    let entry = entry?;
                    let name = entry.file_name().as_str();
                    if !entry.metadata().is_dir()
                        || name.len() != 16
                        || !name
                            .bytes()
                            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                    {
                        continue;
                    }
                    let device = entry.path().join(path!("device.json"));
                    match fs.metadata(&device) {
                        Err(LfsError::NO_SUCH_ENTRY) => {
                            fs.remove_dir_all(entry.path())?;
                        }
                        Err(e) => return Err(e),
                        Ok(_) => {
                            for temp in [path!("device.json.tmp"), path!("hidpp.json.tmp")] {
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
                Ok(()) | Err(LfsError::NO_SUCH_ENTRY) => Ok(()),
                Err(e) => Err(e),
            }
        })
        .map_err(error)?;
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
        Ok(Self {
            driver,
            generation: 0,
        })
    }
    pub fn into_flash(self) -> F {
        self.driver.flash
    }
    pub fn generation(&self) -> u64 {
        self.generation
    }
    pub fn read_file(&mut self, name: &str, offset: u32, bytes: &mut [u8]) -> Result<usize, Error> {
        let path = checked_path(name)?;
        Filesystem::mount_and_then(&mut self.driver, |fs| {
            fs.open_file_and_then(&path, |file| {
                file.seek(SeekFrom::Start(offset))?;
                file.read(bytes)
            })
        })
        .map_err(error)
    }
    pub fn file_size(&mut self, name: &str) -> Result<Option<usize>, Error> {
        let path = checked_path(name)?;
        Filesystem::mount_and_then(&mut self.driver, |fs| match fs.metadata(&path) {
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
        Filesystem::mount_and_then(&mut self.driver, |fs| {
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
        let same = Filesystem::mount_and_then(&mut self.driver, |fs| equal(fs, &path, bytes));
        if same == Ok(true) {
            return Ok(());
        }
        same.map_err(error)?;
        // Even a failed attempt can change temporary files visible to debug readers.
        self.generation = self.generation.checked_add(1).ok_or(Error::Unavailable)?;
        let result = Filesystem::mount_and_then(&mut self.driver, |fs| {
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
                match Filesystem::mount_and_then(&mut self.driver, |fs| equal(fs, &path, bytes)) {
                    Ok(true) => Ok(()),
                    Ok(false) => {
                        // The old destination is authoritative. Reclaim the failed
                        // replacement now; startup retries cleanup after I/O failure.
                        let _ = Filesystem::mount_and_then(&mut self.driver, |fs| fs.remove(&temp));
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
        let result = Filesystem::mount_and_then(&mut self.driver, |fs| {
            match fs.remove(&path) {
                Ok(()) | Err(LfsError::NO_SUCH_ENTRY) => (),
                Err(e) => return Err(e),
            }
            if name.starts_with("/devices/")
                && let Some(parent) = path.parent()
            {
                // Empty per-device directories consume a metadata pair. Their
                // removal is cleanup, after the authoritative file deletion.
                let _ = fs.remove_dir(&parent);
            }
            Ok(())
        });
        match result {
            Ok(()) | Err(LfsError::NO_SUCH_ENTRY) => Ok(()),
            Err(e) => match self.file_size(name) {
                Ok(None) => Ok(()),
                Ok(Some(_)) => Err(error(e)),
                Err(_) => Err(Error::Unknown),
            },
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
        (2, id) if id != 0 => format!("/devices/{id:016x}/device.json"),
        (4, id) if id != 0 => format!("/devices/{id:016x}/hidpp.json"),
        _ => return Err(Error::Bounds),
    })
}
impl<F: NorFlash, const BLOCKS: usize> RecordStore for Storage<F, BLOCKS> {
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
    async fn next_key(&mut self, after: Option<RecordKey>) -> Result<Option<RecordKey>, Error> {
        use cordial_core::storage::record_key;
        let mut next = None;
        Filesystem::mount_and_then(&mut self.driver, |fs| {
            let mut consider = |key| {
                if after.is_none_or(|a| key > a) && next.is_none_or(|n| key < n) {
                    next = Some(key);
                }
            };
            for (kind, name) in [
                (0, path!("/format.json")),
                (1, path!("/adapter.json")),
                (3, path!("/identity.json")),
                (7, path!("/sequence.json")),
            ] {
                match fs.metadata(name) {
                    Ok(_) => consider(record_key(kind, 0)),
                    Err(LfsError::NO_SUCH_ENTRY) => (),
                    Err(e) => return Err(e),
                }
            }
            match fs.read_dir_and_then(path!("/devices"), |iter| {
                for entry in iter {
                    let entry = entry?;
                    let name = entry.file_name().as_str();
                    if !entry.metadata().is_dir()
                        || name.len() != 16
                        || !name
                            .bytes()
                            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                    {
                        continue;
                    }
                    let id = u64::from_str_radix(name, 16).map_err(|_| LfsError::CORRUPTION)?;
                    for (kind, suffix) in [(2, "device.json"), (4, "hidpp.json")] {
                        let path = PathBuf::try_from(format!("/devices/{name}/{suffix}").as_str())
                            .map_err(|_| LfsError::INVALID)?;
                        match fs.metadata(&path) {
                            Ok(_) => consider(record_key(kind, id)),
                            Err(LfsError::NO_SUCH_ENTRY) => (),
                            Err(e) => return Err(e),
                        }
                    }
                }
                Ok(())
            }) {
                Ok(()) | Err(LfsError::NO_SUCH_ENTRY) => Ok(next),
                Err(e) => Err(e),
            }
        })
        .map_err(error)
    }
    async fn available(&mut self) -> Result<usize, Error> {
        Filesystem::mount_and_then(&mut self.driver, |fs| fs.available_space()).map_err(error)
    }
    async fn load(&mut self, key: RecordKey, bytes: &mut [u8]) -> Result<Option<usize>, Error> {
        let name = record_path(key)?;
        let Some(n) = self.file_size(&name)? else {
            return Ok(None);
        };
        if n > bytes.len() {
            return Err(Error::TooLarge);
        }
        let got = self.read_file(&name, 0, &mut bytes[..n])?;
        if got != n {
            return Err(Error::Io);
        }
        Ok(Some(n))
    }
    async fn save(&mut self, key: RecordKey, bytes: &[u8]) -> Result<(), Error> {
        self.replace_file(&record_path(key)?, bytes)
    }
    async fn remove(&mut self, key: RecordKey) -> Result<(), Error> {
        self.remove_file(&record_path(key)?)
    }
}
