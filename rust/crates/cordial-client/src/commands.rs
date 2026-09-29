//! Host command workflows over the current shared wire contract.
#![allow(clippy::result_large_err)] // A desktop error owns its wire details and partial-result pointer.
use crate::{
    client::{Envelope, Error, Wait},
    controller::{
        Command, Failure, JobCounts, Notice, Outcome, RunOptions, State, Subject, Ticket,
    },
    session::Session,
    storage,
    ui::catalog,
    view::{valid_id, validate_device},
};
use cordial_protocol::{
    identifiers::*,
    messages::{self as wire, Candidate, Empty, SettingChunk},
    payloads::{
        BootloaderResult, DeviceResult, ErrorDetails, FileEntry, ScanEnd, SettingOutcomeChunk,
        SettingsSummary,
    },
    settings::SettingKey,
};
use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};

pub(crate) const STARTING: &str =
    "the adapter is still starting; status, files and bootloader work meanwhile";
/// How often transfer progress and listing rows are reported.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(100);

/// Directory rows reported in small batches as they arrive.
struct Batch<'a> {
    session: &'a Session,
    ticket: Ticket,
    entries: Vec<FileEntry>,
    since: Instant,
}
impl<'a> Batch<'a> {
    fn new(session: &'a Session, ticket: Ticket) -> Self {
        Self {
            session,
            ticket,
            entries: Vec::new(),
            since: Instant::now(),
        }
    }
    fn push(&mut self, entry: FileEntry) {
        self.entries.push(entry);
        if self.entries.len() >= 32 || self.since.elapsed() >= PROGRESS_INTERVAL {
            self.flush();
        }
    }
    fn flush(&mut self) {
        self.since = Instant::now();
        if !self.entries.is_empty() {
            self.session.notice(Notice::StorageEntries {
                ticket: self.ticket,
                entries: std::mem::take(&mut self.entries),
            });
        }
    }
}

pub(crate) fn unsupported(state: &State, command: &Command) -> Option<&'static str> {
    use wire::CommandId as Id;
    let caps = &state.capabilities;
    let supports = |id| caps.supports_command(id);
    let id = match command {
        Command::Status => Id::Status,
        Command::Capabilities => Id::Capabilities,
        Command::StorageList(_) | Command::StorageGet { .. } => {
            if !caps.contains(wire::Capability::StorageManagement) {
                return Some("this adapter doesn't offer file access");
            }
            Id::StorageRead
        }
        Command::Platform(_) => Id::Platform,
        Command::Name(_) => Id::Name,
        Command::Monitor(_) => Id::Monitor,
        Command::Scan(transport) => {
            return caps.unsupported(&wire::Command::Scan(wire::Scan {
                transport: *transport,
                duration_ms: 0,
            }));
        }
        Command::ScanOff | Command::Cancel(_) => Id::Cancel,
        Command::Devices => Id::Devices,
        Command::Info(target) | Command::Remove(target) => {
            if matches!(resolve(state, target), Ok(Resolved::Candidate(_))) {
                return None;
            }
            if matches!(command, Command::Info(_)) {
                Id::Info
            } else {
                Id::Unpair
            }
        }
        Command::Pair(target) | Command::Connect(target) => {
            let resolved = if matches!(command, Command::Pair(_)) {
                resolve_candidate(state, target)
                    .ok()
                    .map(Resolved::Candidate)
            } else {
                resolve(state, target).ok()
            };
            let transport = match &resolved {
                Some(Resolved::Saved(id, _)) => state
                    .devices
                    .iter()
                    .find(|d| d.device_id == *id)
                    .map(|d| d.transport),
                Some(Resolved::Candidate(c)) => Some(c.transport),
                None => None,
            };
            let none = !caps.supports_transport(Transport::Classic)
                && !caps.supports_transport(Transport::Ble);
            if none || transport.is_some_and(|t| !caps.supports_transport(t)) {
                return Some("this adapter does not support the device's Bluetooth transport");
            }
            if matches!(command, Command::Pair(_)) && !supports(Id::Pair) {
                return Some("this adapter does not support pairing");
            }
            // Pair also connects the newly saved device.
            Id::Connect
        }
        Command::DeviceInfo(_) => Id::DeviceInfo,
        Command::DeviceInfoRefresh(_) => Id::DeviceInfoRefresh,
        Command::Disconnect(_) => Id::Disconnect,
        Command::Enabled(..) => Id::DeviceEnabled,
        Command::Trusted(..) => Id::DeviceTrusted,
        Command::Blocked(..) => Id::DeviceBlocked,
        Command::Hidpp(..) => Id::Hidpp,
        Command::PairReply { .. } => Id::PairReply,
        Command::Features(_) => Id::Features,
        Command::Settings(_) => Id::Settings,
        Command::SettingGet(..) => Id::SettingsGet,
        Command::SettingSet(..) => {
            if !supports(Id::SettingsGet) {
                return Some("this adapter does not support reading setting metadata");
            }
            Id::SettingsSet
        }
        Command::SettingForget(..) => Id::SettingsForget,
        Command::SettingsRefresh(_) => Id::SettingsRefresh,
        Command::SettingsApply(_) => Id::SettingsApply,
        Command::Bootloader => {
            if !caps.contains(wire::Capability::Debug) {
                return Some("this adapter doesn't offer development functions");
            }
            Id::Bootloader
        }
    };
    (!supports(id)).then_some("this adapter does not support that command")
}

pub(crate) fn execute(
    session: &Session,
    command: &Command,
    options: &RunOptions,
    ticket: Ticket,
) -> Result<Outcome, Failure> {
    let state = session.state();
    if !state.available {
        return Err(Error::new("the control session is unavailable; select an adapter").into());
    }
    if !state.ready && !command.direct() {
        return Err(state
            .ready_error
            .unwrap_or_else(|| Error::new(STARTING))
            .into());
    }
    if let Some(reason) = command.unsupported(&state) {
        return Err(Error::new(reason).into());
    }
    let wait = &options.wait;
    match command {
        Command::Status => {
            session.call(wire::Command::Status(Empty {}), false, wait)?;
            return Ok(Outcome::Status(session.state().status));
        }
        Command::Capabilities => {
            // Read again so scripts see the response; it cannot change
            // within a connection.
            let rows = session.call(wire::Command::Capabilities(Empty {}), false, wait)?;
            let caps: wire::Capabilities = last(&rows)?;
            if caps != state.capabilities {
                return Err(Error::new("adapter capabilities changed; reopen it").into());
            }
            return Ok(Outcome::Capabilities(caps));
        }
        Command::StorageList(path) => {
            let mut batch = Batch::new(session, ticket);
            let count = storage::list(&session.client, path, wait, |entry| batch.push(entry));
            batch.flush();
            return Ok(Outcome::StorageListed {
                path: path.clone(),
                count: count?,
            });
        }
        Command::StorageGet {
            path,
            local,
            overwrite,
        } => {
            let mut last = Instant::now();
            let bytes = storage::read(&session.client, path, local, *overwrite, wait, |bytes| {
                if last.elapsed() >= PROGRESS_INTERVAL {
                    last = Instant::now();
                    session.notice(Notice::StorageProgress { ticket, bytes });
                }
            })?;
            return Ok(Outcome::StorageSaved {
                path: path.clone(),
                local: local.clone(),
                bytes,
            });
        }
        Command::Name(value) => {
            let name = value
                .as_deref()
                .map(|value| {
                    cordial_protocol::adapter_name(value)
                        .ok_or_else(|| Error::new("invalid adapter name"))
                })
                .transpose()?;
            if !state.status.storage_ready {
                return Err(Error::new("adapter storage is not ready").into());
            }
            let rows = session.call(
                wire::Command::Name(wire::AdapterName {
                    name: name.map(Into::into),
                }),
                false,
                wait,
            )?;
            let result: cordial_protocol::payloads::AdapterSettings = last(&rows)?;
            return Ok(Outcome::Name(result.name));
        }
        Command::Platform(platform) => {
            if !state.status.storage_ready {
                return Err(Error::new("adapter storage is not ready").into());
            }
            session.call(
                wire::Command::Platform(wire::Platform {
                    platform: *platform,
                }),
                false,
                wait,
            )?;
            return Ok(Outcome::Platform(*platform));
        }
        Command::Monitor(enabled) => {
            session.monitoring(*enabled, wait)?;
            return Ok(Outcome::Monitor(*enabled));
        }
        Command::Scan(transport) => return scan(session, *transport, options).map_err(Into::into),
        Command::ScanOff => {
            return Ok(Outcome::ScanStopped {
                was_running: session.stop_scan(wait)?,
            });
        }
        Command::Devices => {
            session.refresh(wait)?;
            return Ok(Outcome::Devices);
        }
        Command::Cancel(id) => {
            session.call(
                wire::Command::Cancel(wire::RequestRef { request_id: *id }),
                false,
                wait,
            )?;
            return Ok(Outcome::CancelRequested(*id));
        }
        Command::PairReply {
            request,
            prompt,
            action,
            value,
        } => {
            session.call(
                wire::Command::PairReply(wire::PairReply {
                    request_id: *request,
                    prompt_id: prompt.clone(),
                    action: *action,
                    value: value.clone(),
                }),
                false,
                wait,
            )?;
            return Ok(Outcome::ReplySent);
        }
        Command::Bootloader => {
            let rows = session.call(wire::Command::Bootloader(Empty {}), false, wait)?;
            let result: BootloaderResult = last(&rows)?;
            if !result.rebooting {
                return Err(Error::new("adapter did not acknowledge bootloader entry").into());
            }
            return Ok(Outcome::Bootloader { mode: result.mode });
        }
        _ => {}
    }
    let target = match command {
        Command::Info(s)
        | Command::DeviceInfo(s)
        | Command::DeviceInfoRefresh(s)
        | Command::Pair(s)
        | Command::Connect(s)
        | Command::Disconnect(s)
        | Command::Enabled(s, _)
        | Command::Trusted(s, _)
        | Command::Blocked(s, _)
        | Command::Remove(s)
        | Command::Hidpp(s, _)
        | Command::Features(s)
        | Command::Settings(s)
        | Command::SettingGet(s, _)
        | Command::SettingSet(s, _, _)
        | Command::SettingForget(s, _)
        | Command::SettingsRefresh(s)
        | Command::SettingsApply(s) => s,
        _ => unreachable!(),
    };
    if matches!(command, Command::Pair(_)) && options.one_shot {
        if target.starts_with("c_") {
            return Err(Error::new(
                "candidate IDs are session-local; one-shot pair needs a nearby device name",
            )
            .into());
        }
        scan(session, ScanTransport::Both, options)?;
    }
    if matches!(command, Command::Pair(_)) {
        return pair(session, resolve_candidate(&session.state(), target)?, wait);
    }
    let resolved = resolve(&session.state(), target)?;
    let subject = resolved.subject();
    let id = match resolved {
        Resolved::Saved(id, _) => id,
        Resolved::Candidate(candidate) => match command {
            Command::Info(_) => return Ok(Outcome::Candidate(candidate)),
            Command::Remove(_) => {
                session.hide_candidate(&candidate.candidate_id);
                return Ok(Outcome::Hidden(subject));
            }
            _ => return Err(Error::new("pair the candidate before using this command").into()),
        },
    };
    let device = wire::DeviceRef {
        device_id: id.clone(),
    };
    let request = match command {
        Command::Info(_) => wire::Command::Info(device),
        Command::Connect(_) => wire::Command::Connect(wire::Connect {
            device_id: id,
            timeout_ms: 30_000,
        }),
        Command::Disconnect(_) => wire::Command::Disconnect(device),
        Command::Enabled(_, enabled) => wire::Command::DeviceEnabled(wire::DeviceEnabled {
            device_id: id,
            enabled: *enabled,
        }),
        Command::Trusted(_, trusted) => wire::Command::DeviceTrusted(wire::DeviceTrusted {
            device_id: id,
            trusted: *trusted,
        }),
        Command::Blocked(_, blocked) => wire::Command::DeviceBlocked(wire::DeviceBlocked {
            device_id: id,
            blocked: *blocked,
        }),
        Command::Remove(_) => wire::Command::Unpair(device),
        Command::Hidpp(_, enabled) => wire::Command::Hidpp(wire::DeviceEnabled {
            device_id: id,
            enabled: *enabled,
        }),
        Command::DeviceInfo(_) | Command::DeviceInfoRefresh(_) => {
            let refresh = matches!(command, Command::DeviceInfoRefresh(_));
            session.info(&id, refresh, false, wait)?;
            return Ok(Outcome::DeviceInfo(subject));
        }
        Command::Features(_) | Command::Settings(_) => {
            let features = matches!(command, Command::Features(_));
            session.settings(&id, features, wait)?;
            return Ok(if features {
                Outcome::Features(subject)
            } else {
                Outcome::Settings(subject)
            });
        }
        Command::SettingGet(_, key) => wire::Command::SettingsGet(setting_ref(id, *key)),
        Command::SettingForget(_, key) => wire::Command::SettingsForget(setting_ref(id, *key)),
        Command::SettingSet(_, key, input) => {
            let metadata = session.setting(&id, *key, wait)?;
            let value = catalog::setting_value(&metadata, input).map_err(Error::new)?;
            wire::Command::SettingsSet(wire::SettingSet {
                device_id: id,
                key: key_name(*key),
                value,
            })
        }
        Command::SettingsRefresh(_) => {
            return job(
                session,
                wire::Command::SettingsRefresh(device),
                subject,
                wait,
            );
        }
        Command::SettingsApply(_) => {
            return job(session, wire::Command::SettingsApply(device), subject, wait);
        }
        _ => unreachable!(),
    };
    let rows = session.call(request, false, wait)?;
    match command {
        Command::Info(_) => {
            let mut result: DeviceResult = last(&rows)?;
            validate_device(&result.device)?;
            if result.device.device_id.0 != subject.id {
                return Err(Error::new("adapter returned a different device").into());
            }
            // Named as the session names it, keeping a reported name.
            if let Some(known) = session
                .state()
                .devices
                .into_iter()
                .find(|d| d.device_id == result.device.device_id)
            {
                result.device.name = known.name;
            }
            Ok(Outcome::Device {
                subject,
                device: Some(result.device),
            })
        }
        Command::Connect(_) => Ok(Outcome::Connected(subject)),
        Command::SettingGet(..) | Command::SettingSet(..) | Command::SettingForget(..) => {
            let result: SettingChunk = last(&rows)?;
            Ok(Outcome::Setting {
                subject,
                setting: result.setting,
            })
        }
        _ => Ok(Outcome::Device {
            subject,
            device: None,
        }),
    }
}
fn last<T: serde::de::DeserializeOwned>(rows: &[Envelope]) -> Result<T, Error> {
    rows.last()
        .ok_or_else(|| Error::new("adapter returned no result"))?
        .decode()
}
fn key_name(key: SettingKey) -> String {
    serde_json::to_value(key).unwrap().as_str().unwrap().into()
}
fn setting_ref(id: DeviceId, key: SettingKey) -> wire::SettingRef {
    wire::SettingRef {
        device_id: id,
        key: key_name(key),
    }
}
fn scan(
    session: &Session,
    transport: ScanTransport,
    options: &RunOptions,
) -> Result<Outcome, Error> {
    let transport = session
        .state()
        .capabilities
        .scan_transport(transport)
        .ok_or_else(|| Error::new("this adapter does not support the requested scan"))?;
    let duration_ms = if options.one_shot {
        let ms = options.scan_duration.as_millis();
        if !(1_000..=60_000).contains(&ms) {
            return Err(Error::new("scan duration must be between 1s and 60s"));
        }
        ms as u32
    } else {
        0
    };
    let request = session.start(
        wire::Command::Scan(wire::Scan {
            transport,
            duration_ms,
        }),
        false,
        !options.one_shot,
        &options.wait,
    )?;
    if !options.one_shot {
        return Ok(Outcome::ScanStarted {
            request: request.id,
            transport,
        });
    }
    let rows = session.wait(request, &options.wait)?;
    let summary: ScanEnd = last(&rows)?;
    Ok(Outcome::ScanFinished {
        count: summary.count as u64,
        truncated: summary.truncated,
    })
}
#[derive(Clone)]
enum Resolved {
    Saved(DeviceId, Option<String>),
    Candidate(Candidate),
}
impl Resolved {
    fn subject(&self) -> Subject {
        match self {
            Self::Saved(id, name) => Subject {
                id: id.0.clone(),
                name: name.clone(),
            },
            Self::Candidate(c) => Subject {
                id: c.candidate_id.0.clone(),
                name: c.name.clone(),
            },
        }
    }
}
fn resolve(state: &State, target: &str) -> Result<Resolved, Error> {
    let candidate = |c: &Candidate| Resolved::Candidate(c.clone());
    if let Some(d) = state.devices.iter().find(|d| d.device_id.0 == target) {
        return Ok(Resolved::Saved(d.device_id.clone(), d.name.clone()));
    }
    if let Some(c) = state.candidates.iter().find(|c| c.candidate_id.0 == target) {
        return Ok(candidate(c));
    }
    let mut matches = BTreeMap::new();
    for d in &state.devices {
        if d.name.as_deref() == Some(target) {
            matches.insert(
                d.device_id.0.clone(),
                Resolved::Saved(d.device_id.clone(), d.name.clone()),
            );
        }
    }
    for c in &state.candidates {
        if c.name.as_deref() == Some(target) {
            let r = candidate(c);
            matches.entry(r.subject().id).or_insert(r);
        }
    }
    if matches.len() > 1 {
        return Err(Error::new(format!(
            "name {target:?} is ambiguous; use a device or candidate ID"
        )));
    }
    if let Some((_, resolved)) = matches.pop_first() {
        return Ok(resolved);
    }
    if target.starts_with("d_") && valid_id(target).is_ok() {
        return Ok(Resolved::Saved(DeviceId(target.into()), None));
    }
    Err(Error::new(format!(
        "device {target:?} not found; scan or use a saved device ID"
    )))
}
fn resolve_candidate(state: &State, target: &str) -> Result<Candidate, Error> {
    if let Some(c) = state.candidates.iter().find(|c| c.candidate_id.0 == target) {
        return Ok(c.clone());
    }
    let mut matches = state
        .candidates
        .iter()
        .filter(|c| c.name.as_deref() == Some(target));
    let candidate = matches.next().ok_or_else(|| {
        Error::new(
            "put the device in pairing mode, scan, and pair its Nearby entry or candidate ID",
        )
    })?;
    if matches.next().is_some() {
        return Err(Error::new(
            "more than one nearby device has that name; select its candidate ID in the interactive CLI or TUI",
        ));
    }
    Ok(candidate.clone())
}
fn pair(session: &Session, candidate: Candidate, wait: &Wait) -> Result<Outcome, Failure> {
    let mut subject = Resolved::Candidate(candidate.clone()).subject();
    let rows = session.call(
        wire::Command::Pair(wire::Pair {
            candidate_id: candidate.candidate_id.clone(),
            timeout_ms: 120_000,
        }),
        false,
        wait,
    )?;
    let result: DeviceResult = last(&rows)?;
    validate_device(&result.device)?;
    let id = result.device.device_id;
    let bonded = Some(id.clone());
    subject.id = id.0.clone();
    if !result.device.effective_enabled {
        // Saved but not admitted for connections; connecting would be refused.
        return Ok(Outcome::PairedDisabled(
            subject,
            result.device.enabled_reason,
        ));
    }
    session.notice(Notice::BondSaved(id.clone()));
    session
        .call(
            wire::Command::Connect(wire::Connect {
                device_id: id,
                timeout_ms: 30_000,
            }),
            false,
            wait,
        )
        .map_err(|error| Failure {
            error,
            bonded,
            partial: None,
        })?;
    Ok(Outcome::Connected(subject))
}
fn job(
    session: &Session,
    command: wire::Command,
    subject: Subject,
    wait: &Wait,
) -> Result<Outcome, Failure> {
    let result = session.call(command, false, wait);
    let responses = match &result {
        Ok(rows) => rows,
        Err(error) => &error.responses,
    };
    let rows = responses
        .iter()
        .filter(|r| !r.done())
        .map(|r| {
            r.decode::<SettingOutcomeChunk>()
                .map(|r| (r.setting, r.outcome))
        })
        .collect::<Result<Vec<_>, _>>()?;
    match result {
        Ok(responses) => Ok(Outcome::Job {
            subject,
            rows,
            counts: Some(last::<SettingsSummary>(&responses)?.into()),
        }),
        Err(error) => {
            let counts = error
                .wire
                .as_ref()
                .and_then(|w| w.details.clone())
                .and_then(|d| match d {
                    ErrorDetails::Settings(s) => Some(JobCounts::from(s)),
                    _ => None,
                });
            Err(Failure {
                error,
                bonded: None,
                partial: Some(Box::new(Outcome::Job {
                    subject,
                    rows,
                    counts,
                })),
            })
        }
    }
}
