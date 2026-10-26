//! A simulated Dongle over in-memory byte streams. It answers each request frame from a small
//! model and sends the events a real Dongle would.
use cordial_client::{Connection, Received, order};
use cordial_protocol::{
    self as p, CodeKind, ConfigurationInterface, DeviceState, ErrorCode, IntegrationKind,
    MAX_REQUEST_BYTES, ResourceType, Role, SettingState, Transport, device_list_entry,
    frame::{self, Decoder},
    message, pairing, profile_list_entry, profile_rule, profile_rule_change,
    request::Command,
    response, settings_object, settings_update,
};
mod runtime;
pub use runtime::{Desktop, desktop};

use prost::Message as _;
use std::{
    collections::BTreeMap,
    io::{self, Read, Write},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
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

/// A client's reading half, counted while held, as a serial port's handle is.
struct Held(PipeReader, Arc<AtomicUsize>);

impl Read for Held {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        self.0.read(out)
    }
}

impl Drop for Held {
    fn drop(&mut self) {
        self.1.fetch_sub(1, Ordering::SeqCst);
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
    /// Displays a passkey until the simulated device finishes pairing.
    ShowCode(String),
    /// Saves the device at once.
    Done,
}

/// Everything the simulated Dongle knows.
pub struct Sim {
    pub status: p::Status,
    pub hello: p::AdapterHello,
    pub answer_hello: bool,
    pub hellos: usize,
    /// Saved devices; listed in ascending ID order.
    pub devices: Vec<p::Device>,
    /// Saved devices whose records can't be read, listed with a storage read error.
    pub unreadable: Vec<u32>,
    pub settings: BTreeMap<u32, Vec<p::SettingsObject>>,
    pub candidates: Vec<p::Candidate>,
    /// A scan ends at once with scan_done.
    pub scan_ends: bool,
    /// The error a scan that ends at once reports, keeping its candidates.
    pub scan_error: Option<p::Error>,
    /// Replaces the error a failing pairing ends with.
    pub pairing_error: Option<p::Error>,
    pub plan: Plan,
    /// Candidate-specific pairing plans and saved records, overriding `plan` and `paired`.
    pub pairings: BTreeMap<u32, (Plan, p::Device)>,
    /// Pairing saves the device with this record.
    pub paired: p::Device,
    pub files: BTreeMap<String, Vec<p::FileEntry>>,
    pub data: BTreeMap<String, Vec<u8>>,
    /// Every command received, in order.
    pub log: Vec<Command>,
    /// Commands answered with this error instead.
    pub refuse: BTreeMap<&'static str, p::Error>,
    /// Listings that fail after their first Message, or settings writes rejected before saving, once.
    pub fail_late: BTreeMap<&'static str, ErrorCode>,
    /// Commands that take effect at once but whose replies delay subsequent requests as well.
    pub slow: BTreeMap<&'static str, Duration>,
    /// Saved profiles and their rules; used only when the status has profile support.
    pub profiles: Vec<p::Profile>,
    pub rules: BTreeMap<u32, Vec<p::ProfileRule>>,
    /// The most entries one Message of a listing's response frame holds.
    pub batch_size: usize,
    next_profile: u32,
    pairing: Option<u32>,
    pairing_reconnect: Option<Sender<()>>,
    out: Option<PipeWriter>,
    /// Counts sessions, so a session that ends after the next one started leaves it alone.
    pub session: u64,
    /// Client reading halves not yet released.
    pub handles: Arc<AtomicUsize>,
}

pub fn resource(kind: ResourceType, count: Option<u32>, limit: Option<u32>) -> p::ResourceCapacity {
    p::ResourceCapacity {
        r#type: kind as i32,
        count,
        limit,
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
                enabled: Some(true),
                resources: vec![resource(ResourceType::Devices, None, Some(1))],
            },
            p::TransportSupport {
                transport: Transport::Ble as i32,
                enabled: Some(true),
                resources: vec![resource(ResourceType::Devices, None, Some(7))],
            },
        ],
        information: Some(p::AdapterInformation {
            firmware_version: Some("1.2.3-dev".into()),
            board_name: None,
            development_build: Some(true),
            storage_format: Some(2),
            target_storage_format: Some(2),
        }),
        ..Default::default()
    }
}

/// Advertises HID and XInput gamepad output, four slots each.
pub fn enable_gamepads(status: &mut p::Status) {
    let mode = |mode: p::OutputMode, slots: u32| p::OutputModeSupport {
        mode: mode as i32,
        resources: vec![resource(ResourceType::Slots, None, Some(slots))],
    };
    status.output_support = vec![p::OutputSupport {
        role: Role::Gamepad as i32,
        modes: vec![
            mode(p::OutputMode::Disabled, 0),
            mode(p::OutputMode::Hid, 4),
            mode(p::OutputMode::Xinput, 4),
        ],
    }];
}

pub fn usage(usage_page: u32, usage: u32) -> p::Usage {
    p::Usage { usage_page, usage }
}

fn range(
    kind: p::usage_range::Kind,
    page: u32,
    min: u32,
    max: u32,
    collection: (u32, u32),
) -> p::UsageRange {
    p::UsageRange {
        kind: kind as i32,
        collection: Some(usage(collection.0, collection.1)),
        usage_page: page,
        min,
        max,
    }
}

/// Generic Desktop Keyboard, Mouse, Game Pad, and Consumer Control.
pub const KEYBOARD: (u32, u32) = (0x01, 0x06);
pub const MOUSE: (u32, u32) = (0x01, 0x02);
pub const GAMEPAD: (u32, u32) = (0x01, 0x05);
pub const CONSUMER: (u32, u32) = (0x0c, 0x01);

/// Firmware with profiles: keys, mouse and gamepad buttons, and media controls remap, pointer
/// motion and wheels scale, gamepad axes move to other axes, and VIA and Vial are offered,
/// conflicting and disabled.
pub fn enable_profiles(status: &mut p::Status) {
    status.profile_support = Some(p::ProfileSupport {
        ranges: vec![
            range(p::usage_range::Kind::RemapInput, 0x07, 0x04, 0xe7, KEYBOARD),
            range(p::usage_range::Kind::RemapInput, 0x09, 1, 16, MOUSE),
            range(p::usage_range::Kind::RemapInput, 0x09, 1, 16, GAMEPAD),
            range(
                p::usage_range::Kind::RemapInput,
                0x0c,
                0x00,
                0x3ff,
                CONSUMER,
            ),
            range(p::usage_range::Kind::ScaleInput, 0x01, 0x30, 0x38, MOUSE),
            range(
                p::usage_range::Kind::RemapOutput,
                0x07,
                0x04,
                0xe7,
                KEYBOARD,
            ),
            range(p::usage_range::Kind::RemapOutput, 0x09, 1, 8, MOUSE),
            range(
                p::usage_range::Kind::RemapOutput,
                0x0c,
                0x00,
                0x3ff,
                CONSUMER,
            ),
            range(p::usage_range::Kind::AxisInput, 0x01, 0x30, 0x35, GAMEPAD),
            range(p::usage_range::Kind::AxisOutput, 0x01, 0x30, 0x35, GAMEPAD),
        ],
        resources: vec![
            resource(ResourceType::Memory, Some(1024), Some(8192)),
            resource(ResourceType::Outputs, None, Some(4)),
            resource(ResourceType::Layers, None, Some(3)),
        ],
    });
    status.configuration_interfaces = [
        (ConfigurationInterface::Via, ConfigurationInterface::Vial),
        (ConfigurationInterface::Vial, ConfigurationInterface::Via),
    ]
    .into_iter()
    .map(|(i, other)| p::ConfigurationInterfaceSupport {
        interface: i as i32,
        enabled: false,
        active: false,
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

fn applied() -> Option<p::SettingStatus> {
    Some(p::SettingStatus {
        result: Some(p::setting_status::Result::State(
            SettingState::Applied as i32,
        )),
    })
}

/// A HID++ pointer sensor with a sensitivity range.
pub fn dpi(index: u32, value: i64, saved: Option<i64>) -> p::SettingsObject {
    p::SettingsObject {
        integration: IntegrationKind::Hidpp as i32,
        group: Some(settings_object::Group::PointerSensor(
            p::PointerSensorSettings {
                index,
                dpi: Some(p::IntegerSetting {
                    value: Some(value),
                    saved,
                    status: saved.and_then(|_| applied()),
                    limits: Some(p::integer_setting::Limits::Range(p::IntegerRange {
                        min: 400,
                        max: 4000,
                        step: 50,
                    })),
                }),
            },
        )),
    }
}

impl Default for Sim {
    fn default() -> Self {
        Self {
            status: status(true),
            hello: p::AdapterHello {
                protocol_version: Some(p::CURRENT_PROTOCOL_VERSION),
                ..Default::default()
            },
            answer_hello: true,
            hellos: 0,
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
            candidates: vec![candidate(1, "New Keyboard")],
            scan_ends: true,
            scan_error: None,
            pairing_error: None,
            plan: Plan::Done,
            pairings: BTreeMap::new(),
            paired: device(9, "New Keyboard", Transport::Ble),
            files: BTreeMap::new(),
            data: BTreeMap::new(),
            log: Vec::new(),
            refuse: BTreeMap::new(),
            fail_late: BTreeMap::new(),
            slow: BTreeMap::new(),
            profiles: Vec::new(),
            rules: BTreeMap::new(),
            batch_size: 2,
            next_profile: 1,
            pairing: None,
            pairing_reconnect: None,
            out: None,
            session: 0,
            handles: Arc::default(),
        }
    }
}

fn kind_name(command: &Command) -> &'static str {
    match command {
        Command::GetStatus(_) => "get_status",
        Command::SetAdapter(_) => "set_adapter",
        Command::Restart(_) => "restart",
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
        Command::ListSettings(_) => "list_settings",
        Command::SetSettings(_) => "set_settings",
        Command::ListFeatures(_) => "list_features",
        Command::ListFiles(_) => "list_files",
        Command::ReadFile(_) => "read_file",
        Command::WriteFile(_) => "write_file",
        Command::MoveFile(_) => "move_file",
        Command::DeleteFile(_) => "delete_file",
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
    Some(response::Result::Error(refusal(code)))
}

/// An adapter error with only a code.
pub fn refusal(code: ErrorCode) -> p::Error {
    p::Error {
        code: code as i32,
        ..Default::default()
    }
}

/// A saved record's failed read.
pub fn read_error(id: u32) -> p::RecordError {
    p::RecordError {
        id,
        error: Some(p::Error {
            code: ErrorCode::StorageFailed as i32,
            reason: p::ErrorReason::Storage as i32,
            outcome: p::ErrorOutcome::NoEffect as i32,
            related: Vec::new(),
        }),
    }
}

/// `entries` in key order.
fn sorted<T: Clone, K: Ord>(entries: &[T], key: impl Fn(&T) -> K) -> Vec<T> {
    let mut all = entries.to_vec();
    all.sort_by_key(|e| key(e));
    all
}

fn settings_key(r: &p::SettingsRef) -> (i32, i32, Option<u32>) {
    (r.integration, r.group, r.index)
}

/// A resource's limit; a missing one allows anything.
fn limit(resources: &[p::ResourceCapacity], kind: ResourceType) -> u32 {
    resources
        .iter()
        .find(|r| r.r#type == kind as i32)
        .and_then(|r| r.limit)
        .unwrap_or(u32::MAX)
}

/// Rules in order of input, then collection.
fn ref_key(input: Option<p::Usage>, collection: Option<p::Usage>) -> (u32, u32, u32, u32) {
    let (i, c) = (input.unwrap_or_default(), collection.unwrap_or_default());
    (i.usage_page, i.usage, c.usage_page, c.usage)
}

fn rule_key(r: &p::ProfileRule) -> (u32, u32, u32, u32) {
    ref_key(r.input, r.collection)
}

/// The role a rule adds, from its input's collection.
fn rule_role(rule: &p::ProfileRule) -> Option<Role> {
    let c = rule.collection?;
    Some(match (c.usage_page, c.usage) {
        KEYBOARD => Role::Keyboard,
        MOUSE => Role::Mouse,
        GAMEPAD => Role::Gamepad,
        CONSUMER => Role::ConsumerControl,
        (0x01, 0x80) => Role::SystemControl,
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

    /// Sends the final event and closes the stream after its delimiter.
    pub fn end_session(&mut self, reason: i32) {
        self.event(p::event::Kind::SessionEnding(p::SessionEnding { reason }));
        self.out = None;
    }

    fn respond(&mut self, result: Option<response::Result>) {
        self.send(message::Kind::Response(p::Response { result }));
    }

    /// Answers a listing with every entry in one frame: Messages of `batch_size` entries encoded
    /// back to back, as the firmware streams them. A late failure for `command` follows the
    /// first Message.
    fn list<T: Clone>(
        &mut self,
        command: &'static str,
        entries: Vec<T>,
        result: impl Fn(Vec<T>) -> response::Result,
    ) {
        let late = self.fail_late.remove(command);
        let piece = |result| {
            p::Message {
                kind: Some(message::Kind::Response(p::Response {
                    result: Some(result),
                })),
            }
            .encode_to_vec()
        };
        let mut bytes = Vec::new();
        let mut rest = entries.as_slice();
        loop {
            let (batch, more) = rest.split_at(self.batch_size.min(rest.len()));
            bytes.extend(piece(result(batch.to_vec())));
            rest = more;
            if let Some(code) = late {
                bytes.extend(piece(response::Result::Error(p::Error {
                    code: code as i32,
                    ..Default::default()
                })));
                break;
            }
            if rest.is_empty() {
                break;
            }
        }
        let mut framed = Vec::new();
        frame::encode_bytes(&bytes, &mut framed);
        if let Some(out) = &mut self.out {
            let _ = out.write_all(&framed);
        }
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
        if self.pairings.remove(&candidate).is_some() {
            self.candidates.retain(|c| c.id != candidate);
        }
        self.event(p::event::Kind::Device(device.clone()));
        self.prompt(
            candidate,
            pairing::Step::Done(p::PairingDone { device: device.id }),
        );
        if let Some(notify) = &self.pairing_reconnect {
            let _ = notify.send(());
        }
    }

    fn pair_failed(&mut self, code: ErrorCode) {
        let candidate = self.pairing.take().unwrap_or_default();
        let error = self.pairing_error.take().unwrap_or(p::Error {
            code: code as i32,
            outcome: p::ErrorOutcome::NoEffect as i32,
            ..Default::default()
        });
        self.prompt(candidate, pairing::Step::Error(error));
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
            // Every enabled interface fits.
            if let Some(on) = u.enabled {
                s.enabled = on;
                s.active = on;
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
    /// for the same input and collection in its saved form, and a rule that changes nothing
    /// forgets it.
    fn set_rules(&mut self, id: u32, changes: &[p::ProfileRuleChange]) -> Option<ErrorCode> {
        let support = self.status.profile_support.clone().unwrap_or_default();
        let mut rules = self.rules.get(&id).cloned().unwrap_or_default();
        for change in changes {
            match &change.change {
                Some(profile_rule_change::Change::Rule(rule)) => {
                    let mut rule = rule.clone();
                    if let Some(profile_rule::Effect::Remap(remap)) = &mut rule.effect {
                        let max = limit(&support.resources, ResourceType::Outputs);
                        if remap.outputs.len() > max as usize {
                            return Some(ErrorCode::BadArgs);
                        }
                        for o in &mut remap.outputs {
                            let u = o.usage.unwrap_or_default();
                            let carried: Vec<Option<p::Usage>> = support
                                .ranges
                                .iter()
                                .filter(|r| {
                                    r.kind == p::usage_range::Kind::RemapOutput as i32
                                        && r.usage_page == u.usage_page
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
                    rules.retain(|r| rule_key(r) != rule_key(&rule));
                    rules.extend(cordial_client::rules::normalized(&support, &rule));
                }
                Some(profile_rule_change::Change::Forget(f)) => {
                    rules.retain(|r| rule_key(r) != ref_key(f.input, f.collection));
                }
                None => return Some(ErrorCode::BadArgs),
            }
        }
        rules.sort_by_key(rule_key);
        self.rules.insert(id, rules);
        None
    }

    fn answer(&mut self, command: Command) {
        self.log.push(command.clone());
        if let Some(e) = self.refuse.get(kind_name(&command)) {
            return self.respond(Some(response::Result::Error(e.clone())));
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
            Command::Restart(_) => {
                self.respond(None);
                self.end_session(p::SessionEndingReason::Reboot as i32);
            }
            Command::EnterBootloader(_) => {
                self.respond(None);
                self.end_session(p::SessionEndingReason::Bootloader as i32);
            }
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
                    let error = self.scan_error.clone();
                    self.event(p::event::Kind::ScanDone(p::ScanDone { count, error }));
                }
            }
            Command::StopScan(_) => {
                self.respond(None);
                let count = self.candidates.len() as u32;
                self.event(p::event::Kind::ScanDone(p::ScanDone { count, error: None }));
            }
            Command::StartPairing(start) => {
                if !self.candidates.iter().any(|c| c.id == start.candidate) {
                    return self.respond(error(ErrorCode::NotFound));
                }
                if let Some((plan, device)) = self.pairings.get(&start.candidate) {
                    self.plan = plan.clone();
                    self.paired = device.clone();
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
                    Plan::ShowCode(value) => self.prompt(
                        start.candidate,
                        pairing::Step::ShowCode(p::ShowCode {
                            kind: CodeKind::Passkey as i32,
                            value,
                        }),
                    ),
                    Plan::Done => self.pair_done(),
                }
            }
            Command::AcceptPrompt(accept) => {
                if self.pairing.is_none() || matches!(self.plan, Plan::ShowCode(_)) {
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
            Command::ListDevices(_) => {
                let all: Vec<p::DeviceListEntry> = self
                    .devices
                    .iter()
                    .map(|d| device_list_entry::Entry::Device(d.clone()))
                    .chain(
                        self.unreadable
                            .iter()
                            .map(|id| device_list_entry::Entry::Error(read_error(*id))),
                    )
                    .map(|entry| p::DeviceListEntry { entry: Some(entry) })
                    .collect();
                let entries = sorted(&all, order::device_entry_id);
                self.list("list_devices", entries, |entries| {
                    response::Result::Devices(p::DeviceList { entries })
                });
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
                    if layers.profiles.len()
                        > limit(&support.resources, ResourceType::Layers) as usize
                    {
                        return self.respond(error(ErrorCode::BadArgs));
                    }
                    if !layers.profiles.iter().all(|id| self.has_profile(*id)) {
                        return self.respond(error(ErrorCode::NotFound));
                    }
                }
                let supported =
                    |o: &p::OutputPreference| {
                        o.mode == p::OutputMode::Disabled as i32
                            || self.status.output_support.iter().any(|s| {
                                s.role == o.role && s.modes.iter().any(|m| m.mode == o.mode)
                            })
                    };
                if !update.output_preferences.iter().all(supported) {
                    return self.respond(error(ErrorCode::Unsupported));
                }
                let Some(d) = self.device_mut(update.device) else {
                    return self.respond(error(ErrorCode::NotFound));
                };
                if update.enabled == Some(true) && !d.enabled && enabled >= 7 {
                    return self.respond(Some(response::Result::Error(p::Error {
                        code: ErrorCode::NoCapacity as i32,
                        reason: p::ErrorReason::EnabledDevices as i32,
                        ..Default::default()
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
                for o in update.output_preferences {
                    d.output_preferences.retain(|saved| saved.role != o.role);
                    if o.mode != p::OutputMode::Disabled as i32 {
                        d.output_preferences.push(o);
                    }
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
            Command::ListProfiles(_) => {
                let entries = sorted(&self.profiles, |p| p.id)
                    .into_iter()
                    .map(|profile| p::ProfileListEntry {
                        entry: Some(profile_list_entry::Entry::Profile(profile)),
                    })
                    .collect();
                self.list("list_profiles", entries, |entries| {
                    response::Result::Profiles(p::ProfileList { entries })
                });
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
                self.list("list_profile_rules", sorted(&all, rule_key), |rules| {
                    response::Result::ProfileRules(p::ProfileRules {
                        profile: l.profile,
                        rules,
                    })
                });
            }
            Command::SetProfileRules(s) => {
                if !self.has_profile(s.profile) {
                    return self.respond(error(ErrorCode::NotFound));
                }
                if let Some(code) = self.set_rules(s.profile, &s.changes) {
                    return self.respond(error(code));
                }
                self.respond(None);
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
            Command::ListSettings(l) => {
                let all = self.settings.get(&l.device).cloned().unwrap_or_default();
                let key = |o: &p::SettingsObject| settings_key(&order::settings_ref(o));
                self.list("list_settings", sorted(&all, key), |objects| {
                    response::Result::Settings(p::DeviceSettings {
                        device: l.device,
                        objects,
                    })
                });
            }
            Command::SetSettings(set) => {
                if let Some(code) = self.fail_late.remove("set_settings") {
                    return self.respond(Some(response::Result::Error(p::Error {
                        code: code as i32,
                        outcome: p::ErrorOutcome::NoEffect as i32,
                        ..Default::default()
                    })));
                }
                let list = self.settings.entry(set.device).or_default();
                let before = list.clone();
                for update in &set.changes {
                    crate::settings::take_update(list, update);
                    for object in list.iter_mut() {
                        apply_settings(object, update);
                    }
                }
                let changed = list
                    .iter()
                    .zip(before)
                    .filter(|(object, before)| **object != *before)
                    .map(|(object, _)| object.clone())
                    .collect();
                self.respond(None);
                self.event(p::event::Kind::SettingsChanged(p::SettingsChanged {
                    device: set.device,
                    changed,
                    removed: Vec::new(),
                }));
            }
            Command::ListFeatures(_) => {
                self.respond(Some(response::Result::Features(p::FeatureList {
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
                    let entries = sorted(&all, |e| e.name.clone());
                    self.list("list_files", entries, |entries| {
                        response::Result::Files(p::FileList { entries })
                    })
                }
                None => self.respond(error(ErrorCode::NotFound)),
            },
            Command::ReadFile(r) => match self.data.get(&r.path).cloned() {
                Some(data) => {
                    let start = (r.offset as usize).min(data.len());
                    let end = r
                        .length
                        .map_or(data.len(), |n| (start + n as usize).min(data.len()));
                    self.respond(Some(response::Result::File(p::FileData {
                        path: r.path,
                        offset: r.offset,
                        size: data.len() as u64,
                        data: data[start..end].to_vec(),
                    })))
                }
                None => self.respond(error(ErrorCode::NotFound)),
            },
            Command::WriteFile(w) => {
                let data = self.data.entry(w.path).or_default();
                let start = w.offset as usize;
                data.resize(data.len().max(start + w.data.len()), 0);
                data[start..start + w.data.len()].copy_from_slice(&w.data);
                self.respond(None)
            }
            Command::MoveFile(m) => match self.data.remove(&m.source) {
                Some(data) => {
                    self.data.insert(m.destination, data);
                    self.respond(None)
                }
                None => self.respond(error(ErrorCode::NotFound)),
            },
            Command::DeleteFile(d) => match self.data.remove(&d.path) {
                Some(_) => self.respond(None),
                None => self.respond(error(ErrorCode::NotFound)),
            },
        }
    }
}

fn apply_settings(object: &mut p::SettingsObject, update: &p::SettingsUpdate) {
    if object.integration != update.integration {
        return;
    }
    macro_rules! apply {
        ($setting:expr, $update:expr) => {
            if $update.is_some()
                && let Some(setting) = $setting.as_mut()
                && let Some(saved) = setting.saved
            {
                setting.value = Some(saved);
                setting.status = applied();
            }
        };
    }
    use settings_object::Group as S;
    use settings_update::Group as U;
    match (&mut object.group, &update.group) {
        (Some(S::Keyboard(s)), Some(U::Keyboard(u))) => {
            apply!(s.platform, u.platform);
            apply!(s.function_row, u.function_row);
        }
        (Some(S::Power(s)), Some(U::Power(u))) => apply!(s.auto_off, u.auto_off),
        (Some(S::Backlight(s)), Some(U::Backlight(u))) => {
            apply!(s.enabled, u.enabled);
            apply!(s.mode, u.mode);
            apply!(s.brightness, u.brightness);
            apply!(s.startup_effect, u.startup_effect);
            apply!(s.dial_effect, u.dial_effect);
            apply!(s.power_save, u.power_save);
            apply!(s.effect, u.effect);
            if let (Some(s), Some(u)) = (&mut s.delay, &u.delay) {
                apply!(s.hands_away, u.hands_away);
                apply!(s.hands_nearby, u.hands_nearby);
                apply!(s.external_power, u.external_power);
            }
        }
        (Some(S::PointerSensor(s)), Some(U::PointerSensor(u))) if s.index == u.index => {
            apply!(s.dpi, u.dpi);
        }
        (Some(S::Wheel(s)), Some(U::Wheel(u))) => {
            apply!(s.mode, u.mode);
            apply!(s.free_spin_threshold, u.free_spin_threshold);
            apply!(s.invert, u.invert);
        }
        (Some(S::Thumbwheel(s)), Some(U::Thumbwheel(u))) => apply!(s.invert, u.invert),
        _ => {}
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
        let (session, handles) = {
            let mut sim = self.0.lock().unwrap();
            sim.out = Some(to_client);
            sim.session += 1;
            (sim.session, sim.handles.clone())
        };
        handles.fetch_add(1, Ordering::SeqCst);
        let client_reader = Held(client_reader, handles);
        let shared = self.0.clone();
        thread::spawn(move || {
            let mut decoder = Decoder::new(Some(MAX_REQUEST_BYTES));
            let mut negotiated = false;
            let mut buffer = [0; 256];
            loop {
                let n = match from_client.read(&mut buffer) {
                    Ok(0) => {
                        let mut sim = shared.lock().unwrap();
                        if sim.session == session {
                            sim.out = None;
                        }
                        return;
                    }
                    Ok(n) => n,
                    Err(_) => continue,
                };
                for &byte in &buffer[..n] {
                    if let Some(Ok(frame)) = decoder.push(byte) {
                        if !negotiated {
                            assert_eq!(
                                p::handshake::decode_client_hello(frame).unwrap(),
                                p::ClientHello {}
                            );
                            let mut bytes = Vec::new();
                            let mut sim = shared.lock().unwrap();
                            if sim.session != session || sim.out.is_none() {
                                return;
                            }
                            sim.hellos += 1;
                            frame::encode(&sim.hello, &mut bytes);
                            if sim.answer_hello
                                && let Some(out) = sim.out.as_mut()
                            {
                                let _ = out.write_all(&bytes);
                            }
                            negotiated = true;
                            continue;
                        }
                        if let Ok(request) = p::Request::decode(frame)
                            && let Some(command) = request.command
                        {
                            let mut sim = shared.lock().unwrap();
                            if sim.session != session || sim.out.is_none() {
                                return;
                            }
                            let Some(delay) = sim.slow.get(kind_name(&command)).copied() else {
                                sim.answer(command);
                                continue;
                            };
                            let (held, replies) = mpsc::channel();
                            let out = sim.out.replace(PipeWriter(held));
                            sim.answer(command);
                            let remains_open = sim.out.is_some();
                            sim.out = out;
                            drop(sim);
                            thread::sleep(delay);
                            let mut sim = shared.lock().unwrap();
                            if sim.session != session {
                                return;
                            }
                            if let Some(out) = sim.out.as_mut() {
                                for bytes in replies.try_iter() {
                                    let _ = out.write_all(&bytes);
                                }
                            }
                            if !remains_open {
                                sim.out = None;
                            }
                        }
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

    /// Sends an intentional-ending event through the framed stream, then closes it.
    pub fn end_session(&self, reason: i32) {
        self.0.lock().unwrap().end_session(reason);
    }

    /// Client reading halves not yet released.
    pub fn handles(&self) -> usize {
        self.0.lock().unwrap().handles.load(Ordering::SeqCst)
    }

    /// Sessions opened so far.
    pub fn sessions(&self) -> u64 {
        self.0.lock().unwrap().session
    }

    pub fn sent(&self) -> Vec<&'static str> {
        self.0.lock().unwrap().log.iter().map(kind_name).collect()
    }

    pub fn event(&self, kind: p::event::Kind) {
        self.0.lock().unwrap().event(kind);
    }

    /// Updates a battery reading and delivers the device event through the framed connection.
    pub fn set_battery(&self, id: u32, level: Option<u32>, charging: Option<bool>) -> bool {
        if level.is_some_and(|level| level > 100) {
            return false;
        }
        let mut sim = self.0.lock().unwrap();
        let Some(device) = sim.device_mut(id) else {
            return false;
        };
        if device.state() != DeviceState::Connected {
            return false;
        }
        device.information.get_or_insert_default().battery =
            Some(p::BatteryInformation { level, charging });
        sim.device_event(id);
        true
    }

    /// Changes a device's link state and sends its new record over the connection.
    pub fn set_connected(&self, id: u32, connected: bool) -> bool {
        let mut sim = self.0.lock().unwrap();
        let Some(device) = sim.device_mut(id) else {
            return false;
        };
        device.state = if connected {
            DeviceState::Connected
        } else {
            DeviceState::Disconnected
        } as i32;
        device.paused = false;
        if !connected {
            device.information = None;
        }
        sim.device_event(id);
        true
    }

    /// Makes the next connection require migration and closes the current connection.
    pub fn require_migration(&self) {
        let mut sim = self.0.lock().unwrap();
        sim.status
            .errors
            .retain(|e| e.code() != ErrorCode::StorageMigrationRequired);
        sim.status.errors.push(p::Error {
            code: ErrorCode::StorageMigrationRequired as i32,
            reason: p::ErrorReason::Storage as i32,
            outcome: p::ErrorOutcome::NoEffect as i32,
            ..Default::default()
        });
        sim.status.ready = false;
        sim.out = None;
    }

    /// Finishes a display-code prompt as if the user typed the code on the device.
    pub fn complete_pairing(&self) -> bool {
        let mut sim = self.0.lock().unwrap();
        if sim.pairing.is_none() || !matches!(sim.plan, Plan::ShowCode(_)) {
            return false;
        }
        sim.pair_done();
        true
    }

    pub fn connector(
        &self,
    ) -> impl Fn(&str, Handler) -> io::Result<Connection> + Send + Sync + 'static + use<> {
        let dongle = self.clone();
        move |_, handler| dongle.connect(handler)
    }
}
