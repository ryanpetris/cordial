use crate::model::settings::{SettingKey, SettingScope};
use alloc::vec::Vec;
use cordial_protocol::storage as saved;

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
    /// Saved layouts (kind 5) and profile rules (kind 9) are read by key and need not be
    /// enumerated.
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
    /// The lowest IDs above `after` that have a record of `kind`, at most `limit`, in ascending
    /// order. Filesystem backends select them in one directory pass and list the IDs of record
    /// directories without opening them, so a listed ID's record can be missing; callers read
    /// each record and treat a missing one as absent.
    async fn record_ids(&mut self, kind: u8, after: u64, limit: usize) -> Result<Vec<u64>, Error> {
        let mut ids = Vec::new();
        let mut previous = Some(record_key(kind, after));
        while ids.len() < limit {
            let Some(key) = self.next_key(previous).await? else {
                break;
            };
            if key[0] != kind {
                break;
            }
            previous = Some(key);
            ids.push(u64::from_be_bytes(key[1..].try_into().unwrap()));
        }
        Ok(ids)
    }
    /// Record `key` whole. Backends that know the record's length override this to allocate
    /// once; this default grows a buffer until `load` fits.
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
    /// Reads record `key` from its start, handing it to `f` a part at a time until `f` returns
    /// false or the record ends. Returns whether the record exists. Backends override this to
    /// hold one part at a time; this default reads the record whole and hands it over at once.
    async fn read_parts(
        &mut self,
        key: RecordKey,
        f: &mut dyn FnMut(&[u8]) -> bool,
    ) -> Result<bool, Error> {
        let Some(bytes) = self.load_owned(key).await? else {
            return Ok(false);
        };
        f(&bytes);
        Ok(true)
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
/// Whether record `key` holds exactly `bytes`, or does not exist when `bytes` is `None`. Reads the
/// record a part at a time.
pub async fn holds<S: RecordStore>(
    store: &mut S,
    key: RecordKey,
    bytes: Option<&[u8]>,
) -> Result<bool, Error> {
    let expected = bytes.unwrap_or_default();
    let mut at = 0;
    let mut same = true;
    let found = store
        .read_parts(key, &mut |part| {
            same = expected.get(at..at + part.len()) == Some(part);
            at += part.len();
            same
        })
        .await?;
    Ok(match bytes {
        Some(_) => found && same && at == expected.len(),
        None => !found,
    })
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
    async fn record_ids(&mut self, kind: u8, after: u64, limit: usize) -> Result<Vec<u64>, Error> {
        self.as_mut()
            .map_err(|e| *e)?
            .record_ids(kind, after, limit)
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

    async fn load_owned(&mut self, key: RecordKey) -> Result<Option<Vec<u8>>, Error> {
        self.as_mut().map_err(|e| *e)?.load_owned(key).await
    }
    async fn read_parts(
        &mut self,
        key: RecordKey,
        f: &mut dyn FnMut(&[u8]) -> bool,
    ) -> Result<bool, Error> {
        self.as_mut().map_err(|e| *e)?.read_parts(key, f).await
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

/// The ID a record directory is named after: a positive decimal number of at most 32 bits,
/// without a prefix or padding.
pub fn parse_id(value: &str) -> Option<u64> {
    if value.is_empty() || value.starts_with('0') || !value.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    value.parse().ok().filter(|id| *id <= u64::from(u32::MAX))
}

pub fn record_key(kind: u8, owner: u64) -> RecordKey {
    let mut key = [0; 9];
    key[0] = kind;
    key[1..].copy_from_slice(&owner.to_be_bytes());
    key
}

/// The bytes of a saved record: `message` encoded into a buffer of exactly its length.
pub fn encode<M: prost::Message>(message: &M) -> Result<Vec<u8>, Error> {
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(message.encoded_len())
        .map_err(|_| Error::Unavailable)?;
    message.encode(&mut bytes).map_err(|_| Error::TooLarge)?;
    Ok(bytes)
}
/// The saved record in `bytes`. `Corrupt` when they do not decode as `M`.
pub fn decode<M: prost::Message + Default>(bytes: &[u8]) -> Result<M, Error> {
    M::decode(bytes).map_err(|_| Error::Corrupt)
}
/// A fixed-length byte field of a saved record. `Corrupt` when it has another length.
pub fn array<const N: usize>(bytes: &[u8]) -> Result<[u8; N], Error> {
    bytes.try_into().map_err(|_| Error::Corrupt)
}
/// A numeric field of a saved record that the firmware holds in a narrower type. `Corrupt` when
/// it does not fit.
pub fn narrow<T: TryFrom<u32>>(value: u32) -> Result<T, Error> {
    T::try_from(value).map_err(|_| Error::Corrupt)
}

const SEQUENCE: RecordKey = [7, 0, 0, 0, 0, 0, 0, 0, 0];
/// The sequence a new filesystem starts with.
const FIRST: saved::Sequence = saved::Sequence {
    next_device: 1,
    next_profile: 1,
};

/// Allocates the next device ID, or the next profile ID when `profile`, and saves the sequence
/// before returning it.
pub async fn allocate<S: RecordStore>(store: &mut S, profile: bool) -> Result<u64, Error> {
    let bytes = store.load_owned(SEQUENCE).await?.ok_or(Error::Corrupt)?;
    let mut sequence: saved::Sequence = decode(&bytes)?;
    if sequence.next_device == 0 || sequence.next_profile == 0 {
        return Err(Error::Corrupt);
    }
    let next = if profile {
        &mut sequence.next_profile
    } else {
        &mut sequence.next_device
    };
    let id = *next;
    // Identifiers are 32-bit on the wire.
    if id > u64::from(u32::MAX) {
        return Err(Error::Full);
    }
    *next = id + 1;
    store.save(SEQUENCE, &encode(&sequence)?).await?;
    Ok(id)
}

fn preference(value: &crate::compact::Preference) -> saved::Preference {
    let metadata = &value.metadata;
    saved::Preference {
        integration: saved::Integration::Hidpp.into(),
        metadata: Some(saved::SettingMetadata {
            key: metadata.key.name().into(),
            feature: metadata.feature.0.into(),
            revision: metadata.revision.0.into(),
            scope: match metadata.scope {
                SettingScope::Device => saved::SettingScope::Device,
                SettingScope::CurrentHost => saved::SettingScope::CurrentHost,
            }
            .into(),
            choices: metadata.choices.iter().copied().map(u32::from).collect(),
            range: metadata.range.map(|range| saved::Range {
                min: range.min.into(),
                max: range.max.into(),
                step: range.step.into(),
            }),
        }),
        value: value.value.into(),
    }
}
/// A saved preference of an integration the firmware has; `None` for another integration.
fn saved_preference(value: saved::Preference) -> Result<Option<crate::compact::Preference>, Error> {
    if value.integration != i32::from(saved::Integration::Hidpp) {
        return Ok(None);
    }
    let metadata = value.metadata.ok_or(Error::Corrupt)?;
    let range = metadata
        .range
        .map(|range| {
            Ok::<_, Error>(crate::compact::Range {
                min: narrow(range.min)?,
                max: narrow(range.max)?,
                step: narrow(range.step)?,
            })
        })
        .transpose()?;
    Ok(Some(crate::compact::Preference {
        metadata: crate::compact::Metadata {
            key: SettingKey::from_name(&metadata.key).ok_or(Error::Corrupt)?,
            feature: crate::model::hidpp::FeatureId(narrow(metadata.feature)?),
            revision: crate::model::hidpp::FeatureRevision(narrow(metadata.revision)?),
            scope: match saved::SettingScope::try_from(metadata.scope) {
                Ok(saved::SettingScope::Device) => SettingScope::Device,
                Ok(saved::SettingScope::CurrentHost) => SettingScope::CurrentHost,
                _ => return Err(Error::Corrupt),
            },
            choices: metadata
                .choices
                .into_iter()
                .map(narrow)
                .collect::<Result<_, _>>()?,
            range,
        },
        value: narrow(value.value)?,
    }))
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
        let saved: saved::Settings = decode(&bytes)?;
        let mut result = Vec::new();
        result
            .try_reserve_exact(saved.preferences.len())
            .map_err(|_| Error::Unavailable)?;
        for preference in saved.preferences {
            if let Some(preference) = saved_preference(preference)? {
                result.push(preference);
            }
        }
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
            let saved = saved::Settings {
                preferences: values.iter().map(preference).collect(),
            };
            self.store.save(self.key(), &encode(&saved)?).await
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
const FORMAT: RecordKey = [0; 9];
/// The value of `Format.format`.
const FORMAT_VALUE: u32 = 1;
/// The filesystem's root record. `Layout` when it is missing or is not this firmware's.
pub async fn format<S: RecordStore>(store: &mut S) -> Result<saved::Format, Error> {
    let bytes = store.load_owned(FORMAT).await?.ok_or(Error::Layout)?;
    let value: saved::Format = decode(&bytes).map_err(|_| Error::Layout)?;
    if value.format != FORMAT_VALUE {
        return Err(Error::Layout);
    }
    Ok(value)
}
fn format_record(initialized: bool) -> Result<Vec<u8>, Error> {
    encode(&saved::Format {
        format: FORMAT_VALUE,
        initialized,
    })
}
pub async fn initialized<S: RecordStore>(store: &mut S) -> Result<(), Error> {
    let initialized = format(store).await?.initialized;
    if store.load_owned(SEQUENCE).await?.is_none() {
        if initialized {
            return Err(Error::Corrupt);
        }
        store.save(SEQUENCE, &encode(&FIRST)?).await?;
    }
    if !initialized {
        store.save(FORMAT, &format_record(true)?).await?;
    }
    Ok(())
}
/// A missing header is provisioned only in a verified empty filesystem.
pub async fn open<S: RecordStore>(store: &mut S) -> Result<(), Error> {
    if store.load_owned(FORMAT).await?.is_some() {
        format(store).await.map(|_| ())
    } else if store.next_key(None).await?.is_none() {
        store.save(FORMAT, &format_record(false)?).await
    } else {
        Err(Error::Layout)
    }
}
