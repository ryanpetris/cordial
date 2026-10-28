use super::{Error, Result, ready, storage_error};
use cordial_core::{
    migration as core,
    storage::{self, FileType},
};
use cordial_protocol as p;
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    sync::Arc,
};

pub(crate) const ROOT: &str = "/format.pb";
pub(crate) const SCRATCH: &str = "/.file-upload";
// Includes the serial frame buffer, decoded inventory, map nodes, and bounded transfer buffers.
const FIXED_MEMORY: usize = 96 * 1024 * 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Contents {
    File(Arc<Vec<u8>>),
    Directory,
}

/// A complete immutable RAM dataset, including unknown and empty files and directories.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Snapshot {
    pub(crate) entries: BTreeMap<String, Contents>,
}
impl Snapshot {
    pub fn file(&self, path: &str) -> Option<&[u8]> {
        match self.entries.get(path) {
            Some(Contents::File(bytes)) => Some(bytes),
            _ => None,
        }
    }
    pub fn files(&self) -> impl Iterator<Item = (&str, &[u8])> {
        self.entries.iter().filter_map(|(path, value)| match value {
            Contents::File(bytes) => Some((path.as_str(), bytes.as_slice())),
            Contents::Directory => None,
        })
    }
    /// Adds a file without copying its payload. Paths and memory limits are checked first.
    pub fn insert(&mut self, path: String, bytes: Vec<u8>) -> Result<()> {
        valid_path(&path)?;
        self.check_add(&path, bytes.capacity())?;
        self.entries.insert(path, Contents::File(Arc::new(bytes)));
        Ok(())
    }
    pub fn directory(&mut self, path: String) -> Result<()> {
        valid_path(&path)?;
        self.check_add(&path, 0)?;
        if matches!(self.entries.get(&path), Some(Contents::File(_))) {
            return Err(Error::new("file and directory paths conflict"));
        }
        self.entries.insert(path, Contents::Directory);
        Ok(())
    }
    fn check_add(&self, path: &str, bytes: usize) -> Result<()> {
        if self.entries.len() >= 2 * p::STORAGE_MIGRATION_MAX_FILES
            && !self.entries.contains_key(path)
        {
            return Err(Error::new("too many dataset entries"));
        }
        memory(self.memory()?.checked_add(bytes).ok_or_else(limit)?, 0)
    }
    pub(crate) fn memory(&self) -> Result<usize> {
        self.entries
            .iter()
            .try_fold(0usize, |sum, (path, value)| {
                let bytes = match value {
                    Contents::File(bytes) => bytes.capacity(),
                    _ => 0,
                };
                sum.checked_add(bytes)?
                    .checked_add(path.capacity())?
                    .checked_add(160)
            })
            .ok_or_else(limit)
    }
    fn max_file(&self) -> usize {
        self.files()
            .map(|(_, bytes)| bytes.len())
            .max()
            .unwrap_or(0)
    }
    fn max_record(&self) -> usize {
        self.files()
            .filter(|(path, _)| {
                core::classify_path(path, FileType::File)
                    .is_ok_and(|kind| matches!(kind, core::Path::Root | core::Path::Record(_)))
            })
            .map(|(_, bytes)| bytes.len())
            .max()
            .unwrap_or(0)
    }
    fn input_limits(&self) -> Result<()> {
        let mut count = 0usize;
        let mut total = 0usize;
        for (_, bytes) in self.files() {
            count = count.checked_add(1).ok_or_else(limit)?;
            total = total.checked_add(bytes.len()).ok_or_else(limit)?;
            if count > p::STORAGE_MIGRATION_MAX_FILES
                || total > p::STORAGE_MIGRATION_MAX_INPUT_BYTES
            {
                return Err(Error::new(
                    "migration dataset exceeds the file or byte limit",
                ));
            }
        }
        Ok(())
    }
    fn dataset(&self) -> RamDataset<'_> {
        RamDataset {
            snapshot: self,
            entries: self.entries.iter(),
        }
    }
    fn apply(&mut self, change: &Change) -> Result<()> {
        valid_path(change.path())?;
        if matches!(change.path(), ROOT | SCRATCH) {
            return Err(Error::new("an edge changed a reserved path"));
        }
        match change {
            Change::Replace { path, bytes } => {
                if matches!(self.entries.get(path), Some(Contents::Directory)) {
                    return Err(Error::new("a replacement conflicts with a directory"));
                }
                let mut parent = path.as_str();
                while let Some((prefix, _)) = parent.rsplit_once('/') {
                    if prefix.is_empty() {
                        break;
                    }
                    if matches!(self.entries.get(prefix), Some(Contents::File(_))) {
                        return Err(Error::new("a replacement has a file as its parent"));
                    }
                    self.entries
                        .entry(prefix.into())
                        .or_insert(Contents::Directory);
                    parent = prefix;
                }
                self.entries
                    .insert(path.clone(), Contents::File(bytes.clone()));
            }
            Change::Delete { path } => {
                if matches!(self.entries.get(path), Some(Contents::Directory)) {
                    return Err(Error::new("an edge attempted to delete a directory"));
                }
                self.entries.remove(path);
            }
        }
        if self.entries.len() > 2 * p::STORAGE_MIGRATION_MAX_FILES {
            return Err(Error::new("too many dataset entries"));
        }
        Ok(())
    }
    fn commit(&mut self, root: &Arc<Vec<u8>>) {
        self.entries
            .insert(ROOT.into(), Contents::File(root.clone()));
    }
}

pub(crate) fn limit() -> Error {
    Error::new("migration exceeds the RAM safety limit")
}
pub(crate) fn memory(resident: usize, largest: usize) -> Result<()> {
    // Core inspection can own the read copy, a conversion, and a candidate historical grammar.
    let total = largest
        .checked_mul(3)
        .and_then(|work| resident.checked_add(work))
        .and_then(|n| n.checked_add(FIXED_MEMORY))
        .ok_or_else(limit)?;
    if total > p::STORAGE_MIGRATION_MAX_MEMORY_BYTES {
        return Err(limit());
    }
    Ok(())
}
pub(crate) fn valid_path(path: &str) -> Result<()> {
    if path.len() > p::STORAGE_MIGRATION_PATH_BYTES
        || !path.starts_with('/')
        || path == "/"
        || path.ends_with('/')
        || path.split('/').skip(1).any(|part| {
            part.is_empty()
                || part == "."
                || part == ".."
                || part.bytes().any(|b| b < 32 || b == 127 || b == b'\\')
        })
    {
        return Err(Error::new("invalid dataset path"));
    }
    Ok(())
}

struct RamDataset<'a> {
    snapshot: &'a Snapshot,
    entries: std::collections::btree_map::Iter<'a, String, Contents>,
}
impl core::Dataset for RamDataset<'_> {
    fn rewind(&mut self) {
        self.entries = self.snapshot.entries.iter();
    }
    async fn next_entry(&mut self) -> std::result::Result<Option<core::Entry>, storage::Error> {
        Ok(self.entries.next().map(|(path, value)| core::Entry {
            path: path.clone(),
            kind: match value {
                Contents::File(_) => FileType::File,
                _ => FileType::Directory,
            },
            size: match value {
                Contents::File(bytes) => bytes.len(),
                _ => 0,
            },
        }))
    }
    async fn read(&mut self, path: &str) -> std::result::Result<Option<Vec<u8>>, storage::Error> {
        self.snapshot
            .file(path)
            .map(|bytes| {
                let mut copy = Vec::new();
                copy.try_reserve_exact(bytes.len())
                    .map_err(|_| storage::Error::Unavailable)?;
                copy.extend_from_slice(bytes);
                Ok(copy)
            })
            .transpose()
    }
}

/// The minimum checkpoint required by an edge. Deferral also requires composition recovery.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Checkpoint {
    Immediate,
    MayDefer,
}

/// One durable file operation. Ordering carries split/merge dependencies explicitly.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Change {
    Replace { path: String, bytes: Arc<Vec<u8>> },
    Delete { path: String },
}
impl Change {
    pub fn replace(path: impl Into<String>, bytes: Vec<u8>) -> Self {
        Self::Replace {
            path: path.into(),
            bytes: Arc::new(bytes),
        }
    }
    pub fn delete(path: impl Into<String>) -> Self {
        Self::Delete { path: path.into() }
    }
    pub fn path(&self) -> &str {
        match self {
            Self::Replace { path, .. } | Self::Delete { path } => path,
        }
    }
}
/// Whole-dataset conversion output. The root is installed after every ordered change.
pub struct Transformation {
    pub changes: Vec<Change>,
    pub root: Vec<u8>,
}
/// A supported operating format, with complete validation of its committed dataset.
pub struct Format {
    pub id: u32,
    pub validate: fn(&Snapshot) -> Result<()>,
}
/// A restartable forward edge. `transform` must accept every durable prefix under its source
/// root and reconstruct the same destination using only that mixed dataset. The planner checks
/// this for custom edges, including deletions. Unknown files survive unless explicitly changed.
pub struct Edge {
    pub from: u32,
    pub to: u32,
    pub checkpoint: Checkpoint,
    /// Conservative allocation bound for output bytes, paths, vectors, and additional work.
    /// Checked before `transform` runs. Three largest-file work buffers are reserved separately.
    pub allocation_bound: fn(&Snapshot) -> Result<usize>,
    pub transform: fn(&Snapshot) -> Result<Transformation>,
}
/// Deterministic fewest-edge routing over arbitrary format IDs and forward shortcuts.
pub struct Graph {
    formats: Vec<Format>,
    edges: Vec<Edge>,
    classify: fn(&Snapshot) -> Result<u32>,
    standard: bool,
}
pub(crate) struct Stage {
    pub target: u32,
    pub changes: Vec<Change>,
    pub root: Arc<Vec<u8>>,
}
impl Graph {
    pub fn new(
        formats: Vec<Format>,
        mut edges: Vec<Edge>,
        classify: fn(&Snapshot) -> Result<u32>,
    ) -> Result<Self> {
        let ids: BTreeSet<_> = formats.iter().map(|f| f.id).collect();
        if ids.len() != formats.len()
            || ids.contains(&0)
            || edges
                .iter()
                .any(|e| e.from >= e.to || !ids.contains(&e.from) || !ids.contains(&e.to))
        {
            return Err(Error::new("invalid migration graph"));
        }
        edges.sort_by_key(|e| (e.from, e.to));
        if edges
            .windows(2)
            .any(|e| e[0].from == e[1].from && e[0].to == e[1].to)
        {
            return Err(Error::new("duplicate migration edge"));
        }
        Ok(Self {
            formats,
            edges,
            classify,
            standard: false,
        })
    }
    pub fn standard() -> Self {
        Self {
            formats: vec![
                Format {
                    id: 1,
                    validate: validate_mixed,
                },
                Format {
                    id: 2,
                    validate: validate_current,
                },
            ],
            edges: vec![Edge {
                from: 1,
                to: 2,
                checkpoint: Checkpoint::Immediate,
                allocation_bound: conversion_bound,
                transform: one_to_two,
            }],
            classify: source_format,
            standard: true,
        }
    }
    pub fn check_target(&self, status: &p::Status) -> Result<u32> {
        let target = status
            .information
            .as_ref()
            .and_then(|s| s.target_storage_format)
            .ok_or_else(|| Error::new("missing storage target requires an updated client"))?;
        if !self.formats.iter().any(|f| f.id == target) || self.standard && target != 2 {
            return Err(Error::new(format!(
                "storage target {target} requires an updated client"
            )));
        }
        Ok(target)
    }
    pub fn route(&self, source: u32, target: u32) -> Result<Vec<&Edge>> {
        let mut pending = VecDeque::from([source]);
        let mut previous = BTreeMap::from([(source, None)]);
        while let Some(at) = pending.pop_front() {
            if at == target {
                let mut result = Vec::new();
                let mut at = at;
                while let Some(edge) = previous[&at] {
                    let edge: &Edge = &self.edges[edge];
                    result.push(edge);
                    at = edge.from;
                }
                result.reverse();
                return Ok(result);
            }
            for (index, edge) in self.edges.iter().enumerate().filter(|(_, e)| e.from == at) {
                if let std::collections::btree_map::Entry::Vacant(entry) = previous.entry(edge.to) {
                    entry.insert(Some(index));
                    pending.push_back(edge.to);
                }
            }
        }
        Err(Error::new("no supported forward storage migration path"))
    }
    pub(crate) fn validate(&self, format: u32, snapshot: &Snapshot) -> Result<()> {
        snapshot.input_limits()?;
        memory(snapshot.memory()?, self.work_size(snapshot))?;
        (self
            .formats
            .iter()
            .find(|f| f.id == format)
            .ok_or_else(|| Error::new("unknown storage format"))?
            .validate)(snapshot)
    }
    fn work_size(&self, snapshot: &Snapshot) -> usize {
        if self.standard {
            snapshot.max_record()
        } else {
            snapshot.max_file()
        }
    }
    pub(crate) fn plan(&self, source: &Snapshot, target: u32) -> Result<(u32, Vec<Stage>)> {
        let from = (self.classify)(source)?;
        self.validate(from, source)?;
        let route = self.route(from, target)?;
        let mut current = source.clone();
        let mut stages: Vec<Stage> = Vec::new();
        let mut start = source.clone();
        let mut retained = source.memory()?;
        for (index, edge) in route.iter().enumerate() {
            let stage = transform(edge, &current, retained, self.work_size(&current))?;
            retained = retained
                .checked_add(stage_memory(&stage)?)
                .ok_or_else(limit)?;
            memory(
                retained,
                self.work_size(&current).max(stage_largest(&stage)),
            )?;
            let mut output = current.clone();
            apply_stage(&mut output, &stage)?;
            self.validate(edge.to, &output)?;
            // The fixed 1 -> 2 edge replaces independent records and deletes only empty,
            // inactive, or unusable cache files. Core mixed validation establishes recovery.
            if !self.standard {
                self.prove(&current, &[edge], &stage, &output, retained)?;
            }
            let can_join = index > 0 && route[index - 1].checkpoint == Checkpoint::MayDefer;
            if can_join {
                let prior = stages.last().unwrap();
                let mut combined = Stage {
                    target: edge.to,
                    changes: prior.changes.clone(),
                    root: stage.root.clone(),
                };
                combined.changes.extend(stage.changes.iter().cloned());
                let start_format = (self.classify)(&start)?;
                let first = route.iter().position(|e| e.from == start_format).unwrap();
                if self
                    .prove(&start, &route[first..=index], &combined, &output, retained)
                    .is_ok()
                {
                    *stages.last_mut().unwrap() = combined;
                    current = output;
                    continue;
                }
            }
            start = current.clone();
            stages.push(stage);
            current = output;
        }
        Ok((from, stages))
    }
    fn prove(
        &self,
        start: &Snapshot,
        edges: &[&Edge],
        stage: &Stage,
        expected: &Snapshot,
        retained: usize,
    ) -> Result<()> {
        let mut mixed = start.clone();
        for change in &stage.changes {
            mixed.apply(change)?;
            self.validate(edges[0].from, &mixed)?;
            let mut recovered = mixed.clone();
            for edge in edges {
                let proof_resident = retained
                    .checked_add(recovered.memory()?)
                    .ok_or_else(limit)?;
                let next = transform(edge, &recovered, proof_resident, self.work_size(&recovered))?;
                memory(
                    proof_resident
                        .checked_add(stage_memory(&next)?)
                        .ok_or_else(limit)?,
                    recovered.max_file().max(stage_largest(&next)),
                )?;
                apply_stage(&mut recovered, &next)?;
            }
            if &recovered != expected {
                return Err(Error::new(
                    "migration ordering cannot recover a durable prefix",
                ));
            }
        }
        Ok(())
    }
}
fn stage_memory(stage: &Stage) -> Result<usize> {
    stage
        .changes
        .iter()
        .try_fold(stage.root.capacity(), |sum, change| {
            sum.checked_add(change.path().len())?
                .checked_add(80)?
                .checked_add(match change {
                    Change::Replace { bytes, .. } => bytes.capacity(),
                    _ => 0,
                })
        })
        .ok_or_else(limit)
}
fn stage_largest(stage: &Stage) -> usize {
    stage
        .changes
        .iter()
        .filter_map(|change| match change {
            Change::Replace { bytes, .. } => Some(bytes.len()),
            _ => None,
        })
        .fold(stage.root.len(), usize::max)
}
fn transform(edge: &Edge, source: &Snapshot, retained: usize, working: usize) -> Result<Stage> {
    let bound = (edge.allocation_bound)(source)?;
    memory(retained.checked_add(bound).ok_or_else(limit)?, working)?;
    let value = (edge.transform)(source)?;
    let stage = Stage {
        target: edge.to,
        changes: value.changes,
        root: Arc::new(value.root),
    };
    if stage_memory(&stage)? > bound {
        return Err(Error::new("migration edge exceeded its allocation bound"));
    }
    Ok(stage)
}
fn apply_stage(snapshot: &mut Snapshot, stage: &Stage) -> Result<()> {
    let mut files = snapshot.files().count();
    let mut bytes = snapshot
        .files()
        .try_fold(0usize, |sum, (_, bytes)| sum.checked_add(bytes.len()))
        .ok_or_else(limit)?;
    for change in &stage.changes {
        if let Some(old) = snapshot.file(change.path()) {
            files -= 1;
            bytes -= old.len();
        }
        if let Change::Replace { bytes: value, .. } = change {
            files = files.checked_add(1).ok_or_else(limit)?;
            bytes = bytes.checked_add(value.len()).ok_or_else(limit)?;
        }
        // A fresh worker must be able to inventory every durable prefix, including the
        // temporary overlap while a split or merge retains both old and replacement files.
        if files > p::STORAGE_MIGRATION_MAX_FILES || bytes > p::STORAGE_MIGRATION_MAX_INPUT_BYTES {
            return Err(Error::new(
                "a migration prefix exceeds the file or byte limit",
            ));
        }
        snapshot.apply(change)?;
    }
    snapshot.commit(&stage.root);
    Ok(())
}
fn source_format(snapshot: &Snapshot) -> Result<u32> {
    match core::classify_root(snapshot.file(ROOT)).map_err(storage_error)? {
        core::Root::Legacy { .. } => Ok(1),
        core::Root::Current { .. } => Ok(2),
        _ => Err(Error::new("unsupported or missing storage root")),
    }
}
fn validate_mixed(snapshot: &Snapshot) -> Result<()> {
    ready(core::validate_dataset(&mut snapshot.dataset())).map_err(storage_error)?;
    Ok(())
}
fn validate_current(snapshot: &Snapshot) -> Result<()> {
    ready(core::validate_target_dataset(&mut snapshot.dataset())).map_err(storage_error)?;
    Ok(())
}
fn one_to_two(snapshot: &Snapshot) -> Result<Transformation> {
    // Each converted outer envelope adds at most two bytes. Reserve the complete output and
    // grammar inspection work before any record conversion allocates its replacement.
    let upper = snapshot
        .memory()?
        .checked_mul(2)
        .and_then(|n| n.checked_add(snapshot.entries.len().checked_mul(2)?))
        .ok_or_else(limit)?;
    memory(upper, snapshot.max_record())?;
    let mut dataset = snapshot.dataset();
    let mut changes = Vec::new();
    let mut deletes = Vec::new();
    for (path, value) in &snapshot.entries {
        let entry = core::Entry {
            path: path.clone(),
            kind: match value {
                Contents::File(_) => FileType::File,
                _ => FileType::Directory,
            },
            size: match value {
                Contents::File(bytes) => bytes.len(),
                _ => 0,
            },
        };
        match ready(core::inspect_entry(&mut dataset, 1, &entry)).map_err(storage_error)? {
            core::Disposition::Record(record) if record.source == core::RecordFormat::Legacy => {
                changes.push(Change::replace(path, record.bytes));
            }
            core::Disposition::Obsolete(_) => deletes.push(Change::delete(path)),
            _ => {}
        }
    }
    changes.extend(deletes);
    let root = core::convert_root(
        snapshot
            .file(ROOT)
            .ok_or_else(|| Error::new("missing storage root"))?,
    )
    .map_err(storage_error)?;
    Ok(Transformation { changes, root })
}
fn conversion_bound(snapshot: &Snapshot) -> Result<usize> {
    snapshot
        .memory()?
        .checked_add(snapshot.entries.len().checked_mul(2).ok_or_else(limit)?)
        .ok_or_else(limit)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_opaque_file_at_the_input_limit_needs_no_grammar_work_buffers() {
        let mut snapshot = Snapshot::default();
        snapshot.insert(ROOT.into(), vec![8, 1]).unwrap();
        snapshot
            .insert(
                "/opaque".into(),
                vec![0; p::STORAGE_MIGRATION_MAX_INPUT_BYTES - 2],
            )
            .unwrap();
        let (source, stages) = Graph::standard().plan(&snapshot, 2).unwrap();
        assert_eq!(source, 1);
        assert_eq!(stages.len(), 1);
        assert!(stages[0].changes.is_empty());
        assert_eq!(*stages[0].root, [8, 2]);
        snapshot.insert("/one-byte".into(), vec![0]).unwrap();
        assert!(Graph::standard().plan(&snapshot, 2).is_err());
    }
}
