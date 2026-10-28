//! The foreground session with one adapter, shared by the shell, scripts and the TUI. It keeps
//! the view current from the Dongle's events and runs each command on its own thread.
pub use crate::view::{SessionId, State};
use crate::{
    error::Error,
    profiles::{Deletion, InterfaceUpdate},
    settings::{self, Field, Setting},
    view::Pending,
};
use cordial_client::{Connection, Received};
use cordial_protocol::{self as p, request};
use std::{
    fmt, io,
    path::PathBuf,
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

pub type Ticket = u64;

/// Something an interface reacts to.
#[derive(Clone, Debug)]
#[allow(clippy::large_enum_variant)] // Each event owns its record; none is kept long.
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
    /// A running profile deletion's changes the adapter has confirmed so far.
    Progress {
        session: SessionId,
        ticket: Ticket,
        progress: Progress,
    },
    Done {
        session: SessionId,
        ticket: Ticket,
        result: Result<Outcome, Error>,
    },
    /// The controller's session closed after `close`, and its port is released.
    Closed,
}

/// How far a profile deletion has come, counting its changes in plan order: each device, each
/// configuration interface, then the deletion itself. The interfaces change in one request, so
/// they are confirmed together.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Progress {
    /// Changes the adapter has confirmed.
    pub done: usize,
    /// The adapter is restarting its USB configuration, and the next change waits for it to
    /// reconnect.
    pub waiting: bool,
}

#[derive(Clone, Debug)]
pub enum Phase {
    /// The port is open and the adapter's status read.
    Opened,
    /// The port is open, but the adapter's saved data must be migrated first. Only status is
    /// read until the session closes; this replaces `Opened`.
    Migrate,
    /// The port is open and the adapter waits for a migration, but its other errors stop Cordial
    /// from running one. Only status and restart work until the session closes; this replaces
    /// `Opened`.
    Blocked(Error),
    /// The port is open, but this version of Cordial can't use the adapter. Only status is read
    /// until the session closes; this replaces `Opened`.
    Unsupported(Error),
    /// The adapter is starting; commands that need Bluetooth or storage wait.
    Waiting,
    /// The adapter is ready and its saved devices are loaded.
    Ready,
    /// The port answered for an adapter other than the one it was opened for. The session
    /// closed after reading only status; this replaces `Opened`.
    Other,
    /// Opening failed. With `open`, the session stays for status and development commands.
    Failed {
        error: Error,
        open: bool,
    },
    Lost(Error),
}

#[derive(Clone, Debug)]
#[allow(clippy::large_enum_variant)]
pub enum Notice {
    /// An event, already applied to the view. `first` marks a candidate or device the view
    /// didn't hold before; `changed` lists the fields a settings_changed event added or changed.
    Event {
        event: p::Event,
        first: bool,
        changed: Vec<Field>,
    },
    /// A response to a command an interface ran.
    Response(p::Response),
    /// Loading the saved devices failed after the adapter became ready.
    RefreshFailed(Error),
}

/// Lets an interface stop a command that waits, such as pairing or a scan.
#[derive(Clone, Debug, Default)]
pub struct Cancellation(Arc<AtomicBool>);
impl Cancellation {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }
    pub fn cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

/// How long a command may wait, and what stops it.
#[derive(Clone, Debug, Default)]
pub struct Wait {
    pub deadline: Option<Instant>,
    pub cancellation: Cancellation,
}
impl Wait {
    pub fn unlimited() -> Self {
        Self::default()
    }
    pub fn timeout(after: Duration) -> Self {
        Self {
            deadline: Some(Instant::now() + after),
            cancellation: Cancellation::default(),
        }
    }
    pub fn check(&self) -> Result<(), Error> {
        if self.cancellation.cancelled() {
            return Err(Error::new("the operation was cancelled"));
        }
        if self.deadline.is_some_and(|d| Instant::now() >= d) {
            return Err(Error::new("operation timed out"));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Default)]
pub struct RunOptions {
    /// A one-shot command waits for its whole effect, such as a scan's end.
    pub one_shot: bool,
    pub wait: Wait,
}

/// A saved device preference set with `device set`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Toggle {
    Enabled,
    Trusted,
    Blocked,
    Hidpp,
}

/// A device, candidate or profile as a command names it: an ID, or a name to look up.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Target {
    Id(u32),
    Name(String),
}

impl Target {
    /// A typed word: a number is always an ID, and anything else is a name.
    pub fn parse(word: &str) -> Result<Self, String> {
        if word.is_empty() || !word.bytes().all(|b| b.is_ascii_digit()) {
            return Ok(Self::Name(word.to_owned()));
        }
        word.parse()
            .map(Self::Id)
            .map_err(|_| format!("{word} is not a valid ID"))
    }
}

impl fmt::Display for Target {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Id(id) => write!(f, "{id}"),
            Self::Name(name) => f.write_str(name),
        }
    }
}

/// Adapter settings saved together in one request; `None` and an empty list leave a setting
/// unchanged. Lists apply in order.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AdapterUpdate {
    pub platform: Option<p::Platform>,
    /// Whether each listed transport is enabled.
    pub transports: Vec<(p::Transport, bool)>,
    pub interfaces: Vec<InterfaceUpdate>,
}

impl AdapterUpdate {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// Device preferences saved together in one request; `None` leaves a preference unchanged.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DeviceUpdate {
    pub enabled: Option<bool>,
    pub trusted: Option<bool>,
    pub blocked: Option<bool>,
    pub hidpp: Option<bool>,
    pub gamepad: Option<p::OutputMode>,
    /// The device's layers: profile IDs in the order they apply.
    pub layers: Option<Vec<u32>>,
}

impl DeviceUpdate {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum SettingInput {
    /// Text typed in the shell, typed by the setting.
    Text(String),
    Value(settings::Value),
}

/// A profile chosen for a configuration interface.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Pick {
    /// No profile.
    Clear,
    Profile(Target),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Command {
    Status,
    /// Renames the adapter; `None` restores its default name.
    Name(Option<String>),
    Platform(p::Platform),
    /// Enables or disables a transport.
    Transport(p::Transport, bool),
    /// Changes one configuration interface's preferences.
    Interface {
        interface: p::ConfigurationInterface,
        enabled: Option<bool>,
        profile: Option<Pick>,
    },
    /// Restarts the adapter, which closes the connection.
    Restart,
    Bootloader,
    /// Scans the transports, or every supported one when empty, for `seconds` (0 means the
    /// adapter's default).
    Scan {
        transports: Vec<p::Transport>,
        seconds: u32,
    },
    ScanStop,
    /// Pairs a candidate.
    Pair(Target),
    /// Answers the open pairing prompt, with the code it asks for.
    Accept(Option<String>),
    Reject,
    CancelPairing,
    Devices,
    Get(Target),
    Connect(Target),
    Disconnect(Target),
    Unpair(Target),
    Refresh(Target),
    Set(Target, Toggle, bool),
    GamepadOutput(Target, p::OutputMode),
    /// Sets a device's layers: profiles in the order they apply, or none.
    Layers(Target, Vec<Target>),
    Settings(Target),
    SettingGet(Target, Field),
    SettingSet(Target, Field, SettingInput),
    SettingForget(Target, Field),
    /// Saves and forgets several settings of one device in one request.
    SettingsSave {
        device: u32,
        set: Vec<(Field, settings::Value)>,
        forget: Vec<Field>,
    },
    Features(Target),
    /// Every saved profile.
    Profiles,
    /// A profile's name and roles.
    ProfileShow(Target),
    /// Reads a profile's name, for showing where it is used.
    ProfileLookup(u32),
    /// Creates an empty profile.
    ProfileCreate(String),
    /// Copies a profile under a new name.
    ProfileCopy(Target, String),
    /// Deletes a profile after removing it from the devices and configuration interfaces that
    /// the view shows using it.
    ProfileDelete(Target),
    /// Deletes a profile with the changes a confirmation listed, and no others.
    ProfileDeletePlanned(Deletion),
    /// Every rule of a profile.
    Rules(Target),
    /// Saves or forgets one rule of a profile.
    RuleChange(Target, p::ProfileRuleChange),
    /// Saves several adapter settings in one request.
    AdapterSave(AdapterUpdate),
    /// Saves several preferences of one device in one request.
    DeviceSave(u32, DeviceUpdate),
    Files(String),
    /// Downloads an adapter file; an existing destination is replaced only with `overwrite`. A
    /// known record is saved as JSON unless `raw`.
    FileGet {
        path: String,
        local: PathBuf,
        overwrite: bool,
        raw: bool,
    },
    /// Uploads local bytes, replacing the adapter file atomically.
    FilePut {
        local: PathBuf,
        path: String,
    },
    FileDelete(String),
}

impl Command {
    /// Runs without adapter readiness: inspection, file access and recovery work while
    /// Bluetooth or storage is unavailable.
    pub fn direct(&self) -> bool {
        matches!(
            self,
            Self::Status
                | Self::Restart
                | Self::Bootloader
                | Self::Files(_)
                | Self::FileGet { .. }
                | Self::FilePut { .. }
                | Self::FileDelete(_)
        )
    }

    /// Saves a change on the adapter.
    pub fn saves(&self) -> bool {
        matches!(
            self,
            Self::Name(_)
                | Self::Platform(_)
                | Self::Transport(..)
                | Self::Interface { .. }
                | Self::Unpair(_)
                | Self::Set(..)
                | Self::GamepadOutput(..)
                | Self::Layers(..)
                | Self::SettingSet(..)
                | Self::SettingForget(..)
                | Self::SettingsSave { .. }
                | Self::ProfileCreate(_)
                | Self::ProfileCopy(..)
                | Self::ProfileDelete(_)
                | Self::ProfileDeletePlanned(_)
                | Self::RuleChange(..)
                | Self::AdapterSave(_)
                | Self::DeviceSave(..)
                | Self::FilePut { .. }
                | Self::FileDelete(_)
        )
    }
}

/// A device or candidate as a result names it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Subject {
    pub id: u32,
    pub name: String,
}

#[derive(Clone, Debug)]
#[allow(clippy::large_enum_variant)] // One result per command.
pub enum Outcome {
    Status(p::Status),
    /// The adapter's new name; `reset` when it is the adapter's default name.
    Name {
        name: String,
        reset: bool,
    },
    Platform(p::Platform),
    /// Whether a transport is enabled, as the adapter saved it.
    Transport(p::Transport, bool),
    /// A configuration interface's preferences, as the adapter saved them.
    Interface(p::ConfigurationInterface, p::Status),
    Restart,
    Bootloader,
    ScanStarted(Vec<p::Transport>),
    /// `error` says why the scan ended early or lost candidates; the found ones stay usable.
    ScanFinished {
        count: u32,
        error: Option<p::Error>,
    },
    ScanStopped {
        was_running: bool,
    },
    /// The bond is saved; `device` is the saved record once known.
    Paired {
        subject: Subject,
        device: Option<p::Device>,
    },
    /// The bond is saved, but its device ID is not yet known.
    PairedWithoutDevice {
        name: String,
    },
    Answered,
    PairingCancelled,
    Devices,
    Device {
        subject: Subject,
        device: p::Device,
    },
    Unpaired(Subject),
    Refreshing(Subject),
    Settings(Subject),
    Setting {
        subject: Subject,
        setting: Setting,
    },
    /// Saved and forgotten fields of one device, with the settings as saved.
    Saved {
        subject: Subject,
        set: Vec<Field>,
        forget: Vec<Field>,
        settings: Vec<Setting>,
    },
    Features {
        subject: Subject,
        features: Vec<p::Feature>,
    },
    /// Every saved profile.
    Profiles(p::ProfileList),
    /// A created, copied, shown or looked up profile.
    Profile(p::Profile),
    ProfileDeleted(p::Profile),
    /// A profile and its rules: every rule as listed, or the input's rule as a change saved it,
    /// none when the change forgot it.
    Rules {
        profile: p::Profile,
        rules: Vec<p::ProfileRule>,
    },
    /// The adapter's status after an `AdapterSave`.
    AdapterSaved(p::Status),
    Files {
        path: String,
        entries: Vec<p::FileEntry>,
    },
    /// A download saved to `local`: the record's JSON, or the bytes as they are when `raw` or
    /// when `unconverted` says why.
    FileSaved {
        path: String,
        local: PathBuf,
        bytes: u64,
        json: bool,
        unconverted: Option<crate::records::Unconverted>,
    },
    FileUploaded {
        path: String,
        bytes: u64,
    },
    FileDeleted(String),
}

/// Opens a connection whose handler receives everything the Dongle sends.
pub type Connector =
    dyn Fn(&str, Box<dyn FnMut(Received<'_>) + Send>) -> io::Result<Connection> + Send + Sync;

type Sink = Arc<dyn Fn(Event) + Send + Sync>;
type Handler = Arc<Mutex<Box<dyn FnMut(Received<'_>) + Send>>>;
type Ports = dyn Fn() -> io::Result<Vec<cordial_client::serial::PortInfo>> + Send + Sync;

/// A profile deletion owns discovery until its configuration reconnect finishes.
pub const PROFILE_RECONNECT: &str = "profile delete reconnect";

/// The view and what waits on it.
pub(crate) struct Cell {
    pub state: Mutex<State>,
    pub changed: Condvar,
}

impl Cell {
    pub fn snapshot(&self) -> State {
        let mut st = self.state.lock().unwrap().clone();
        let hidden = st.hidden.clone();
        st.candidates.retain(|c| !hidden.contains(&c.id));
        st
    }

    pub fn update<T>(&self, f: impl FnOnce(&mut State) -> T) -> T {
        let result = f(&mut self.state.lock().unwrap());
        self.changed.notify_all();
        result
    }

    /// Waits until `done` holds, the connection closes, or `wait` ends.
    pub fn wait_for<T>(
        &self,
        wait: &Wait,
        mut done: impl FnMut(&State) -> Option<T>,
    ) -> Result<T, Error> {
        let mut st = self.state.lock().unwrap();
        loop {
            if let Some(result) = done(&st) {
                return Ok(result);
            }
            if !st.available {
                return Err(Error::new("the connection to the adapter closed"));
            }
            wait.check()?;
            st = self
                .changed
                .wait_timeout(st, Duration::from_millis(50))
                .unwrap()
                .0;
        }
    }
}

pub(crate) struct Session {
    pub id: SessionId,
    connection: Mutex<Arc<Connection>>,
    connect: Arc<Connector>,
    ports: Arc<Ports>,
    handler: Handler,
    ending: Arc<Mutex<Option<p::SessionEndingReason>>>,
    pub cell: Arc<Cell>,
    /// File operations share the adapter's upload scratch path.
    pub files: Mutex<()>,
    sink: Sink,
    closing: Arc<AtomicBool>,
}

impl Session {
    pub fn connection(&self) -> Arc<Connection> {
        self.connection.lock().unwrap().clone()
    }

    fn reconnecting(&self) -> bool {
        self.state().pending_for(PROFILE_RECONNECT, 0)
    }

    pub fn profile_reconnect(&self, profile: u32) -> ReconnectGuard<'_> {
        ReconnectGuard {
            pending: Some(self.pending(PROFILE_RECONNECT, profile)),
            resumed: false,
        }
    }

    /// Waits for the announced configuration restart, then reads the same adapter again.
    pub fn reconnect(&self, wait: &Wait) -> Result<(), Error> {
        let mut wait = wait.clone();
        let limit = Instant::now() + Duration::from_secs(10);
        wait.deadline = Some(wait.deadline.map_or(limit, |d| d.min(limit)));
        self.cell
            .wait_for(&wait, |st| st.session_ending.map(|_| ()))?;
        if *self.ending.lock().unwrap() != Some(p::SessionEndingReason::ConfigurationChange) {
            return Err(Error::new(
                "the adapter ended the session for another operation",
            ));
        }
        self.connection().close_and_wait_release();
        let identity = self.state().status.id;
        loop {
            wait.check()?;
            if self.closing.load(Ordering::Acquire) {
                return Err(Error::new("the operation was cancelled"));
            }
            let port = (self.ports)()
                .ok()
                .and_then(|ports| ports.into_iter().find(|p| p.is_adapter(&identity)))
                .map_or_else(|| self.state().port, |p| p.port);
            let admitted = Arc::new(AtomicBool::new(false));
            let (accept, handler) = (admitted.clone(), self.handler.clone());
            let opened = (self.connect)(
                &port,
                Box::new(move |received| {
                    if accept.load(Ordering::Acquire) {
                        handler.lock().unwrap()(received);
                    }
                }),
            );
            let connection = match opened {
                Ok(connection) => Arc::new(connection),
                Err(_) => {
                    // Port discovery retries are bounded by the operation's deadline.
                    let state = self.cell.state.lock().unwrap();
                    let _ = self
                        .cell
                        .changed
                        .wait_timeout(state, Duration::from_millis(50))
                        .unwrap();
                    continue;
                }
            };
            {
                let mut active = self.connection.lock().unwrap();
                if self.closing.load(Ordering::Acquire) {
                    return Err(Error::new("the operation was cancelled"));
                }
                *active = connection.clone();
            }
            connection.set_deadline(wait.deadline);
            let status = match connection.status() {
                Ok(status) => status,
                Err(error) => {
                    connection.close_and_wait_release();
                    if reconnect_transport_error(&error) {
                        let state = self.cell.state.lock().unwrap();
                        let _ = self
                            .cell
                            .changed
                            .wait_timeout(state, Duration::from_millis(50))
                            .unwrap();
                        continue;
                    }
                    return Err(Error::from_client(error, "adapter status"));
                }
            };
            if identity.is_empty() || !status.id.eq_ignore_ascii_case(&identity) {
                return Err(Error::new("another adapter answered on the adapter's port"));
            }
            if !matches!(
                crate::migration::gate(&status),
                crate::migration::Gate::Normal
            ) {
                return Err(Error::new(
                    "the adapter is not ready to finish deleting the profile",
                ));
            }
            wait.check()?;
            {
                let _active = self.connection.lock().unwrap();
                if self.closing.load(Ordering::Acquire) {
                    return Err(Error::new("the operation was cancelled"));
                }
                if let Some(error) = connection.closed() {
                    return Err(Error::from_client(error, "adapter status"));
                }
                self.cell.update(|st| {
                    st.port = port;
                    st.status = status;
                    st.available = true;
                    st.loaded = false;
                    st.session_ending = None;
                });
                *self.ending.lock().unwrap() = None;
                admitted.store(true, Ordering::Release);
            }
            // Admission follows identity verification; read again to include intervening events.
            connection
                .status()
                .map_err(|e| Error::from_client(e, "adapter status"))?;
            self.cell
                .wait_for(&wait, |st| st.status.ready.then_some(()))?;
            self.load()?;
            return Ok(());
        }
    }

    fn phase(&self, phase: Phase) {
        (self.sink)(Event::Connection {
            session: self.id,
            port: self.state().port,
            phase,
        });
    }

    pub fn state(&self) -> State {
        self.cell.snapshot()
    }

    /// Sends one command. A command an interface ran reports its response, in order with the
    /// events around it.
    pub fn call(
        &self,
        command: request::Command,
        name: &'static str,
        reported: bool,
    ) -> Result<p::Response, Error> {
        let response = if reported {
            let (sink, session) = (self.sink.clone(), self.id);
            self.connection().request_with(command, move |response| {
                sink(Event::Notice {
                    session,
                    notice: Notice::Response(response.clone()),
                })
            })
        } else {
            self.connection().request(command)
        }
        .map_err(|e| Error::from_client(e, name))?;
        if let Some(p::response::Result::Error(e)) = response.result {
            return Err(Error::dongle(e, Some(name)));
        }
        Ok(response)
    }

    /// Lists the saved devices, and reads the names of the profiles devices and configuration
    /// interfaces use.
    fn load(&self) -> Result<(), Error> {
        self.connection()
            .list_devices()
            .map_err(|e| Error::from_client(e, "device list"))?;
        for id in crate::profiles::unnamed(&self.state()) {
            // A name that can't be read is shown as the profile's ID.
            let _ = self.connection().get_profile(id);
        }
        self.cell.update(|st| st.loaded = true);
        Ok(())
    }

    /// Closes the connection and waits until the port is released, so whatever opens next,
    /// such as a migration, owns it alone.
    fn shutdown(&self) {
        self.closing.store(true, Ordering::Release);
        self.connection().close_and_wait_release();
    }

    /// Marks a running command for progress shown beside its target; 0 for none.
    pub fn pending(&self, command: &'static str, target: u32) -> PendingGuard<'_> {
        let pending = Pending { command, target };
        self.cell.update(|st| st.pending.push(pending.clone()));
        PendingGuard {
            session: self,
            pending,
        }
    }
}

fn reconnect_transport_error(error: &cordial_client::Error) -> bool {
    use cordial_client::Error;
    match error {
        Error::Handshake(error) | Error::NotSent(error) => reconnect_transport_error(error),
        Error::Closed | Error::Io(_) | Error::Timeout => true,
        _ => false,
    }
}

pub(crate) struct ReconnectGuard<'a> {
    pending: Option<PendingGuard<'a>>,
    resumed: bool,
}

impl ReconnectGuard<'_> {
    pub fn reconnect(&mut self, wait: &Wait) -> Result<(), Error> {
        self.pending.as_ref().unwrap().session.reconnect(wait)?;
        self.resumed = true;
        Ok(())
    }
}

impl Drop for ReconnectGuard<'_> {
    fn drop(&mut self) {
        let pending = self.pending.take().unwrap();
        let session = pending.session;
        session.connection().set_deadline(None);
        if !session.state().available || session.connection().closed().is_some() {
            // Loss reaches the interface before the deletion's result, including when its
            // reply waiter wakes before the reader has delivered Closed.
            session.connection().close_and_wait_release();
            // A candidate's Closed callback can precede admission and never reach the view.
            session.cell.update(|st| st.available = false);
        }
        drop(pending);
        if !session.closing.load(Ordering::Acquire) {
            if !session.state().available {
                session.phase(Phase::Lost(Error::new(
                    "the connection to the adapter closed",
                )));
            } else if self.resumed {
                session.phase(Phase::Ready);
            }
        }
    }
}

pub(crate) struct PendingGuard<'a> {
    session: &'a Session,
    pending: Pending,
}

impl Drop for PendingGuard<'_> {
    fn drop(&mut self) {
        self.session.cell.update(|st| {
            if let Some(i) = st.pending.iter().position(|p| *p == self.pending) {
                st.pending.remove(i);
            }
        });
    }
}

struct Active {
    generation: SessionId,
    session: Option<Arc<Session>>,
    opening: Option<Arc<Session>>,
    closing: bool,
}

#[derive(Default)]
struct OpeningPhases {
    announced: bool,
    lost: Option<Error>,
}

struct Shared {
    active: Mutex<Active>,
    /// Opening and closing run one at a time, so an old session ends before a new one opens.
    gate: Mutex<()>,
    ticket: AtomicU64,
    sink: Sink,
    connect: Arc<Connector>,
    ports: Arc<Ports>,
}

#[derive(Clone)]
pub struct Controller {
    inner: Arc<Shared>,
}

impl Controller {
    pub fn new(sink: impl Fn(Event) + Send + Sync + 'static) -> Self {
        Self::with_connector(sink, |port, handler| {
            cordial_client::serial::open_with_handler(port, handler)
        })
    }

    /// Opens connections with `connect` instead of real serial ports, as tests do.
    pub fn with_connector(
        sink: impl Fn(Event) + Send + Sync + 'static,
        connect: impl Fn(&str, Box<dyn FnMut(Received<'_>) + Send>) -> io::Result<Connection>
        + Send
        + Sync
        + 'static,
    ) -> Self {
        Self::with_backend(sink, connect, cordial_client::serial::ports)
    }

    /// Uses the same port discovery for reconnects as the owning interface.
    pub(crate) fn with_backend(
        sink: impl Fn(Event) + Send + Sync + 'static,
        connect: impl Fn(&str, Box<dyn FnMut(Received<'_>) + Send>) -> io::Result<Connection>
        + Send
        + Sync
        + 'static,
        ports: impl Fn() -> io::Result<Vec<cordial_client::serial::PortInfo>> + Send + Sync + 'static,
    ) -> Self {
        Self {
            inner: Arc::new(Shared {
                active: Mutex::new(Active {
                    generation: 0,
                    session: None,
                    opening: None,
                    closing: false,
                }),
                gate: Mutex::new(()),
                ticket: AtomicU64::new(0),
                sink: Arc::new(sink),
                connect: Arc::new(connect),
                ports: Arc::new(ports),
            }),
        }
    }

    /// Opens a port, replacing any session. Phases follow as `Event::Connection`.
    pub fn open(&self, port: String) -> SessionId {
        self.open_as(port, None)
    }

    /// Opens a port for the adapter with ID `adapter`, as `open` does. Another adapter on the
    /// port reports `Phase::Other` and receives nothing after its status.
    pub fn open_adapter(&self, port: String, adapter: String) -> SessionId {
        self.open_as(port, Some(adapter))
    }

    fn open_as(&self, port: String, adapter: Option<String>) -> SessionId {
        let (id, old) = {
            let mut active = self.inner.active.lock().unwrap();
            active.generation += 1;
            active.closing = false;
            let old = active.session.take().or_else(|| active.opening.take());
            // Close can wait for this port while the replacement worker releases it.
            active.opening = old.clone();
            (active.generation, old)
        };
        let inner = self.inner.clone();
        thread::spawn(move || open(&inner, id, port, adapter, old));
        id
    }

    /// Closes the session; `Event::Closed` follows.
    pub fn close(&self) {
        let (id, old) = {
            let mut active = self.inner.active.lock().unwrap();
            if active.closing {
                return;
            }
            active.closing = true;
            (
                active.generation,
                active.session.take().or_else(|| active.opening.take()),
            )
        };
        let inner = self.inner.clone();
        thread::spawn(move || {
            if let Some(old) = old {
                old.shutdown();
            }
            let _gate = inner.gate.lock().unwrap();
            let current = {
                let active = inner.active.lock().unwrap();
                active.generation == id && active.closing
            };
            if current {
                (inner.sink)(Event::Closed);
            }
        });
    }

    /// Runs a command on its own thread; `Event::Done` reports its result.
    pub fn run(&self, command: Command, options: RunOptions) -> Ticket {
        let ticket = self.inner.ticket.fetch_add(1, Ordering::Relaxed) + 1;
        let (id, session) = {
            let active = self.inner.active.lock().unwrap();
            (active.generation, active.session.clone())
        };
        let sink = self.inner.sink.clone();
        thread::spawn(move || {
            let report = |progress| {
                sink(Event::Progress {
                    session: id,
                    ticket,
                    progress,
                })
            };
            let result = match session {
                Some(session) => crate::commands::execute(&session, &command, &options, &report),
                None => Err(Error::new(
                    "no adapter selected; use adapter list and adapter select PORT",
                )),
            };
            sink(Event::Done {
                session: id,
                ticket,
                result,
            });
        });
        ticket
    }

    /// The session's view, without hidden candidates.
    pub fn state(&self) -> Option<State> {
        let session = self.inner.active.lock().unwrap().session.clone();
        session.map(|s| s.state())
    }

    /// Hides a candidate until the next scan.
    pub fn hide_candidate(&self, id: u32) {
        let session = self.inner.active.lock().unwrap().session.clone();
        if let Some(session) = session {
            session.cell.update(|st| st.hidden.insert(id));
        }
    }
}

impl Drop for Shared {
    fn drop(&mut self) {
        let active = self.active.get_mut().unwrap();
        if let Some(session) = active.session.take().or_else(|| active.opening.take()) {
            session.shutdown();
        }
    }
}

fn open(
    inner: &Arc<Shared>,
    id: SessionId,
    port: String,
    adapter: Option<String>,
    old: Option<Arc<Session>>,
) {
    if let Some(old) = old {
        old.shutdown();
    }
    let gate = inner.gate.lock().unwrap();
    {
        let mut active = inner.active.lock().unwrap();
        if active.generation != id || active.closing {
            return;
        }
        active.opening = None;
    }
    let report = |phase| {
        (inner.sink)(Event::Connection {
            session: id,
            port: port.clone(),
            phase,
        });
    };
    let failed = |error: Error| report(Phase::Failed { error, open: false });
    let cell = Arc::new(Cell {
        state: Mutex::new(State {
            session: id,
            port: port.clone(),
            available: true,
            ..State::default()
        }),
        changed: Condvar::new(),
    });
    let closing = Arc::new(AtomicBool::new(false));
    // The interface registers a session before it handles that session's loss.
    let phases = Arc::new(Mutex::new(OpeningPhases::default()));
    let ending = Arc::new(Mutex::new(None));
    let handler: Handler = Arc::new(Mutex::new({
        let (cell, sink, closing, port, phases) = (
            cell.clone(),
            inner.sink.clone(),
            closing.clone(),
            port.clone(),
            phases.clone(),
        );
        let ending = ending.clone();
        Box::new(move |received: Received<'_>| match received {
            Received::Event(event) => {
                let Some(kind) = &event.kind else { return };
                if let p::event::Kind::SessionEnding(event) = kind {
                    *ending.lock().unwrap() = Some(event.reason());
                }
                let (first, changed) = cell.update(|st| {
                    let first = match kind {
                        p::event::Kind::ScanFound(c) => st.candidate(c.id).is_none(),
                        p::event::Kind::Device(d) => st.device(d.id).is_none(),
                        _ => false,
                    };
                    let changed = match kind {
                        p::event::Kind::SettingsChanged(s) => {
                            let old = st.settings_of(s.device);
                            settings::settings(&s.changed)
                                .into_iter()
                                .filter(|n| !old.contains(n))
                                .map(|n| n.field)
                                .collect()
                        }
                        _ => Vec::new(),
                    };
                    st.event(kind);
                    (first, changed)
                });
                sink(Event::Notice {
                    session: id,
                    notice: Notice::Event {
                        event,
                        first,
                        changed,
                    },
                });
            }
            Received::PairingComplete { candidate } => cell.update(|st| {
                st.pairing = None;
                st.paired_candidate = Some(candidate);
            }),
            Received::Response(request, response) => {
                cell.update(|st| st.response(request, response))
            }
            Received::Closed(error) => {
                cell.update(|st| st.available = false);
                if !closing.load(Ordering::Acquire)
                    && !cell.snapshot().pending_for(PROFILE_RECONNECT, 0)
                {
                    let error = Error::from_client(error.clone(), "");
                    let mut phases = phases.lock().unwrap();
                    if phases.announced {
                        sink(Event::Connection {
                            session: id,
                            port: port.clone(),
                            phase: Phase::Lost(error),
                        });
                    } else {
                        phases.lost = Some(error);
                    }
                }
            }
        })
    }));
    let receive = handler.clone();
    let connection = match (inner.connect)(
        &port,
        Box::new(move |received| receive.lock().unwrap()(received)),
    ) {
        Ok(connection) => connection,
        Err(error) => return failed(Error::new(error.to_string())),
    };
    let session = Arc::new(Session {
        id,
        connection: Mutex::new(Arc::new(connection)),
        connect: inner.connect.clone(),
        ports: inner.ports.clone(),
        handler,
        ending,
        cell,
        files: Mutex::new(()),
        sink: inner.sink.clone(),
        closing,
    });
    {
        let mut active = inner.active.lock().unwrap();
        if active.generation != id || active.closing {
            drop(active);
            session.shutdown();
            return;
        }
        active.opening = Some(session.clone());
    }
    let handshake = session.connection().handshake();
    let established = handshake.is_ok();
    let opened = handshake.and_then(|hello| {
        if hello.compatibility() != cordial_client::compatibility::Compatibility::Current {
            return Err(cordial_client::Error::Incompatible(hello));
        }
        session.connection().status()
    });
    if let Err(e) = opened {
        let report = {
            let mut active = inner.active.lock().unwrap();
            let current = active.generation == id && !active.closing;
            if current {
                active.opening = None;
            }
            current
        };
        session.shutdown();
        if report {
            let error = Error::from_client(e, "adapter status");
            if established && error.native_hello.is_none() {
                session.phase(Phase::Lost(error));
            } else {
                failed(error);
            }
        }
        return;
    }
    if adapter.is_some_and(|a| !session.state().status.id.eq_ignore_ascii_case(&a)) {
        session.shutdown();
        return report(Phase::Other);
    }
    {
        let mut active = inner.active.lock().unwrap();
        if active.generation != id || active.closing {
            drop(active);
            session.shutdown();
            return;
        }
        active.opening = None;
        active.session = Some(session.clone());
    }
    drop(gate);
    let phase = match crate::migration::gate(&session.state().status) {
        crate::migration::Gate::Normal => Phase::Opened,
        crate::migration::Gate::Assist => Phase::Migrate,
        crate::migration::Gate::Blocked(e) => Phase::Blocked(e),
        crate::migration::Gate::Unsupported(e) => Phase::Unsupported(e),
    };
    let normal = matches!(phase, Phase::Opened);
    {
        let mut phases = phases.lock().unwrap();
        session.phase(phase);
        phases.announced = true;
        if let Some(error) = phases.lost.take() {
            session.phase(Phase::Lost(error));
            return;
        }
    }
    if !normal {
        return;
    }
    let current = || {
        let active = inner.active.lock().unwrap();
        active.generation == id && !active.closing
    };
    // Waits for readiness, then loads the saved devices. Readiness can also be lost and come
    // back, which loads them again.
    let mut waiting = false;
    loop {
        if session.reconnecting() {
            let st = session.cell.state.lock().unwrap();
            let _ = session
                .cell
                .changed
                .wait_timeout_while(st, Duration::from_millis(50), |st| {
                    st.pending_for(PROFILE_RECONNECT, 0)
                })
                .unwrap();
            if !current() {
                return;
            }
            continue;
        }
        let ready = session
            .cell
            .wait_for(&Wait::unlimited(), |st| {
                if !current() || st.pending_for(PROFILE_RECONNECT, 0) {
                    return Some(None);
                }
                (st.status.ready && !st.loaded)
                    .then_some(Some(true))
                    .or((!st.status.ready && !waiting).then_some(Some(false)))
            })
            .ok()
            .flatten();
        match ready {
            None if session.reconnecting() && current() => continue,
            None => return,
            Some(false) => {
                waiting = true;
                session.cell.update(|st| st.loaded = false);
                session.phase(Phase::Waiting);
            }
            Some(true) => {
                waiting = false;
                match session.load() {
                    Ok(()) => session.phase(Phase::Ready),
                    Err(error) if !current() || !session.state().available => {
                        let _ = error;
                        return;
                    }
                    Err(error) => {
                        session.phase(Phase::Failed { error, open: true });
                        return;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod reconnect_tests {
    use super::*;
    use crate::sim::Dongle;
    use std::sync::mpsc;

    #[test]
    fn a_connection_closed_between_identity_and_admission_stays_unavailable() {
        let dongle = Dongle::default();
        let adapter = dongle.clone();
        let (tx, events) = mpsc::channel();
        let (entered, identity) = mpsc::channel();
        let (release, gate) = mpsc::channel();
        let gate = Arc::new(Mutex::new(gate));
        let (seen, delivered) = mpsc::channel();
        let controller = Controller::with_connector(
            move |e| {
                let _ = tx.send(e);
            },
            move |_, mut handler| {
                let replacement = adapter.sessions() == 1;
                let (entered, gate, seen) = (entered.clone(), gate.clone(), seen.clone());
                adapter.connect(Box::new(move |received| {
                    if replacement {
                        match &received {
                            Received::Response(_, _) => {
                                entered.send(()).unwrap();
                                gate.lock()
                                    .unwrap()
                                    .recv_timeout(Duration::from_secs(5))
                                    .unwrap();
                            }
                            Received::Event(_) => {
                                seen.send(()).unwrap();
                            }
                            _ => {}
                        }
                    }
                    handler(received);
                }))
            },
        );
        controller.open("sim".into());
        while !matches!(
            events.recv_timeout(Duration::from_secs(5)).unwrap(),
            Event::Connection {
                phase: Phase::Ready,
                ..
            }
        ) {}
        let session = controller
            .inner
            .active
            .lock()
            .unwrap()
            .session
            .clone()
            .unwrap();
        let reconnecting = session.clone();
        let (started, pending) = mpsc::channel();
        let worker = thread::spawn(move || {
            let mut guard = reconnecting.profile_reconnect(1);
            started.send(()).unwrap();
            guard.reconnect(&Wait::timeout(Duration::from_secs(3)))
        });
        pending.recv_timeout(Duration::from_secs(5)).unwrap();
        dongle.end_session(p::SessionEndingReason::ConfigurationChange as i32);
        identity.recv_timeout(Duration::from_secs(5)).unwrap();
        // Admission needs this lock. The following event proves the identity response has
        // already been queued to the request waiter before the connection closes.
        let active = session.connection.lock().unwrap();
        release.send(()).unwrap();
        dongle.event(p::event::Kind::DeviceRemoved(p::DeviceRemoved { id: 999 }));
        delivered.recv_timeout(Duration::from_secs(5)).unwrap();
        active.close_and_wait_release();
        drop(active);
        assert!(worker.join().unwrap().is_err());
        assert!(!session.state().available);
        assert_eq!(dongle.handles(), 0);
        assert!(events.try_iter().any(|e| matches!(
            e,
            Event::Connection {
                phase: Phase::Lost(_),
                ..
            }
        )));
        controller.close();
    }
}
