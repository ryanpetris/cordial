//! Startup classification and restartable installation on a mounted store.
use super::*;

/// Storage gate applied before cleanup, identity, topology, or radio initialization.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Startup {
    Normal,
    Assisted,
    Blocked(Error),
}

/// Inspects all raw entries before permitting any storage mutation.
pub async fn classify<S: RecordStore>(store: &mut S) -> Result<Root, Error> {
    let mut dataset = Inventory::new(store);
    let root = classify_root(dataset.read("/format.pb").await?.as_deref())?;
    validate_dataset(&mut dataset).await?;
    Ok(root)
}

impl<S: RecordStore> Inventory<'_, S> {
    /// Validates an older source once and retains host assistance after an incomplete install.
    /// Unidentified, unsupported, and malformed sources never grant assisted writes.
    pub async fn migrate(&mut self) -> Startup {
        let root = match self
            .read("/format.pb")
            .await
            .and_then(|bytes| classify_root(bytes.as_deref()))
        {
            Ok(root @ Root::Legacy { .. }) => root,
            Ok(_) => return Startup::Blocked(Error::Layout),
            Err(error) => return Startup::Blocked(error),
        };
        match validate_dataset(self).await {
            Ok(_) => (),
            Err(Error::Unavailable) => return Startup::Assisted,
            Err(error) => return Startup::Blocked(error),
        }
        match install_validated(self.store, root).await {
            Ok(()) => Startup::Normal,
            Err(_) => Startup::Assisted,
        }
    }
}

/// Converts one file at a time, with readback, then commits the root last.
/// Replacements retain directory indices. Deletions restart the scan because indices shift.
pub async fn install<S: RecordStore>(store: &mut S) -> Result<(), Error> {
    let root = classify(store).await?;
    install_validated(store, root).await
}

async fn install_validated<S: RecordStore>(store: &mut S, root: Root) -> Result<(), Error> {
    let format = match root {
        Root::Legacy { .. } => 1,
        Root::Current { .. } => 2,
        _ => return Err(Error::Layout),
    };
    // Reserve the root replacement before the first write.
    let raw = Inventory::new(store)
        .read("/format.pb")
        .await?
        .ok_or(Error::Corrupt)?;
    let target = convert_root(&raw)?;
    drop(raw);
    let mut dataset = Inventory::new(store);
    while let Some(entry) = dataset.next_entry().await? {
        let disposition = inspect_entry(&mut dataset, format, &entry).await?;
        match disposition {
            Disposition::Record(record) if record.source == RecordFormat::Legacy => {
                let Path::Record(key) = classify_path(&entry.path, FileType::File)? else {
                    unreachable!()
                };
                dataset.store.save(key, &record.bytes).await?;
                if !storage::holds(dataset.store, key, Some(&record.bytes)).await? {
                    return Err(Error::Unknown);
                }
            }
            Disposition::Obsolete(_) | Disposition::Scratch => {
                dataset.store.file_delete(&entry.path).await?;
                dataset.rewind();
            }
            _ => (),
        }
    }
    validate_target_dataset(&mut Inventory::new(store)).await?;
    store.save(record_key(0, 0), &target).await?;
    if !storage::holds(store, record_key(0, 0), Some(&target)).await? {
        return Err(Error::Unknown);
    }
    Ok(())
}
