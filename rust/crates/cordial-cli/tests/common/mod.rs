//! A simulated Dongle over in-memory byte streams. It answers each request frame from a small
//! model and sends the events a real Dongle would.
#![allow(dead_code)]
use cordial_client::{Connection, Received};
use cordial_protocol::{
    self as p, CodeKind, DeviceState, ErrorCode, IntegrationKind, MAX_REQUEST_BYTES, SettingState,
    Transport,
    frame::{self, Decoder},
    keys, message, pairing,
    request::Command,
    response, setting,
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
    pub devices: Vec<p::Device>,
    pub settings: BTreeMap<String, Vec<p::Setting>>,
    pub warnings: BTreeMap<String, Vec<p::DeviceWarning>>,
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
    pairing: Option<String>,
    out: Option<PipeWriter>,
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
            },
            p::TransportSupport {
                transport: Transport::Ble as i32,
                max_enabled: Some(7),
            },
        ],
        info: vec![
            info(keys::FIRMWARE_VERSION, Value::Text("1.2.3".into())),
            info(keys::BUILD_DEVELOPMENT, Value::Bool(true)),
        ],
    }
}

pub fn device(id: &str, name: &str, transport: Transport) -> p::Device {
    p::Device {
        id: id.into(),
        transport: transport as i32,
        name: name.into(),
        kind: p::Kind::Mouse as i32,
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

pub fn candidate(id: &str, name: &str) -> p::Candidate {
    p::Candidate {
        id: id.into(),
        transport: Transport::Ble as i32,
        name: name.into(),
        kind: p::Kind::Keyboard as i32,
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
                device("d_1", "Office Mouse", Transport::Ble),
                p::Device {
                    enabled: false,
                    inactive: Some(p::InactiveReason::Disabled as i32),
                    ..device("d_2", "Old Keyboard", Transport::Classic)
                },
            ],
            settings: BTreeMap::new(),
            warnings: BTreeMap::new(),
            candidates: vec![candidate("c_1", "New Keyboard")],
            scan_ends: true,
            plan: Plan::Done,
            paired: device("d_9", "New Keyboard", Transport::Ble),
            files: BTreeMap::new(),
            data: BTreeMap::new(),
            log: Vec::new(),
            refuse: BTreeMap::new(),
            pairing: None,
            out: None,
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
        Command::ForgetSettings(_) => "forget_settings",
        Command::ListFeatures(_) => "list_features",
        Command::ListFiles(_) => "list_files",
        Command::ReadFile(_) => "read_file",
    }
}

fn error(code: ErrorCode) -> Option<response::Result> {
    Some(response::Result::Error(p::Error {
        code: code as i32,
        ..Default::default()
    }))
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

    fn device_mut(&mut self, id: &str) -> Option<&mut p::Device> {
        self.devices.iter_mut().find(|d| d.id == id)
    }

    fn device_event(&mut self, id: &str) {
        if let Some(d) = self.devices.iter().find(|d| d.id == id).cloned() {
            self.event(p::event::Kind::Device(d));
        }
    }

    fn prompt(&mut self, candidate: &str, step: pairing::Step) {
        self.event(p::event::Kind::Pairing(p::Pairing {
            candidate: candidate.into(),
            step: Some(step),
        }));
    }

    fn pair_done(&mut self) {
        let candidate = self.pairing.take().unwrap_or_default();
        let device = self.paired.clone();
        self.devices.push(device.clone());
        self.event(p::event::Kind::Device(device.clone()));
        self.prompt(
            &candidate,
            pairing::Step::Done(p::PairingDone { device: device.id }),
        );
    }

    fn pair_failed(&mut self, code: ErrorCode) {
        let candidate = self.pairing.take().unwrap_or_default();
        self.prompt(&candidate, pairing::Step::Failed(code as i32));
    }

    fn answer(&mut self, command: Command) {
        self.log.push(command.clone());
        if let Some(code) = self.refuse.get(kind_name(&command)) {
            return self.respond(error(*code));
        }
        match command {
            Command::GetStatus(_) => {
                let status = self.status.clone();
                self.respond(Some(response::Result::Status(status)));
            }
            Command::SetAdapter(update) => {
                if let Some(name) = update.name {
                    self.status.name = if name.is_empty() {
                        "Cordial".into()
                    } else {
                        name
                    };
                }
                if let Some(platform) = update.platform {
                    self.status.platform = platform;
                }
                let status = self.status.clone();
                self.respond(Some(response::Result::Status(status.clone())));
                self.event(p::event::Kind::Adapter(status));
            }
            Command::EnterBootloader(_) => self.respond(None),
            Command::StartScan(scan) => {
                if scan.transports.is_empty() {
                    return self.respond(error(ErrorCode::BadArgs));
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
                self.pairing = Some(start.candidate.clone());
                self.prompt(
                    &start.candidate,
                    pairing::Step::Connecting(p::PairingConnecting {}),
                );
                match self.plan.clone() {
                    Plan::EnterCode(_) => self.prompt(
                        &start.candidate,
                        pairing::Step::EnterCode(p::EnterCode {
                            kind: CodeKind::Passkey as i32,
                        }),
                    ),
                    Plan::Confirm(passkey) => self.prompt(
                        &start.candidate,
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
            Command::ListDevices(_) => {
                let devices = self.devices.clone();
                self.respond(Some(response::Result::Devices(p::DeviceList { devices })));
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
                let Some(d) = self.device_mut(&update.device) else {
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
                for i in &update.integrations {
                    if let Some(on) = i.enabled
                        && let Some(h) = d.integrations.iter_mut().find(|x| x.kind == i.kind)
                    {
                        h.enabled = on;
                    }
                }
                let d = d.clone();
                self.respond(Some(response::Result::Device(d.clone())));
                self.event(p::event::Kind::Device(d));
            }
            Command::ConnectDevice(c) => {
                let Some(d) = self.device_mut(&c.device) else {
                    return self.respond(error(ErrorCode::NotFound));
                };
                d.state = DeviceState::Connecting as i32;
                d.paused = false;
                let record = d.clone();
                self.respond(Some(response::Result::Device(record)));
                let d = self.device_mut(&c.device).unwrap();
                d.state = DeviceState::Connected as i32;
                self.device_event(&c.device);
            }
            Command::DisconnectDevice(c) => {
                let Some(d) = self.device_mut(&c.device) else {
                    return self.respond(error(ErrorCode::NotFound));
                };
                d.state = DeviceState::Disconnected as i32;
                d.paused = true;
                let record = d.clone();
                self.respond(Some(response::Result::Device(record)));
                self.device_event(&c.device);
            }
            Command::UnpairDevice(u) => {
                self.devices.retain(|d| d.id != u.device);
                self.respond(None);
                self.event(p::event::Kind::DeviceRemoved(p::DeviceRemoved {
                    id: u.device,
                }));
            }
            Command::RefreshDevice(_) => self.respond(None),
            Command::ListWarnings(l) => {
                let warnings = self.warnings.get(&l.device).cloned().unwrap_or_default();
                self.respond(Some(response::Result::Warnings(p::DeviceWarnings {
                    device: l.device,
                    warnings,
                })));
            }
            Command::ListSettings(l) => {
                let settings = self.settings.get(&l.device).cloned().unwrap_or_default();
                self.respond(Some(response::Result::Settings(p::DeviceSettings {
                    device: l.device,
                    settings,
                })));
            }
            Command::SetSettings(set) => {
                let list = self.settings.entry(set.device.clone()).or_default();
                for change in &set.changes {
                    let Some(s) = list.iter_mut().find(|s| s.key == change.key) else {
                        continue;
                    };
                    if let (
                        Some(setting::Type::Integer(t)),
                        Some(p::Value {
                            value: Some(Value::Integer(n)),
                        }),
                    ) = (&mut s.r#type, &change.value)
                    {
                        t.saved = Some(*n);
                    }
                    s.status = Some(setting::Status::State(SettingState::Pending as i32));
                }
                let settings = list.clone();
                self.respond(Some(response::Result::Settings(p::DeviceSettings {
                    device: set.device.clone(),
                    settings,
                })));
                let list = self.settings.get_mut(&set.device).unwrap();
                for s in list.iter_mut() {
                    if let Some(setting::Type::Integer(t)) = &mut s.r#type
                        && t.saved.is_some()
                    {
                        t.value = t.saved;
                        s.status = Some(setting::Status::State(SettingState::Applied as i32));
                    }
                }
                let settings = list.clone();
                self.event(p::event::Kind::Settings(p::DeviceSettings {
                    device: set.device,
                    settings,
                }));
            }
            Command::ForgetSettings(f) => {
                let list = self.settings.entry(f.device.clone()).or_default();
                for r in &f.settings {
                    if let Some(s) = list.iter_mut().find(|s| s.key == r.key) {
                        if let Some(setting::Type::Integer(t)) = &mut s.r#type {
                            t.saved = None;
                        }
                        s.status = None;
                    }
                }
                let settings = list.clone();
                self.respond(Some(response::Result::Settings(p::DeviceSettings {
                    device: f.device,
                    settings,
                })));
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
                Some(entries) => {
                    self.respond(Some(response::Result::Files(p::FileList { entries })))
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
        self.0.lock().unwrap().out = Some(to_client);
        let sim = self.0.clone();
        thread::spawn(move || {
            let mut decoder = Decoder::new(Some(MAX_REQUEST_BYTES));
            let mut buffer = [0; 256];
            loop {
                let n = match from_client.read(&mut buffer) {
                    Ok(0) => {
                        sim.lock().unwrap().out = None;
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
