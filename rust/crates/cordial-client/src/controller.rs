//! Shared foreground session and command orchestration for all host interfaces.
use crate::{
    client::{Cancellation, Client, Envelope, Error, Wait},
    session::Session,
};
use cordial_protocol::{
    errors::ErrorCode,
    hidpp::Feature,
    identifiers::*,
    info::InfoField,
    messages::{Candidate, Capabilities, Device, PairAction, Prompt, Status},
    payloads::FileEntry,
    settings::{Setting, SettingKey, SettingOutcome, SettingValue},
};
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

pub type SessionId = u64;
pub type Ticket = u64;

#[derive(Clone, Debug)]
#[allow(clippy::large_enum_variant)] // Desktop messages own their result; no extra allocation per command.
pub enum Event {
    Connection {
        session: SessionId,
        port: String,
        phase: Phase,
    },
    Notice {
        session: SessionId,
        notice: Notice,
    },
    Done {
        session: SessionId,
        ticket: Ticket,
        result: Result<Outcome, Failure>,
    },
    Closed,
}
#[derive(Clone, Debug)]
pub enum Phase {
    Opened,
    Waiting,
    Ready,
    Failed { error: Error, open: bool },
    Lost(Error),
}
#[derive(Clone, Debug)]
pub enum Notice {
    Message {
        envelope: Envelope,
        background: bool,
    },
    RequestPending {
        id: RequestId,
        command: &'static str,
        target: String,
    },
    BondSaved(DeviceId),
    MonitorExpired,
    Skipped(u64),
    RefreshFailed(Error),
    /// Directory entries of a running listing, in adapter order.
    StorageEntries {
        ticket: Ticket,
        entries: Vec<FileEntry>,
    },
    /// Bytes written so far by a running download.
    StorageProgress {
        ticket: Ticket,
        bytes: u64,
    },
}
#[derive(Clone, Debug)]
pub struct State {
    pub session: SessionId,
    pub port: String,
    /// What this connection's firmware offers; cleared with the session.
    pub capabilities: Capabilities,
    pub status: Status,
    pub devices: Vec<Device>,
    pub candidates: Vec<Candidate>,
    pub pending: Vec<Pending>,
    pub auth: Option<Auth>,
    pub available: bool,
    pub current: bool,
    pub monitor: bool,
    pub saturated: bool,
    pub revision: u64,
    pub ready: bool,
    pub waiting: bool,
    pub ready_error: Option<Error>,
    pub settings: BTreeMap<DeviceId, DeviceSettings>,
    /// Device information snapshots, kept only in memory for this session.
    pub info: BTreeMap<DeviceId, DeviceInfoView>,
}
#[derive(Clone, Debug)]
pub struct Pending {
    pub id: RequestId,
    pub device_id: Option<DeviceId>,
    pub command: &'static str,
    pub target: Option<String>,
}
#[derive(Clone, Debug)]
pub struct Auth {
    pub request: RequestId,
    pub prompt: Prompt,
    pub display: bool,
    pub expires: Instant,
    pub expires_in_ms: u32,
}
#[derive(Clone, Debug, Default)]
pub struct DeviceSettings {
    pub loaded: bool,
    pub current: bool,
    pub revision: u64,
    pub settings: Vec<Setting>,
    pub state: Option<SettingsState>,
    pub error: Option<ErrorCode>,
    pub features_loaded: bool,
    pub features_current: bool,
    pub feature_revision: u64,
    pub features: Vec<Feature>,
}
/// A device's merged information in display order. `current` is false until
/// a snapshot follows any loss of events.
#[derive(Clone, Debug, Default)]
pub struct DeviceInfoView {
    pub current: bool,
    pub revision: u64,
    pub fields: Vec<InfoField>,
}
pub struct OpenOptions {
    pub wait: Wait,
    pub keep_unready: bool,
}
#[derive(Clone)]
pub struct RunOptions {
    pub one_shot: bool,
    pub scan_duration: Duration,
    pub wait: Wait,
}
impl Default for RunOptions {
    fn default() -> Self {
        Self {
            one_shot: false,
            scan_duration: Duration::from_secs(10),
            wait: Wait::unlimited(),
        }
    }
}
#[derive(Clone, Debug)]
pub enum Command {
    Status,
    Capabilities,
    Platform(HostPlatform),
    Name(Option<String>),
    Monitor(bool),
    Scan(ScanTransport),
    ScanOff,
    Devices,
    Info(String),
    /// Reads the device information snapshot.
    DeviceInfo(String),
    /// Asks the device for fresh information, then reads the snapshot.
    DeviceInfoRefresh(String),
    Pair(String),
    Connect(String),
    Disconnect(String),
    Enabled(String, bool),
    Trusted(String, bool),
    Blocked(String, bool),
    Remove(String),
    Hidpp(String, bool),
    Cancel(RequestId),
    PairReply {
        request: RequestId,
        prompt: String,
        action: PairAction,
        value: Option<String>,
    },
    Features(String),
    Settings(String),
    SettingGet(String, SettingKey),
    SettingSet(String, SettingKey, SettingInput),
    SettingForget(String, SettingKey),
    SettingsRefresh(String),
    SettingsApply(String),
    Bootloader,
    StorageList(String),
    /// Downloads an adapter file; an existing destination is replaced only
    /// with `overwrite`.
    StorageGet {
        path: String,
        local: PathBuf,
        overwrite: bool,
    },
}
impl Command {
    /// Capability support only; current readiness and device state are checked separately.
    pub fn unsupported(&self, state: &State) -> Option<&'static str> {
        crate::commands::unsupported(state, self)
    }
    pub fn supported(&self, state: &State) -> bool {
        self.unsupported(state).is_none()
    }
    /// Runs without adapter readiness: inspection, file access and recovery
    /// work while the radio or saved devices are unavailable.
    pub fn direct(&self) -> bool {
        matches!(
            self,
            Self::Status
                | Self::Capabilities
                | Self::Bootloader
                | Self::StorageList(_)
                | Self::StorageGet { .. }
        )
    }
}
#[derive(Clone, Debug)]
pub enum SettingInput {
    Text(String),
    Value(SettingValue),
}
#[derive(Clone, Debug)]
pub struct Subject {
    pub id: String,
    pub name: Option<String>,
}
#[derive(Clone, Debug)]
#[allow(clippy::large_enum_variant)] // One result per command; boxing status gains nothing.
pub enum Outcome {
    Status(Status),
    Capabilities(Capabilities),
    Platform(HostPlatform),
    Name(String),
    Monitor(bool),
    ScanStarted {
        request: RequestId,
        transport: ScanTransport,
    },
    ScanFinished {
        count: u64,
        truncated: bool,
    },
    ScanStopped {
        was_running: bool,
    },
    Devices,
    Device {
        subject: Subject,
        device: Option<Device>,
    },
    Candidate(Candidate),
    Hidden(Subject),
    Connected(Subject),
    /// Pairing committed the bond, but the device is not effectively enabled.
    PairedDisabled(Subject, Option<cordial_protocol::errors::DisabledReason>),
    CancelRequested(RequestId),
    ReplySent,
    DeviceInfo(Subject),
    Features(Subject),
    Settings(Subject),
    Setting {
        subject: Subject,
        setting: Setting,
    },
    Job {
        subject: Subject,
        rows: Vec<(Setting, SettingOutcome)>,
        counts: Option<JobCounts>,
    },
    Bootloader {
        mode: cordial_protocol::payloads::BootloaderMode,
    },
    StorageListed {
        path: String,
        count: u64,
    },
    StorageSaved {
        path: String,
        local: PathBuf,
        bytes: u64,
    },
}
#[derive(Clone, Debug, Default)]
pub struct JobCounts {
    pub count: u64,
    pub read: u64,
    pub applied: u64,
    pub unchanged: u64,
    pub unsupported: u64,
    pub failed: u64,
    pub uncertain: u64,
}
impl From<cordial_protocol::payloads::SettingsSummary> for JobCounts {
    fn from(s: cordial_protocol::payloads::SettingsSummary) -> Self {
        Self {
            count: s.count as u64,
            read: s.read as u64,
            applied: s.applied as u64,
            unchanged: s.unchanged as u64,
            unsupported: s.unsupported as u64,
            failed: s.failed as u64,
            uncertain: s.uncertain as u64,
        }
    }
}
#[derive(Clone, Debug)]
pub struct Failure {
    pub error: Error,
    pub bonded: Option<DeviceId>,
    pub partial: Option<Box<Outcome>>,
}
impl From<Error> for Failure {
    fn from(error: Error) -> Self {
        Self {
            error,
            bonded: None,
            partial: None,
        }
    }
}

type Connector = dyn Fn(&str, &Wait) -> crate::client::Result<Client> + Send + Sync;
struct Active {
    generation: SessionId,
    session: Option<Arc<Session>>,
    opening: Cancellation,
    closing: bool,
}
struct Shared {
    active: Mutex<Active>,
    connection_gate: Mutex<()>,
    ticket: AtomicU64,
    sink: Arc<dyn Fn(Event) + Send + Sync>,
    connect: Arc<Connector>,
}
#[derive(Clone)]
pub struct Controller {
    inner: Arc<Shared>,
}
impl Controller {
    pub fn new(sink: impl Fn(Event) + Send + Sync + 'static) -> Self {
        Self::with_connector(sink, Client::open)
    }
    /// Supplies a simulated transport without opening real hardware in tests.
    pub fn with_connector(
        sink: impl Fn(Event) + Send + Sync + 'static,
        connect: impl Fn(&str, &Wait) -> crate::client::Result<Client> + Send + Sync + 'static,
    ) -> Self {
        Self {
            inner: Arc::new(Shared {
                active: Mutex::new(Active {
                    generation: 0,
                    session: None,
                    opening: Cancellation::default(),
                    closing: false,
                }),
                connection_gate: Mutex::new(()),
                ticket: AtomicU64::new(0),
                sink: Arc::new(sink),
                connect: Arc::new(connect),
            }),
        }
    }
    pub fn open(&self, port: String, options: OpenOptions) -> SessionId {
        self.open_session(port, options, true)
    }
    /// Opens only the control link for status and development recovery commands.
    pub fn open_direct(&self, port: String, options: OpenOptions) -> SessionId {
        self.open_session(port, options, false)
    }
    fn open_session(&self, port: String, options: OpenOptions, prepare: bool) -> SessionId {
        let (id, old) = {
            let mut active = self.inner.active.lock().unwrap();
            active.opening.cancel();
            active.generation += 1;
            active.closing = false;
            active.opening = options.wait.cancellation.clone();
            (active.generation, active.session.take())
        };
        let inner = self.inner.clone();
        thread::spawn(move || {
            let gate = inner.connection_gate.lock().unwrap();
            if let Some(old) = old {
                old.shutdown();
            }
            if let Err(error) = options.wait.check() {
                (inner.sink)(Event::Connection {
                    session: id,
                    port,
                    phase: Phase::Failed { error, open: false },
                });
                return;
            }
            let client = match (inner.connect)(&port, &options.wait) {
                Ok(client) => client,
                Err(error) => {
                    drop(gate);
                    (inner.sink)(Event::Connection {
                        session: id,
                        port,
                        phase: Phase::Failed { error, open: false },
                    });
                    return;
                }
            };
            let session = Session::new(client, id, port.clone(), inner.sink.clone());
            {
                let mut active = inner.active.lock().unwrap();
                if active.generation != id
                    || active.closing
                    || options.wait.cancellation.cancelled()
                {
                    drop(active);
                    session.shutdown();
                    return;
                }
                active.session = Some(session.clone());
            }
            session.phase(Phase::Opened);
            session.listen();
            drop(gate);
            if !prepare {
                return;
            }
            let wait = Wait {
                deadline: Some(Instant::now() + Duration::from_secs(35)),
                cancellation: options.wait.cancellation.clone(),
            };
            let result = session
                .ready(&wait)
                .and_then(|()| session.monitoring(true, &wait));
            let current = {
                let active = inner.active.lock().unwrap();
                active.generation == id && !active.closing
            };
            if !current {
                session.shutdown();
                return;
            }
            match result {
                Ok(()) => session.phase(Phase::Ready),
                Err(error) => {
                    let keep = options.keep_unready && session.client.error().is_none();
                    if !keep {
                        session.shutdown();
                        let mut active = inner.active.lock().unwrap();
                        if active.generation == id {
                            active.session = None;
                        }
                    }
                    session.phase(Phase::Failed { error, open: keep });
                }
            }
        });
        id
    }
    pub fn close(&self) {
        let (id, old) = {
            let mut active = self.inner.active.lock().unwrap();
            if active.closing {
                return;
            }
            active.closing = true;
            active.opening.cancel();
            (active.generation, active.session.take())
        };
        let inner = self.inner.clone();
        thread::spawn(move || {
            let _gate = inner.connection_gate.lock().unwrap();
            if let Some(old) = old {
                old.shutdown();
            }
            let current = {
                let active = inner.active.lock().unwrap();
                active.generation == id && active.closing
            };
            if current {
                (inner.sink)(Event::Closed);
            }
        });
    }
    pub fn run(&self, command: Command, options: RunOptions) -> Ticket {
        let ticket = self.inner.ticket.fetch_add(1, Ordering::Relaxed) + 1;
        let (id, session) = {
            let active = self.inner.active.lock().unwrap();
            (active.generation, active.session.clone())
        };
        let sink = self.inner.sink.clone();
        thread::spawn(move || {
            let result = match session {
                Some(session) => crate::commands::execute(&session, &command, &options, ticket),
                None => Err(Error::new(
                    "no adapter selected; use adapter list and adapter select PORT",
                )
                .into()),
            };
            sink(Event::Done {
                session: id,
                ticket,
                result,
            });
        });
        ticket
    }
    pub fn state(&self) -> Option<State> {
        self.inner
            .active
            .lock()
            .unwrap()
            .session
            .as_ref()
            .map(|s| s.state())
    }
    pub fn hide_candidate(&self, id: &CandidateId) {
        if let Some(session) = &self.inner.active.lock().unwrap().session {
            session.hide_candidate(id);
        }
    }
}
impl Drop for Shared {
    fn drop(&mut self) {
        let active = self.active.get_mut().unwrap();
        active.opening.cancel();
        if let Some(session) = active.session.take() {
            session.shutdown();
        }
    }
}
