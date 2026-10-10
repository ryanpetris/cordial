//! The foreground session with one adapter, shared by the shell, scripts and the TUI. It keeps
//! the view current from the Dongle's events and runs each command on its own thread.
pub use crate::view::{SessionId, State};
use crate::{error::Error, profiles::InterfaceUpdate, view::Pending};
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
    Done {
        session: SessionId,
        ticket: Ticket,
        result: Result<Outcome, Error>,
    },
    /// The controller's session closed after `close`.
    Closed,
}

#[derive(Clone, Debug)]
pub enum Phase {
    /// The port is open and the adapter's status read.
    Opened,
    /// The adapter is starting; commands that need Bluetooth or storage wait.
    Waiting,
    /// The adapter is ready and its saved devices are loaded.
    Ready,
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
    /// didn't hold before; `changed` lists the setting keys a settings_changed event changed.
    Event {
        event: p::Event,
        first: bool,
        changed: Vec<String>,
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
    Value(p::value::Value),
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
    /// Sets a device's layers: profiles in the order they apply, or none.
    Layers(Target, Vec<Target>),
    Warnings(Target),
    Settings(Target),
    SettingGet(Target, String),
    SettingSet(Target, String, SettingInput),
    SettingForget(Target, String),
    /// Saves and forgets several settings of one device in one request.
    SettingsSave {
        device: u32,
        set: Vec<(String, p::value::Value)>,
        forget: Vec<String>,
    },
    Features(Target),
    /// One page of saved profiles with IDs above `after`.
    Profiles {
        after: u32,
    },
    /// Every saved profile, read page by page.
    AllProfiles,
    /// A profile's name and roles.
    ProfileShow(Target),
    /// Reads a profile's name, for showing where it is used.
    ProfileLookup(u32),
    /// Creates an empty profile.
    ProfileCreate(String),
    /// Copies a profile under a new name.
    ProfileCopy(Target, String),
    ProfileDelete(Target),
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
}

impl Command {
    /// Runs without adapter readiness: inspection, file access and recovery work while
    /// Bluetooth or storage is unavailable.
    pub fn direct(&self) -> bool {
        matches!(
            self,
            Self::Status | Self::Bootloader | Self::Files(_) | Self::FileGet { .. }
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
    Bootloader,
    ScanStarted(Vec<p::Transport>),
    ScanFinished {
        count: u32,
        truncated: bool,
    },
    ScanStopped {
        was_running: bool,
    },
    /// The bond is saved; `device` is the saved record once known.
    Paired {
        subject: Subject,
        device: Option<p::Device>,
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
    Warnings {
        subject: Subject,
        warnings: Vec<p::DeviceWarning>,
    },
    Settings(Subject),
    Setting {
        subject: Subject,
        setting: p::Setting,
    },
    /// Saved values and forgotten keys of one device, with the settings as saved.
    Saved {
        subject: Subject,
        set: Vec<String>,
        forget: Vec<String>,
        settings: Vec<p::Setting>,
    },
    Features {
        subject: Subject,
        features: Vec<p::Feature>,
    },
    /// A page of profiles and the cursor it was read after, or every profile as one page that
    /// ends the listing.
    Profiles {
        after: u32,
        list: p::ProfileList,
    },
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
}

/// Opens a connection whose handler receives everything the Dongle sends.
pub type Connector =
    dyn Fn(&str, Box<dyn FnMut(Received<'_>) + Send>) -> io::Result<Connection> + Send + Sync;

type Sink = Arc<dyn Fn(Event) + Send + Sync>;

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
    pub port: String,
    pub connection: Connection,
    pub cell: Arc<Cell>,
    sink: Sink,
    closing: Arc<AtomicBool>,
}

impl Session {
    fn phase(&self, phase: Phase) {
        (self.sink)(Event::Connection {
            session: self.id,
            port: self.port.clone(),
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
            self.connection.request_with(command, move |response| {
                sink(Event::Notice {
                    session,
                    notice: Notice::Response(response.clone()),
                })
            })
        } else {
            self.connection.request(command)
        }
        .map_err(|e| Error::from_client(e, name))?;
        if let Some(p::response::Result::Error(e)) = response.result {
            return Err(Error::dongle(e, Some(name)));
        }
        Ok(response)
    }

    /// Reads every page of saved devices and the warnings of each connected one, and the
    /// names of the profiles devices and configuration interfaces use.
    fn load(&self) -> Result<(), Error> {
        let devices = self
            .connection
            .all_devices()
            .map_err(|e| Error::from_client(e, "device list"))?;
        for id in crate::profiles::unnamed(&self.state()) {
            // A name that can't be read is shown as the profile's ID.
            let _ = self.connection.get_profile(id);
        }
        // A device has warnings only while it has a link.
        for d in devices.iter().filter_map(|e| match &e.entry {
            Some(p::device_list_entry::Entry::Device(d))
                if d.state() != p::DeviceState::Disconnected =>
            {
                Some(d)
            }
            _ => None,
        }) {
            self.connection
                .all_warnings(d.id)
                .map_err(|e| Error::from_client(e, "warning list"))?;
        }
        self.cell.update(|st| st.loaded = true);
        Ok(())
    }

    fn shutdown(&self) {
        self.closing.store(true, Ordering::Release);
        self.connection.close();
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
    closing: bool,
}

struct Shared {
    active: Mutex<Active>,
    /// Opening and closing run one at a time, so an old session ends before a new one opens.
    gate: Mutex<()>,
    ticket: AtomicU64,
    sink: Sink,
    connect: Arc<Connector>,
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
        Self {
            inner: Arc::new(Shared {
                active: Mutex::new(Active {
                    generation: 0,
                    session: None,
                    closing: false,
                }),
                gate: Mutex::new(()),
                ticket: AtomicU64::new(0),
                sink: Arc::new(sink),
                connect: Arc::new(connect),
            }),
        }
    }

    /// Opens a port, replacing any session. Phases follow as `Event::Connection`.
    pub fn open(&self, port: String) -> SessionId {
        let (id, old) = {
            let mut active = self.inner.active.lock().unwrap();
            active.generation += 1;
            active.closing = false;
            (active.generation, active.session.take())
        };
        let inner = self.inner.clone();
        thread::spawn(move || open(&inner, id, port, old));
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
            (active.generation, active.session.take())
        };
        let inner = self.inner.clone();
        thread::spawn(move || {
            let _gate = inner.gate.lock().unwrap();
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

    /// Runs a command on its own thread; `Event::Done` reports its result.
    pub fn run(&self, command: Command, options: RunOptions) -> Ticket {
        let ticket = self.inner.ticket.fetch_add(1, Ordering::Relaxed) + 1;
        let (id, session) = {
            let active = self.inner.active.lock().unwrap();
            (active.generation, active.session.clone())
        };
        let sink = self.inner.sink.clone();
        thread::spawn(move || {
            let result = match session {
                Some(session) => crate::commands::execute(&session, &command, &options),
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
        if let Some(session) = self.active.get_mut().unwrap().session.take() {
            session.shutdown();
        }
    }
}

fn open(inner: &Arc<Shared>, id: SessionId, port: String, old: Option<Arc<Session>>) {
    let gate = inner.gate.lock().unwrap();
    if let Some(old) = old {
        old.shutdown();
    }
    let failed = |error: Error| {
        (inner.sink)(Event::Connection {
            session: id,
            port: port.clone(),
            phase: Phase::Failed { error, open: false },
        });
    };
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
    let handler = {
        let (cell, sink, closing, port) = (
            cell.clone(),
            inner.sink.clone(),
            closing.clone(),
            port.clone(),
        );
        Box::new(move |received: Received<'_>| match received {
            Received::Event(event) => {
                let Some(kind) = &event.kind else { return };
                let (first, changed) = cell.update(|st| {
                    let first = match kind {
                        p::event::Kind::ScanFound(c) => st.candidate(c.id).is_none(),
                        p::event::Kind::Device(d) => st.device(d.id).is_none(),
                        _ => false,
                    };
                    let changed = match kind {
                        p::event::Kind::SettingsChanged(s) => {
                            let old = st.settings_of(s.device);
                            s.changed
                                .iter()
                                .filter(|n| !old.contains(n))
                                .map(|n| n.key.clone())
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
            Received::Response(request, response) => {
                cell.update(|st| st.response(request, response))
            }
            Received::Closed(error) => {
                cell.update(|st| st.available = false);
                if !closing.load(Ordering::Acquire) {
                    sink(Event::Connection {
                        session: id,
                        port: port.clone(),
                        phase: Phase::Lost(Error::from_client(error.clone(), "")),
                    });
                }
            }
        })
    };
    let connection = match (inner.connect)(&port, handler) {
        Ok(connection) => connection,
        Err(error) => return failed(Error::new(error.to_string())),
    };
    let session = Arc::new(Session {
        id,
        port: port.clone(),
        connection,
        cell,
        sink: inner.sink.clone(),
        closing,
    });
    if let Err(e) = session.connection.status() {
        session.shutdown();
        return failed(Error::from_client(e, "adapter status"));
    }
    {
        let mut active = inner.active.lock().unwrap();
        if active.generation != id || active.closing {
            drop(active);
            session.shutdown();
            return;
        }
        active.session = Some(session.clone());
    }
    drop(gate);
    session.phase(Phase::Opened);
    let current = || {
        let active = inner.active.lock().unwrap();
        active.generation == id && !active.closing
    };
    // Waits for readiness, then loads the saved devices. Readiness can also be lost and come
    // back, which loads them again.
    let mut waiting = false;
    loop {
        let ready = session
            .cell
            .wait_for(&Wait::unlimited(), |st| {
                if !current() {
                    return Some(None);
                }
                (st.status.ready && !st.loaded)
                    .then_some(Some(true))
                    .or((!st.status.ready && !waiting).then_some(Some(false)))
            })
            .ok()
            .flatten();
        match ready {
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
