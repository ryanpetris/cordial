//! A simulated Dongle over in-memory byte streams. It answers each request frame from a small
//! model and sends the events a real Dongle would.
#![allow(dead_code)]
use cordial_client::{Connection, Received, paging};
use cordial_protocol::{
    self as p, CodeKind, ConfigurationInterface, DeviceState, ErrorCode, IntegrationKind,
    MAX_REQUEST_BYTES, Role, SettingState, Transport, device_list_entry,
    frame::{self, Decoder},
    keys, message, pairing, profile_list_entry, profile_rule, profile_rule_change,
    request::Command,
    response, setting, setting_change,
    value::Value,
};
use prost::Message as _;
use std::{
    collections::BTreeMap,
    io::{self, Read, Write},
    sync::{
        Arc, Mutex,
        mpsc::{self, Receiver, RecvTimeoutError, Sender},
    },
    thread,
    time::Duration,
};

pub struct PipeReader {
    rx: Receiver<Vec<u8>>,
    buffer: Vec<u8>,
}

impl Read for PipeReader {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if self.buffer.is_empty() {
            match self.rx.recv_timeout(Duration::from_millis(10)) {
                Ok(bytes) => self.buffer = bytes,
                Err(RecvTimeoutError::Timeout) => return Err(io::ErrorKind::TimedOut.into()),
                Err(RecvTimeoutError::Disconnected) => return Ok(0),
            }
        }
        let n = out.len().min(self.buffer.len());
        out[..n].copy_from_slice(&self.buffer[..n]);
        self.buffer.drain(..n);
        Ok(n)
    }
}

#[derive(Clone)]
pub struct PipeWriter(Sender<Vec<u8>>);

impl Write for PipeWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0
            .send(bytes.to_vec())
            .map_err(|_| io::Error::from(io::ErrorKind::BrokenPipe))?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn pipe() -> (PipeWriter, PipeReader) {
    let (tx, rx) = mpsc::channel();
    (
        PipeWriter(tx),
        PipeReader {
            rx,
            buffer: Vec::new(),
        },
    )
}

/// How a pairing goes once started.
#[derive(Clone, Debug)]
pub enum Plan {
    /// Asks for a passkey; the right one saves the device.
    EnterCode(String),
    /// Asks to compare a passkey; accepting saves the device.
    Confirm(String),
    /// Saves the device at once.
    Done,
}

/// Everything the simulated Dongle knows.
pub struct Sim {
    pub status: p::Status,
    /// Saved devices; listed in ascending ID order.
    pub devices: Vec<p::Device>,
    /// Saved devices whose records can't be read.
    pub unreadable: Vec<u32>,
    pub settings: BTreeMap<u32, Vec<p::Setting>>,
    pub warnings: BTreeMap<u32, Vec<p::DeviceWarning>>,
    pub candidates: Vec<p::Candidate>,
    /// A scan ends at once with scan_done.
    pub scan_ends: bool,
    pub plan: Plan,
    /// Pairing saves the device with this record.
    pub paired: p::Device,
    pub files: BTreeMap<String, Vec<p::FileEntry>>,
    pub data: BTreeMap<String, Vec<u8>>,
    /// Every command received, in order.
    pub log: Vec<Command>,
    /// Commands answered with this error instead.
    pub refuse: BTreeMap<&'static str, ErrorCode>,
    /// Saved profiles and their rules; used only when the status has profile support.
    pub profiles: Vec<p::Profile>,
    pub rules: BTreeMap<u32, Vec<p::ProfileRule>>,
    /// The most records one listing page holds.
    pub page_size: usize,
    next_profile: u32,
    pairing: Option<u32>,
    out: Option<PipeWriter>,
    /// Counts sessions, so a session that ends after the next one started leaves it alone.
    session: u64,
}

pub fn info(key: &str, value: Value) -> p::Info {
    p::Info {
        key: key.into(),
        value: Some(p::Value { value: Some(value) }),
    }
}

pub fn status(ready: bool) -> p::Status {
    p::Status {
        id: "ADAPTER01".into(),
        name: "Desk".into(),
        platform: p::Platform::Linux as i32,
        ready,
        transports: vec![
            p::TransportSupport {
                transport: Transport::Classic as i32,
                max_enabled: Some(1),
                enabled: Some(true),
            },
            p::TransportSupport {
                transport: Transport::Ble as i32,
                max_enabled: Some(7),
                enabled: Some(true),
            },
        ],
        info: vec![
            info(keys::FIRMWARE_VERSION, Value::Text("1.2.3-dev".into())),
            info(keys::BUILD_DEVELOPMENT, Value::Bool(true)),
        ],
        profile_support: None,
        configuration_interfaces: Vec::new(),
    }
}

pub fn usage(usage_page: u32, usage: u32) -> p::Usage {
    p::Usage { usage_page, usage }
}

fn range(page: u32, min: u32, max: u32, collection: Option<p::Usage>) -> p::UsageRange {
    p::UsageRange {
        collection,
        usage_page: page,
        min,
        max,
    }
}

/// Generic Desktop Keyboard, Mouse, and Consumer Control.
pub const KEYBOARD: (u32, u32) = (0x01, 0x06);
pub const MOUSE: (u32, u32) = (0x01, 0x02);
pub const CONSUMER: (u32, u32) = (0x0c, 0x01);

fn collection(c: (u32, u32)) -> Option<p::Usage> {
    Some(usage(c.0, c.1))
}

/// Firmware with profiles: keys, buttons and media controls remap, pointer motion and wheels
/// scale, and VIA and Vial are offered, conflicting and disabled.
pub fn enable_profiles(status: &mut p::Status) {
    status.profile_support = Some(p::ProfileSupport {
        remap_inputs: vec![
            range(0x07, 0x04, 0xe7, None),
            range(0x09, 1, 16, None),
            range(0x0c, 0x00, 0x3ff, None),
        ],
        scale_inputs: vec![range(0x01, 0x30, 0x38, None)],
        remap_outputs: vec![
            range(0x07, 0x04, 0xe7, collection(KEYBOARD)),
            range(0x09, 1, 8, collection(MOUSE)),
            range(0x0c, 0x00, 0x3ff, collection(CONSUMER)),
        ],
        memory_budget: 8192,
        max_remap_outputs: 4,
        max_layers: 3,
        memory_used: 1024,
    });
    status.configuration_interfaces = [
        (ConfigurationInterface::Via, ConfigurationInterface::Vial),
        (ConfigurationInterface::Vial, ConfigurationInterface::Via),
    ]
    .into_iter()
    .map(|(i, other)| p::ConfigurationInterfaceSupport {
        interface: i as i32,
        enabled: false,
        profile: 0,
        conflicts: vec![other as i32],
    })
    .collect();
}

pub fn profile(id: u32, name: &str, roles: &[Role]) -> p::Profile {
    p::Profile {
        id,
        name: name.into(),
        roles: roles.iter().map(|r| *r as i32).collect(),
    }
}

pub fn device(id: u32, name: &str, transport: Transport) -> p::Device {
    p::Device {
        id,
        transport: transport as i32,
        name: name.into(),
        kinds: vec![p::Kind::Mouse as i32],
        state: DeviceState::Disconnected as i32,
        enabled: true,
        trusted: true,
        integrations: vec![p::Integration {
            kind: IntegrationKind::Hidpp as i32,
            enabled: true,
            detected: None,
            status: Some(p::integration::Status::State(
                p::IntegrationState::Disconnected as i32,
            )),
        }],
        ..Default::default()
    }
}

pub fn candidate(id: u32, name: &str) -> p::Candidate {
    p::Candidate {
        id,
        transport: Transport::Ble as i32,
        name: name.into(),
        kinds: vec![p::Kind::Keyboard as i32],
        rssi: Some(-55),
    }
}

/// A HID++ integer setting with a range.
pub fn dpi(value: i64, saved: Option<i64>) -> p::Setting {
    p::Setting {
        integration: IntegrationKind::Hidpp as i32,
        key: "pointer.sensor.0.dpi".into(),
        status: saved.map(|_| setting::Status::State(SettingState::Applied as i32)),
        r#type: Some(setting::Type::Integer(p::IntegerSetting {
            value: Some(value),
            saved,
            limits: Some(p::integer_setting::Limits::Range(p::IntegerRange {
                min: 400,
                max: 4000,
                step: 50,
            })),
        })),
    }
}

impl Default for Sim {
    fn default() -> Self {
        Self {
            status: status(true),
            devices: vec![
                device(1, "Office Mouse", Transport::Ble),
                p::Device {
                    enabled: false,
                    inactive: Some(p::InactiveReason::Disabled as i32),
                    ..device(2, "Old Keyboard", Transport::Classic)
                },
            ],
            unreadable: Vec::new(),
            settings: BTreeMap::new(),
            warnings: BTreeMap::new(),
            candidates: vec![candidate(1, "New Keyboard")],
            scan_ends: true,
            plan: Plan::Done,
            paired: device(9, "New Keyboard", Transport::Ble),
            files: BTreeMap::new(),
            data: BTreeMap::new(),
            log: Vec::new(),
            refuse: BTreeMap::new(),
            profiles: Vec::new(),
            rules: BTreeMap::new(),
            page_size: 2,
            next_profile: 1,
            pairing: None,
            out: None,
            session: 0,
        }
    }
}

fn kind_name(command: &Command) -> &'static str {
    match command {
        Command::GetStatus(_) => "get_status",
        Command::SetAdapter(_) => "set_adapter",
        Command::EnterBootloader(_) => "enter_bootloader",
        Command::StartScan(_) => "start_scan",
        Command::StopScan(_) => "stop_scan",
        Command::StartPairing(_) => "start_pairing",
        Command::AcceptPrompt(_) => "accept_prompt",
        Command::RejectPrompt(_) => "reject_prompt",
        Command::CancelPairing(_) => "cancel_pairing",
        Command::ListDevices(_) => "list_devices",
        Command::GetDevice(_) => "get_device",
        Command::SetDevice(_) => "set_device",
        Command::ConnectDevice(_) => "connect_device",
        Command::DisconnectDevice(_) => "disconnect_device",
        Command::UnpairDevice(_) => "unpair_device",
        Command::RefreshDevice(_) => "refresh_device",
        Command::ListWarnings(_) => "list_warnings",
        Command::ListSettings(_) => "list_settings",
        Command::SetSettings(_) => "set_settings",
        Command::ListFeatures(_) => "list_features",
        Command::ListFiles(_) => "list_files",
        Command::ReadFile(_) => "read_file",
        Command::ListProfiles(_) => "list_profiles",
        Command::GetProfile(_) => "get_profile",
        Command::CreateProfile(_) => "create_profile",
        Command::CopyProfile(_) => "copy_profile",
        Command::DeleteProfile(_) => "delete_profile",
        Command::ListProfileRules(_) => "list_profile_rules",
        Command::SetProfileRules(_) => "set_profile_rules",
    }
}

fn error(code: ErrorCode) -> Option<response::Result> {
    Some(response::Result::Error(p::Error {
        code: code as i32,
        ..Default::default()
    }))
}

/// The page of `ids` after `after`: the IDs it covers and its `next`.
/// The page of `entries` after the key `after`, in key order, and whether it ends the listing.
fn page<T: Clone, K: Ord>(
    entries: &[T],
    key: impl Fn(&T) -> K,
    after: Option<K>,
    size: usize,
) -> (Vec<T>, bool) {
    let mut later: Vec<T> = entries
        .iter()
        .filter(|e| after.as_ref().is_none_or(|a| key(e) > *a))
        .cloned()
        .collect();
    later.sort_by_key(|e| key(e));
    let end = later.len() <= size;
    later.truncate(size);
    (later, end)
}

fn id_after(after: u32) -> Option<u32> {
    (after != 0).then_some(after)
}

fn setting_key(s: &p::Setting) -> (i32, String) {
    (s.integration, s.key.clone())
}

fn warning_key(w: &p::DeviceWarning) -> impl Ord + use<> {
    (
        w.service,
        w.report_type,
        w.report_id,
        w.bit_offset,
        w.usage_page,
        w.usage,
        w.code,
    )
}

fn rule_key(r: &p::ProfileRule) -> (u32, u32) {
    let i = r.input.unwrap_or_default();
    (i.usage_page, i.usage)
}

/// The role a rule adds, from its input.
fn rule_role(rule: &p::ProfileRule) -> Option<Role> {
    let by = rule.input?;
    Some(match (by.usage_page, by.usage) {
        (0x07, _) => Role::Keyboard,
        (0x09, _) | (0x01, 0x30 | 0x31 | 0x38) | (0x0c, 0x238) => Role::Mouse,
        (0x0c, _) => Role::ConsumerControl,
        (0x01, 0x80..=0xb7) => Role::SystemControl,
        _ => return None,
    })
}

impl Sim {
    fn send(&mut self, kind: message::Kind) {
        let mut bytes = Vec::new();
        frame::encode(&p::Message { kind: Some(kind) }, &mut bytes);
        if let Some(out) = &mut self.out {
            let _ = out.write_all(&bytes);
        }
    }

    pub fn event(&mut self, kind: p::event::Kind) {
        self.send(message::Kind::Event(p::Event { kind: Some(kind) }));
    }

    fn respond(&mut self, result: Option<response::Result>) {
        self.send(message::Kind::Response(p::Response { result }));
    }

    fn device_mut(&mut self, id: u32) -> Option<&mut p::Device> {
        self.devices.iter_mut().find(|d| d.id == id)
    }

    fn device_event(&mut self, id: u32) {
        if let Some(d) = self.devices.iter().find(|d| d.id == id).cloned() {
            self.event(p::event::Kind::Device(d));
        }
    }

    fn has_profile(&self, id: u32) -> bool {
        self.profiles.iter().any(|p| p.id == id)
    }

    /// Adds a profile with rules, as if saved earlier.
    pub fn add_profile(&mut self, name: &str, rules: Vec<p::ProfileRule>) -> u32 {
        let id = self.next_profile;
        self.next_profile += 1;
        self.profiles.push(profile(id, name, &[]));
        self.rules.insert(id, rules);
        self.update_roles(id);
        id
    }

    /// Works out a profile's roles from its rules; true when they changed.
    fn update_roles(&mut self, id: u32) -> bool {
        let mut roles: Vec<i32> = self
            .rules
            .get(&id)
            .into_iter()
            .flatten()
            .filter_map(rule_role)
            .map(|r| r as i32)
            .collect();
        roles.sort_unstable();
        roles.dedup();
        let Some(profile) = self.profiles.iter_mut().find(|p| p.id == id) else {
            return false;
        };
        let changed = profile.roles != roles;
        profile.roles = roles;
        changed
    }

    /// Enables or disables a supported transport: its saved devices are inactive while it is
    /// disabled. Enabled-place capacity isn't modeled.
    pub fn set_transport(&mut self, transport: Transport, on: bool) {
        let transport = transport as i32;
        for t in self
            .status
            .transports
            .iter_mut()
            .filter(|t| t.transport == transport)
        {
            t.enabled = Some(on);
        }
        let ids: Vec<u32> = self
            .devices
            .iter()
            .filter(|d| d.transport == transport)
            .map(|d| d.id)
            .collect();
        for id in ids {
            let d = self.device_mut(id).unwrap();
            d.inactive = if !on {
                Some(p::InactiveReason::TransportDisabled as i32)
            } else if d.blocked {
                Some(p::InactiveReason::Blocked as i32)
            } else if !d.enabled {
                Some(p::InactiveReason::Disabled as i32)
            } else {
                None
            };
            self.device_event(id);
        }
    }

    fn prompt(&mut self, candidate: u32, step: pairing::Step) {
        self.event(p::event::Kind::Pairing(p::Pairing {
            candidate,
            step: Some(step),
        }));
    }

    fn pair_done(&mut self) {
        let candidate = self.pairing.take().unwrap_or_default();
        let device = self.paired.clone();
        self.devices.push(device.clone());
        self.event(p::event::Kind::Device(device.clone()));
        self.prompt(
            candidate,
            pairing::Step::Done(p::PairingDone { device: device.id }),
        );
    }

    fn pair_failed(&mut self, code: ErrorCode) {
        let candidate = self.pairing.take().unwrap_or_default();
        self.prompt(candidate, pairing::Step::Failed(code as i32));
    }

    /// Applies configuration interface updates in order, as SetAdapter does.
    fn set_interfaces(&mut self, updates: &[p::ConfigurationInterfaceUpdate]) -> Option<ErrorCode> {
        if updates.is_empty() {
            return None;
        }
        if self.status.profile_support.is_none() {
            return Some(ErrorCode::Unsupported);
        }
        let mut next = self.status.configuration_interfaces.clone();
        for u in updates {
            let Some(s) = next.iter_mut().find(|s| s.interface == u.interface) else {
                return Some(ErrorCode::Unsupported);
            };
            if let Some(on) = u.enabled {
                s.enabled = on;
            }
            if let Some(profile) = u.profile {
                if profile != 0 && !self.profiles.iter().any(|p| p.id == profile) {
                    return Some(ErrorCode::NotFound);
                }
                s.profile = profile;
            }
        }
        let enabled: Vec<&p::ConfigurationInterfaceSupport> =
            next.iter().filter(|s| s.enabled).collect();
        if enabled.iter().any(|s| s.profile == 0) {
            return Some(ErrorCode::BadArgs);
        }
        if enabled.iter().any(|a| {
            enabled
                .iter()
                .any(|b| b.interface != a.interface && a.conflicts.contains(&b.interface))
        }) {
            return Some(ErrorCode::Unsupported);
        }
        self.status.configuration_interfaces = next;
        None
    }

    /// Applies rule changes in order, as SetProfileRules does: a saved rule replaces the one
    /// for the same input in its saved form, and a rule that changes nothing forgets it.
    fn set_rules(&mut self, id: u32, changes: &[p::ProfileRuleChange]) -> Option<ErrorCode> {
        let support = self.status.profile_support.clone().unwrap_or_default();
        let mut rules = self.rules.get(&id).cloned().unwrap_or_default();
        for change in changes {
            match &change.change {
                Some(profile_rule_change::Change::Rule(rule)) => {
                    let mut rule = rule.clone();
                    if let Some(profile_rule::Effect::Remap(remap)) = &mut rule.effect {
                        if remap.outputs.len() > support.max_remap_outputs as usize {
                            return Some(ErrorCode::BadArgs);
                        }
                        for o in &mut remap.outputs {
                            let u = o.usage.unwrap_or_default();
                            let carried: Vec<Option<p::Usage>> = support
                                .remap_outputs
                                .iter()
                                .filter(|r| {
                                    r.usage_page == u.usage_page
                                        && (r.min..=r.max).contains(&u.usage)
                                })
                                .map(|r| r.collection)
                                .collect();
                            match (o.collection, carried.as_slice()) {
                                (None, [only]) => o.collection = *only,
                                (Some(c), list) if list.contains(&Some(c)) => {}
                                _ => return Some(ErrorCode::BadArgs),
                            }
                        }
                    }
                    rules.retain(|r| r.input != rule.input);
                    rules.extend(cordial_client::rules::normalized(&support, &rule));
                }
                Some(profile_rule_change::Change::Forget(f)) => {
                    rules.retain(|r| r.input != f.input);
                }
                None => return Some(ErrorCode::BadArgs),
            }
        }
        let key = |r: &p::ProfileRule| {
            let i = r.input.unwrap_or_default();
            (i.usage_page, i.usage)
        };
        rules.sort_by_key(key);
        self.rules.insert(id, rules);
        None
    }

    fn answer(&mut self, command: Command) {
        self.log.push(command.clone());
        if let Some(code) = self.refuse.get(kind_name(&command)) {
            return self.respond(error(*code));
        }
        let profiles = self.status.profile_support.is_some();
        match command {
            Command::GetStatus(_) => {
                let status = self.status.clone();
                self.respond(Some(response::Result::Status(status)));
            }
            Command::SetAdapter(update) => {
                let supported = |t: i32| self.status.transports.iter().any(|s| s.transport == t);
                if !update.transports.iter().all(|t| supported(t.transport)) {
                    return self.respond(error(ErrorCode::Unsupported));
                }
                // The adapter saves a name trimmed, and "" restores the default.
                let name = match update.name.as_deref() {
                    None => None,
                    Some("") => Some("Cordial"),
                    Some(name)
                        if !name.chars().any(char::is_control)
                            && (1..=64).contains(&name.trim().len()) =>
                    {
                        Some(name.trim())
                    }
                    Some(_) => return self.respond(error(ErrorCode::BadArgs)),
                };
                let before = self.status.clone();
                if let Some(code) = self.set_interfaces(&update.configuration_interfaces) {
                    return self.respond(error(code));
                }
                if let Some(name) = name {
                    self.status.name = name.into();
                }
                if let Some(platform) = update.platform {
                    self.status.platform = platform;
                }
                for t in update.transports {
                    if let (Ok(transport), Some(on)) = (Transport::try_from(t.transport), t.enabled)
                    {
                        self.set_transport(transport, on);
                    }
                }
                let status = self.status.clone();
                self.respond(None);
                if status != before {
                    self.event(p::event::Kind::Adapter(status));
                }
            }
            Command::EnterBootloader(_) => self.respond(None),
            Command::StartScan(scan) => {
                if scan.transports.is_empty() {
                    return self.respond(error(ErrorCode::BadArgs));
                }
                // Transports that are unsupported or disabled are left out.
                let usable =
                    self.status.transports.iter().any(|s| {
                        scan.transports.contains(&s.transport) && s.enabled != Some(false)
                    });
                if !usable {
                    return self.respond(error(ErrorCode::Unsupported));
                }
                self.respond(None);
                for c in self.candidates.clone() {
                    self.event(p::event::Kind::ScanFound(c));
                }
                if self.scan_ends {
                    let count = self.candidates.len() as u32;
                    self.event(p::event::Kind::ScanDone(p::ScanDone {
                        count,
                        truncated: false,
                    }));
                }
            }
            Command::StopScan(_) => {
                self.respond(None);
                let count = self.candidates.len() as u32;
                self.event(p::event::Kind::ScanDone(p::ScanDone {
                    count,
                    truncated: false,
                }));
            }
            Command::StartPairing(start) => {
                if !self.candidates.iter().any(|c| c.id == start.candidate) {
                    return self.respond(error(ErrorCode::NotFound));
                }
                self.respond(None);
                self.pairing = Some(start.candidate);
                self.prompt(
                    start.candidate,
                    pairing::Step::Connecting(p::PairingConnecting {}),
                );
                match self.plan.clone() {
                    Plan::EnterCode(_) => self.prompt(
                        start.candidate,
                        pairing::Step::EnterCode(p::EnterCode {
                            kind: CodeKind::Passkey as i32,
                        }),
                    ),
                    Plan::Confirm(passkey) => self.prompt(
                        start.candidate,
                        pairing::Step::ConfirmCode(p::ConfirmCode { passkey }),
                    ),
                    Plan::Done => self.pair_done(),
                }
            }
            Command::AcceptPrompt(accept) => {
                if self.pairing.is_none() {
                    return self.respond(error(ErrorCode::NoPrompt));
                }
                self.respond(None);
                match self.plan.clone() {
                    Plan::EnterCode(code) if code != accept.value => {
                        self.pair_failed(ErrorCode::AuthFailed)
                    }
                    _ => self.pair_done(),
                }
            }
            Command::RejectPrompt(_) => {
                self.respond(None);
                self.pair_failed(ErrorCode::Rejected);
            }
            Command::CancelPairing(_) => {
                self.respond(None);
                self.pair_failed(ErrorCode::Cancelled);
            }
            Command::ListDevices(l) => {
                let all: Vec<p::DeviceListEntry> = self
                    .devices
                    .iter()
                    .map(|d| device_list_entry::Entry::Device(d.clone()))
                    .chain(
                        self.unreadable
                            .iter()
                            .map(|id| device_list_entry::Entry::Unreadable(*id)),
                    )
                    .map(|entry| p::DeviceListEntry { entry: Some(entry) })
                    .collect();
                let (entries, end) = page(
                    &all,
                    paging::device_entry_id,
                    id_after(l.after),
                    self.page_size,
                );
                self.respond(Some(response::Result::Devices(p::DeviceList {
                    entries,
                    end,
                })));
            }
            Command::GetDevice(get) => match self.devices.iter().find(|d| d.id == get.device) {
                Some(d) => {
                    let d = d.clone();
                    self.respond(Some(response::Result::Device(d)));
                }
                None => self.respond(error(ErrorCode::NotFound)),
            },
            Command::SetDevice(update) => {
                let enabled = self
                    .devices
                    .iter()
                    .filter(|d| d.id != update.device && d.enabled)
                    .count();
                if let Some(layers) = &update.profiles {
                    let Some(support) = &self.status.profile_support else {
                        return self.respond(error(ErrorCode::Unsupported));
                    };
                    if layers.profiles.len() > support.max_layers as usize {
                        return self.respond(error(ErrorCode::BadArgs));
                    }
                    if !layers.profiles.iter().all(|id| self.has_profile(*id)) {
                        return self.respond(error(ErrorCode::NotFound));
                    }
                }
                let Some(d) = self.device_mut(update.device) else {
                    return self.respond(error(ErrorCode::NotFound));
                };
                if update.enabled == Some(true) && !d.enabled && enabled >= 7 {
                    return self.respond(Some(response::Result::Error(p::Error {
                        code: ErrorCode::NoCapacity as i32,
                        reason: p::CapacityReason::Enabled as i32,
                        outcome_unknown: false,
                    })));
                }
                if let Some(v) = update.enabled {
                    d.enabled = v;
                    d.inactive = (!v).then_some(p::InactiveReason::Disabled as i32);
                }
                if let Some(v) = update.trusted {
                    d.trusted = v;
                }
                if let Some(v) = update.blocked {
                    d.blocked = v;
                }
                if let Some(layers) = update.profiles {
                    d.profiles = Some(layers);
                }
                for i in &update.integrations {
                    if let Some(on) = i.enabled
                        && let Some(h) = d.integrations.iter_mut().find(|x| x.kind == i.kind)
                    {
                        h.enabled = on;
                    }
                }
                let d = d.clone();
                self.respond(None);
                self.event(p::event::Kind::Device(d));
            }
            Command::ConnectDevice(c) => {
                let Some(d) = self.device_mut(c.device) else {
                    return self.respond(error(ErrorCode::NotFound));
                };
                d.state = DeviceState::Connecting as i32;
                d.paused = false;
                let record = d.clone();
                self.respond(Some(response::Result::Device(record)));
                let d = self.device_mut(c.device).unwrap();
                d.state = DeviceState::Connected as i32;
                self.device_event(c.device);
            }
            Command::DisconnectDevice(c) => {
                let Some(d) = self.device_mut(c.device) else {
                    return self.respond(error(ErrorCode::NotFound));
                };
                d.state = DeviceState::Disconnected as i32;
                d.paused = true;
                let record = d.clone();
                self.respond(Some(response::Result::Device(record)));
                self.device_event(c.device);
            }
            Command::UnpairDevice(u) => {
                self.devices.retain(|d| d.id != u.device);
                self.respond(None);
                self.event(p::event::Kind::DeviceRemoved(p::DeviceRemoved {
                    id: u.device,
                }));
            }
            Command::RefreshDevice(_) => self.respond(None),
            Command::ListProfiles(_)
            | Command::GetProfile(_)
            | Command::CreateProfile(_)
            | Command::CopyProfile(_)
            | Command::DeleteProfile(_)
            | Command::ListProfileRules(_)
            | Command::SetProfileRules(_)
                if !profiles =>
            {
                self.respond(error(ErrorCode::UnknownCommand))
            }
            Command::ListProfiles(l) => {
                let (profiles, end) =
                    page(&self.profiles, |p| p.id, id_after(l.after), self.page_size);
                let entries = profiles
                    .into_iter()
                    .map(|profile| p::ProfileListEntry {
                        entry: Some(profile_list_entry::Entry::Profile(profile)),
                    })
                    .collect();
                self.respond(Some(response::Result::Profiles(p::ProfileList {
                    entries,
                    end,
                })));
            }
            Command::GetProfile(g) => match self.profiles.iter().find(|p| p.id == g.profile) {
                Some(profile) => {
                    let profile = profile.clone();
                    self.respond(Some(response::Result::Profile(profile)));
                }
                None => self.respond(error(ErrorCode::NotFound)),
            },
            Command::CreateProfile(c) => {
                let id = self.add_profile(&c.name, Vec::new());
                let saved = self.profiles.iter().find(|p| p.id == id).unwrap().clone();
                self.respond(Some(response::Result::ProfileCreated(p::ProfileCreated {
                    profile: id,
                })));
                self.event(p::event::Kind::Profile(saved));
            }
            Command::CopyProfile(c) => {
                let Some(rules) = self
                    .has_profile(c.profile)
                    .then(|| self.rules.get(&c.profile).cloned().unwrap_or_default())
                else {
                    return self.respond(error(ErrorCode::NotFound));
                };
                let id = self.add_profile(&c.name, rules);
                let copy = self.profiles.iter().find(|p| p.id == id).unwrap().clone();
                self.respond(Some(response::Result::ProfileCreated(p::ProfileCreated {
                    profile: id,
                })));
                self.event(p::event::Kind::Profile(copy));
            }
            Command::DeleteProfile(d) => {
                if !self.has_profile(d.profile) {
                    return self.respond(error(ErrorCode::NotFound));
                }
                let used = self
                    .status
                    .configuration_interfaces
                    .iter()
                    .any(|s| s.profile == d.profile)
                    || self.devices.iter().any(|x| {
                        x.profiles
                            .as_ref()
                            .is_some_and(|l| l.profiles.contains(&d.profile))
                    });
                if used {
                    return self.respond(error(ErrorCode::InUse));
                }
                self.profiles.retain(|p| p.id != d.profile);
                self.rules.remove(&d.profile);
                self.respond(None);
                self.event(p::event::Kind::ProfileRemoved(p::ProfileRemoved {
                    id: d.profile,
                }));
            }
            Command::ListProfileRules(l) => {
                if !self.has_profile(l.profile) {
                    return self.respond(error(ErrorCode::NotFound));
                }
                let all = self.rules.get(&l.profile).cloned().unwrap_or_default();
                let after = l.after.map(|u| (u.usage_page, u.usage));
                let (rules, end) = page(&all, rule_key, after, self.page_size);
                self.respond(Some(response::Result::ProfileRules(p::ProfileRules {
                    profile: l.profile,
                    rules,
                    end,
                })));
            }
            Command::SetProfileRules(s) => {
                if !self.has_profile(s.profile) {
                    return self.respond(error(ErrorCode::NotFound));
                }
                if let Some(code) = self.set_rules(s.profile, &s.changes) {
                    return self.respond(error(code));
                }
                self.respond(None);
                let rules = self.rules.get(&s.profile).cloned().unwrap_or_default();
                let mut changed = p::ProfileRulesChanged {
                    profile: s.profile,
                    ..Default::default()
                };
                for change in &s.changes {
                    let input = match &change.change {
                        Some(profile_rule_change::Change::Rule(r)) => r.input,
                        Some(profile_rule_change::Change::Forget(f)) => f.input,
                        None => None,
                    };
                    match rules.iter().find(|r| r.input == input) {
                        Some(rule) => changed.changed.push(rule.clone()),
                        None => changed.removed.extend(input),
                    }
                }
                self.event(p::event::Kind::ProfileRulesChanged(changed));
                if self.update_roles(s.profile) {
                    let profile = self
                        .profiles
                        .iter()
                        .find(|p| p.id == s.profile)
                        .unwrap()
                        .clone();
                    self.event(p::event::Kind::Profile(profile));
                }
            }
            Command::ListWarnings(l) => {
                let all = self.warnings.get(&l.device).cloned().unwrap_or_default();
                let after = l.after.as_ref().map(warning_key);
                let (warnings, end) = page(&all, warning_key, after, self.page_size);
                self.respond(Some(response::Result::Warnings(p::DeviceWarnings {
                    device: l.device,
                    warnings,
                    end,
                })));
            }
            Command::ListSettings(l) => {
                let all = self.settings.get(&l.device).cloned().unwrap_or_default();
                let after = l.after.map(|r| (r.integration, r.key));
                let (settings, end) = page(&all, setting_key, after, self.page_size);
                self.respond(Some(response::Result::Settings(p::DeviceSettings {
                    device: l.device,
                    settings,
                    end,
                })));
            }
            Command::SetSettings(set) => {
                let list = self.settings.entry(set.device).or_default();
                for change in &set.changes {
                    let Some(s) = list.iter_mut().find(|s| s.key == change.key) else {
                        continue;
                    };
                    match &change.change {
                        Some(setting_change::Change::Value(p::Value {
                            value: Some(Value::Integer(n)),
                        })) => {
                            if let Some(setting::Type::Integer(t)) = &mut s.r#type {
                                t.saved = Some(*n);
                            }
                            s.status = Some(setting::Status::State(SettingState::Pending as i32));
                        }
                        Some(setting_change::Change::Forget(_)) => {
                            if let Some(setting::Type::Integer(t)) = &mut s.r#type {
                                t.saved = None;
                            }
                            s.status = None;
                        }
                        _ => {}
                    }
                }
                self.respond(None);
                let list = self.settings.get_mut(&set.device).unwrap();
                for s in list.iter_mut() {
                    if let Some(setting::Type::Integer(t)) = &mut s.r#type
                        && t.saved.is_some()
                    {
                        t.value = t.saved;
                        s.status = Some(setting::Status::State(SettingState::Applied as i32));
                    }
                }
                let changed = list
                    .iter()
                    .filter(|s| set.changes.iter().any(|c| c.key == s.key))
                    .cloned()
                    .collect();
                self.event(p::event::Kind::SettingsChanged(p::SettingsChanged {
                    device: set.device,
                    changed,
                    removed: Vec::new(),
                }));
            }
            Command::ListFeatures(_) => {
                self.respond(Some(response::Result::Features(p::FeatureList {
                    end: true,
                    features: vec![p::Feature {
                        integration: IntegrationKind::Hidpp as i32,
                        supported: true,
                        detail: Some(p::feature::Detail::Hidpp(p::HidppFeature {
                            index: 1,
                            id: 0x0001,
                            version: 2,
                            flags: 0,
                        })),
                    }],
                })))
            }
            Command::ListFiles(l) => match self.files.get(&l.path).cloned() {
                Some(all) => {
                    let after = (!l.after.is_empty()).then_some(l.after);
                    let (entries, end) = page(&all, |e| e.name.clone(), after, self.page_size);
                    self.respond(Some(response::Result::Files(p::FileList { entries, end })))
                }
                None => self.respond(error(ErrorCode::NotFound)),
            },
            Command::ReadFile(r) => match self.data.get(&r.path).cloned() {
                Some(data) => self.respond(Some(response::Result::File(p::FileData { data }))),
                None => self.respond(error(ErrorCode::NotFound)),
            },
        }
    }
}

/// A shared simulated Dongle that connections attach to.
#[derive(Clone, Default)]
pub struct Dongle(pub Arc<Mutex<Sim>>);

type Handler = Box<dyn FnMut(Received<'_>) + Send>;

impl Dongle {
    pub fn with(f: impl FnOnce(&mut Sim)) -> Self {
        let d = Self::default();
        f(&mut d.0.lock().unwrap());
        d
    }

    /// Starts a session: the Dongle answers every request frame it receives.
    pub fn connect(&self, handler: Handler) -> io::Result<Connection> {
        let (to_client, client_reader) = pipe();
        let (client_writer, mut from_client) = pipe();
        // The session starts with the Dongle's delimiter.
        let _ = to_client.clone().write_all(&[frame::DELIMITER]);
        let session = {
            let mut sim = self.0.lock().unwrap();
            sim.out = Some(to_client);
            sim.session += 1;
            sim.session
        };
        let sim = self.0.clone();
        thread::spawn(move || {
            let mut decoder = Decoder::new(Some(MAX_REQUEST_BYTES));
            let mut buffer = [0; 256];
            loop {
                let n = match from_client.read(&mut buffer) {
                    Ok(0) => {
                        let mut sim = sim.lock().unwrap();
                        if sim.session == session {
                            sim.out = None;
                        }
                        return;
                    }
                    Ok(n) => n,
                    Err(_) => continue,
                };
                for &byte in &buffer[..n] {
                    if let Some(Ok(frame)) = decoder.push(byte)
                        && let Ok(request) = p::Request::decode(frame)
                        && let Some(command) = request.command
                    {
                        sim.lock().unwrap().answer(command);
                    }
                }
            }
        });
        Ok(Connection::with_handler(
            client_reader,
            client_writer,
            handler,
        ))
    }

    /// Ends the Dongle's side of the session, as unplugging it would.
    pub fn unplug(&self) {
        self.0.lock().unwrap().out = None;
    }

    pub fn sent(&self) -> Vec<&'static str> {
        self.0.lock().unwrap().log.iter().map(kind_name).collect()
    }

    pub fn event(&self, kind: p::event::Kind) {
        self.0.lock().unwrap().event(kind);
    }

    pub fn connector(
        &self,
    ) -> impl Fn(&str, Handler) -> io::Result<Connection> + Send + Sync + 'static + use<> {
        let dongle = self.clone();
        move |_, handler| dongle.connect(handler)
    }
}
