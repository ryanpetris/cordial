//! The foreground session with one adapter, shared by the shell, scripts and the TUI. It keeps
//! the view current from the Dongle's events and runs each command on its own thread.
pub use crate::view::{SessionId, State};
use crate::{error::Error, view::Pending};
use cordial_client::{Connection, Received};
use cordial_protocol::{self as p, request};
use std::{
    io,
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
    /// didn't hold before; `changed` lists the setting keys a settings event changed.
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
            return Err(Error::new("cancelled"));
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

#[derive(Clone, Debug, PartialEq)]
pub enum SettingInput {
    /// Text typed in the shell, typed by the setting.
    Text(String),
    Value(p::value::Value),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Command {
    Status,
    /// Renames the adapter; `None` restores its default name.
    Name(Option<String>),
    Platform(p::Platform),
    Bootloader,
    /// Scans the transports, or every supported one when empty, for `seconds` (0 means the
    /// adapter's default).
    Scan {
        transports: Vec<p::Transport>,
        seconds: u32,
    },
    ScanStop,
    Pair(String),
    /// Answers the open pairing prompt, with the code it asks for.
    Accept(Option<String>),
    Reject,
    CancelPairing,
    Devices,
    Get(String),
    Connect(String),
    Disconnect(String),
    Unpair(String),
    Refresh(String),
    Set(String, Toggle, bool),
    Warnings(String),
    Settings(String),
    SettingGet(String, String),
    SettingSet(String, String, SettingInput),
    SettingForget(String, String),
    /// Saves and forgets several settings of one device: values in one write, then forgets in
    /// one write.
    SettingsSave {
        device: String,
        set: Vec<(String, p::value::Value)>,
        forget: Vec<String>,
    },
    Features(String),
    Files(String),
    /// Downloads an adapter file; an existing destination is replaced only with `overwrite`.
    FileGet {
        path: String,
        local: PathBuf,
        overwrite: bool,
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
    pub id: String,
    pub name: String,
}

#[derive(Clone, Debug)]
#[allow(clippy::large_enum_variant)] // One result per command.
pub enum Outcome {
    Status(p::Status),
    Name(String),
    Platform(p::Platform),
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
    Candidate(p::Candidate),
    Hidden(Subject),
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
    Files {
        path: String,
        entries: Vec<p::FileEntry>,
    },
    FileSaved {
        path: String,
        local: PathBuf,
        bytes: u64,
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
                return Err(Error::new("the adapter connection closed"));
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
    pub fn notice(&self, notice: Notice) {
        (self.sink)(Event::Notice {
            session: self.id,
            notice,
        });
    }

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

    /// Sends one command. A command an interface ran reports its response.
    pub fn call(
        &self,
        command: request::Command,
        name: &'static str,
        reported: bool,
    ) -> Result<p::Response, Error> {
        let response = self
            .connection
            .request(command)
            .map_err(|e| Error::from_client(e, name))?;
        if reported {
            self.notice(Notice::Response(response.clone()));
        }
        if let Some(p::response::Result::Error(e)) = response.result {
            return Err(Error::dongle(e, Some(name)));
        }
        Ok(response)
    }

    /// Lists the saved devices and their warnings.
    fn load(&self) -> Result<(), Error> {
        let devices = self
            .connection
            .list_devices()
            .map_err(|e| Error::from_client(e, "device list"))?;
        for d in &devices {
            self.connection
                .list_warnings(&d.id)
                .map_err(|e| Error::from_client(e, "warning list"))?;
        }
        self.cell.update(|st| st.loaded = true);
        Ok(())
    }

    fn shutdown(&self) {
        self.closing.store(true, Ordering::Release);
        self.connection.close();
    }

    /// Marks a running command for progress shown beside its target.
    pub fn pending(&self, command: &'static str, target: Option<&str>) -> PendingGuard<'_> {
        let pending = Pending {
            command,
            target: target.map(str::to_owned),
        };
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
    pub fn hide_candidate(&self, id: &str) {
        let session = self.inner.active.lock().unwrap().session.clone();
        if let Some(session) = session {
            session.cell.update(|st| st.hidden.insert(id.to_owned()));
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
                        p::event::Kind::ScanFound(c) => st.candidate(&c.id).is_none(),
                        p::event::Kind::Device(d) => st.device(&d.id).is_none(),
                        _ => false,
                    };
                    let changed = match kind {
                        p::event::Kind::Settings(s) => {
                            let old = st.settings_of(&s.device);
                            s.settings
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
