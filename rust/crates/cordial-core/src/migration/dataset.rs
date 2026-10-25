//! Complete raw inventory validation with one file resident at a time, plus its v1 conversion.
use super::*;
use storage::FileEntry;

/// One raw filesystem entry. Paths are absolute; directories are included, even when empty.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Entry {
    pub path: String,
    pub kind: FileType,
    pub size: usize,
}
/// A stable read-only dataset, supplied by a mounted filesystem or a host RAM snapshot.
///
/// Implementations enumerate every entry exactly once after `rewind`, including empty files,
/// unknown files and directories. `read` returns `None` only for a missing path; an empty file is
/// `Some(Vec::new())`. The dataset must stay unchanged throughout validation. No filesystem data
/// is retained by the validator between calls. A current record uses one file buffer; converting
/// a legacy record also holds its replacement. Parent commit records are released before child
/// payloads are read. File buffers and inventory paths use fallible allocations.
#[allow(async_fn_in_trait)]
pub trait Dataset {
    fn rewind(&mut self);
    async fn next_entry(&mut self) -> Result<Option<Entry>, Error>;
    async fn read(&mut self, path: &str) -> Result<Option<Vec<u8>>, Error>;
}
struct Directory {
    path: String,
    index: usize,
}
/// Raw inventory adapter for `RecordStore`'s directory and file APIs.
/// The backing store must remain mounted and unchanged throughout inspection.
pub struct Inventory<'a, S> {
    pub(super) store: &'a mut S,
    directories: Vec<Directory>,
    started: bool,
}
impl<'a, S: RecordStore> Inventory<'a, S> {
    pub fn new(store: &'a mut S) -> Self {
        Self {
            store,
            directories: Vec::new(),
            started: false,
        }
    }
    /// Preserves recognizable historical or unsupported records under a current root.
    /// Payload corruption stays with the ordinary record loaders and essential-root recovery.
    /// Migration requires full dataset validation.
    pub async fn validate_current_startup(&mut self) -> Result<(), Error> {
        self.rewind();
        while let Some(entry) = self.next_entry().await? {
            let Path::Record(key) = classify_path(&entry.path, entry.kind)? else {
                continue;
            };
            if entry.size == 0 {
                continue;
            }
            let mut reader = Some(saved::FormatReader::default());
            self.store
                .read_parts(key, &mut |bytes| {
                    if let Some(value) = &mut reader
                        && value.feed(bytes).is_err()
                    {
                        reader = None;
                    }
                    true
                })
                .await?;
            let format = reader.and_then(|reader| reader.finish().ok());
            if format.is_some_and(|format| format != saved::CURRENT_FORMAT) {
                return Err(Error::Layout);
            }
            // Historical Sequence.next_device can look exactly like a current format header.
            if format == Some(saved::CURRENT_FORMAT) && key[0] != 7 {
                continue;
            }
            let bytes = self.store.load_owned(key).await?.ok_or(Error::Corrupt)?;
            if wire::validate(schema(key)?, &bytes, true).is_ok() {
                return Err(Error::Layout);
            }
            // A damaged tail must not hide an unsupported version already declared in the file.
            let mut remaining = bytes.as_slice();
            while !remaining.is_empty() {
                let Ok(field) = wire::field(&mut remaining) else {
                    break;
                };
                if field.tag == 1 && field.wire == prost::encoding::WireType::Varint {
                    let mut contents = field.contents;
                    let version = prost::encoding::decode_varint(&mut contents)
                        .map_err(|_| Error::Corrupt)?;
                    if version != 0 && version != u64::from(saved::CURRENT_FORMAT) {
                        return Err(Error::Layout);
                    }
                }
            }
        }
        Ok(())
    }
    async fn lookup(&mut self, path: &str) -> Result<Option<FileEntry>, Error> {
        let (parent, name) = path.rsplit_once('/').ok_or(Error::Bounds)?;
        let parent = if parent.is_empty() { "/" } else { parent };
        let mut index = 0usize;
        loop {
            let entry = match self.store.file_entry(parent, index).await {
                Ok(entry) => entry,
                Err(Error::Missing) => return Ok(None),
                Err(error) => return Err(error),
            };
            let Some(entry) = entry else {
                return Ok(None);
            };
            if entry.name == name {
                return Ok(Some(entry));
            }
            index = index.checked_add(1).ok_or(Error::TooLarge)?;
        }
    }
}
impl<S: RecordStore> Dataset for Inventory<'_, S> {
    fn rewind(&mut self) {
        self.directories.clear();
        self.started = false;
    }
    async fn next_entry(&mut self) -> Result<Option<Entry>, Error> {
        if !self.started {
            self.directories
                .try_reserve(1)
                .map_err(|_| Error::Unavailable)?;
            self.directories.push(Directory {
                path: copy_string("/")?,
                index: 0,
            });
            self.started = true;
        }
        while let Some(directory) = self.directories.last_mut() {
            let Some(entry) = self
                .store
                .file_entry(&directory.path, directory.index)
                .await?
            else {
                self.directories.pop();
                continue;
            };
            directory.index = directory.index.checked_add(1).ok_or(Error::TooLarge)?;
            if entry.name.is_empty()
                || entry.name.contains('/')
                || matches!(entry.name.as_str(), "." | "..")
            {
                return Err(Error::Corrupt);
            }
            let parent = if directory.path == "/" {
                ""
            } else {
                &directory.path
            };
            let length = parent
                .len()
                .checked_add(entry.name.len())
                .and_then(|n| n.checked_add(1))
                .ok_or(Error::TooLarge)?;
            let mut path = String::new();
            path.try_reserve_exact(length)
                .map_err(|_| Error::Unavailable)?;
            path.push_str(parent);
            path.push('/');
            path.push_str(&entry.name);
            classify_path(&path, entry.kind)?;
            if entry.kind == FileType::Directory {
                self.directories
                    .try_reserve(1)
                    .map_err(|_| Error::Unavailable)?;
                self.directories.push(Directory {
                    path: copy_string(&path)?,
                    index: 0,
                });
            }
            return Ok(Some(Entry {
                path,
                kind: entry.kind,
                size: entry.size,
            }));
        }
        Ok(None)
    }
    async fn read(&mut self, path: &str) -> Result<Option<Vec<u8>>, Error> {
        classify_path(path, FileType::File)?;
        let Some(entry) = self.lookup(path).await? else {
            return Ok(None);
        };
        if entry.kind != FileType::File {
            return Err(Error::Corrupt);
        }
        let mut bytes = Vec::new();
        if entry.size > u32::MAX as usize {
            return Err(Error::TooLarge);
        }
        bytes
            .try_reserve_exact(entry.size)
            .map_err(|_| Error::Unavailable)?;
        bytes.resize(entry.size, 0);
        let mut offset = 0;
        while offset < bytes.len() {
            let end = bytes.len().min(offset.saturating_add(512));
            let n = self
                .store
                .file_read(path, offset as u32, &mut bytes[offset..end])
                .await?;
            if n == 0 || n > end - offset {
                return Err(Error::Corrupt);
            }
            offset += n;
        }
        let mut tail = [0];
        if self.store.file_read(path, offset as u32, &mut tail).await? != 0 {
            return Err(Error::Corrupt);
        }
        Ok(Some(bytes))
    }
}
/// Validation totals. Obsolete files must be removed before committing the target root.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Summary {
    pub records: usize,
    pub legacy_records: usize,
    pub current_records: usize,
    pub obsolete_files: usize,
    pub unknown_files: usize,
    pub unknown_directories: usize,
}
/// Why a recognized file does not hold data needed by the running firmware.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Obsolete {
    Empty,
    Inactive,
    LayoutCache,
}
/// The installer's action for one inventoried entry. Inspection performs no writes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Disposition {
    Root,
    Directory,
    /// Unknown data is retained unchanged, including unknown directories and their contents.
    PreserveUnknown,
    /// The transfer owner's reserved scratch file is not part of the dataset.
    Scratch,
    /// Delete this file; no replacement data depends on its payload.
    Obsolete(Obsolete),
    /// Install and verify these bytes before committing the target root.
    Record(Normalized),
}
fn scratch(entry: &Entry) -> bool {
    entry.path == "/.file-upload" && entry.kind == FileType::File
}
async fn record<D: Dataset>(
    dataset: &mut D,
    root: u32,
    key: RecordKey,
) -> Result<Option<Vec<u8>>, Error> {
    let Some(bytes) = dataset.read(&record_path(key)?).await? else {
        return Ok(None);
    };
    if bytes.is_empty() {
        return Ok(None);
    }
    normalize_owned(root, key, bytes).map(|value| Some(value.bytes))
}
/// Classifies one entry using its commit record before reading an inactive payload.
/// Both installers use this decision after whole-dataset validation and before any mutation.
/// Empty essential records are absence here; `validate_dataset` requires them when initialized.
/// The dataset must remain unchanged between validation and planning the installation.
pub async fn inspect_entry<D: Dataset>(
    dataset: &mut D,
    root: u32,
    entry: &Entry,
) -> Result<Disposition, Error> {
    if !matches!(root, 1 | 2) {
        return Err(Error::Layout);
    }
    let key = match classify_path(&entry.path, entry.kind)? {
        Path::Root => return Ok(Disposition::Root),
        Path::Directory => return Ok(Disposition::Directory),
        Path::Unknown => {
            return Ok(if scratch(entry) {
                Disposition::Scratch
            } else {
                Disposition::PreserveUnknown
            });
        }
        Path::Record(key) => key,
    };
    let id = u64::from_be_bytes(key[1..].try_into().unwrap());
    let transport = if matches!(key[0], 4 | 5 | 9) {
        let parent = if key[0] == 9 { 8 } else { 2 };
        let Some(bytes) = record(dataset, root, record_key(parent, id)).await? else {
            return Ok(Disposition::Obsolete(Obsolete::Inactive));
        };
        if key[0] == 5 {
            Some(validate::device(&bytes, id)?)
        } else {
            None
        }
    } else {
        None
    };
    let bytes = dataset.read(&entry.path).await?.ok_or(Error::Corrupt)?;
    if bytes.len() != entry.size {
        return Err(Error::Corrupt);
    }
    if bytes.is_empty() {
        return Ok(Disposition::Obsolete(Obsolete::Empty));
    }
    let value = match normalize_owned(root, key, bytes) {
        Ok(value) => value,
        Err(Error::Corrupt) if key[0] == 5 => {
            return Ok(Disposition::Obsolete(Obsolete::LayoutCache));
        }
        Err(error) => return Err(error),
    };
    if let Some(transport) = transport {
        match validate::layout(&value.bytes, transport) {
            Ok(()) => {}
            Err(Error::Corrupt) => return Ok(Disposition::Obsolete(Obsolete::LayoutCache)),
            Err(error) => return Err(error),
        }
    }
    Ok(Disposition::Record(value))
}
/// Validates every committed record without writes or repairs.
///
/// Root 1 accepts frozen v1 records and completed v2 replacements. Root 2 requires v2.
/// Empty files, inactive children and unusable layout caches become explicit deletion candidates.
/// Dangling profile references and stale role summaries retain their exact committed payloads;
/// ordinary startup repairs those relationships. Unknown entries are preserved unchanged.
/// Missing roots require an empty inventory apart from the transfer owner's scratch file.
/// Hardware address matching remains the radio owner's responsibility after radio initialization.
pub async fn validate_dataset<D: Dataset>(dataset: &mut D) -> Result<Summary, Error> {
    let raw_root = dataset.read("/format.pb").await?;
    let (root, initialized) = match classify_root(raw_root.as_deref())? {
        Root::Legacy { initialized } => (1, initialized),
        Root::Current { initialized } => (2, initialized),
        Root::Missing => {
            dataset.rewind();
            while let Some(entry) = dataset.next_entry().await? {
                if !scratch(&entry) {
                    return Err(Error::Layout);
                }
            }
            return Ok(Summary::default());
        }
        Root::Unsupported { .. } => return Err(Error::Layout),
    };
    let root_size = raw_root.as_ref().unwrap().len();
    drop(raw_root);
    let identity_present = record(dataset, root, record_key(3, 0)).await?.is_some();
    let counters = record(dataset, root, record_key(7, 0))
        .await?
        .map(|bytes| sequence(&bytes))
        .transpose()?;
    if initialized && (!identity_present || counters.is_none())
        || !initialized && !identity_present && counters.is_some()
    {
        return Err(Error::Corrupt);
    }
    let mut summary = Summary::default();
    let mut root_seen = false;
    let (mut max_device, mut max_profile) = (0, 0);
    dataset.rewind();
    while let Some(entry) = dataset.next_entry().await? {
        let value = match inspect_entry(dataset, root, &entry).await? {
            Disposition::Root => {
                if root_seen || root_size != entry.size {
                    return Err(Error::Corrupt);
                }
                root_seen = true;
                continue;
            }
            Disposition::Directory | Disposition::Scratch => continue,
            Disposition::PreserveUnknown => {
                match entry.kind {
                    FileType::File => summary.unknown_files += 1,
                    FileType::Directory => summary.unknown_directories += 1,
                }
                continue;
            }
            Disposition::Obsolete(_) => {
                summary.obsolete_files += 1;
                continue;
            }
            Disposition::Record(value) => value,
        };
        summary.records += 1;
        match value.source {
            RecordFormat::Legacy => summary.legacy_records += 1,
            RecordFormat::Current => summary.current_records += 1,
        }
        let Path::Record(key) = classify_path(&entry.path, entry.kind)? else {
            unreachable!()
        };
        let id = u64::from_be_bytes(key[1..].try_into().unwrap());
        match key[0] {
            2 | 8 if !initialized => return Err(Error::Corrupt),
            2 => max_device = max_device.max(id),
            8 => max_profile = max_profile.max(id),
            _ => {}
        }
    }
    if !root_seen
        || counters
            .as_ref()
            .is_some_and(|s| s.next_device <= max_device || s.next_profile <= max_profile)
    {
        return Err(Error::Corrupt);
    }
    Ok(summary)
}

/// Independently validates the final target dataset before its root is committed.
/// A historical root is allowed, but all retained recognized records must be v2 and obsolete
/// files must be deleted. Unknown data is preserved and scratch is owned by the transfer layer.
/// The installer commits and verifies the root only after this read-only check succeeds.
pub async fn validate_target_dataset<D: Dataset>(dataset: &mut D) -> Result<Summary, Error> {
    if matches!(
        classify_root(dataset.read("/format.pb").await?.as_deref())?,
        Root::Missing
    ) {
        return Err(Error::Layout);
    }
    let summary = validate_dataset(dataset).await?;
    if summary.legacy_records != 0 || summary.obsolete_files != 0 {
        return Err(Error::Corrupt);
    }
    Ok(summary)
}
