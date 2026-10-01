use alloc::vec::Vec;

/// Logical document selector. Backends map kind and owner to filesystem paths.
pub type RecordKey = [u8; 9];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    Unavailable,
    Unprovisioned,
    Layout,
    Bounds,
    TooLarge,
    Full,
    Io,
    Corrupt,
    /// A failed mutation could not be resolved by reading the record back.
    Unknown,
    /// The file or directory does not exist.
    Missing,
}

/// Whether a development file entry is a file or a directory.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileType {
    File,
    Directory,
}

/// One entry of a development directory listing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileEntry {
    pub name: alloc::string::String,
    pub kind: FileType,
    pub size: usize,
}

#[allow(async_fn_in_trait)]
pub trait RecordStore {
    async fn generation(&mut self) -> Result<u64, Error> {
        Err(Error::Unavailable)
    }
    async fn file_entry(&mut self, _path: &str, _index: usize) -> Result<Option<FileEntry>, Error> {
        Err(Error::Unavailable)
    }
    async fn file_read(
        &mut self,
        _path: &str,
        _offset: u32,
        _bytes: &mut [u8],
    ) -> Result<usize, Error> {
        Err(Error::Unavailable)
    }
    /// Enumerate unique live keys. Physical obsolete versions are not entries.
    async fn keys(&mut self) -> Result<Vec<RecordKey>, Error>;
    /// One key at a time. Backends override this without collecting the namespace.
    async fn next_key(&mut self, after: Option<RecordKey>) -> Result<Option<RecordKey>, Error> {
        Ok(self
            .keys()
            .await?
            .into_iter()
            .filter(|key| after.is_none_or(|a| *key > a))
            .min())
    }
    async fn load_owned(&mut self, key: RecordKey) -> Result<Option<Vec<u8>>, Error> {
        let mut bytes = Vec::new();
        let mut size = 512usize;
        loop {
            bytes
                .try_reserve_exact(size.saturating_sub(bytes.len()))
                .map_err(|_| Error::Unavailable)?;
            bytes.resize(size, 0);
            match self.load(key, &mut bytes).await {
                Ok(Some(n)) => {
                    bytes.truncate(n);
                    return Ok(Some(bytes));
                }
                Ok(None) => return Ok(None),
                Err(Error::TooLarge) => size = size.checked_mul(2).ok_or(Error::TooLarge)?,
                Err(e) => return Err(e),
            }
        }
    }
    /// Conservative remaining payload budget after backend reclamation reserves.
    async fn available(&mut self) -> Result<usize, Error>;
    async fn load(&mut self, key: RecordKey, value: &mut [u8]) -> Result<Option<usize>, Error>;
    async fn save(&mut self, key: RecordKey, value: &[u8]) -> Result<(), Error>;
    async fn remove(&mut self, key: RecordKey) -> Result<(), Error>;
}

/// Keep management available when opening storage failed. Every record access
/// reports that same startup error until the platform is restarted.
/// Resolve errors at the record commit boundary before updating application RAM.
pub async fn save_confirmed<S: RecordStore>(
    store: &mut S,
    key: RecordKey,
    value: &[u8],
) -> Result<(), Error> {
    store.save(key, value).await
}
pub async fn remove_confirmed<S: RecordStore>(store: &mut S, key: RecordKey) -> Result<(), Error> {
    store.remove(key).await
}
fn preference_error(error: Error) -> crate::settings::Error {
    match error {
        Error::Full => crate::settings::Error::StorageFull,
        Error::Unknown => crate::settings::Error::StorageUnknown,
        _ => crate::settings::Error::Storage,
    }
}

impl<S: RecordStore> RecordStore for Result<S, Error> {
    async fn generation(&mut self) -> Result<u64, Error> {
        self.as_mut().map_err(|e| *e)?.generation().await
    }
    async fn file_entry(&mut self, path: &str, index: usize) -> Result<Option<FileEntry>, Error> {
        self.as_mut().map_err(|e| *e)?.file_entry(path, index).await
    }
    async fn file_read(
        &mut self,
        path: &str,
        offset: u32,
        bytes: &mut [u8],
    ) -> Result<usize, Error> {
        self.as_mut()
            .map_err(|e| *e)?
            .file_read(path, offset, bytes)
            .await
    }
    async fn next_key(&mut self, after: Option<RecordKey>) -> Result<Option<RecordKey>, Error> {
        self.as_mut().map_err(|e| *e)?.next_key(after).await
    }
    async fn keys(&mut self) -> Result<Vec<RecordKey>, Error> {
        self.as_mut().map_err(|e| *e)?.keys().await
    }
    async fn available(&mut self) -> Result<usize, Error> {
        self.as_mut().map_err(|e| *e)?.available().await
    }

    async fn load(&mut self, key: RecordKey, value: &mut [u8]) -> Result<Option<usize>, Error> {
        self.as_mut().map_err(|e| *e)?.load(key, value).await
    }
    async fn save(&mut self, key: RecordKey, value: &[u8]) -> Result<(), Error> {
        self.as_mut().map_err(|e| *e)?.save(key, value).await
    }
    async fn remove(&mut self, key: RecordKey) -> Result<(), Error> {
        self.as_mut().map_err(|e| *e)?.remove(key).await
    }
}

pub fn record_key(kind: u8, owner: u64) -> RecordKey {
    let mut key = [0; 9];
    key[0] = kind;
    key[1..].copy_from_slice(&owner.to_be_bytes());
    key
}

pub struct Preferences<'a, S> {
    pub store: &'a mut S,
    pub device: u64,
}
impl<S: RecordStore> Preferences<'_, S> {
    fn key(&self) -> RecordKey {
        record_key(4, self.device)
    }
    pub async fn load_all(&mut self) -> Result<Vec<crate::compact::Preference>, Error> {
        let Some(bytes) = self.store.load_owned(self.key()).await? else {
            return Ok(Vec::new());
        };
        let result: Vec<crate::compact::Preference> =
            serde_json::from_slice(&bytes).map_err(|_| Error::Corrupt)?;
        for (i, p) in result.iter().enumerate() {
            if !p.valid()
                || result[..i]
                    .iter()
                    .any(|old| old.metadata.key == p.metadata.key)
            {
                return Err(Error::Corrupt);
            }
        }
        Ok(result)
    }
    async fn write(&mut self, values: &[crate::compact::Preference]) -> Result<(), Error> {
        if values.is_empty() {
            self.store.remove(self.key()).await
        } else {
            self.store.save(self.key(), &json(values)?).await
        }
    }
}
impl<S: RecordStore> crate::settings::PreferenceStore for Preferences<'_, S> {
    async fn save(
        &mut self,
        preference: &crate::compact::Preference,
    ) -> Result<(), crate::settings::Error> {
        if !preference.valid() {
            return Err(crate::settings::Error::InvalidValue);
        }
        let mut values = self.load_all().await.map_err(preference_error)?;
        if let Some(old) = values
            .iter_mut()
            .find(|p| p.metadata.key == preference.metadata.key)
        {
            *old = preference.clone();
        } else {
            if self.store.available().await.map_err(preference_error)?
                < crate::bonds::MAINTENANCE_BYTES
            {
                return Err(crate::settings::Error::StorageFull);
            }
            values
                .try_reserve(1)
                .map_err(|_| crate::settings::Error::Limit)?;
            values.push(preference.clone());
        }
        self.write(&values).await.map_err(preference_error)
    }
    async fn replace(
        &mut self,
        values: &[crate::compact::Preference],
    ) -> Result<(), crate::settings::Error> {
        if values.iter().any(|p| !p.valid()) {
            return Err(crate::settings::Error::InvalidValue);
        }
        let current = self.load_all().await.map_err(preference_error)?;
        let added = values
            .iter()
            .any(|p| !current.iter().any(|c| c.metadata.key == p.metadata.key));
        if added
            && self.store.available().await.map_err(preference_error)?
                < crate::bonds::MAINTENANCE_BYTES
        {
            return Err(crate::settings::Error::StorageFull);
        }
        self.write(values).await.map_err(preference_error)
    }
    async fn remove(
        &mut self,
        key: crate::model::settings::SettingKey,
    ) -> Result<(), crate::settings::Error> {
        let mut values = self.load_all().await.map_err(preference_error)?;
        values.retain(|p| p.metadata.key != key);
        self.write(&values).await.map_err(preference_error)
    }
    async fn remove_all(&mut self) -> Result<(), crate::settings::Error> {
        self.store
            .remove(self.key())
            .await
            .map_err(preference_error)
    }
}
pub fn json<T: serde::Serialize + ?Sized>(value: &T) -> Result<Vec<u8>, Error> {
    let mut bytes = Vec::new();
    let mut size = 512;
    loop {
        bytes
            .try_reserve_exact(size - bytes.len())
            .map_err(|_| Error::Unavailable)?;
        bytes.resize(size, 0);
        match serde_json_core::to_slice(value, &mut bytes) {
            Ok(n) => {
                bytes.truncate(n);
                return Ok(bytes);
            }
            Err(serde_json_core::ser::Error::BufferFull) => {
                size = size.checked_mul(2).ok_or(Error::TooLarge)?
            }
            Err(_) => return Err(Error::Corrupt),
        }
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Format {
    pub format: u8,
    pub initialized: bool,
}
pub async fn format<S: RecordStore>(store: &mut S) -> Result<Format, Error> {
    let bytes = store
        .load_owned(record_key(0, 0))
        .await?
        .ok_or(Error::Layout)?;
    let value: Format = serde_json::from_slice(&bytes).map_err(|_| Error::Layout)?;
    if value.format != 1 {
        return Err(Error::Layout);
    }
    Ok(value)
}
pub async fn initialized<S: RecordStore>(store: &mut S) -> Result<(), Error> {
    let sequence = record_key(7, 0);
    let initialized = format(store).await?.initialized;
    if store.load_owned(sequence).await?.is_none() {
        if initialized {
            return Err(Error::Corrupt);
        }
        store.save(sequence, b"0").await?;
    }
    if !initialized {
        store
            .save(
                record_key(0, 0),
                &json(&Format {
                    format: 1,
                    initialized: true,
                })?,
            )
            .await?;
    }
    Ok(())
}
/// A missing header is provisioned only in a verified empty filesystem.
pub async fn open<S: RecordStore>(store: &mut S) -> Result<(), Error> {
    if store.load_owned(record_key(0, 0)).await?.is_some() {
        format(store).await.map(|_| ())
    } else if store.next_key(None).await?.is_none() {
        store
            .save(
                record_key(0, 0),
                &json(&Format {
                    format: 1,
                    initialized: false,
                })?,
            )
            .await
    } else {
        Err(Error::Layout)
    }
}
