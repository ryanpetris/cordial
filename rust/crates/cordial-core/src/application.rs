//! Serial command ownership and asynchronous Bluetooth operations.
use crate::{
    bluetooth::{Bluetooth, Event},
    compact::Record,
    control::{EmitError, Readiness, Session},
    devices::{self, Peer},
    link::LinkId,
    manager::Manager,
    settings::{MAX_FEATURES, MAX_RECORDS, MAX_SAVED, Saved},
    storage::{Preferences, RecordStore},
};
use alloc::{boxed::Box, format, string::String, vec::Vec};
use cordial_protocol::{
    self as protocol,
    errors::ErrorCode as Error,
    identifiers::*,
    messages::{self, Command, Request, WireError},
    payloads::*,
    settings::{SettingKey, SettingOutcome},
};
use serde_json::{Value, json};

pub struct Build {
    pub profile: messages::BuildProfile,
    pub version: &'static str,
    pub hardware: &'static str,
    pub default_adapter_name: &'static str,
    pub digest: &'static str,
    pub radio_backend: &'static str,
    pub adapter_id: String,
    pub boot_id: String,
    /// Production composition supplies None and does not link the entry function.
    pub bootloader: Option<Bootloader>,
}
pub struct Bootloader {
    pub mode: &'static str,
    pub enter: fn() -> !,
}
struct Candidate {
    peer: Peer,
    address: Peer,
    record: messages::Candidate,
    dirty: bool,
}
struct Prompt {
    value: messages::Prompt,
    deadline: u64,
    dirty: bool,
    answered: bool,
}
#[derive(Clone, Copy)]
enum Wait {
    Connect,
    Disconnect,
    Unpair,
    Block,
}
enum Work {
    Information {
        link: LinkId,
        deadline: u64,
    },
    #[cfg(feature = "development")]
    Storage {
        path: String,
        listing: bool,
        cursor: u32,
        generation: u64,
    },
    Finished(Result<Value, WireError>),
    Devices {
        revision: u64,
        filter: messages::Filter,
        cursor: usize,
        count: usize,
    },
    Settings {
        revision: u64,
        records: Box<[Record]>,
        cursor: usize,
        state: SettingsState,
        error: Option<Error>,
    },
    Features {
        revision: u64,
        records: Vec<crate::compact::FeatureEntry>,
        cursor: usize,
        state: SettingsState,
        error: Option<Error>,
    },
    Job {
        apply: bool,
        cursor: usize,
        outcomes: [usize; 6],
        detached: Option<Vec<(Record, SettingOutcome)>>,
        terminal: Option<Error>,
    },
    Scan {
        token: u64,
        classic: bool,
        ble: bool,
        deadline: Option<u64>,
        stopped: bool,
        error: Option<Error>,
    },
    Pair {
        link: Option<LinkId>,
        address: Peer,
        expected: Option<Peer>,
        cleanup: Option<Peer>,
        deadline: u64,
        name: Box<str>,
        cancelling: Option<Error>,
        duplicate: Option<DeviceId>,
    },
    Wait {
        link: LinkId,
        kind: Wait,
        deadline: u64,
        cancelling: Option<Error>,
    },
}
struct Operation {
    id: Option<RequestId>,
    generation: u64,
    command: &'static str,
    slot: Option<usize>,
    reference: String,
    work: Work,
}
pub struct Application<'a> {
    pub manager: Manager,
    pub serial: Session<'a>,
    build: Build,
    operations: Vec<Operation>,
    candidates: Vec<Candidate>,
    candidate_seq: u64,
    scan_seq: u64,
    radio_scan: Option<(u64, bool, bool)>,
    prompt_seq: u64,
    prompt: Option<Prompt>,
    truncated: bool,
    startup: Readiness,
    reboot_at: Option<u64>,
}
fn failure(code: Error) -> WireError {
    WireError {
        code,
        details: match code {
            Error::StorageFull => Some(ErrorDetails::Mutation(MutationDetails {
                outcome: StorageOutcome::NotSaved,
            })),
            _ => None,
        },
    }
}
fn mutation_failure(code: Error, uncertain: bool) -> WireError {
    if code == Error::StorageFailed && uncertain {
        WireError {
            code,
            details: Some(ErrorDetails::Mutation(MutationDetails {
                outcome: StorageOutcome::Unknown,
            })),
        }
    } else {
        failure(code)
    }
}
fn capacity_failure(reason: CapacityReason) -> WireError {
    WireError {
        code: Error::Capacity,
        details: Some(ErrorDetails::Capacity(CapacityDetails { reason })),
    }
}
fn settings_failure(error: crate::settings::Error) -> WireError {
    mutation_failure(
        error.code(),
        error == crate::settings::Error::StorageUnknown,
    )
}
impl<'a> Application<'a> {
    pub fn new(input: &'a mut [u8], build: Build) -> Self {
        Self {
            manager: Manager::default(),
            serial: Session::new(input),
            build,
            operations: Vec::new(),
            candidates: Vec::new(),
            candidate_seq: 0,
            scan_seq: 0,
            radio_scan: None,
            prompt_seq: 0,
            prompt: None,
            truncated: false,
            startup: Readiness::Starting,
            reboot_at: None,
        }
    }
    pub fn capabilities<B: Bluetooth>(&self, radio: &B) -> messages::Capabilities {
        use messages::Capability::*;
        let radio = radio.capabilities();
        let mut caps = Vec::new();
        if radio.classic {
            caps.push(Classic);
        }
        if radio.ble {
            caps.push(Ble);
        }
        if cfg!(feature = "development")
            && self.build.profile == messages::BuildProfile::Development
        {
            if self.build.bootloader.is_some() {
                caps.push(Debug);
            }
            caps.push(StorageManagement);
        }
        messages::Capabilities(caps)
    }
    pub fn status<B: Bluetooth>(&self, radio: &B, now: u64) -> messages::Status {
        let caps = radio.capabilities();
        messages::Status {
            gatt_writes: if cfg!(feature = "development") {
                radio.gatt_writes()
            } else {
                None
            },
            authentication_failure: if cfg!(feature = "development") {
                radio.authentication_failure()
            } else {
                None
            },
            protocol: protocol::PROTOCOL_VERSION,
            firmware_version: self.build.version.into(),
            hardware_config: self.build.hardware.into(),
            hardware_digest: self.build.digest.into(),
            radio_backend: self.build.radio_backend.into(),
            adapter_id: self.build.adapter_id.clone(),
            boot_id: self.build.boot_id.clone(),
            session_id: format!("{}-{}", self.build.boot_id, self.serial.generation()),
            build_profile: self.build.profile,
            limits: messages::Limits {
                max_line_bytes: protocol::MAX_LINE_BYTES,
                max_pending_requests: devices::PENDING_REQUESTS,
                saved_devices: (self.manager.devices.iter().flatten().count()
                    + self.manager.available_bytes / 8192)
                    .max(1),
                active_connections: devices::ACTIVE_CONNECTIONS,
                scan_candidates: devices::SCAN_CANDIDATES,
                hidpp_settings: MAX_RECORDS,
                hidpp_saved_settings: MAX_SAVED,
                hidpp_sensors: 2,
                hidpp_firmware_entities: 2,
                hidpp_setting_choices: crate::settings::MAX_CHOICES,
                hidpp_features: MAX_FEATURES,
            },
            counts: messages::Counts {
                saved: self.manager.devices.iter().flatten().count(),
                preferred_enabled: self
                    .manager
                    .devices
                    .iter()
                    .flatten()
                    .filter(|d| d.policy.enabled)
                    .count(),
                enabled: self
                    .manager
                    .devices
                    .iter()
                    .flatten()
                    .filter(|d| d.effective_enabled)
                    .count(),
                paired: self
                    .manager
                    .devices
                    .iter()
                    .flatten()
                    .filter(|d| d.pairing_state == PairingState::Paired)
                    .count(),
                connected: self
                    .manager
                    .devices
                    .iter()
                    .flatten()
                    .filter(|d| d.state == ConnectionState::Connected)
                    .count(),
            },
            capacity: {
                let mut capacity = self.manager.capacity(caps);
                if self.pairing() {
                    for p in &mut capacity.pairing {
                        p.available = false;
                        p.reason = Some(cordial_protocol::errors::PairUnavailable::PairingActive);
                    }
                }
                capacity
            },
            revision: self.manager.revision,
            host_platform: self.manager.preference.host_platform,
            name: self.adapter_name().into(),
            monitor: self.serial.monitoring(),
            radio_ready: self.manager.radio_ready,
            storage_ready: self.manager.storage_ready,
            heartbeat: messages::Heartbeat {
                interval_ms: protocol::HEARTBEAT_INTERVAL_MS,
                timeout_ms: protocol::HEARTBEAT_TIMEOUT_MS,
                remaining_ms: self.serial.remaining_ms(now),
            },
            pending: self
                .operations
                .iter()
                .filter_map(|o| {
                    o.id.map(|id| messages::PendingRequest {
                        id,
                        cmd: o.command.into(),
                        device_id: o
                            .slot
                            .and_then(|s| self.manager.devices[s].as_ref())
                            .map(|d| d.policy.device_id()),
                        candidate_id: (o.command == "pairing.start" && !o.reference.is_empty())
                            .then(|| CandidateId(o.reference.clone())),
                    })
                })
                .collect(),
        }
    }
    /// Saves first-connection setup progress for connected devices. A failed
    /// save leaves the remaining steps to the device's next connection.
    /// `write_uncertain` describes the last requested write, so these
    /// background saves leave it as they found it.
    async fn setup<S: RecordStore>(&mut self, store: &mut S, now: u64) {
        if !self.manager.storage_ready {
            return;
        }
        while let Some((index, slot, policy, setup)) = self.manager.setup() {
            let uncertain = self.manager.write_uncertain;
            let saved = self.manager.policy(slot, policy, store).await;
            self.manager.write_uncertain = uncertain;
            if saved.is_ok() {
                self.manager.devices[slot].as_mut().unwrap().setup = setup;
                self.changed(slot, "device.changed", now);
            } else if let Some(c) = &mut self.manager.connections[index] {
                c.setup_failed = true;
            }
        }
    }
    fn changed(&mut self, slot: usize, event: &str, now: u64) {
        let mut revision = self.manager.changed();
        let catalog_changed = self.manager.devices[slot].as_mut().is_some_and(|d| {
            // A grouped feature observation uses one invalidation watermark so
            // it fits even the smallest board's two optional output slots.
            let changed = core::mem::take(&mut d.catalog.catalog_changed)
                || d.catalog
                    .records()
                    .iter()
                    .filter(|r| r.changed.get())
                    .take(2)
                    .count()
                    > 1;
            if changed {
                d.settings_revision = revision;
            }
            changed
        });
        self.serial.revision(revision);
        let monitor = self.serial.monitoring();
        if monitor && let Some(device) = self.manager.record(slot) {
            if event == "device.disconnected" {
                let reason = if device.blocked || device.reconnect == Reconnect::Paused {
                    DisconnectReason::Requested
                } else {
                    DisconnectReason::Unknown
                };
                let _ = self.serial.event(
                    event,
                    None,
                    DeviceDisconnected {
                        revision,
                        device,
                        reason,
                    },
                    true,
                    now,
                );
            } else {
                let _ =
                    self.serial
                        .event(event, None, DeviceSnapshot { revision, device }, true, now);
            }
        }
        if let Some(device) = self.manager.devices[slot].as_mut() {
            let id = monitor.then(|| device.policy.device_id());
            for row in device.catalog.take_changed() {
                if catalog_changed {
                    continue;
                }
                revision = revision.saturating_add(1).min(protocol::MAX_REVISION);
                self.serial.revision(revision);
                if let Some(id) = &id {
                    let _ = self.serial.event(
                        "hidpp.setting.changed",
                        None,
                        messages::SettingChunk {
                            revision,
                            device_id: id.clone(),
                            setting: row.wire(),
                        },
                        true,
                        now,
                    );
                }
            }
        }
        self.manager.revision = revision;
    }
    fn information(&self, slot: usize) -> Value {
        let d = self.manager.devices[slot].as_ref().unwrap();
        json!(protocol::info::DeviceInfo {
            revision: self.manager.revision,
            device_id: d.policy.device_id(),
            fields: d.catalog.info.snapshot()
        })
    }
    fn information_changes(&mut self, now: u64) {
        for slot in 0..self.manager.devices.len() {
            let Some(d) = self.manager.devices[slot].as_mut() else {
                continue;
            };
            let fields = d.catalog.info.changes();
            if fields.is_empty() {
                continue;
            }
            let device_id = d.policy.device_id();
            let revision = self.manager.changed();
            self.serial.revision(revision);
            if self.serial.monitoring() {
                let _ = self.serial.event(
                    "device.info.changed",
                    None,
                    protocol::info::DeviceInfo {
                        revision,
                        device_id,
                        fields,
                    },
                    true,
                    now,
                );
            }
        }
    }
    fn pairing(&self) -> bool {
        self.operations
            .iter()
            .any(|o| matches!(o.work, Work::Pair { .. }))
    }
    fn device_busy(&self, slot: usize, disruptive: bool) -> bool {
        self.operations.iter().any(|o| {
            o.slot == Some(slot)
                && match &o.work {
                    Work::Finished(_)
                    | Work::Devices { .. }
                    | Work::Settings { .. }
                    | Work::Features { .. } => false,
                    Work::Job { .. }
                    | Work::Information { .. }
                    | Work::Wait {
                        kind: Wait::Connect,
                        ..
                    } if disruptive => false,
                    _ => true,
                }
        })
    }
    fn settings_busy(&self, slot: usize) -> bool {
        self.manager
            .link_for(slot)
            .and_then(|id| self.manager.connection(id))
            .and_then(|c| c.runtime.as_ref())
            .is_some_and(|l| l.busy())
    }
    fn state(&self, slot: usize) -> (SettingsState, Option<Error>) {
        let d = self.manager.record(slot).unwrap();
        (d.settings_state, d.settings_error)
    }
    fn setting(&self, slot: usize, key: SettingKey) -> Result<Value, Error> {
        let d = self.manager.devices[slot].as_ref().ok_or(Error::NotFound)?;
        let row = d
            .catalog
            .records()
            .iter()
            .find(|r| r.metadata.key == key)
            .ok_or(Error::NotFound)?;
        Ok(json!(messages::SettingChunk {
            revision: self.manager.revision,
            device_id: d.policy.device_id(),
            setting: row.wire()
        }))
    }
    fn start_job(
        &mut self,
        slot: usize,
        apply: bool,
        key: Option<SettingKey>,
        explicit: bool,
        now: u64,
    ) -> Result<(), Error> {
        let id = self.manager.link_for(slot).ok_or(Error::NotConnected)?;
        let c = self.manager.connections[id.slot as usize].as_mut().unwrap();
        let runtime = c
            .runtime
            .as_mut()
            .filter(|_| !c.closing)
            .ok_or(Error::NotConnected)?;
        runtime.start_settings(
            &mut self.manager.devices[slot].as_mut().unwrap().catalog,
            apply,
            key,
            explicit,
            now,
        )
    }
    /// Feed/dispatch are adjacent in the owner loop. On Full, retry this same
    /// request before accepting more input; accepted mutations own an Operation.
    pub async fn dispatch<S: RecordStore, B: Bluetooth>(
        &mut self,
        request: &Request,
        store: &mut S,
        radio: &mut B,
        now: u64,
    ) -> Result<(), EmitError> {
        let id = request.id;
        if matches!(
            request.command,
            Command::StorageList(_) | Command::StorageRead(_)
        ) && (!cfg!(feature = "development")
            || self.build.profile != messages::BuildProfile::Development)
        {
            return self.serial.error(id, Error::UnknownCommand, now);
        }
        match &request.command {
            Command::Protocol(_) => return self.serial.protocol(id, now),
            Command::Capabilities(_) => {
                return self
                    .serial
                    .response(id, true, self.capabilities(radio), now);
            }
            Command::Status(_) => {
                if self.manager.storage_ready {
                    match store.available().await {
                        Ok(bytes) => self.manager.available_bytes = bytes,
                        Err(_) => {
                            self.manager.storage_ready = false;
                            self.manager.available_bytes = 0;
                        }
                    }
                }
                return self.serial.response(id, true, self.status(radio, now), now);
            }
            Command::Ready(_) => {
                return self
                    .serial
                    .ready(id, self.startup, &self.status(radio, now), now);
            }
            Command::Heartbeat(_) => return self.serial.heartbeat(id, now),
            Command::Monitor(a) => return self.serial.monitor(id, a.enabled, now),
            Command::Bootloader(_) => {
                let Some(boot) = &self.build.bootloader else {
                    return self.serial.error(id, Error::UnknownCommand, now);
                };
                if !self.operations.is_empty()
                    || self
                        .manager
                        .connections
                        .iter()
                        .flatten()
                        .any(|c| c.device.is_none())
                {
                    return self.serial.error(id, Error::Busy, now);
                }
                self.serial.response(
                    id,
                    true,
                    BootloaderResult {
                        rebooting: true,
                        mode: if boot.mode == "bootsel" {
                            BootloaderMode::Bootsel
                        } else {
                            BootloaderMode::Download
                        },
                    },
                    now,
                )?;
                self.serial.stop_commands();
                for source in 0..devices::ACTIVE_CONNECTIONS {
                    self.manager.forward.remove(source);
                }
                self.reboot_at = Some(now.saturating_add(250));
                return Ok(());
            }
            Command::PairReply(a) => return self.reply(id, a, radio, now),
            Command::Cancel(a) => return self.cancel(id, a.request_id, radio, now),
            _ => {}
        }
        if self.operations.len() >= devices::PENDING_REQUESTS {
            return self.serial.error(id, Error::Busy, now);
        }
        let mut op = Operation {
            id: Some(id),
            generation: self.serial.generation(),
            command: request.command.name(),
            slot: None,
            reference: String::new(),
            work: Work::Finished(Ok(Value::Null)),
        };
        match self
            .prepare(&request.command, &mut op, store, radio, now)
            .await
        {
            Ok(work) => op.work = work,
            Err(error) => op.work = Work::Finished(Err(error)),
        }
        self.operations.push(op);
        Ok(())
    }
}

impl Application<'_> {
    async fn prepare<S: RecordStore, B: Bluetooth>(
        &mut self,
        command: &Command,
        op: &mut Operation,
        store: &mut S,
        radio: &mut B,
        now: u64,
    ) -> Result<Work, WireError> {
        let done = |value| Ok(Work::Finished(Ok(value)));
        match command {
            #[cfg(feature = "development")]
            Command::StorageList(args) | Command::StorageRead(args) => {
                if self.build.profile != messages::BuildProfile::Development {
                    return Err(failure(Error::UnknownCommand));
                }
                let generation = store
                    .generation()
                    .await
                    .map_err(|_| failure(Error::StorageFailed))?;
                return Ok(Work::Storage {
                    path: args.path.clone(),
                    listing: matches!(command, Command::StorageList(_)),
                    cursor: 0,
                    generation,
                });
            }
            Command::Devices(args) => {
                return Ok(Work::Devices {
                    revision: self.manager.revision,
                    filter: args.filter,
                    count: 0,
                    cursor: 0,
                });
            }
            Command::Scan(args) => {
                if !self.serial.present() {
                    return Err(failure(Error::HeartbeatRequired));
                }
                if !self.manager.radio_ready {
                    return Err(failure(Error::RadioUnavailable));
                }
                if self
                    .operations
                    .iter()
                    .any(|o| matches!(o.work, Work::Scan { .. }))
                {
                    return Err(failure(Error::Busy));
                }
                let caps = radio.capabilities();
                let (classic, ble) = match args.transport {
                    ScanTransport::Both => (caps.classic, caps.ble),
                    ScanTransport::Classic => (true, false),
                    ScanTransport::Ble => (false, true),
                };
                if (classic && !caps.classic) || (ble && !caps.ble) || (!classic && !ble) {
                    return Err(failure(Error::UnsupportedTransport));
                }
                self.scan_seq = self.scan_seq.checked_add(1).ok_or(Error::InternalError)?;
                radio.scan(self.scan_seq, classic, ble)?;
                self.radio_scan = Some((self.scan_seq, classic, ble));
                self.candidates.clear();
                self.truncated = false;
                return Ok(Work::Scan {
                    token: self.scan_seq,
                    classic,
                    ble,
                    deadline: (args.duration_ms != 0)
                        .then(|| now.saturating_add(args.duration_ms.into())),
                    stopped: false,
                    error: None,
                });
            }
            Command::Pair(args) => {
                if !self.serial.present() {
                    return Err(failure(Error::HeartbeatRequired));
                }
                if self
                    .operations
                    .iter()
                    .any(|o| matches!(o.work, Work::Pair { .. }))
                {
                    return Err(failure(Error::Busy));
                }
                let c = self
                    .candidates
                    .iter()
                    .find(|c| c.record.candidate_id == args.candidate_id)
                    .ok_or(Error::CandidateExpired)?;
                if !self.manager.storage_ready {
                    return Err(failure(Error::StorageFailed));
                }
                if !self.manager.radio_ready {
                    return Err(failure(Error::RadioUnavailable));
                }
                if !radio.capabilities().supports(c.peer.transport) {
                    return Err(failure(Error::UnsupportedTransport));
                }
                if radio.bond_capacity(c.peer.transport) == 0 {
                    return Err(capacity_failure(CapacityReason::SetupCapacity));
                }
                let slot = self.manager.peer(c.peer);
                if let Some(slot) = slot {
                    if self.manager.devices[slot].as_ref().unwrap().policy.blocked {
                        return Err(failure(Error::Blocked));
                    }
                    if self.operations.iter().any(|o| {
                        o.slot == Some(slot)
                            && !matches!(
                                o.work,
                                Work::Finished(_)
                                    | Work::Devices { .. }
                                    | Work::Settings { .. }
                                    | Work::Features { .. }
                                    | Work::Job { .. }
                            )
                    }) {
                        return Err(failure(Error::Busy));
                    }
                }
                self.manager.available_bytes = store
                    .available()
                    .await
                    .map_err(|_| failure(Error::StorageFailed))?;
                if self.manager.available_bytes
                    < crate::bonds::MAINTENANCE_BYTES + crate::bonds::PAIR_BYTES
                {
                    return Err(capacity_failure(CapacityReason::StorageFull));
                }
                // The selected link and background setup can free a slot. Keep
                // unrelated established input links and explicit operations intact.
                let mut closing = Vec::new();
                for c in self.manager.connections.iter().flatten() {
                    if c.device == slot && slot.is_some() || c.runtime.is_none() {
                        if self.operations.iter().any(|o| {
                            matches!(o.work,
                            Work::Wait { link, .. } if link == c.id)
                        }) {
                            return Err(failure(Error::Busy));
                        }
                        closing.push(c.id);
                    }
                }
                if self.manager.connections.iter().all(Option::is_some) && closing.is_empty() {
                    return Err(capacity_failure(CapacityReason::ConnectionsFull));
                }
                let address = c.address;
                let expected = slot.map(|_| c.peer);
                let name = c.record.name.as_deref().unwrap_or("").into();
                op.slot = slot;
                op.reference = args.candidate_id.0.clone();
                for link in closing {
                    self.close(link, None, radio, now);
                }
                self.prompt = None;
                return Ok(Work::Pair {
                    link: None,
                    address,
                    expected,
                    cleanup: None,
                    deadline: now.saturating_add(args.timeout_ms.into()),
                    name,
                    cancelling: None,
                    duplicate: None,
                });
            }
            Command::Name(args) => {
                let name = args
                    .name
                    .as_deref()
                    .map(|value| {
                        protocol::adapter_name(value).ok_or_else(|| failure(Error::InvalidArgs))
                    })
                    .transpose()?;
                if !self.manager.storage_ready {
                    return Err(mutation_failure(Error::StorageFailed, false));
                }
                let changed = match name {
                    Some(name) => name != self.adapter_name(),
                    None => self.manager.preference.name.is_some(),
                };
                if changed {
                    self.manager
                        .name(name, store)
                        .await
                        .map_err(|code| mutation_failure(code, self.manager.write_uncertain))?;
                    self.adapter_changed(now);
                }
                return done(json!(AdapterSettings {
                    revision: self.manager.revision,
                    name: self.adapter_name().into(),
                    host_platform: self.manager.preference.host_platform,
                }));
            }
            Command::Platform(args) => {
                let changed = self.manager.preference.host_platform != args.platform;
                if self.pairing() {
                    return Err(failure(Error::Busy));
                }
                self.manager
                    .platform(args.platform, store)
                    .await
                    .map_err(|code| mutation_failure(code, self.manager.write_uncertain))?;
                if changed {
                    self.adapter_changed(now);
                    for slot in 0..self.manager.devices.len() {
                        if self.manager.devices[slot].as_ref().is_some_and(|d| {
                            d.state == ConnectionState::Connected && d.policy.hidpp_enabled
                        }) {
                            self.changed(slot, "device.changed", now);
                        }
                    }
                }
                self.serial.revision(self.manager.revision);
                let result = json!(AdapterSettings {
                    revision: self.manager.revision,
                    name: self.adapter_name().into(),
                    host_platform: args.platform
                });
                return done(result);
            }
            _ => {}
        }
        let reference = match command {
            Command::DeviceInfo(a)
            | Command::DeviceInfoRefresh(a)
            | Command::Info(a)
            | Command::Disconnect(a)
            | Command::Unpair(a)
            | Command::Features(a)
            | Command::Settings(a)
            | Command::SettingsRefresh(a)
            | Command::SettingsApply(a) => &a.device_id,
            Command::Connect(a) => &a.device_id,
            Command::Hidpp(a) | Command::DeviceEnabled(a) => &a.device_id,
            Command::DeviceTrusted(a) => &a.device_id,
            Command::DeviceBlocked(a) => &a.device_id,
            Command::SettingsGet(a) | Command::SettingsForget(a) => &a.device_id,
            Command::SettingsSet(a) => &a.device_id,
            _ => return Err(failure(Error::UnknownCommand)),
        };
        op.reference = reference.0.clone();
        let slot = match self.manager.find(reference) {
            Err(Error::NotFound) if matches!(command, Command::Unpair(_)) => {
                return done(json!(DeviceRemoved {
                    device_id: reference.clone(),
                    removed: false
                }));
            }
            result => result?,
        };
        op.slot = Some(slot);
        if matches!(command, Command::Connect(_))
            && self.manager.devices[slot].as_ref().unwrap().pairing_state
                == PairingState::NeedsPairing
        {
            return Err(failure(Error::PairingRequired));
        }
        let read = matches!(
            command,
            Command::Info(_)
                | Command::DeviceInfo(_)
                | Command::Devices(_)
                | Command::Features(_)
                | Command::Settings(_)
                | Command::SettingsGet(_)
        );
        let disruptive = matches!(
            command,
            Command::Disconnect(_)
                | Command::Unpair(_)
                | Command::DeviceBlocked(messages::DeviceBlocked { blocked: true, .. })
                | Command::Hidpp(_)
        );
        if !read && self.device_busy(slot, disruptive) {
            return Err(failure(Error::Busy));
        }
        // An unresolved pairing may still reveal this retained identity after
        // native bonding. Do not delete its policy or keys during that window.
        if matches!(
            command,
            Command::Unpair(_)
                | Command::DeviceEnabled(_)
                | Command::DeviceBlocked(_)
                | Command::DeviceTrusted(_)
                | Command::Hidpp(_)
                | Command::SettingsSet(_)
                | Command::SettingsForget(_)
        ) && self.pairing()
        {
            return Err(failure(Error::Busy));
        }
        let deadline = now.saturating_add(15_000);
        match command {
            Command::DeviceInfo(_) => done(self.information(slot)),
            Command::DeviceInfoRefresh(_) => {
                let link = self.manager.link_for(slot).ok_or(Error::NotConnected)?;
                if self.manager.devices[slot].as_ref().unwrap().state != ConnectionState::Connected
                {
                    return Err(failure(Error::NotConnected));
                }
                if self.settings_busy(slot) {
                    return Err(failure(Error::Busy));
                }
                radio.refresh_info(link)?;
                if self.manager.devices[slot]
                    .as_ref()
                    .unwrap()
                    .policy
                    .hidpp_enabled
                {
                    let c = self.manager.connections[link.slot as usize]
                        .as_mut()
                        .unwrap();
                    c.runtime
                        .as_mut()
                        .ok_or(Error::NotConnected)?
                        .start_information(
                            &mut self.manager.devices[slot].as_mut().unwrap().catalog,
                            now,
                        )?;
                }
                Ok(Work::Information {
                    link,
                    deadline: now.saturating_add(120_000),
                })
            }
            Command::Info(_) => done(json!(DeviceSnapshot {
                revision: self.manager.revision,
                device: self.manager.record(slot).unwrap()
            })),
            Command::Connect(args) => {
                if self.pairing() {
                    return Err(failure(Error::Busy));
                }
                let deadline = now.saturating_add(args.timeout_ms.into());
                match self
                    .manager
                    .connect(slot, true, now.saturating_add(30_000), radio)
                    .map_err(|code| {
                        if code == Error::Capacity {
                            capacity_failure(
                                if self.manager.devices[slot]
                                    .as_ref()
                                    .unwrap()
                                    .effective_enabled
                                {
                                    CapacityReason::ConnectionsFull
                                } else {
                                    CapacityReason::EnabledFull
                                },
                            )
                        } else {
                            failure(code)
                        }
                    })? {
                    None => done(json!(DeviceResult {
                        device: self.manager.record(slot).unwrap()
                    })),
                    Some(link) => {
                        self.changed(slot, "device.changed", now);
                        Ok(Work::Wait {
                            link,
                            kind: Wait::Connect,
                            deadline,
                            cancelling: None,
                        })
                    }
                }
            }
            Command::Disconnect(_) | Command::Unpair(_) => {
                self.cancel_connect(slot, radio, now);
                let connected = self.manager.devices[slot].as_ref().unwrap().state
                    == ConnectionState::Connected;
                self.manager.disconnect(slot, radio)?;
                self.changed(
                    slot,
                    if connected {
                        "device.disconnected"
                    } else {
                        "device.changed"
                    },
                    now,
                );
                if let Some(link) = self.manager.link_for(slot) {
                    Ok(Work::Wait {
                        link,
                        kind: if matches!(command, Command::Unpair(_)) {
                            Wait::Unpair
                        } else {
                            Wait::Disconnect
                        },
                        deadline,
                        cancelling: None,
                    })
                } else if matches!(command, Command::Unpair(_)) {
                    self.remove(slot, store, radio, now).await?;
                    done(json!(DeviceRemoved {
                        device_id: reference.clone(),
                        removed: true
                    }))
                } else {
                    done(json!(DeviceResult {
                        device: self.manager.record(slot).unwrap()
                    }))
                }
            }
            Command::DeviceEnabled(_)
            | Command::DeviceTrusted(_)
            | Command::DeviceBlocked(_)
            | Command::Hidpp(_) => {
                let d = self.manager.devices[slot].as_ref().unwrap();
                let mut policy = d.policy.clone();
                let mut setup = d.setup;
                match command {
                    Command::DeviceEnabled(a) => policy.enabled = a.enabled,
                    Command::DeviceTrusted(a) => policy.trusted = a.trusted,
                    Command::DeviceBlocked(a) => policy.blocked = a.blocked,
                    Command::Hidpp(a) => {
                        // A user choice settles setup's HID++ detection.
                        policy.hidpp_enabled = a.enabled;
                        setup.hidpp = true;
                        policy.setup_pending &= !setup.complete();
                    }
                    _ => unreachable!(),
                }
                self.manager
                    .policy(slot, policy, store)
                    .await
                    .map_err(|code| {
                        if code == Error::Capacity
                            && matches!(
                                command,
                                Command::DeviceEnabled(messages::DeviceEnabled {
                                    enabled: true,
                                    ..
                                }) | Command::DeviceBlocked(messages::DeviceBlocked {
                                    blocked: false,
                                    ..
                                })
                            )
                        {
                            WireError {
                                code,
                                details: Some(ErrorDetails::Capacity(CapacityDetails {
                                    reason: CapacityReason::EnabledFull,
                                })),
                            }
                        } else {
                            mutation_failure(code, self.manager.write_uncertain)
                        }
                    })?;
                self.manager.devices[slot].as_mut().unwrap().setup = setup;
                self.changed(slot, "device.changed", now);
                if matches!(
                    command,
                    Command::DeviceBlocked(messages::DeviceBlocked { blocked: true, .. })
                        | Command::DeviceEnabled(messages::DeviceEnabled { enabled: false, .. })
                ) && let Some(link) = self.manager.link_for(slot)
                {
                    self.cancel_connect(slot, radio, now);
                    self.close(link, None, radio, now);
                    self.manager.sync_bonds(store, radio).await?;
                    return Ok(Work::Wait {
                        link,
                        kind: Wait::Block,
                        deadline,
                        cancelling: None,
                    });
                }
                self.manager.sync_bonds(store, radio).await?;
                done(json!(DeviceResult {
                    device: self.manager.record(slot).unwrap()
                }))
            }
            Command::Settings(_) | Command::Features(_) => {
                let (state, error) = self.state(slot);
                let d = self.manager.devices[slot].as_ref().unwrap();
                if matches!(command, Command::Features(_)) {
                    if d.state != ConnectionState::Connected {
                        return Err(failure(Error::NotConnected));
                    }
                    if d.catalog.features().is_empty() && state != SettingsState::Ready {
                        return Err(failure(Error::SettingsUnavailable));
                    }
                    Ok(Work::Features {
                        revision: self.manager.revision,
                        records: d.catalog.features().to_vec(),
                        cursor: 0,
                        state,
                        error,
                    })
                } else {
                    if d.catalog.records().is_empty() && state != SettingsState::Ready {
                        return Err(failure(Error::SettingsUnavailable));
                    }
                    Ok(Work::Settings {
                        revision: self.manager.revision,
                        records: d.catalog.snapshot().map_err(|e| e.code())?,
                        cursor: 0,
                        state,
                        error,
                    })
                }
            }
            Command::SettingsGet(a) => {
                if self.manager.devices[slot].as_ref().unwrap().state != ConnectionState::Connected
                {
                    return Err(failure(Error::NotConnected));
                }
                done(self.setting(slot, SettingKey::from_name(&a.key).ok_or(Error::NotFound)?)?)
            }
            Command::SettingsSet(_) | Command::SettingsForget(_) => {
                if self.settings_busy(slot) {
                    return Err(failure(Error::Busy));
                }
                if !self.manager.storage_ready {
                    return Err(failure(Error::StorageFailed));
                }
                let (key, value) = match command {
                    Command::SettingsSet(a) => (&a.key, Some(a.value.clone())),
                    Command::SettingsForget(a) => (&a.key, None),
                    _ => unreachable!(),
                };
                let key = SettingKey::from_name(key).ok_or(Error::NotFound)?;
                let d = self.manager.devices[slot].as_mut().unwrap();
                if value.is_some() && d.state != ConnectionState::Connected {
                    return Err(failure(Error::NotConnected));
                }
                let mut prefs = Preferences {
                    store,
                    device: d.policy.id,
                };
                let apply = if let Some(value) = value {
                    d.catalog
                        .set(key, value, &mut prefs)
                        .await
                        .map_err(|error| {
                            if error == crate::settings::Error::StorageUnknown {
                                self.manager.storage_ready = false;
                            }
                            settings_failure(error)
                        })?
                        == Saved::Apply
                } else {
                    d.catalog.forget(key, &mut prefs).await.map_err(|error| {
                        if error == crate::settings::Error::StorageUnknown {
                            self.manager.storage_ready = false;
                        }
                        settings_failure(error)
                    })?;
                    false
                };
                if apply {
                    self.start_job(slot, true, Some(key), false, now)?;
                }
                self.changed(slot, "device.changed", now);
                let result = self.setting(slot, key)?;
                done(result)
            }
            Command::SettingsRefresh(_) | Command::SettingsApply(_) => {
                self.start_job(
                    slot,
                    matches!(command, Command::SettingsApply(_)),
                    None,
                    true,
                    now,
                )?;
                self.changed(slot, "device.changed", now);
                Ok(Work::Job {
                    apply: matches!(command, Command::SettingsApply(_)),
                    cursor: 0,
                    outcomes: [0; 6],
                    detached: None,
                    terminal: None,
                })
            }
            _ => Err(failure(Error::UnknownCommand)),
        }
    }
    async fn remove<S: RecordStore, B: Bluetooth>(
        &mut self,
        slot: usize,
        store: &mut S,
        radio: &mut B,
        now: u64,
    ) -> Result<(), Error> {
        let id = self.manager.devices[slot]
            .as_ref()
            .ok_or(Error::NotFound)?
            .policy
            .device_id();
        self.manager.unpair(slot, store, radio).await?;
        self.manager.changed();
        self.serial.revision(self.manager.revision);
        let _ = self.serial.event(
            "device.unpaired",
            None,
            DeviceUnpaired {
                revision: self.manager.revision,
                device_id: id,
            },
            true,
            now,
        );
        Ok(())
    }
}

impl Application<'_> {
    fn adapter_name(&self) -> &str {
        self.manager
            .preference
            .name
            .as_deref()
            .unwrap_or(self.build.default_adapter_name)
    }
    fn adapter_changed(&mut self, now: u64) {
        let revision = self.manager.changed();
        self.serial.revision(revision);
        let _ = self.serial.event(
            "adapter.changed",
            None,
            AdapterSettings {
                revision,
                host_platform: self.manager.preference.host_platform,
                name: self.adapter_name().into(),
            },
            true,
            now,
        );
    }
    fn close<B: Bluetooth>(&mut self, link: LinkId, error: Option<Error>, radio: &mut B, now: u64) {
        let before = self
            .manager
            .connection(link)
            .and_then(|c| c.device)
            .map(|slot| (slot, self.manager.devices[slot].as_ref().unwrap().state));
        self.manager.close(link, error, radio);
        if let Some((slot, state)) = before
            && self.manager.devices[slot].as_ref().unwrap().state != state
        {
            self.changed(
                slot,
                if state == ConnectionState::Connected {
                    "device.disconnected"
                } else {
                    "device.changed"
                },
                now,
            );
        }
    }
    fn stop<B: Bluetooth>(&mut self, index: usize, reason: Error, radio: &mut B, now: u64) {
        let pairing = matches!(self.operations[index].work, Work::Pair { .. });
        let mut closing = None;
        match &mut self.operations[index].work {
            #[cfg(feature = "development")]
            Work::Storage { .. } => {
                self.operations[index].work = Work::Finished(Err(failure(reason)))
            }
            Work::Scan {
                token,
                stopped,
                error,
                ..
            } => {
                if !*stopped {
                    let _ = radio.scan(*token, false, false);
                    self.radio_scan = None;
                    *stopped = true;
                    *error = Some(reason);
                }
            }
            Work::Pair {
                link,
                deadline,
                cancelling,
                ..
            } if cancelling.is_none() => {
                *cancelling = Some(reason);
                *deadline = now.saturating_add(15_000);
                closing = *link;
            }
            Work::Wait {
                link,
                deadline,
                kind: Wait::Connect,
                cancelling,
            } if cancelling.is_none() => {
                *cancelling = Some(reason);
                *deadline = now.saturating_add(15_000);
                closing = Some(*link);
            }
            _ => {}
        }
        if let Some(link) = closing {
            // A user cancellation is a request result, not a link/profile failure.
            self.close(
                link,
                (reason != Error::Cancelled).then_some(reason),
                radio,
                now,
            );
            if pairing {
                self.prompt = None;
            }
        }
    }
    fn cancel_connect<B: Bluetooth>(&mut self, slot: usize, radio: &mut B, now: u64) {
        for i in 0..self.operations.len() {
            if self.operations[i].slot == Some(slot)
                && matches!(
                    self.operations[i].work,
                    Work::Wait {
                        kind: Wait::Connect,
                        ..
                    }
                )
            {
                self.stop(i, Error::Cancelled, radio, now);
            }
        }
    }
    fn cancel<B: Bluetooth>(
        &mut self,
        id: RequestId,
        target: RequestId,
        radio: &mut B,
        now: u64,
    ) -> Result<(), EmitError> {
        let Some(index) = self
            .operations
            .iter()
            .position(|o| o.id == Some(target) && !matches!(o.work, Work::Finished(_)))
        else {
            return self.serial.error(id, Error::NotPending, now);
        };
        #[cfg(feature = "development")]
        if matches!(self.operations[index].work, Work::Storage { .. }) {
            self.serial.response(
                id,
                true,
                CancelResult {
                    request_id: target,
                    requested: true,
                },
                now,
            )?;
            self.stop(index, Error::Cancelled, radio, now);
            return Ok(());
        }
        if !matches!(
            self.operations[index].work,
            Work::Scan { .. }
                | Work::Pair { .. }
                | Work::Wait {
                    kind: Wait::Connect,
                    ..
                }
        ) {
            return self.serial.error(id, Error::NotCancellable, now);
        }
        self.serial.response(
            id,
            true,
            CancelResult {
                request_id: target,
                requested: true,
            },
            now,
        )?;
        if let Some(slot) = self.operations[index].slot
            && matches!(
                self.operations[index].work,
                Work::Wait {
                    kind: Wait::Connect,
                    ..
                }
            )
        {
            self.manager.devices[slot].as_mut().unwrap().paused = true;
        }
        self.stop(index, Error::Cancelled, radio, now);
        Ok(())
    }
    fn reply<B: Bluetooth>(
        &mut self,
        id: RequestId,
        args: &messages::PairReply,
        radio: &mut B,
        now: u64,
    ) -> Result<(), EmitError> {
        let pair = self
            .operations
            .iter()
            .find(|o| o.id == Some(args.request_id));
        let Some(Operation {
            work:
                Work::Pair {
                    link: Some(link),
                    cancelling: None,
                    ..
                },
            ..
        }) = pair
        else {
            return self.serial.error(id, Error::StalePrompt, now);
        };
        let Some(prompt) = &mut self.prompt else {
            return self.serial.error(id, Error::StalePrompt, now);
        };
        if prompt.answered
            || prompt.value.prompt_id != args.prompt_id
            || now >= prompt.deadline
            || !self.serial.present()
        {
            return self.serial.error(id, Error::StalePrompt, now);
        }
        if !prompt
            .value
            .method
            .valid_reply(args.action, args.value.as_deref())
        {
            return self.serial.error(id, Error::InvalidArgs, now);
        }
        // Admission reserved a reply slot. Queue acknowledgement before allowing
        // the backend to complete or reject the parent pairing operation.
        self.serial
            .response(id, true, PairReplyResult { accepted: true }, now)?;
        let result = radio.pair_reply(
            *link,
            prompt.value.method,
            args.action == messages::PairAction::Accept,
            args.value.as_deref(),
        );
        prompt.answered = true;
        prompt.dirty = false;
        if let Some(index) = self
            .operations
            .iter()
            .position(|o| o.id == Some(args.request_id))
        {
            if let Err(e) = result {
                self.stop(index, e, radio, now);
            } else if args.action == messages::PairAction::Reject {
                self.stop(index, Error::AuthenticationRejected, radio, now);
            }
        }
        Ok(())
    }
    fn end_session<B: Bluetooth>(&mut self, radio: &mut B, now: u64) {
        for i in 0..self.operations.len() {
            if matches!(
                self.operations[i].work,
                Work::Scan { .. } | Work::Pair { .. }
            ) {
                self.stop(i, Error::Cancelled, radio, now);
            }
            self.operations[i].id = None;
        }
        self.operations.retain(|o| {
            matches!(
                o.work,
                Work::Pair { .. }
                    | Work::Job { .. }
                    | Work::Wait {
                        kind: Wait::Disconnect | Wait::Unpair | Wait::Block,
                        ..
                    }
            )
        });
        if self.operations.is_empty() {
            self.operations = Vec::new();
        }
        self.candidates = Vec::new();
        self.prompt = None;
    }
    pub fn session<B: Bluetooth>(&mut self, active: bool, radio: &mut B, now: u64) {
        if self.serial.session(active, now) {
            self.end_session(radio, now);
        }
    }
    /// Backends deliver one owned event at a time, after returning from vendor callbacks.
    pub async fn event<S: RecordStore, B: Bluetooth>(
        &mut self,
        event: Event,
        store: &mut S,
        radio: &mut B,
        now: u64,
    ) {
        if self.reboot_at.is_some() {
            return;
        }
        match event {
            Event::Ready => {
                self.manager.radio_ready = true;
                let loaded = !self.manager.storage_ready;
                let ready = if self.manager.storage_ready {
                    self.manager.sync_bonds(store, radio).await
                } else {
                    self.manager.load(store, radio).await
                };
                self.startup = match ready {
                    Ok(()) => Readiness::Ready,
                    Err(Error::StorageFailed) => {
                        self.manager.storage_ready = false;
                        Readiness::StorageFailed
                    }
                    Err(_) => Readiness::RadioFailed,
                };
                self.manager.radio_ready = self.startup != Readiness::RadioFailed;
                if loaded && self.startup == Readiness::Ready {
                    self.adapter_changed(now);
                }
            }
            Event::Failed(error) | Event::Restarting(error) => {
                self.radio_scan = None;
                for op in &mut self.operations {
                    if let Work::Scan {
                        stopped,
                        error: failure,
                        ..
                    } = &mut op.work
                    {
                        *stopped = true;
                        *failure = Some(error);
                    }
                }
                for slot in 0..devices::ACTIVE_CONNECTIONS {
                    if let Some(id) = self.manager.connections[slot].as_ref().map(|c| c.id) {
                        self.dropped(id, Some(error), now);
                    }
                }
                self.manager.radio_ready = false;
                self.startup = if matches!(event, Event::Restarting(_)) {
                    Readiness::Starting
                } else if error == Error::StorageFailed {
                    self.manager.storage_ready = false;
                    Readiness::StorageFailed
                } else {
                    Readiness::RadioFailed
                };
            }
            Event::Found {
                scan,
                peer,
                address,
                connectable,
                kind,
                name,
                rssi,
            } => {
                if connectable
                    && self.radio_scan.is_some_and(|(token, _, _)| token == scan)
                    && let Some(slot) = self.manager.peer(peer)
                {
                    self.manager.devices[slot].as_mut().unwrap().seen(now);
                }
                let Some(address) = address else { return };
                if !self.operations.iter().any(|o| matches!(o.work, Work::Scan { token, classic, ble, stopped: false,.. } if token == scan && if peer.transport == Transport::Classic { classic } else { ble })) { return; }
                let name = devices::display_name(name.as_bytes());
                if let Some(c) = self.candidates.iter_mut().find(|c| c.peer == peer) {
                    c.address = address;
                    if !name.is_empty() && c.record.name.as_deref() != Some(&name) {
                        c.record.name = Some(name.into());
                        c.dirty = true;
                    }
                    if kind != messages::DeviceKind::Unknown && c.record.kind != kind {
                        c.record.kind = kind;
                        c.dirty = true;
                    }
                    c.record.rssi = rssi.filter(|r| (-127..=20).contains(r));
                } else if self.candidates.len() == devices::SCAN_CANDIDATES {
                    self.truncated = true;
                } else {
                    self.candidate_seq = match self.candidate_seq.checked_add(1) {
                        Some(n) => n,
                        None => {
                            self.truncated = true;
                            return;
                        }
                    };
                    self.candidates.push(Candidate {
                        peer,
                        address,
                        dirty: true,
                        record: messages::Candidate {
                            candidate_id: CandidateId(format!("c_{}", self.candidate_seq)),
                            kind,
                            name: (!name.is_empty()).then(|| name.into()),
                            transport: peer.transport,
                            rssi: rssi.filter(|r| (-127..=20).contains(r)),
                        },
                    });
                }
            }
            Event::Incoming { attempt, peer } => {
                if self.pairing()
                    || (peer.transport == Transport::Ble
                        && self
                            .manager
                            .peer(peer)
                            .is_some_and(|slot| self.device_busy(slot, false)))
                {
                    let _ = radio.incoming(attempt, None);
                    return;
                }
                if let Ok(Some(slot)) = self.manager.incoming(attempt, peer, now, radio) {
                    self.changed(slot, "device.changed", now);
                }
            }
            Event::Prompt {
                link,
                method,
                value,
            } => {
                let Some(index) = self.operations.iter().position(
                    |o| matches!(o.work, Work::Pair {link: Some(id), cancelling: None,..} if id == link),
                ) else {
                    self.close(link, Some(Error::AuthenticationFailed), radio, now);
                    return;
                };
                let Work::Pair { deadline, .. } = self.operations[index].work else {
                    unreachable!()
                };
                if !self.serial.present()
                    || now >= deadline
                    || self.prompt.as_ref().is_some_and(|p| p.dirty || !p.answered)
                {
                    self.stop(index, Error::AuthenticationFailed, radio, now);
                    return;
                }
                let numeric = matches!(
                    method,
                    messages::PromptMethod::ConfirmPasskey | messages::PromptMethod::DisplayPasskey
                );
                if (numeric
                    && !value
                        .as_ref()
                        .is_some_and(|v| v.len() == 6 && v.bytes().all(|b| b.is_ascii_digit())))
                    || value.as_ref().is_some_and(|v| {
                        v.len() > 16 || !v.bytes().all(|b| (0x20..=0x7e).contains(&b))
                    })
                {
                    self.stop(index, Error::AuthenticationFailed, radio, now);
                    return;
                }
                self.prompt_seq = match self.prompt_seq.checked_add(1) {
                    Some(n) => n,
                    None => {
                        self.stop(index, Error::InternalError, radio, now);
                        return;
                    }
                };
                let deadline = deadline.min(now.saturating_add(30_000));
                self.prompt = Some(Prompt {
                    deadline,
                    dirty: true,
                    answered: method.display(),
                    value: messages::Prompt {
                        candidate_id: CandidateId(self.operations[index].reference.clone()),
                        prompt_id: format!("p_{}", self.prompt_seq),
                        method,
                        expires_in_ms: (deadline - now) as u32,
                        value: value.map(String::from),
                    },
                });
            }
            Event::Bonded { link, identity } => {
                // Stale callbacks cannot delete a newer connection's native bond.
                // Backends finish rejected-pair cleanup before Disconnected.
                if self
                    .manager
                    .connection(link)
                    .is_none_or(|c| c.device.is_some())
                {
                    return;
                }
                let pair = self.operations.iter().position(|o| {
                    matches!(o.work,
                    Work::Pair { link: Some(id), .. } if id == link)
                });
                let Some(index) = pair else {
                    self.close(link, Some(Error::AuthenticationRejected), radio, now);
                    return;
                };
                let retained = self.manager.peer(identity);
                let result = {
                    let op = &self.operations[index];
                    let Work::Pair {
                        name,
                        deadline,
                        cancelling,
                        ..
                    } = &op.work
                    else {
                        unreachable!()
                    };
                    if cancelling.is_some()
                        || now >= *deadline
                        || op.id.is_none()
                        || !self.serial.present()
                    {
                        Err(Error::AuthenticationRejected)
                    } else if retained.is_some_and(|slot| {
                        self.operations.iter().enumerate().any(|(i, o)| {
                            i != index
                                && o.slot == Some(slot)
                                && !matches!(
                                    o.work,
                                    Work::Finished(_)
                                        | Work::Devices { .. }
                                        | Work::Settings { .. }
                                        | Work::Features { .. }
                                )
                        })
                    }) {
                        Err(Error::Busy)
                    } else {
                        self.manager
                            .bonded(link, identity, name.as_bytes(), store, radio)
                            .await
                    }
                };
                match result {
                    Ok(slot) => {
                        self.manager.connection_mut(link).unwrap().deadline =
                            now.saturating_add(30_000);
                        self.operations[index].slot = Some(slot);
                        for c in &mut self.candidates {
                            if c.peer == identity
                                || c.record.candidate_id.0 == self.operations[index].reference
                            {
                                c.peer = identity;
                                c.dirty = true;
                            }
                        }
                        if !self.manager.devices[slot]
                            .as_ref()
                            .unwrap()
                            .effective_enabled
                        {
                            self.close(link, None, radio, now);
                        }
                        self.changed(slot, "device.paired", now);
                        self.operations[index].work = Work::Finished(Ok(json!(DeviceResult {
                            device: self.manager.record(slot).unwrap()
                        })));
                        self.prompt = None;
                    }
                    Err(error) => {
                        let protected = retained.is_some_and(|s| {
                            self.manager.devices[s]
                                .as_ref()
                                .is_some_and(|d| d.pairing_state == PairingState::Paired)
                        });
                        if let Work::Pair {
                            cleanup, duplicate, ..
                        } = &mut self.operations[index].work
                        {
                            // Adopt may already have succeeded before the marker
                            // write failed. Explicit cleanup waits for disconnection.
                            if !protected {
                                *cleanup = Some(identity);
                            }
                            if error == Error::AuthenticationFailed {
                                *duplicate = retained.map(|s| {
                                    self.manager.devices[s].as_ref().unwrap().policy.device_id()
                                });
                            }
                        }
                        if self.operations[index].slot.is_none() && !protected {
                            self.operations[index].slot = retained;
                        }
                        self.stop(index, error, radio, now);
                    }
                }
            }
            Event::Connected {
                link,
                descriptors,
                max_output,
            } => match self.manager.connected(link, descriptors, max_output, now) {
                Ok(Some(slot)) => {
                    self.changed(slot, "device.connected", now);
                    for op in &mut self.operations {
                        if let Work::Wait {
                            link: id,
                            kind: Wait::Connect,
                            cancelling: None,
                            deadline,
                        } = op.work
                            && id == link
                        {
                            op.work = Work::Finished(if now >= deadline {
                                Err(failure(Error::Timeout))
                            } else {
                                Ok(json!(DeviceResult {
                                    device: self.manager.record(slot).unwrap()
                                }))
                            });
                        }
                    }
                }
                Ok(None) => {}
                Err(e) => self.close(link, Some(e), radio, now),
            },
            Event::Security { link, security } => {
                if let Some(slot) = self.manager.security(link, security) {
                    self.changed(slot, "device.changed", now);
                }
            }
            Event::Disconnected { link, error } => {
                self.dropped(link, error, now);
                if !self.pairing()
                    && self.manager.storage_ready
                    && self.manager.sync_bonds(store, radio).await.is_err()
                {
                    self.manager.storage_ready = false;
                }
            }
            Event::Input(report) => {
                let slot = self.manager.connection(report.link).and_then(|c| c.device);
                match self.manager.input(&report, now) {
                    Ok(true) => {
                        if let Some(slot) = slot {
                            self.changed(slot, "device.changed", now);
                        }
                    }
                    Err(e) => self.close(report.link, Some(e), radio, now),
                    _ => {}
                }
            }
            Event::Written { id, result } => match self.manager.written(id, result.is_ok(), now) {
                Ok(Some(slot)) => self.changed(slot, "device.changed", now),
                Err(e) => self.close(id.link, Some(e), radio, now),
                _ => {}
            },
            Event::Information {
                link,
                uuid,
                instance,
                success,
                bytes,
            } => {
                if let Some(slot) = self
                    .manager
                    .connection(link)
                    .filter(|c| !c.closing && c.runtime.is_some())
                    .and_then(|c| c.device)
                {
                    let info = &mut self.manager.devices[slot].as_mut().unwrap().catalog.info;
                    if !success {
                        crate::info::standard_failed(info, uuid, instance);
                        return;
                    }
                    crate::info::standard(info, uuid, instance, &bytes);
                }
            }
            Event::Read {
                id,
                report_type,
                result,
            } => {
                if let Some(c) = self
                    .manager
                    .connections
                    .get_mut(id.link.slot as usize)
                    .and_then(Option::as_mut)
                    .filter(|c| c.id == id.link && !c.closing)
                    && let Some(slot) = c.device
                    && let Some(link) = &mut c.runtime
                {
                    link.battery_read_complete(
                        id,
                        report_type,
                        result.as_ref().map_err(|e| *e),
                        &mut self.manager.devices[slot].as_mut().unwrap().catalog,
                    );
                }
            }
        }
    }
}

impl Application<'_> {
    async fn emit<S: RecordStore>(
        &mut self,
        op: &mut Operation,
        _store: &mut S,
        now: u64,
    ) -> Result<bool, EmitError> {
        let Some(id) = op.id.filter(|_| op.generation == self.serial.generation()) else {
            return Ok(matches!(op.work, Work::Finished(_)));
        };
        match &mut op.work {
            Work::Information { .. } => return Ok(false),
            #[cfg(feature = "development")]
            Work::Storage {
                path,
                listing,
                cursor,
                generation,
            } => {
                if !self.serial.can_stream() {
                    return Err(EmitError::Full);
                }
                match _store.generation().await {
                    Ok(current) if current == *generation => (),
                    result => {
                        let code = if result.is_ok() {
                            Error::StorageChanged
                        } else {
                            Error::StorageFailed
                        };
                        self.serial.failure(id, failure(code), now)?;
                        return Ok(true);
                    }
                }
                if *listing {
                    match _store.file_entry(path, *cursor as usize).await {
                        Ok(Some(entry)) => {
                            self.serial.response(id, false, entry, now)?;
                            *cursor += 1;
                        }
                        Ok(None) => {
                            self.serial.response(
                                id,
                                true,
                                StorageListEnd {
                                    count: *cursor as usize,
                                },
                                now,
                            )?;
                            return Ok(true);
                        }
                        Err(_) => {
                            self.serial
                                .failure(id, failure(Error::StorageFailed), now)?;
                            return Ok(true);
                        }
                    }
                } else {
                    let mut bytes = [0; 512];
                    match _store.file_read(path, *cursor, &mut bytes).await {
                        Ok(0) => {
                            self.serial.response(
                                id,
                                true,
                                StorageReadEnd { bytes: *cursor },
                                now,
                            )?;
                            return Ok(true);
                        }
                        Ok(n) => {
                            use base64::Engine;
                            let mut encoded = [0; 684];
                            let len = base64::engine::general_purpose::STANDARD
                                .encode_slice(&bytes[..n], &mut encoded)
                                .map_err(|_| EmitError::Encoding)?;
                            let data = core::str::from_utf8(&encoded[..len])
                                .map_err(|_| EmitError::Encoding)?;
                            self.serial.response(
                                id,
                                false,
                                StorageChunk {
                                    offset: *cursor,
                                    data: data.into(),
                                },
                                now,
                            )?;
                            *cursor += n as u32;
                        }
                        Err(_) => {
                            self.serial
                                .failure(id, failure(Error::StorageFailed), now)?;
                            return Ok(true);
                        }
                    }
                }
            }
            Work::Finished(result) => {
                match result {
                    Ok(value) => self.serial.response(id, true, value, now)?,
                    Err(error) => self.serial.failure(id, error.clone(), now)?,
                }
                return Ok(true);
            }
            Work::Devices {
                revision,
                filter,
                cursor,
                count,
            } => {
                if *revision != self.manager.revision {
                    self.serial.failure(id, failure(Error::Busy), now)?;
                    return Ok(true);
                }
                let next = (*cursor..self.manager.devices.len()).find_map(|slot| {
                    let device = self.manager.record(slot)?;
                    let matches = match filter {
                        messages::Filter::Saved => true,
                        messages::Filter::Paired => device.pairing_state == PairingState::Paired,
                        messages::Filter::Connected => device.state == ConnectionState::Connected,
                    };
                    matches.then_some((slot, device))
                });
                if let Some((slot, device)) = next {
                    self.serial.response(
                        id,
                        false,
                        json!(DeviceSnapshot {
                            revision: *revision,
                            device
                        }),
                        now,
                    )?;
                    *cursor = slot + 1;
                    *count += 1;
                } else {
                    self.serial.response(
                        id,
                        true,
                        DeviceListEnd {
                            count: *count,
                            revision: *revision,
                        },
                        now,
                    )?;
                    return Ok(true);
                }
            }
            Work::Settings {
                revision,
                records,
                cursor,
                state,
                error,
            } => {
                if let Some(record) = records.get(*cursor) {
                    self.serial.response(
                        id,
                        false,
                        messages::SettingChunk {
                            revision: *revision,
                            device_id: DeviceId(op.reference.clone()),
                            setting: record.wire(),
                        },
                        now,
                    )?;
                    *cursor += 1;
                } else {
                    self.serial.response(
                        id,
                        true,
                        SettingsListEnd {
                            revision: *revision,
                            device_id: DeviceId(op.reference.clone()),
                            count: records.len(),
                            settings_state: *state,
                            settings_error: *error,
                        },
                        now,
                    )?;
                    return Ok(true);
                }
            }
            Work::Features {
                revision,
                records,
                cursor,
                state,
                error,
            } => {
                if let Some(feature) = records.get(*cursor) {
                    self.serial.response(
                        id,
                        false,
                        messages::FeatureChunk {
                            revision: *revision,
                            device_id: DeviceId(op.reference.clone()),
                            feature: feature.wire(*cursor),
                        },
                        now,
                    )?;
                    *cursor += 1;
                } else {
                    self.serial.response(
                        id,
                        true,
                        SettingsListEnd {
                            revision: *revision,
                            device_id: DeviceId(op.reference.clone()),
                            count: records.len(),
                            settings_state: *state,
                            settings_error: *error,
                        },
                        now,
                    )?;
                    return Ok(true);
                }
            }
            Work::Scan { stopped, error, .. } => {
                if let Some(c) = self.candidates.iter_mut().find(|c| c.dirty) {
                    self.serial
                        .event("discovery.result", Some(id), &c.record, false, now)?;
                    c.dirty = false;
                } else if *stopped {
                    if let Some(error) = error {
                        self.serial.error(id, *error, now)?;
                    } else {
                        self.serial.response(
                            id,
                            true,
                            ScanEnd {
                                count: self.candidates.len(),
                                truncated: self.truncated,
                            },
                            now,
                        )?;
                    }
                    return Ok(true);
                }
            }
            Work::Job {
                apply,
                cursor,
                outcomes,
                detached,
                terminal,
            } => {
                let slot = op.slot.unwrap();
                let runtime = self
                    .manager
                    .link_for(slot)
                    .and_then(|id| self.manager.connection(id))
                    .and_then(|c| c.runtime.as_deref());
                let row = if let Some(records) = detached {
                    records.get(*cursor).map(|(r, o)| (r.wire(), *o))
                } else {
                    runtime
                        .and_then(|r| r.settings.results().get(*cursor))
                        .and_then(|result| {
                            self.manager.devices[slot]
                                .as_ref()?
                                .catalog
                                .records()
                                .iter()
                                .find(|r| r.metadata.key == result.key)
                                .map(|r| (r.wire(), result.outcome))
                        })
                };
                if let Some((row, outcome)) = row {
                    self.serial.response(
                        id,
                        false,
                        SettingOutcomeChunk {
                            revision: self.manager.revision,
                            device_id: DeviceId(op.reference.clone()),
                            setting: row,
                            outcome,
                        },
                        now,
                    )?;
                    *cursor += 1;
                    outcomes[outcome as usize] += 1;
                } else if detached.is_some() || runtime.is_none_or(|r| r.settings.done()) {
                    let summary = SettingsSummary {
                        revision: self.manager.revision,
                        device_id: DeviceId(op.reference.clone()),
                        count: outcomes.iter().sum::<usize>(),
                        read: outcomes[0],
                        applied: outcomes[1],
                        unchanged: outcomes[2],
                        unsupported: outcomes[3],
                        failed: outcomes[4],
                        uncertain: outcomes[5],
                    };
                    let failed = outcomes[3..].iter().any(|n| *n != 0)
                        || runtime.is_some_and(|r| {
                            matches!(
                                r.settings.state,
                                SettingsState::Error | SettingsState::Unsupported
                            )
                        });
                    if let Some(code) = terminal.or(failed.then_some(if *apply {
                        Error::SettingsApplyFailed
                    } else {
                        Error::SettingsRefreshFailed
                    })) {
                        self.serial.failure(
                            id,
                            WireError {
                                code,
                                details: Some(ErrorDetails::Settings(summary)),
                            },
                            now,
                        )?;
                    } else {
                        self.serial.response(id, true, summary, now)?;
                    }
                    self.release_job(slot);
                    return Ok(true);
                }
            }
            _ => {}
        }
        Ok(false)
    }
    fn release_job(&mut self, slot: usize) {
        if let Some(id) = self.manager.link_for(slot)
            && let Some(runtime) = self.manager.connections[id.slot as usize]
                .as_mut()
                .and_then(|c| c.runtime.as_mut())
            && let Some(d) = &mut self.manager.devices[slot]
        {
            runtime.settings.release(&mut d.catalog);
        }
    }
    fn update_scan<B: Bluetooth>(&mut self, radio: &mut B, reconnecting: bool, now: u64) {
        if !self.manager.radio_ready {
            return;
        }
        let mut wanted = self.operations.iter().find_map(|o| match o.work {
            Work::Scan {
                token,
                classic,
                ble,
                stopped: false,
                ..
            } => Some((token, classic, ble)),
            _ => None,
        });
        // Exclusive backends alternate one-second windows while saved peers are
        // eligible. The logical scan stays open, retaining its token and results.
        if reconnecting && !radio.capabilities().ble_scan_and_connect && now % 2000 >= 1000 {
            wanted = wanted.map(|(token, classic, _)| (token, classic, false));
            if wanted.is_some_and(|(_, classic, ble)| !classic && !ble) {
                wanted = None;
            }
        }
        if wanted != self.radio_scan {
            let (token, classic, ble) = wanted.unwrap_or((self.scan_seq, false, false));
            if radio.scan(token, classic, ble).is_ok() {
                self.radio_scan = wanted;
            }
        }
    }
    async fn advance_pair<S: RecordStore, B: Bluetooth>(
        &mut self,
        op: &mut Operation,
        store: &mut S,
        radio: &mut B,
        now: u64,
    ) {
        let Work::Pair {
            link,
            address,
            cancelling,
            cleanup,
            duplicate,
            deadline,
            expected,
            ..
        } = &mut op.work
        else {
            return;
        };
        if link.is_some() {
            if cancelling.is_some()
                && now >= *deadline
                && let Some(id) = op.id
                && self.serial.error(id, Error::Timeout, now).is_ok()
            {
                // End the request deadline, retaining its device/controller
                // reservation until native teardown and bond cleanup finish.
                op.id = None;
            }
            return;
        }
        if let Some(error) = *cancelling {
            let result = if let Some(peer) = cleanup.take() {
                radio.forget(peer).await
            } else {
                Ok(())
            };
            let restored = self.manager.finish_pair(store, radio).await;
            let code = result.err().or(restored.err()).unwrap_or(error);
            if let Some(slot) = op.slot
                && self.manager.devices[slot].as_ref().unwrap().pairing_state
                    == PairingState::NeedsPairing
            {
                self.manager.devices[slot].as_mut().unwrap().error = Some(code);
                self.changed(slot, "device.changed", now);
            }
            op.work = Work::Finished(Err(WireError {
                code,
                details: if code == Error::StorageFailed && self.manager.write_uncertain {
                    Some(ErrorDetails::Mutation(MutationDetails {
                        outcome: StorageOutcome::Unknown,
                    }))
                } else {
                    duplicate.clone().map(|device_id| {
                        ErrorDetails::ExistingDevice(ExistingDeviceDetails { device_id })
                    })
                },
            }));
            return;
        }
        if self
            .manager
            .connections
            .iter()
            .flatten()
            .any(|c| c.closing || c.runtime.is_none())
        {
            return;
        }
        let result = async {
            if !self.manager.radio_ready {
                return Err(Error::RadioUnavailable);
            }
            if !self.manager.storage_ready {
                return Err(Error::StorageFailed);
            }
            self.manager
                .prepare_pair(*address, *expected, store, radio)
                .await?;
            self.manager.pair(*address, *deadline, radio)
        }
        .await;
        match result {
            Ok(id) => *link = Some(id),
            Err(error) => {
                let error = self
                    .manager
                    .finish_pair(store, radio)
                    .await
                    .err()
                    .unwrap_or(error);
                if let Some(slot) = op.slot
                    && self.manager.devices[slot].as_ref().unwrap().pairing_state
                        == PairingState::NeedsPairing
                {
                    self.manager.devices[slot].as_mut().unwrap().error = Some(error);
                    self.changed(slot, "device.changed", now);
                }
                op.work = Work::Finished(Err(if error == Error::StorageFull {
                    capacity_failure(CapacityReason::StorageFull)
                } else if error == Error::Capacity {
                    capacity_failure(if radio.bond_capacity(address.transport) == 0 {
                        CapacityReason::SetupCapacity
                    } else if self.manager.connections.iter().all(Option::is_some) {
                        CapacityReason::ConnectionsFull
                    } else {
                        CapacityReason::StorageFull
                    })
                } else {
                    mutation_failure(error, self.manager.write_uncertain)
                }));
            }
        }
    }
    pub async fn poll<S: RecordStore, B: Bluetooth>(
        &mut self,
        store: &mut S,
        radio: &mut B,
        leds: u8,
        now: u64,
    ) {
        if let Some(deadline) = self.reboot_at {
            if ((self.serial.queued() == 0 && self.manager.forward.pending() == 0)
                || now >= deadline)
                && let Some(boot) = &self.build.bootloader
            {
                (boot.enter)();
            }
            return;
        }
        let tick = self.serial.tick(now);
        if tick.session_fault {
            self.end_session(radio, now);
        } else if tick.presence_expired {
            for i in 0..self.operations.len() {
                #[cfg(feature = "development")]
                if matches!(self.operations[i].work, Work::Storage { .. }) {
                    self.stop(i, Error::ClientTimeout, radio, now);
                }

                if matches!(
                    self.operations[i].work,
                    Work::Pair { .. } | Work::Scan { .. }
                ) {
                    self.stop(i, Error::ClientTimeout, radio, now);
                }
            }
        }
        if self
            .prompt
            .as_ref()
            .is_some_and(|p| !p.answered && now >= p.deadline)
            && let Some(i) = self
                .operations
                .iter()
                .position(|o| matches!(o.work, Work::Pair { .. }))
        {
            self.stop(i, Error::Timeout, radio, now);
        }
        if let Some(prompt) = &mut self.prompt
            && prompt.dirty
        {
            prompt.value.expires_in_ms = prompt.deadline.saturating_sub(now) as u32;
            let request = self
                .operations
                .iter()
                .find(|o| matches!(o.work, Work::Pair { .. }))
                .and_then(|o| o.id);
            if request.is_some()
                && self
                    .serial
                    .event(
                        if prompt.value.method.display() {
                            "pairing.display"
                        } else {
                            "pairing.prompt"
                        },
                        request,
                        &prompt.value,
                        false,
                        now,
                    )
                    .is_ok()
            {
                prompt.dirty = false;
            }
        }
        for index in 0..devices::ACTIVE_CONNECTIONS {
            let id = self.manager.connections[index].as_ref().map(|c| c.id);
            match self.manager.poll_link(index, leds, now, radio) {
                Ok(Some(slot)) => self.changed(slot, "device.changed", now),
                Err(e) => {
                    if let Some(id) = id {
                        self.close(id, Some(e), radio, now);
                    }
                }
                _ => {}
            }
        }
        self.information_changes(now);
        self.setup(store, now).await;
        let mut i = 0;
        while i < self.operations.len() {
            let expired = match self.operations[i].work {
                Work::Pair { deadline, .. } => now >= deadline,
                _ => false,
            };
            if expired {
                self.stop(i, Error::Timeout, radio, now);
            }
            let mut op = self.operations.remove(i);
            match &mut op.work {
                Work::Information { link, deadline } => {
                    let slot = op.slot.unwrap();
                    if self.manager.connection(*link).is_none_or(|c| c.closing) {
                        op.work = Work::Finished(Err(failure(Error::NotConnected)));
                    } else if !radio.info_busy(*link)
                        && !self.settings_busy(slot)
                        && !self
                            .manager
                            .connection(*link)
                            .and_then(|c| c.runtime.as_ref())
                            .is_some_and(|r| {
                                r.battery_busy(
                                    &self.manager.devices[slot].as_ref().unwrap().catalog,
                                )
                            })
                    {
                        op.work = Work::Finished(Ok(self.information(slot)));
                    } else if now >= *deadline {
                        op.work = Work::Finished(Err(failure(Error::Timeout)));
                    }
                }
                Work::Wait {
                    kind: Wait::Connect,
                    deadline,
                    cancelling: None,
                    ..
                } if now >= *deadline => {
                    op.work = Work::Finished(Err(failure(Error::Timeout)));
                }
                Work::Scan {
                    token,
                    deadline: Some(deadline),
                    stopped,
                    error,
                    ..
                } if now >= *deadline && !*stopped => {
                    *error = radio.scan(*token, false, false).err();
                    self.radio_scan = None;
                    *stopped = true;
                }
                Work::Wait {
                    link,
                    kind: Wait::Disconnect | Wait::Unpair | Wait::Block,
                    deadline,
                    ..
                } => {
                    if self.manager.connection(*link).is_none() {
                        let slot = op.slot.unwrap();
                        op.work = Work::Finished(if op.command == "device.unpair" {
                            self.remove(slot, store, radio, now)
                                .await
                                .map(|()| {
                                    json!(DeviceRemoved {
                                        device_id: DeviceId(op.reference.clone()),
                                        removed: true
                                    })
                                })
                                .map_err(failure)
                        } else {
                            Ok(json!(DeviceResult {
                                device: self.manager.record(slot).unwrap()
                            }))
                        });
                    } else if now >= *deadline {
                        op.work = Work::Finished(Err(failure(Error::Timeout)));
                    }
                }
                Work::Wait {
                    deadline,
                    cancelling: Some(_),
                    ..
                } if now >= *deadline => op.work = Work::Finished(Err(failure(Error::Timeout))),
                Work::Job { detached, .. } if op.id.is_none() => {
                    let slot = op.slot.unwrap();
                    let done = detached.is_some()
                        || self
                            .manager
                            .link_for(slot)
                            .and_then(|id| self.manager.connection(id))
                            .and_then(|c| c.runtime.as_ref())
                            .is_none_or(|r| r.settings.done());
                    if done {
                        self.release_job(slot);
                        op.work = Work::Finished(Ok(Value::Null));
                    }
                }
                _ => {}
            }
            self.advance_pair(&mut op, store, radio, now).await;
            let complete = match self.emit(&mut op, store, now).await {
                Ok(done) => done,
                Err(EmitError::Full | EmitError::Unavailable) => false,
                Err(_) => {
                    if let Some(slot) = op.slot
                        && matches!(op.work, Work::Job { .. })
                    {
                        self.release_job(slot);
                    }
                    op.work = Work::Finished(Err(failure(Error::MessageTooLarge)));
                    false
                }
            };
            if !complete {
                self.operations.insert(i, op);
                i += 1;
            }
        }
        if !self.pairing() && self.manager.storage_ready && self.manager.radio_ready {
            for slot in 0..self.manager.devices.len() {
                if self.manager.connections.iter().flatten().count()
                    >= devices::ACTIVE_CONNECTIONS - 1
                    || self
                        .manager
                        .connections
                        .iter()
                        .flatten()
                        .any(|c| c.runtime.is_none() || c.closing)
                {
                    break;
                }
                if self.device_busy(slot, false)
                    || !self.manager.devices[slot].as_ref().is_some_and(|d| {
                        d.policy.peer.transport == Transport::Classic && d.reconnect_due(now)
                    })
                {
                    continue;
                }
                match self
                    .manager
                    .connect(slot, false, now.saturating_add(30_000), radio)
                {
                    Ok(_) => self.changed(slot, "device.changed", now),
                    Err(Error::Busy | Error::Capacity) => {}
                    Err(e) => {
                        self.manager.devices[slot].as_mut().unwrap().connection(
                            ConnectionState::Disconnected,
                            Some(e),
                            now,
                        );
                        self.changed(slot, "device.changed", now);
                    }
                }
            }
        }
        let available = !self.pairing()
            && self.manager.storage_ready
            && self.manager.radio_ready
            && self.manager.connections.iter().flatten().count() < devices::ACTIVE_CONNECTIONS - 1
            && !self
                .manager
                .connections
                .iter()
                .flatten()
                .any(|c| c.runtime.is_none() || c.closing);
        let peers: Vec<_> = self
            .manager
            .devices
            .iter()
            .enumerate()
            .filter_map(|(slot, device)| {
                let d = device.as_ref()?;
                (available
                    && !self.device_busy(slot, false)
                    && d.policy.peer.transport == Transport::Ble
                    && d.reconnect_due(now))
                .then_some(d.policy.peer)
            })
            .collect();
        if self.manager.radio_ready
            && let Err(error) = radio.reconnect(&peers)
            && error != Error::Busy
        {
            self.event(Event::Failed(error), store, radio, now).await;
        }
        self.update_scan(radio, !peers.is_empty(), now);
        if self.serial.waiting_ready() {
            let status = self.status(radio, now);
            let _ = self.serial.ready_poll(self.startup, &status, now);
        }
    }
}

impl Application<'_> {
    fn dropped(&mut self, link: LinkId, error: Option<Error>, now: u64) {
        let connected = self
            .manager
            .connection(link)
            .and_then(|c| c.device)
            .is_some_and(|s| {
                self.manager.devices[s].as_ref().unwrap().state == ConnectionState::Connected
            });
        if let Some((slot, runtime)) = self.manager.disconnected(link, error, now) {
            if let Some(slot) = slot {
                self.changed(
                    slot,
                    if connected {
                        "device.disconnected"
                    } else {
                        "device.changed"
                    },
                    now,
                );
                if let Some(mut runtime) = runtime {
                    for op in &mut self.operations {
                        if op.slot == Some(slot)
                            && let Work::Job {
                                detached, terminal, ..
                            } = &mut op.work
                        {
                            let catalog = &self.manager.devices[slot].as_ref().unwrap().catalog;
                            *detached = Some(
                                runtime
                                    .settings
                                    .results()
                                    .iter()
                                    .filter_map(|result| {
                                        catalog
                                            .records()
                                            .iter()
                                            .find(|r| r.metadata.key == result.key)
                                            .map(|r| (r.clone(), result.outcome))
                                    })
                                    .collect(),
                            );
                            *terminal = Some(Error::NotConnected);
                        }
                    }
                    runtime
                        .settings
                        .release(&mut self.manager.devices[slot].as_mut().unwrap().catalog);
                }
            }
            for op in &mut self.operations {
                match &mut op.work {
                    Work::Pair {
                        link: id,
                        cancelling,
                        ..
                    } if *id == Some(link) => {
                        *id = None;
                        if cancelling.is_none() {
                            *cancelling = Some(error.unwrap_or(Error::ConnectionFailed));
                        }
                        self.prompt = None;
                    }
                    Work::Wait {
                        link: id,
                        kind: Wait::Connect,
                        cancelling,
                        ..
                    } if *id == link => {
                        op.work = Work::Finished(Err(failure(
                            cancelling
                                .or(error)
                                .or(slot.and_then(|s| self.manager.devices[s].as_ref()?.error))
                                .unwrap_or(Error::ConnectionFailed),
                        )))
                    }
                    _ => {}
                }
            }
        }
    }
}
