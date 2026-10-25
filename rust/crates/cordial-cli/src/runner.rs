//! Foreground, non-terminal command runner. No background service or reconnect loop.
use crate::{
    controller::{
        Cancellation, Command, Controller, Event, Notice, Outcome, Phase, RunOptions, SessionId,
        State, Ticket, Wait,
    },
    error::Error,
    migration::{self, Engine, Worker},
    ui::{
        command::{self, Line},
        text::{self, Filter},
    },
};
use cordial_client::{Connection, Received, serial};
use cordial_protocol::{self as p, event, message};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::{self, BufRead, BufReader, Read, Write},
    sync::mpsc::{self, Receiver, RecvTimeoutError},
    thread,
    time::{Duration, Instant},
};

#[derive(Clone, Debug, Default)]
pub struct Options {
    pub port: Option<String>,
    pub json: bool,
    pub timeout: Duration,
    pub args: Vec<String>,
}
impl Options {
    pub fn parse(args: impl IntoIterator<Item = String>) -> Result<Self, String> {
        let mut options = Self::default();
        let mut args = args.into_iter();
        while let Some(arg) = args.next() {
            let (flag, value) = arg
                .split_once('=')
                .map_or((arg.as_str(), None), |(k, v)| (k, Some(v.to_string())));
            match flag {
                "--port" => {
                    options.port = Some(
                        value
                            .or_else(|| args.next())
                            .filter(|v| !v.is_empty())
                            .ok_or("--port needs a serial port")?,
                    )
                }
                "--json" if value.is_none() => options.json = true,
                "--timeout" => {
                    let value = value
                        .or_else(|| args.next())
                        .ok_or("--timeout needs seconds")?;
                    let seconds: f64 = value
                        .parse()
                        .map_err(|_| "--timeout must be 0 through 3600 seconds")?;
                    if !seconds.is_finite() || !(0.0..=3600.0).contains(&seconds) {
                        return Err("--timeout must be 0 through 3600 seconds".into());
                    }
                    options.timeout = Duration::from_secs_f64(seconds);
                }
                "--" => {
                    options.args.extend(args);
                    break;
                }
                "--version" | "--help" | "-h" if value.is_none() => {
                    options.args = vec![flag.into()];
                    return Ok(options);
                }
                _ if arg.starts_with('-') => return Err(format!("unknown option: {arg}")),
                _ => {
                    options.args.push(arg);
                    options.args.extend(args);
                    break;
                }
            }
        }
        Ok(options)
    }
}

/// How long a script waits for an adapter that is still starting.
const STARTUP_LIMIT: Duration = Duration::from_secs(35);

/// How long a one-shot scan that ends at the timeout may take to stop.
const SCAN_STOP: Duration = Duration::from_secs(2);

fn write_line(out: &mut dyn Write, line: &str) -> Result<(), Error> {
    if !line.is_empty() {
        writeln!(out, "{line}")?;
        out.flush()?;
    }
    Ok(())
}

/// A message as one line of protobuf JSON.
fn json(kind: message::Kind) -> String {
    let message = p::Message { kind: Some(kind) };
    text::terminal_json(&serde_json::to_string(&message).unwrap_or_default())
}

pub struct Script {
    pub options: Options,
    pub cancellation: Cancellation,
    /// Migrates an adapter whose saved data needs it before any command runs.
    pub engine: Engine,
}

type Handler = Box<dyn FnMut(Received<'_>) + Send>;

impl Script {
    pub fn run(
        &self,
        input: impl Read + Send + 'static,
        out: &mut dyn Write,
        diagnostics: &mut dyn Write,
    ) -> Result<(), Error> {
        self.with_connector(input, out, diagnostics, |port, handler| {
            serial::open_with_handler(port, handler)
        })
    }

    /// Runs the same foreground loop against an in-memory adapter in tests.
    pub fn with_connector(
        &self,
        input: impl Read + Send + 'static,
        out: &mut dyn Write,
        diagnostics: &mut dyn Write,
        connect: impl Fn(&str, Handler) -> io::Result<Connection> + Send + Sync + 'static,
    ) -> Result<(), Error> {
        let one_shot = !self.options.args.is_empty();
        let first = if one_shot {
            Some(command::parse(&self.options.args).map_err(Error::new)?)
        } else {
            None
        };
        if let Some(Line::Help(_) | Line::List) = &first {
            return self.local(first.as_ref().unwrap(), None, out, diagnostics);
        }
        let port = match &self.options.port {
            Some(port) => port.clone(),
            None => {
                let ports = serial::ports()?;
                if ports.len() != 1 {
                    return Err(Error::new(
                        "select an adapter with --port; use cordial adapter list",
                    ));
                }
                ports[0].port.clone()
            }
        };
        let (sink, events) = mpsc::sync_channel(512);
        let controller = Controller::with_connector(
            move |event| {
                let _ = sink.send(event);
            },
            connect,
        );
        let mut run = Run {
            script: self,
            controller: &controller,
            events: &events,
            input: Some(Box::new(input)),
            lines: None,
            one_shot,
            session: 0,
            foreground: None,
            pending: BTreeMap::new(),
            answered: None,
            migrated: BTreeSet::new(),
            closed: false,
        };
        let result = run.run_loop(port, first, out, diagnostics);
        controller.close();
        // With --json, events that arrive before the session closes are still printed. A
        // migration that ended the run has closed it already.
        let end = Instant::now() + Duration::from_secs(2);
        while !run.closed && Instant::now() < end {
            match events.recv_timeout(Duration::from_millis(25)) {
                Ok(Event::Closed) | Err(RecvTimeoutError::Disconnected) => break,
                Ok(Event::Notice { session, notice })
                    if self.options.json && session == run.session =>
                {
                    let _ = run.notice(&notice, None, out, diagnostics);
                }
                _ => {}
            }
        }
        result
    }

    fn local(
        &self,
        line: &Line,
        state: Option<&State>,
        out: &mut dyn Write,
        diagnostics: &mut dyn Write,
    ) -> Result<(), Error> {
        match line {
            Line::Help(topic) => write_line(
                if self.options.json { diagnostics } else { out },
                &command::help_on(state, topic.as_deref()),
            ),
            Line::List => {
                let ports = serial::ports()?;
                let value = if self.options.json {
                    text::ports_json(&ports)
                } else {
                    text::ports(&ports)
                };
                write_line(out, &value)
            }
            _ => Ok(()),
        }
    }

    fn wait(&self, deadline: Option<Instant>) -> Wait {
        Wait {
            deadline,
            cancellation: self.cancellation.clone(),
        }
    }
}

type Lines = Receiver<Result<Option<String>, Error>>;

struct Run<'a> {
    script: &'a Script,
    controller: &'a Controller,
    events: &'a Receiver<Event>,
    /// Standard input, until the first line is needed.
    input: Option<Box<dyn Read + Send>>,
    lines: Option<Lines>,
    one_shot: bool,
    session: SessionId,
    foreground: Option<Ticket>,
    pending: BTreeMap<Ticket, (Command, Filter)>,
    /// The pairing step last answered, so one prompt takes one answer.
    answered: Option<p::Pairing>,
    /// The adapters migrated in this run. One that still needs migration when it reopens isn't
    /// migrated again.
    migrated: BTreeSet<String>,
    /// The session has closed for a migration and nothing was opened since.
    closed: bool,
}

impl Run<'_> {
    fn json(&self) -> bool {
        self.script.options.json
    }

    fn notice(
        &self,
        notice: &Notice,
        state: Option<&State>,
        out: &mut dyn Write,
        diagnostics: &mut dyn Write,
    ) -> Result<(), Error> {
        if self.json() {
            match notice {
                Notice::Event { event, .. } => {
                    return write_line(out, &json(message::Kind::Event(event.clone())));
                }
                Notice::Response(response) => {
                    return write_line(out, &json(message::Kind::Response(response.clone())));
                }
                Notice::RefreshFailed(_) => {}
            }
        }
        // A one-shot scan reports its own summary.
        if let Notice::Event { event, .. } = notice
            && matches!(event.kind, Some(event::Kind::ScanDone(_)))
            && self.one_shot
            && self
                .pending
                .values()
                .any(|(c, _)| matches!(c, Command::Scan { .. }))
        {
            return Ok(());
        }
        if let Some(line) = text::notice(notice, state) {
            write_line(if self.json() { diagnostics } else { out }, &line)?;
        }
        Ok(())
    }

    /// The prompt typed lines answer now, unless it was already answered.
    fn prompt(&self) -> Option<crate::model::Prompt> {
        let st = self.controller.state()?;
        let prompt = command::answerable(&st)?;
        let answering = self
            .pending
            .values()
            .any(|(c, _)| matches!(c, Command::Accept(_) | Command::Reject));
        (!answering && self.answered != st.pairing).then_some(prompt)
    }

    /// The next typed line, if one has arrived. Input is read only from the first time a line
    /// is needed, so a one-shot command that never asks for one leaves standard input unread.
    fn line(&mut self) -> Result<Result<Option<String>, Error>, mpsc::TryRecvError> {
        let input = &mut self.input;
        let lines = self
            .lines
            .get_or_insert_with(|| read_lines(input.take().expect("input is read once")));
        lines.try_recv()
    }

    fn start(&mut self, command: Command, filter: Filter, deadline: Option<Instant>) -> Ticket {
        let ticket = self.controller.run(
            command.clone(),
            RunOptions {
                one_shot: self.one_shot,
                wait: self.script.wait(deadline),
            },
        );
        self.pending.insert(ticket, (command, filter));
        if self.foreground.is_none() {
            self.foreground = Some(ticket);
        }
        ticket
    }

    fn run_loop(
        &mut self,
        port: String,
        mut next: Option<Line>,
        out: &mut dyn Write,
        diagnostics: &mut dyn Write,
    ) -> Result<(), Error> {
        let timeout = self.script.options.timeout;
        let mut deadline = (self.one_shot && !timeout.is_zero()).then(|| Instant::now() + timeout);
        self.session = self.controller.open(port);
        let mut opened = false;
        let mut ready = false;
        // Why the adapter didn't become ready; the session stays for direct commands.
        let mut unready: Option<Error> = None;
        let mut lost: Option<Error> = None;
        let mut starting: Option<Instant> = None;
        let mut told = false;
        // A one-shot scan as long as the timeout, which ends at the deadline, and whether it was
        // stopped there.
        let mut timed_scan = false;
        let mut stopped = false;
        loop {
            if self.script.cancellation.cancelled() {
                return Err(Error::new("interrupted"));
            }
            if let Some(d) = deadline
                && Instant::now() >= d
            {
                let scanning = || {
                    self.controller
                        .state()
                        .is_some_and(|s| s.scanning.is_some())
                };
                if timed_scan && !stopped && scanning() {
                    // Stopped only once the adapter has started it, so the stop can't arrive
                    // first. The scan reports its own result once it stops; this one isn't
                    // awaited.
                    self.controller.run(
                        Command::ScanStop,
                        RunOptions {
                            one_shot: true,
                            wait: self.script.wait(None),
                        },
                    );
                    stopped = true;
                } else if !timed_scan || Instant::now() >= d + SCAN_STOP {
                    // A save already sent may have been stored without its answer arriving.
                    let timed_out = Error::from_client(cordial_client::Error::Timeout, "");
                    return Err(Error::new(
                        match self.pending.values().find(|(c, _)| c.saves()) {
                            Some((command, _)) => text::failure_text(command, &timed_out),
                            None => "operation timed out".into(),
                        },
                    ));
                }
            }
            if opened && next.is_none() {
                let prompt = self.prompt();
                if self.foreground.is_none() || prompt.is_some() {
                    match self.line() {
                        Ok(Ok(Some(line))) => {
                            if let Some(prompt) = prompt
                                && !line.starts_with('/')
                                && !line.starts_with("pair ")
                            {
                                let command =
                                    command::answer(&prompt, &line).map_err(Error::new)?;
                                self.answered = self.controller.state().and_then(|s| s.pairing);
                                self.start(command, Filter::All, deadline);
                            } else {
                                let words = command::split(line.strip_prefix('/').unwrap_or(&line))
                                    .map_err(Error::new)?;
                                if !words.is_empty() {
                                    next = Some(command::parse(&words).map_err(Error::new)?);
                                }
                            }
                        }
                        Ok(Ok(None)) | Err(mpsc::TryRecvError::Disconnected) => {
                            if prompt.is_some() {
                                return Err(Error::new("pairing needs an answer before EOF"));
                            }
                            if self.foreground.is_none() {
                                return Ok(());
                            }
                        }
                        Ok(Err(error)) => return Err(error),
                        Err(mpsc::TryRecvError::Empty) => {}
                    }
                }
            }
            if self.foreground.is_none()
                && let Some(error) = lost.take()
            {
                return Err(error);
            }
            // A command that needs the adapter ready waits for it before it runs.
            let held = !ready && next.as_ref().is_some_and(needs_ready);
            if held {
                if let Some(error) = unready.clone() {
                    return Err(error);
                }
                if starting.is_some_and(|d| Instant::now() >= d) {
                    return Err(Error::new("the adapter didn't finish starting in time"));
                }
                if starting.is_some() && !told {
                    told = true;
                    write_line(diagnostics, "Waiting for adapter readiness…")?;
                }
            }
            if opened
                && !held
                && let Some(line) = next.take()
            {
                let (line, filter) = match line {
                    Line::Devices(filter) => (Line::Run(Command::Devices), filter),
                    line => (line, Filter::All),
                };
                match line {
                    Line::Devices(_) => unreachable!(),
                    Line::Quit => return Ok(()),
                    Line::Migrate => {
                        let status = self.controller.state().unwrap_or_default().status;
                        if let migration::Gate::Blocked(e) = migration::gate(&status) {
                            return Err(Error::new(text::blocked_line(&e)));
                        }
                        let line = if self
                            .controller
                            .state()
                            .is_some_and(|s| self.migrated.contains(&s.status.id))
                        {
                            MIGRATED
                        } else {
                            "The adapter's saved data doesn't need migration."
                        };
                        write_line(if self.json() { diagnostics } else { out }, line)?;
                        if self.one_shot && self.foreground.is_none() {
                            return Ok(());
                        }
                    }
                    Line::MigrateCancel => return Err(Error::new("no migration is running")),
                    Line::Select(port) => {
                        self.session = self.controller.open(port);
                        opened = false;
                        ready = false;
                        unready = None;
                        starting = None;
                        told = false;
                        self.foreground = None;
                        self.pending.clear();
                        lost = None;
                        if self.one_shot {
                            next = Some(Line::Quit);
                        }
                    }
                    Line::Help(_) | Line::List => {
                        let st = self.controller.state();
                        self.script.local(&line, st.as_ref(), out, diagnostics)?;
                        if self.one_shot && self.foreground.is_none() {
                            return Ok(());
                        }
                    }
                    Line::Run(mut command) => {
                        // A one-shot scan without its own length scans until the timeout ends
                        // it; the adapter's own end comes later.
                        let timed = deadline.is_some()
                            && matches!(command, Command::Scan { seconds: 0, .. });
                        if let Command::Scan { seconds, .. } = &mut command
                            && timed
                        {
                            *seconds = timeout
                                .as_secs_f64()
                                .ceil()
                                .clamp(1.0, f64::from(crate::commands::MAX_SCAN_SECONDS))
                                as u32;
                        }
                        let grace = if timed {
                            SCAN_STOP
                        } else {
                            Duration::from_secs(1)
                        };
                        self.start(command, filter, deadline.map(|d| d + grace));
                        timed_scan |= timed;
                    }
                }
            }
            match self.events.recv_timeout(Duration::from_millis(20)) {
                Ok(Event::Connection {
                    session: id, phase, ..
                }) if id == self.session => match phase {
                    Phase::Opened => opened = true,
                    // A restart doesn't need the saved data migrated.
                    Phase::Migrate if matches!(next, Some(Line::Run(Command::Restart))) => {
                        opened = true;
                    }
                    // The migration replaces the session and waits as long as it takes.
                    Phase::Migrate => {
                        let id = self.controller.state().unwrap_or_default().status.id;
                        if self.migrated.contains(&id) {
                            return Err(Error::new(migration::REQUIRED));
                        }
                        let began = Instant::now();
                        let quiet = matches!(next, Some(Line::Migrate));
                        let port = self.migrate(quiet, diagnostics)?;
                        self.migrated.insert(id.clone());
                        deadline = deadline.map(|d| d + began.elapsed());
                        // The adapter's status must name it before anything else is read.
                        self.session = self.controller.open_adapter(port, id);
                        self.closed = false;
                        (opened, ready, unready, starting, told) =
                            (false, false, None, None, false);
                    }
                    Phase::Other => {
                        return Err(Error::new(
                            "another adapter answered on the migrated adapter's port",
                        ));
                    }
                    // Status and restart still run.
                    Phase::Blocked(error) => {
                        opened = true;
                        unready = Some(Error::new(text::blocked_line(&error)));
                    }
                    Phase::Unsupported(error) => return Err(error),
                    Phase::Ready => ready = true,
                    Phase::Failed { open: true, error } => unready = Some(error),
                    Phase::Failed { error, .. } => return Err(error),
                    // The command running reports its own outcome first.
                    Phase::Lost(error) => lost = Some(error),
                    Phase::Waiting => {
                        starting.get_or_insert(Instant::now() + STARTUP_LIMIT);
                    }
                },
                Ok(Event::Notice {
                    session: id,
                    notice,
                }) if id == self.session => {
                    let st = self.controller.state();
                    self.notice(&notice, st.as_ref(), out, diagnostics)?
                }
                Ok(Event::Done {
                    session: id,
                    ticket,
                    result,
                }) if id == self.session => {
                    if let Some((command, filter)) = self.pending.remove(&ticket) {
                        let state = self.controller.state();
                        let dest: &mut dyn Write = if self.json() { diagnostics } else { out };
                        match result {
                            Ok(outcome) => {
                                let line = if matches!(outcome, Outcome::Devices) {
                                    state
                                        .as_ref()
                                        .map(|s| text::devices(s, filter))
                                        .unwrap_or_default()
                                } else {
                                    text::outcome(&command, &outcome, state.as_ref())
                                };
                                write_line(dest, &line)?;
                            }
                            Err(error) => {
                                let error = text::failure_text(&command, &error);
                                if self.foreground == Some(ticket) {
                                    return Err(Error::new(error));
                                }
                                write_line(diagnostics, &format!("Error: {error}"))?;
                            }
                        }
                        if self.foreground == Some(ticket) {
                            self.foreground = None;
                            if self.one_shot {
                                return Ok(());
                            }
                        }
                    }
                }
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(Error::new("control event stream closed"));
                }
                _ => {}
            }
        }
    }
}

const MIGRATED: &str = "Migrated the adapter's saved data.";

impl Run<'_> {
    /// Migrates the open adapter and returns the port to open it on again. The session closes
    /// and releases its port first, and the worker runs until it finishes or Ctrl+C cancels it.
    fn migrate(&mut self, quiet: bool, diagnostics: &mut dyn Write) -> Result<String, Error> {
        let st = self.controller.state().unwrap_or_default();
        let (id, port) = (st.status.id, st.port);
        self.controller.close();
        loop {
            match self.events.recv() {
                Ok(Event::Closed) => break,
                Ok(_) => {}
                Err(_) => return Err(Error::new("control event stream closed")),
            }
        }
        self.closed = true;
        // The worker's result is awaited even if diagnostics can't be written.
        let _ = write_line(diagnostics, "Migrating the adapter's saved data…");
        let (tx, rx) = mpsc::channel();
        let done = tx.clone();
        let worker = Worker::start(
            &self.script.engine,
            id.clone(),
            Some(port.clone()),
            move |event| {
                let _ = tx.send(Ok(event));
            },
            move |result| {
                let _ = done.send(Err(result));
            },
        );
        let (mut waiting, mut cancelled) = (false, false);
        let result = loop {
            if !cancelled && self.script.cancellation.cancelled() {
                cancelled = true;
                worker.cancel();
            }
            use cordial_protocol::migration::migration_event::Event as M;
            match rx.recv_timeout(Duration::from_millis(50)) {
                Ok(Ok(event)) => match event.event {
                    Some(M::Waiting(_)) if !waiting => {
                        waiting = true;
                        let _ = write_line(diagnostics, "Waiting for the adapter…");
                    }
                    Some(M::Progress(_)) => waiting = false,
                    _ => {}
                },
                Ok(Err(result)) => break result,
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => {
                    unreachable!("the worker reports its result")
                }
            }
        };
        match result {
            Ok(cordial_client::migration::Outcome::Success) => {
                if !quiet {
                    write_line(diagnostics, MIGRATED)?;
                }
                let found = serial::ports()
                    .ok()
                    .and_then(|ports| ports.into_iter().find(|p| p.is_adapter(&id)));
                Ok(found.map_or(port, |p| p.port))
            }
            Ok(cordial_client::migration::Outcome::Cancelled) => Err(Error::new("interrupted")),
            Err(e) => Err(Error::new(migration::failure_text(&e))),
        }
    }
}

/// Whether a line runs only once the adapter is ready.
fn needs_ready(line: &Line) -> bool {
    match line {
        Line::Run(command) => !command.direct(),
        Line::Devices(_) => true,
        _ => false,
    }
}

/// Reads standard input line by line on its own thread.
fn read_lines(input: Box<dyn Read + Send>) -> Lines {
    let (send, lines) = mpsc::sync_channel(1);
    thread::spawn(move || {
        let mut input = BufReader::new(input);
        loop {
            let mut line = String::new();
            let result = match input.by_ref().take(4097).read_line(&mut line) {
                Ok(0) => Ok(None),
                Ok(_) if line.len() > 4096 => Err(Error::new("command line exceeds 4096 bytes")),
                Ok(_) => Ok(Some(line.trim_end_matches(['\r', '\n']).to_string())),
                Err(error) => Err(error.into()),
            };
            let end = !matches!(result, Ok(Some(_)));
            if send.send(result).is_err() || end {
                break;
            }
        }
    });
    lines
}
