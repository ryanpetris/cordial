//! Read-only inspection and the restartable storage-format 1 to 2 conversion.
//!
//! Conversion changes only outer protobuf keys. Nested and unknown payload bytes are retained.
//! The historical outer grammar is frozen: unknown v1 outer fields are rejected because their
//! meaning after a partially completed conversion cannot be established from the bytes alone.
//! Callers validate the whole dataset before replacing any file and again before committing the
//! root. This module never writes storage or changes the root during record conversion.
use crate::{bonds, devices, identity::Identity, profiles, storage};
use alloc::{string::String, vec::Vec};
use cordial_protocol::storage as saved;
use core::fmt::Write;
use prost::encoding::encode_key;
use storage::{Error, FileType, RecordKey, RecordStore, record_key};
mod dataset;
mod install;
pub mod management;
pub use install::{Startup, classify, install};
mod validate;
mod wire;
pub use dataset::{
    Dataset, Disposition, Entry, Inventory, Obsolete, Summary, inspect_entry, validate_dataset,
    validate_target_dataset,
};
use validate::sequence;

/// The reliably identified format of the root file. Missing is distinct from an empty file.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Root {
    Missing,
    Legacy { initialized: bool },
    Current { initialized: bool },
    Unsupported { format: u32, initialized: bool },
}
/// Classifies the raw root without applying recovery or interpreting other records.
pub fn classify_root(bytes: Option<&[u8]>) -> Result<Root, Error> {
    let Some(bytes) = bytes else {
        return Ok(Root::Missing);
    };
    wire::validate(wire::Schema::Root, bytes, false)?;
    let format = saved::extract_format(bytes).map_err(|_| Error::Corrupt)?;
    use prost::Message;
    let root = saved::Format::decode(bytes).map_err(|_| Error::Corrupt)?;
    Ok(match format {
        1 => Root::Legacy {
            initialized: root.initialized,
        },
        2 => Root::Current {
            initialized: root.initialized,
        },
        _ => Root::Unsupported {
            format,
            initialized: root.initialized,
        },
    })
}

/// Reads the committed root version without requiring the firmware's target format.
pub async fn committed_format<S: RecordStore>(store: &mut S) -> Result<u32, Error> {
    let bytes = store.load_owned(record_key(0, 0)).await?;
    match classify_root(bytes.as_deref())? {
        Root::Legacy { .. } => Ok(1),
        Root::Current { .. } => Ok(2),
        Root::Unsupported { format, .. } => Ok(format),
        Root::Missing => Err(Error::Layout),
    }
}

/// Reports a storage gate failure without treating an unknown root as an upgrade request.
pub(crate) fn startup_error() -> cordial_protocol::Error {
    crate::wire::error(crate::model::errors::ErrorCode::StorageFailed, None, false)
}

/// Prepares the target root while preserving initialization and every other field's bytes.
/// The installer writes this separately, last, after `validate_target_dataset` succeeds.
/// This function only prepares bytes and never commits a checkpoint.
pub fn convert_root(bytes: &[u8]) -> Result<Vec<u8>, Error> {
    match classify_root(Some(bytes))? {
        Root::Current { .. } => return copy(bytes),
        Root::Legacy { .. } => {}
        _ => return Err(Error::Layout),
    }
    let mut result = Vec::new();
    result
        .try_reserve_exact(bytes.len())
        .map_err(|_| Error::Unavailable)?;
    let mut remaining = bytes;
    while !remaining.is_empty() {
        let start = remaining;
        let field = wire::field(&mut remaining)?;
        if field.tag == 1 {
            result.extend_from_slice(&[8, 2]);
        } else {
            result.extend_from_slice(&start[..start.len() - remaining.len()]);
        }
    }
    Ok(result)
}

/// A canonical inventory path. Unknown paths are inventoried, never silently discarded.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Path {
    Root,
    Record(RecordKey),
    Directory,
    Unknown,
}
/// Checks path syntax and recognizes the current and historical record locations.
/// Invalid IDs in the device/profile namespaces are corruption, not extension paths.
pub fn classify_path(path: &str, kind: FileType) -> Result<Path, Error> {
    if !path.starts_with('/')
        || path == "/"
        || path.ends_with('/')
        || path.split('/').skip(1).any(|s| {
            s.is_empty()
                || s == "."
                || s == ".."
                || s.bytes().any(|b| b < 0x20 || b == 0x7f || b == b'\\')
        })
    {
        return Err(Error::Corrupt);
    }
    let root = match path {
        "/format.pb" => Some(Path::Root),
        "/adapter.pb" => Some(Path::Record(record_key(1, 0))),
        "/identity.pb" => Some(Path::Record(record_key(3, 0))),
        "/sequence.pb" => Some(Path::Record(record_key(7, 0))),
        _ => None,
    };
    if let Some(root) = root {
        return if kind == FileType::File {
            Ok(root)
        } else {
            Err(Error::Corrupt)
        };
    }
    let mut parts = path[1..].split('/');
    let namespace = parts.next().unwrap();
    if !matches!(namespace, "devices" | "profiles") {
        return Ok(Path::Unknown);
    }
    let Some(id) = parts.next() else {
        return if kind == FileType::Directory {
            Ok(Path::Directory)
        } else {
            Err(Error::Corrupt)
        };
    };
    let id = storage::parse_id(id).ok_or(Error::Corrupt)?;
    let Some(name) = parts.next() else {
        return if kind == FileType::Directory {
            Ok(Path::Directory)
        } else {
            Err(Error::Corrupt)
        };
    };
    let record = match (namespace, name) {
        ("devices", "device.pb") => 2,
        ("devices", "settings.pb") => 4,
        ("devices", "layout.pb") => 5,
        ("profiles", "profile.pb") => 8,
        ("profiles", "rules.pb") => 9,
        _ => return Ok(Path::Unknown),
    };
    if parts.next().is_some() || kind != FileType::File {
        return Err(Error::Corrupt);
    }
    Ok(Path::Record(record_key(record, id)))
}
/// The canonical path of a recognized record key, including the root key.
pub fn record_path(key: RecordKey) -> Result<String, Error> {
    let id = u64::from_be_bytes(key[1..].try_into().unwrap());
    let (directory, file) = match (key[0], id) {
        (0, 0) => ("", "format.pb"),
        (1, 0) => ("", "adapter.pb"),
        (3, 0) => ("", "identity.pb"),
        (7, 0) => ("", "sequence.pb"),
        (2 | 4 | 5, 1..=0xffff_ffff) => (
            "devices",
            match key[0] {
                2 => "device.pb",
                4 => "settings.pb",
                _ => "layout.pb",
            },
        ),
        (8 | 9, 1..=0xffff_ffff) => (
            "profiles",
            if key[0] == 8 {
                "profile.pb"
            } else {
                "rules.pb"
            },
        ),
        _ => return Err(Error::Bounds),
    };
    let mut path = String::new();
    // A canonical path contains at most ten ID digits and two separators before the filename.
    path.try_reserve_exact(directory.len() + file.len() + 13)
        .map_err(|_| Error::Unavailable)?;
    if directory.is_empty() {
        path.push('/');
        path.push_str(file);
    } else {
        write!(path, "/{directory}/{id}/{file}").map_err(|_| Error::Unavailable)?;
    }
    Ok(path)
}
fn copy_string(value: &str) -> Result<String, Error> {
    let mut result = String::new();
    result
        .try_reserve_exact(value.len())
        .map_err(|_| Error::Unavailable)?;
    result.push_str(value);
    Ok(result)
}

fn schema(key: RecordKey) -> Result<wire::Schema, Error> {
    let id = u64::from_be_bytes(key[1..].try_into().unwrap());
    if !matches!(
        (key[0], id),
        (1 | 3 | 7, 0) | (2 | 4 | 5 | 8 | 9, 1..=0xffff_ffff)
    ) {
        return Err(Error::Bounds);
    }
    use wire::Schema::*;
    Ok(match key[0] {
        1 => Adapter,
        2 => Device,
        3 => Identity,
        4 => Settings,
        5 => Layout,
        7 => Sequence,
        8 => Profile,
        9 => Rules,
        _ => return Err(Error::Bounds),
    })
}
/// Whether a record is historical or already replaced under a historical root.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecordFormat {
    Legacy,
    Current,
}
/// A validated record's exact target bytes. The root is handled separately and committed last.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Normalized {
    pub source: RecordFormat,
    pub bytes: Vec<u8>,
}
fn copy(bytes: &[u8]) -> Result<Vec<u8>, Error> {
    let mut result = Vec::new();
    result
        .try_reserve_exact(bytes.len())
        .map_err(|_| Error::Unavailable)?;
    result.extend_from_slice(bytes);
    Ok(result)
}
fn convert(key: RecordKey, mut bytes: &[u8]) -> Result<Vec<u8>, Error> {
    wire::validate(schema(key)?, bytes, true)?;
    let mut result = Vec::new();
    // Every historical outer key fits in one byte, including its shifted key.
    result
        .try_reserve_exact(bytes.len().checked_add(2).ok_or(Error::TooLarge)?)
        .map_err(|_| Error::Unavailable)?;
    result.extend_from_slice(&[8, 2]);
    while !bytes.is_empty() {
        let f = wire::field(&mut bytes)?;
        encode_key(f.tag + 1, f.wire, &mut result);
        result.extend_from_slice(f.value);
    }
    validate_current_record(key, &result)?;
    Ok(result)
}
/// Normalizes one recognized non-root record under root 1 or root 2.
/// A record must satisfy exactly one grammar under root 1. Header detection alone is unsafe:
/// v1 Sequence's `next_device = 2` occupies the same bytes as a v2 format field.
pub fn normalize_record(
    root_format: u32,
    key: RecordKey,
    bytes: &[u8],
) -> Result<Normalized, Error> {
    let (source, replacement) = replacement(root_format, key, bytes)?;
    Ok(Normalized {
        source,
        bytes: match replacement {
            Some(bytes) => bytes,
            None => copy(bytes)?,
        },
    })
}
fn replacement(
    root_format: u32,
    key: RecordKey,
    bytes: &[u8],
) -> Result<(RecordFormat, Option<Vec<u8>>), Error> {
    if bytes.is_empty() {
        return Err(Error::Corrupt);
    }
    schema(key)?;
    match root_format {
        2 => {
            validate_current_record(key, bytes)?;
            Ok((RecordFormat::Current, None))
        }
        1 => {
            let current = match validate_current_record(key, bytes) {
                Ok(()) => true,
                Err(Error::Corrupt) => false,
                Err(error) => return Err(error),
            };
            let legacy = match convert(key, bytes) {
                Ok(value) => Some(value),
                Err(Error::Corrupt) => None,
                Err(error) => return Err(error),
            };
            match (current, legacy) {
                (true, None) => Ok((RecordFormat::Current, None)),
                (false, Some(bytes)) => Ok((RecordFormat::Legacy, Some(bytes))),
                _ => Err(Error::Corrupt),
            }
        }
        _ => Err(Error::Layout),
    }
}
// An owned current record needs no second file buffer during final firmware validation.
fn normalize_owned(root_format: u32, key: RecordKey, bytes: Vec<u8>) -> Result<Normalized, Error> {
    let (source, replacement) = replacement(root_format, key, &bytes)?;
    Ok(Normalized {
        source,
        bytes: replacement.unwrap_or(bytes),
    })
}
/// Normalizes a raw file selected by its canonical path. Unknown files and the root require
/// a separate installer decision and are never converted by this function.
pub fn normalize_path(root_format: u32, path: &str, bytes: &[u8]) -> Result<Normalized, Error> {
    let Path::Record(key) = classify_path(path, FileType::File)? else {
        return Err(Error::Bounds);
    };
    normalize_record(root_format, key, bytes)
}
/// Strict framing and local semantic validation for a target-format record.
/// Validation borrows payloads and uses bounded stack state; it makes no heap allocations.
/// Unknown compatibility entries are scanned without building generated repeated-field vectors.
pub fn validate_current_record(key: RecordKey, bytes: &[u8]) -> Result<(), Error> {
    wire::validate(schema(key)?, bytes, false)?;
    saved::validate_format(bytes).map_err(|_| Error::Corrupt)?;
    validate::record(key, bytes)
}
