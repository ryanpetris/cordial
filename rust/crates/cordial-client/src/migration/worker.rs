use super::{
    Change, Error, Graph, Outcome, Result, Snapshot,
    graph::{self, Contents, ROOT, SCRATCH},
};
use crate::{Error as ClientError, protocol as p};
use p::{migration as ipc, request::Command, response};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::atomic::{AtomicBool, Ordering},
};

/// A migration connection. `close` returns only after all underlying handles are released.
pub trait Transport {
    fn request(&mut self, command: Command, cancelled: &AtomicBool) -> crate::Result<p::Response>;
    fn close(&mut self);
}
/// Discovery metadata, read without opening a serial connection.
#[derive(Clone, Debug)]
pub struct AdapterPort {
    pub port: String,
    pub adapter_id: String,
}
/// Port discovery and a cancellation-aware retry delay. Discovery never probes other adapters.
pub trait Discovery {
    type Connection: Transport;
    fn discover(&mut self) -> std::io::Result<Vec<AdapterPort>>;
    fn open(&mut self, port: &str) -> crate::Result<Self::Connection>;
    fn wait(&mut self, cancelled: &AtomicBool);
}

/// Runs an exclusive synchronous worker over a testable transport. Events use the subprocess
/// IPC schema. Reconnection has no deadline; cancellation stops new commands and closes handles.
/// Only the exact migration-required status error authorizes file access. The downloaded root
/// selects the source format; informational status formats do not control migration or success.
pub fn migrate_with<D: Discovery>(
    discovery: &mut D,
    graph: &Graph,
    adapter_id: &str,
    port_hint: Option<&str>,
    cancelled: &AtomicBool,
    mut report: impl FnMut(ipc::MigrationEvent),
) -> Result<Outcome> {
    let mut worker = Worker {
        discovery,
        graph,
        adapter_id,
        port_hint,
        cancelled,
        report: &mut report,
        connection: None,
        target: None,
        source: 0,
        checkpoint: 0,
        uncertain: false,
        completed: false,
        generation: 0,
    };
    let mut result = worker.run();
    if worker.completed {
        result = Ok(());
    }
    if let Err(error) = &mut result {
        error.outcome_unknown |= worker.uncertain;
    }
    worker.close();
    if cancelled.load(Ordering::Acquire) {
        worker.emit(ipc::migration_event::Event::Cancelled(
            ipc::MigrationCancelled {},
        ));
        return Ok(Outcome::Cancelled);
    }
    match &result {
        Ok(()) => worker.emit(ipc::migration_event::Event::Success(
            ipc::MigrationSuccess {
                adapter_id: adapter_id.into(),
                target_format: worker.target.unwrap_or(0),
            },
        )),
        Err(error) => worker.emit(ipc::migration_event::Event::Failure(
            ipc::MigrationFailure {
                reason: error.reason.clone(),
                outcome_unknown: error.outcome_unknown || worker.uncertain,
            },
        )),
    }
    result.map(|()| Outcome::Success)
}

struct Worker<'a, D: Discovery, F: FnMut(ipc::MigrationEvent)> {
    discovery: &'a mut D,
    graph: &'a Graph,
    adapter_id: &'a str,
    port_hint: Option<&'a str>,
    cancelled: &'a AtomicBool,
    report: &'a mut F,
    connection: Option<D::Connection>,
    target: Option<u32>,
    source: u32,
    checkpoint: u32,
    uncertain: bool,
    completed: bool,
    generation: u64,
}
impl<D: Discovery, F: FnMut(ipc::MigrationEvent)> Worker<'_, D, F> {
    fn emit(&mut self, event: ipc::migration_event::Event) {
        (self.report)(ipc::MigrationEvent { event: Some(event) });
    }
    fn progress(&mut self, phase: ipc::MigrationPhase, path: &str, completed: usize, total: usize) {
        self.emit(ipc::migration_event::Event::Progress(
            ipc::MigrationProgress {
                phase: phase.into(),
                source_format: self.source,
                target_format: self.target.unwrap_or(0),
                checkpoint_format: self.checkpoint,
                path: path.into(),
                completed_bytes: completed as u64,
                total_bytes: total as u64,
            },
        ));
    }
    fn check_cancel(&self) -> Result<()> {
        if self.cancelled.load(Ordering::Acquire) {
            Err(Error::new("migration cancelled"))
        } else {
            Ok(())
        }
    }
    fn close(&mut self) {
        if let Some(mut connection) = self.connection.take() {
            connection.close();
        }
    }
    fn waiting(&mut self) -> Result<()> {
        self.check_cancel()?;
        self.emit(ipc::migration_event::Event::Waiting(
            ipc::MigrationWaiting {
                adapter_id: self.adapter_id.into(),
                port_hint: self.port_hint.unwrap_or_default().into(),
                verifying_uncertain_write: self.uncertain,
            },
        ));
        self.discovery.wait(self.cancelled);
        self.check_cancel()
    }
    fn connect(&mut self) -> Result<p::Status> {
        self.close();
        let active = self.target.is_some();
        loop {
            self.check_cancel()?;
            if let Ok(mut ports) = self.discovery.discover() {
                ports.retain(|p| {
                    !p.adapter_id.is_empty() && p.adapter_id.eq_ignore_ascii_case(self.adapter_id)
                });
                ports.sort_by(|a, b| {
                    (Some(a.port.as_str()) != self.port_hint, &a.port)
                        .cmp(&(Some(b.port.as_str()) != self.port_hint, &b.port))
                });
                for port in ports {
                    self.check_cancel()?;
                    let connection = match self.discovery.open(&port.port) {
                        Ok(connection) => connection,
                        Err(error @ ClientError::Incompatible(_)) => {
                            return Err(Error::new(error.to_string()));
                        }
                        Err(_) => continue,
                    };
                    self.connection = Some(connection);
                    let status = self.raw(Command::GetStatus(p::GetStatus {}));
                    match status {
                        Ok(Some(response::Result::Status(status)))
                            if status.id.eq_ignore_ascii_case(self.adapter_id) =>
                        {
                            let target = self.graph.check_target(&status)?;
                            if self.target.is_some_and(|previous| previous != target) {
                                return Err(Error::new(
                                    "adapter storage target changed during migration",
                                ));
                            }
                            self.target = Some(target);
                            // The marker allows nothing else. Normal startup may carry warnings.
                            let assisted = status.errors.iter().any(assisted_error);
                            if status.errors.iter().any(|e| {
                                if assisted {
                                    !assisted_error(e)
                                } else {
                                    !advisory(e)
                                }
                            }) {
                                return Err(status_error(&status));
                            }
                            if active && !assisted && !status.ready {
                                self.close();
                                break;
                            }
                            self.generation = self.generation.wrapping_add(1);
                            self.completed = !assisted && status.ready;
                            return Ok(status);
                        }
                        Ok(Some(response::Result::Status(_))) => {}
                        Ok(_) => {
                            return Err(Error::new("adapter returned an invalid bootstrap status"));
                        }
                        Err(ClientError::Dongle(error)) => return Err(device_error(&error)),
                        Err(error @ ClientError::Incompatible(_)) => {
                            return Err(Error::new(error.to_string()));
                        }
                        Err(_) => {}
                    }
                    self.close();
                }
            }
            self.waiting()?;
        }
    }
    fn raw(&mut self, command: Command) -> crate::Result<Option<response::Result>> {
        let response = self
            .connection
            .as_mut()
            .ok_or(ClientError::Closed)?
            .request(command, self.cancelled)?;
        match response.result {
            Some(response::Result::Error(error)) => Err(ClientError::Dongle(error)),
            other => Ok(other),
        }
    }
    fn call(&mut self, command: Command) -> Result<Option<response::Result>> {
        loop {
            self.check_cancel()?;
            if self.completed {
                return Err(Error::new("adapter completed migration after reconnect"));
            }
            match self.raw(command.clone()) {
                Ok(result) => return Ok(result),
                Err(ClientError::Dongle(error)) => return Err(device_error(&error)),
                Err(error @ ClientError::Incompatible(_)) => {
                    return Err(Error::new(error.to_string()));
                }
                Err(
                    ClientError::Protocol | ClientError::TooLong | ClientError::UnexpectedResponse,
                ) => {
                    return Err(Error::new("invalid file protocol response"));
                }
                Err(_) => {
                    self.reconnect()?;
                }
            }
        }
    }
    fn reconnect(&mut self) -> Result<()> {
        self.connect()?;
        Ok(())
    }
    fn run(&mut self) -> Result<()> {
        if self.adapter_id.is_empty() {
            return Err(Error::new("a stable adapter ID is required"));
        }
        self.progress(ipc::MigrationPhase::Preflight, "", 0, 0);
        let status = self.connect()?;
        if self.completed {
            return Ok(());
        }
        if !status.errors.iter().any(assisted_error) {
            return Err(Error::new("the adapter isn't ready"));
        }
        let snapshot = self.download()?;
        self.progress(ipc::MigrationPhase::Convert, "", 0, 0);
        let (source, stages) = self.graph.plan(&snapshot, self.target.unwrap())?;
        self.source = source;
        let mut durable = snapshot.entries;
        // Every transformation, recovery proof, allocation bound and path is settled before here.
        for stage in stages {
            self.checkpoint = stage.target;
            for change in stage.changes {
                match change {
                    Change::Replace { path, bytes } => {
                        let old = file_value(&durable, &path);
                        self.install(&path, &bytes, old)?;
                        durable.insert(path, Contents::File(bytes));
                    }
                    Change::Delete { path } => {
                        self.delete(&path, file_value(&durable, &path))?;
                        durable.remove(&path);
                    }
                }
            }
            // Scratch is excluded from conversion but must be absent before root commit.
            self.delete(SCRATCH, None)?;
            self.progress(ipc::MigrationPhase::Checkpoint, ROOT, 0, stage.root.len());
            self.install(ROOT, &stage.root, file_value(&durable, ROOT))?;
            durable.insert(ROOT.into(), Contents::File(stage.root));
            self.emit(ipc::migration_event::Event::Checkpoint(
                ipc::MigrationCheckpoint {
                    committed_format: stage.target,
                    target_format: self.target.unwrap(),
                },
            ));
            self.source = stage.target;
        }
        drop(durable);
        self.restart()
    }
    fn restart(&mut self) -> Result<()> {
        self.progress(ipc::MigrationPhase::Confirm, "", 0, 0);
        loop {
            self.check_cancel()?;
            self.uncertain = true;
            match self.raw(Command::Restart(p::Restart {})) {
                Ok(None) => {}
                Err(ClientError::Dongle(error)) if error.outcome() == p::ErrorOutcome::NoEffect => {
                    self.uncertain = false;
                    return Err(device_error(&error));
                }
                Ok(_) | Err(_) => {}
            }
            // Normal readiness without the marker confirms startup after the hardware reset.
            self.connect()?;
            if self.completed {
                self.uncertain = false;
                return Ok(());
            }
            self.waiting()?;
        }
    }
    fn inventory(&mut self) -> Result<Vec<(String, bool, usize)>> {
        let mut directories = vec!["/".to_string()];
        let mut seen = BTreeSet::new();
        let mut entries = Vec::new();
        let mut files = 0usize;
        let mut total = 0usize;
        while let Some(directory) = directories.pop() {
            let Some(response::Result::Files(list)) =
                self.call(Command::ListFiles(p::ListFiles {
                    path: directory.clone(),
                }))?
            else {
                return Err(Error::new("invalid directory listing response"));
            };
            let mut previous: Option<String> = None;
            for entry in list.entries {
                if entry.name.is_empty()
                    || entry.name.contains('/')
                    || previous
                        .as_ref()
                        .is_some_and(|p| p.as_bytes() >= entry.name.as_bytes())
                {
                    return Err(Error::new("directory entries are invalid or unordered"));
                }
                let path = format!("{}/{}", directory.trim_end_matches('/'), entry.name);
                graph::valid_path(&path)?;
                previous = Some(entry.name);
                if !seen.insert(path.clone()) || seen.len() > 2 * p::STORAGE_MIGRATION_MAX_FILES {
                    return Err(Error::new("too many or duplicate dataset entries"));
                }
                if path == SCRATCH {
                    if entry.directory {
                        return Err(Error::new("the upload scratch path is a directory"));
                    }
                    // Scratch may duplicate a large source file. It is not dataset input and
                    // is never downloaded, decoded or included in the input-byte budget.
                    continue;
                }
                let size = usize::try_from(entry.size).map_err(|_| graph::limit())?;
                if entry.directory {
                    directories.push(path.clone());
                } else {
                    files = files.checked_add(1).ok_or_else(graph::limit)?;
                    total = total.checked_add(size).ok_or_else(graph::limit)?;
                    if files > p::STORAGE_MIGRATION_MAX_FILES
                        || total > p::STORAGE_MIGRATION_MAX_INPUT_BYTES
                    {
                        return Err(Error::new("migration input exceeds the file or byte limit"));
                    }
                    graph::memory(total, 0)?;
                }
                entries.push((path, entry.directory, size));
            }
        }
        Ok(entries)
    }
    fn download(&mut self) -> Result<Snapshot> {
        let inventory = self.inventory()?;
        let total = inventory
            .iter()
            .filter(|(_, dir, _)| !dir)
            .try_fold(0usize, |n, (_, _, size)| n.checked_add(*size))
            .ok_or_else(graph::limit)?;
        let mut snapshot = Snapshot::default();
        let mut completed = 0usize;
        for (path, directory, size) in inventory {
            self.check_cancel()?;
            if directory {
                snapshot.directory(path)?;
                continue;
            }
            self.progress(ipc::MigrationPhase::Download, &path, completed, total);
            if path == SCRATCH {
                continue;
            }
            graph::memory(
                snapshot
                    .memory()?
                    .checked_add(size)
                    .ok_or_else(graph::limit)?,
                0,
            )?;
            let mut bytes = Vec::new();
            bytes.try_reserve_exact(size).map_err(|_| graph::limit())?;
            loop {
                let data = self
                    .read(&path, bytes.len(), Some(size))?
                    .ok_or_else(|| Error::new("an inventoried file disappeared"))?;
                if data.data.is_empty() {
                    break;
                }
                bytes.extend_from_slice(&data.data);
                completed = completed
                    .checked_add(data.data.len())
                    .ok_or_else(graph::limit)?;
                self.progress(ipc::MigrationPhase::Download, &path, completed, total);
            }
            snapshot.insert(path, bytes)?;
        }
        Ok(snapshot)
    }
    fn read(
        &mut self,
        path: &str,
        offset: usize,
        size: Option<usize>,
    ) -> Result<Option<p::FileData>> {
        let command = Command::ReadFile(p::ReadFile {
            path: path.into(),
            offset: offset as u64,
            length: Some(p::STORAGE_MIGRATION_CHUNK_BYTES as u32),
        });
        let result = self.call(command);
        let result = match result {
            Err(error) if error.reason == "file not found" => return Ok(None),
            other => other?,
        };
        let Some(response::Result::File(data)) = result else {
            return Err(Error::new("invalid file read response"));
        };
        let reported = usize::try_from(data.size).map_err(|_| graph::limit())?;
        if data.path != path
            || data.offset != offset as u64
            || size.is_some_and(|s| reported != s)
            || offset > reported
            || data.data.len() != p::STORAGE_MIGRATION_CHUNK_BYTES.min(reported - offset)
        {
            return Err(Error::new("file read metadata or length mismatch"));
        }
        Ok(Some(data))
    }
    // Compares bytes a chunk at a time without allocating a second whole-file readback.
    fn matches(&mut self, path: &str, expected: Option<&[u8]>) -> Result<bool> {
        let mut offset = 0;
        loop {
            let Some(data) = self.read(path, offset, None)? else {
                return Ok(expected.is_none());
            };
            let Some(expected) = expected else {
                return Ok(false);
            };
            if data.size != expected.len() as u64
                || expected.get(offset..offset + data.data.len()) != Some(data.data.as_slice())
            {
                return Ok(false);
            }
            if data.data.is_empty() {
                return Ok(true);
            }
            offset += data.data.len();
        }
    }
    fn mutate(&mut self, command: Command) -> Result<bool> {
        self.check_cancel()?;
        self.uncertain = true;
        match self.raw(command) {
            Ok(None) => Ok(true),
            Err(ClientError::Dongle(error)) if error.outcome() == p::ErrorOutcome::NoEffect => {
                self.uncertain = false;
                Err(device_error(&error))
            }
            Ok(_) | Err(_) => {
                self.uncertain = true;
                self.reconnect()?;
                Ok(false)
            }
        }
    }
    fn install(&mut self, path: &str, expected: &[u8], old: Option<&[u8]>) -> Result<()> {
        loop {
            self.check_cancel()?;
            if self.matches(path, Some(expected))? {
                self.uncertain = false;
                return Ok(());
            }
            if !self.matches(path, old)? {
                return Err(Error::new(
                    "file contains neither the previous nor proposed contents",
                ));
            }
            self.uncertain = false;
            self.progress(ipc::MigrationPhase::Upload, path, 0, expected.len());
            let acknowledged = if expected.len() <= p::STORAGE_MIGRATION_CHUNK_BYTES {
                self.mutate(Command::WriteFile(p::WriteFile {
                    path: path.into(),
                    offset: 0,
                    data: expected.into(),
                }))?
            } else {
                // Scratch is disposable. Rebuild it after any uncertain chunk rather than
                // appending to bytes whose durable offset has not been established.
                let mut restart = false;
                for (index, chunk) in expected
                    .chunks(p::STORAGE_MIGRATION_CHUNK_BYTES)
                    .enumerate()
                {
                    let offset = index * p::STORAGE_MIGRATION_CHUNK_BYTES;
                    if !self.mutate(Command::WriteFile(p::WriteFile {
                        path: SCRATCH.into(),
                        offset: offset as u64,
                        data: chunk.into(),
                    }))? {
                        restart = true;
                        break;
                    }
                    self.progress(
                        ipc::MigrationPhase::Upload,
                        path,
                        offset + chunk.len(),
                        expected.len(),
                    );
                }
                if restart {
                    continue;
                }
                let generation = self.generation;
                if !self.matches(SCRATCH, Some(expected))? {
                    if self.generation != generation {
                        // Startup may discard abandoned scratch while reconnecting. The
                        // destination is checked again before rebuilding the temporary file.
                        continue;
                    }
                    return Err(Error::new("temporary file readback mismatch"));
                }
                self.mutate(Command::MoveFile(p::MoveFile {
                    source: SCRATCH.into(),
                    destination: path.into(),
                }))?
            };
            self.progress(ipc::MigrationPhase::Verify, path, 0, expected.len());
            if self.matches(path, Some(expected))? {
                self.uncertain = false;
                return Ok(());
            }
            if acknowledged {
                if self.matches(path, old)? {
                    self.uncertain = false;
                }
                return Err(Error::new("file readback mismatch"));
            }
        }
    }
    fn delete(&mut self, path: &str, old: Option<&[u8]>) -> Result<()> {
        loop {
            if self.matches(path, None)? {
                self.uncertain = false;
                return Ok(());
            }
            if path != SCRATCH && !self.matches(path, old)? {
                return Err(Error::new("obsolete file changed before deletion"));
            }
            self.uncertain = false;
            self.progress(ipc::MigrationPhase::Delete, path, 0, 0);
            let acknowledged =
                self.mutate(Command::DeleteFile(p::DeleteFile { path: path.into() }))?;
            if self.matches(path, None)? {
                self.uncertain = false;
                return Ok(());
            }
            if acknowledged {
                if path == SCRATCH || self.matches(path, old)? {
                    self.uncertain = false;
                }
                return Err(Error::new("file deletion readback mismatch"));
            }
        }
    }
}
fn file_value<'a>(entries: &'a BTreeMap<String, Contents>, path: &str) -> Option<&'a [u8]> {
    match entries.get(path) {
        Some(Contents::File(bytes)) => Some(bytes),
        _ => None,
    }
}
fn assisted_error(error: &p::Error) -> bool {
    error.code() == p::ErrorCode::StorageMigrationRequired
        && error.reason() == p::ErrorReason::Storage
        && error.outcome() == p::ErrorOutcome::NoEffect
        && error.related.is_empty()
}
/// Warnings that normal operation reports beside readiness. They don't stop a migration from
/// completing, but they do stop one from starting.
fn advisory(error: &p::Error) -> bool {
    use p::ErrorCode as C;
    matches!(
        error.code(),
        C::StorageFull
            | C::ProfileMemoryLow
            | C::UsbRequiredCapacityExceeded
            | C::UsbReservedCapacityExceeded
    ) && error.related.is_empty()
}
/// The status errors other than the migration marker, including any warnings reported beside
/// the errors that block or end the migration.
fn status_error(status: &p::Status) -> Error {
    let errors: Vec<_> = status
        .errors
        .iter()
        .filter(|e| !assisted_error(e))
        .map(device_error)
        .collect();
    Error {
        reason: errors
            .iter()
            .map(|e| e.reason.as_str())
            .collect::<Vec<_>>()
            .join("; "),
        outcome_unknown: false,
    }
}
/// An adapter error in words, followed by its related failures. The CLI and desktop app show
/// these reasons as they are, so they never name protocol values.
fn device_error(error: &p::Error) -> Error {
    use p::{ErrorCode as C, ErrorReason as R};
    // Only a plain no-effect answer proves the file is absent.
    if error.code() == C::NotFound
        && error.outcome() == p::ErrorOutcome::NoEffect
        && error.related.is_empty()
    {
        return Error::new("file not found");
    }
    let words = match (error.code(), error.reason()) {
        (C::NotFound, _) => "the adapter couldn't find a migration file",
        (C::StorageFailed, _) => "the adapter couldn't read or write its saved data",
        (C::StorageFull, _) => "the adapter's storage is full",
        (C::StorageMigrationRequired, _) => "the adapter's saved data must be migrated",
        (C::NotReady, R::Radio) => "the adapter's Bluetooth radio isn't ready",
        (C::NotReady, R::Storage) => "the adapter's storage isn't ready",
        (C::NotReady, _) => "the adapter isn't ready",
        (C::Busy, _) => "the adapter is busy",
        (C::BadRequest | C::BadArgs, _) => "the adapter couldn't accept a migration request",
        (C::TooLong, _) => "a migration request was too large for the adapter",
        (C::UnknownCommand | C::Unsupported, _) => {
            "the adapter's firmware doesn't support a migration request"
        }
        (C::ProfileMemoryLow, _) => "the adapter is almost out of profile memory",
        (C::ProfileMemoryExceeded, _) => "the adapter doesn't have room for every profile",
        (C::ProfileLoadFailed, _) => "the adapter couldn't read a saved profile",
        (C::UsbRequiredCapacityExceeded, _) => {
            "the adapter doesn't have enough USB capacity for every required interface"
        }
        (C::UsbReservedCapacityExceeded, _) => {
            "the adapter doesn't have enough USB capacity for every reserved interface"
        }
        (C::Internal, _) => "the adapter hit an unexpected failure",
        _ => "the adapter reported an error this version of Cordial doesn't recognize",
    };
    let related: Vec<_> = error.related.iter().map(device_error).collect();
    Error {
        reason: std::iter::once(words.to_owned())
            .chain(related.iter().map(|e| format!("also {}", e.reason)))
            .collect::<Vec<_>>()
            .join("; "),
        outcome_unknown: error.outcome() != p::ErrorOutcome::NoEffect
            || related.iter().any(|e| e.outcome_unknown),
    }
}

impl Transport for crate::Connection {
    fn request(&mut self, command: Command, cancelled: &AtomicBool) -> crate::Result<p::Response> {
        self.migration_request(command, cancelled)
    }
    fn close(&mut self) {
        assert!(
            self.close_and_wait_release(),
            "migration connection cannot close from its reader handler"
        );
    }
}
#[cfg(feature = "serialport")]
struct SerialDiscovery;
#[cfg(feature = "serialport")]
impl Discovery for SerialDiscovery {
    type Connection = crate::Connection;
    fn discover(&mut self) -> std::io::Result<Vec<AdapterPort>> {
        Ok(crate::serial::ports()?
            .into_iter()
            .map(|port| AdapterPort {
                adapter_id: port.id().into(),
                port: port.port,
            })
            .collect())
    }
    fn open(&mut self, port: &str) -> crate::Result<Self::Connection> {
        let connection = crate::serial::open_with_handler(port, |_| {})?;
        connection.set_migration_limits();
        Ok(connection)
    }
    fn wait(&mut self, cancelled: &AtomicBool) {
        if !cancelled.load(Ordering::Acquire) {
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    }
}
/// Runs migration on a real adapter. The caller closes every existing adapter connection and
/// suspends discovery before starting this synchronous worker. Failure keeps that ownership
/// blocked until explicit cancellation; success and cancellation events follow handle release.
#[cfg(feature = "serialport")]
pub fn migrate(
    adapter_id: &str,
    port_hint: Option<&str>,
    cancelled: &AtomicBool,
    report: impl FnMut(ipc::MigrationEvent),
) -> Result<Outcome> {
    migrate_with(
        &mut SerialDiscovery,
        &Graph::standard(),
        adapter_id,
        port_hint,
        cancelled,
        report,
    )
}
