//! Foreground, non-terminal command runner. No background service or reconnect loop.
use crate::{
    controller::{
        Cancellation, Command, Connector, Controller, Event, Notice, Outcome, Phase, RunOptions,
        SessionId, State, Ticket, Wait,
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
    sync::{
        Arc,
        mpsc::{self, Receiver, RecvTimeoutError},
    },
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
                    if arg == "tui" {
                        options.args.push(arg);
                        continue;
                    }
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
pub(crate) fn json(kind: message::Kind) -> String {
    let message = p::Message { kind: Some(kind) };
    text::terminal_json(&serde_json::to_string(&message).unwrap_or_default())
}

pub struct Script {
    pub options: Options,
    pub cancellation: Cancellation,
    /// Overrides the connector-backed migration engine used before commands run.
    pub engine: Option<Engine>,
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
        self.with_backend(input, out, diagnostics, connect, serial::ports)
    }

    /// Runs the foreground loop with supplied connections and port discovery.
    pub fn with_backend(
        &self,
        input: impl Read + Send + 'static,
        out: &mut dyn Write,
        diagnostics: &mut dyn Write,
        connect: impl Fn(&str, Handler) -> io::Result<Connection> + Send + Sync + 'static,
        ports: impl Fn() -> io::Result<Vec<serial::PortInfo>> + Send + Sync + 'static,
    ) -> Result<(), Error> {
        let one_shot = !self.options.args.is_empty();
        let first = if one_shot {
            Some(command::parse(&self.options.args).map_err(Error::new)?)
        } else {
            None
        };
        if let Some(Line::Help(_) | Line::List) = &first {
            return self.local(first.as_ref().unwrap(), None, out);
        }
        let port = match &self.options.port {
            Some(port) => port.clone(),
            None => {
                let ports = ports()?;
                if ports.len() != 1 {
                    return Err(Error::new(
                        "select an adapter with --port; use cordial adapter list",
                    ));
                }
                ports[0].port.clone()
            }
        };
        let (sink, events) = mpsc::sync_channel(512);
        let connector: Arc<Connector> = Arc::new(connect);
        let connector = if self.options.json {
            let raw = sink.clone();
            crate::json::observe(
                connector,
                Arc::new(move |diagnostic| {
                    use cordial_client::diagnostics::Diagnostic;
                    let kind = match diagnostic {
                        Diagnostic::Response(_, response) => {
                            message::Kind::Response(response.clone())
                        }
                        Diagnostic::Event(event) => message::Kind::Event(event.clone()),
                        Diagnostic::Completed(_) => return,
                    };
                    let _ = raw.try_send(RunMessage::Raw(kind));
                }),
            )
        } else {
            connector
        };
        let connect = connector.clone();
        let ports = Arc::new(ports);
        let reconnect_ports = ports.clone();
        let explicit = self.options.port.is_some();
        let controller = Controller::with_backend(
            move |event| {
                let _ = sink.send(RunMessage::Controller(event));
            },
            move |port, handler| connect(port, handler),
            move || {
                if explicit {
                    Ok(Vec::new())
                } else {
                    reconnect_ports()
                }
            },
        );
        let mut run = Run {
            script: self,
            controller: &controller,
            events: &events,
            connector,
            ports,
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
                Ok(RunMessage::Controller(Event::Closed)) | Err(RecvTimeoutError::Disconnected) => {
                    break;
                }
                Ok(RunMessage::Raw(kind)) => write_line(diagnostics, &json(kind))?,
                _ => {}
            }
        }
        result
    }

    fn local(&self, line: &Line, state: Option<&State>, out: &mut dyn Write) -> Result<(), Error> {
        match line {
            Line::Help(topic) => write_line(out, &command::help_on(state, topic.as_deref())),
            Line::List => {
                let ports = serial::ports()?;
                let value = text::ports(&ports);
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

enum RunMessage {
    Controller(Event),
    Raw(message::Kind),
}

struct Run<'a> {
    script: &'a Script,
    controller: &'a Controller,
    events: &'a Receiver<RunMessage>,
    connector: Arc<Connector>,
    ports: Arc<dyn Fn() -> io::Result<Vec<serial::PortInfo>> + Send + Sync>,
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
    fn notice(
        &self,
        notice: &Notice,
        state: Option<&State>,
        out: &mut dyn Write,
    ) -> Result<(), Error> {
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
            write_line(out, &line)?;
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
                        write_line(out, line)?;
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
                        self.script.local(&line, st.as_ref(), out)?;
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
                Ok(RunMessage::Raw(kind)) => write_line(diagnostics, &json(kind))?,
                Ok(RunMessage::Controller(Event::Connection {
                    session: id, phase, ..
                })) if id == self.session => match phase {
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
                Ok(RunMessage::Controller(Event::Notice {
                    session: id,
                    notice,
                })) if id == self.session => {
                    let st = self.controller.state();
                    self.notice(&notice, st.as_ref(), out)?
                }
                Ok(RunMessage::Controller(Event::Done {
                    session: id,
                    ticket,
                    result,
                })) if id == self.session => {
                    if let Some((command, filter)) = self.pending.remove(&ticket) {
                        let state = self.controller.state();
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
                                write_line(out, &line)?;
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

enum MigrationMessage {
    Event(p::migration::MigrationEvent),
    Done(cordial_client::migration::Result<cordial_client::migration::Outcome>),
}

impl Run<'_> {
    /// Migrates the open adapter and returns the port to open it on again. The session closes
    /// and releases its port first, and the worker runs until it finishes or Ctrl+C cancels it.
    fn migrate(&mut self, quiet: bool, diagnostics: &mut dyn Write) -> Result<String, Error> {
        let st = self.controller.state().unwrap_or_default();
        let (id, port) = (st.status.id, st.port);
        self.controller.close();
        loop {
            match self.events.recv() {
                Ok(RunMessage::Controller(Event::Closed)) => break,
                Ok(RunMessage::Raw(kind)) => write_line(diagnostics, &json(kind))?,
                Ok(_) => {}
                Err(_) => return Err(Error::new("control event stream closed")),
            }
        }
        self.closed = true;
        // The worker's result is awaited even if diagnostics can't be written.
        let _ = write_line(diagnostics, "Migrating the adapter's saved data…");
        let (tx, rx) = mpsc::channel();
        let engine = self.script.engine.clone().unwrap_or_else(|| {
            let connector = self.connector.clone();
            let ports = self.ports.clone();
            migration::with_backend(connector, move || ports())
        });
        let done = tx.clone();
        let worker = Worker::start(
            &engine,
            id.clone(),
            Some(port.clone()),
            move |event| {
                let _ = tx.send(MigrationMessage::Event(event));
            },
            move |result| {
                let _ = done.send(MigrationMessage::Done(result));
            },
        );
        let (mut waiting, mut cancelled) = (false, false);
        let mut output_error = None;
        let result = loop {
            if !cancelled && self.script.cancellation.cancelled() {
                cancelled = true;
                worker.cancel();
            }
            while let Ok(message) = self.events.try_recv() {
                if let RunMessage::Raw(kind) = message
                    && output_error.is_none()
                    && let Err(error) = write_line(diagnostics, &json(kind))
                {
                    output_error = Some(error);
                    worker.cancel();
                }
            }
            use cordial_protocol::migration::migration_event::Event as M;
            match rx.recv_timeout(Duration::from_millis(50)) {
                Ok(MigrationMessage::Event(event)) => match event.event {
                    Some(M::Waiting(_)) if !waiting => {
                        waiting = true;
                        let _ = write_line(diagnostics, "Waiting for the adapter…");
                    }
                    Some(M::Progress(_)) => waiting = false,
                    _ => {}
                },
                // The worker closes its connection before reporting completion, so all raw
                // messages, including those received during close, precede this result.
                Ok(MigrationMessage::Done(result)) => {
                    while let Ok(message) = self.events.try_recv() {
                        if let RunMessage::Raw(kind) = message
                            && output_error.is_none()
                            && let Err(error) = write_line(diagnostics, &json(kind))
                        {
                            output_error = Some(error);
                        }
                    }
                    break result;
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => {
                    unreachable!("the worker reports its result")
                }
            }
        };
        if let Some(error) = output_error {
            return Err(error);
        }
        match result {
            Ok(cordial_client::migration::Outcome::Success) => {
                if !quiet {
                    write_line(diagnostics, MIGRATED)?;
                }
                let found = (self.ports)()
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sim::Dongle;
    use std::{fs, io::Cursor, path::PathBuf};

    struct Directory(PathBuf);

    impl Directory {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir()
                .join(format!("cordial-cli-files-{}-{name}", std::process::id()));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for Directory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn script(dongle: &Dongle, args: &[&str], json: bool) -> (Result<(), Error>, String, String) {
        let mut out = Vec::new();
        let mut diagnostics = Vec::new();
        let result = Script {
            options: Options {
                port: Some("simulated".into()),
                json,
                args: args.iter().map(|s| (*s).into()).collect(),
                ..Default::default()
            },
            cancellation: Cancellation::default(),
            engine: Some(Arc::new(|_, _, _, _| panic!("no migration is expected"))),
        }
        .with_connector(
            Cursor::new(Vec::new()),
            &mut out,
            &mut diagnostics,
            dongle.connector(),
        );
        (
            result,
            String::from_utf8(out).unwrap(),
            String::from_utf8(diagnostics).unwrap(),
        )
    }

    fn responses(out: &str) -> Vec<p::Response> {
        out.lines()
            .filter_map(|line| {
                let message: p::Message = serde_json::from_str(line).unwrap();
                match message.kind {
                    Some(message::Kind::Response(response)) => Some(response),
                    _ => None,
                }
            })
            .collect()
    }

    #[test]
    fn json_file_download_includes_the_complete_response_contents() {
        let dir = Directory::new("json");
        let local = dir.0.join("download.bin");
        let data: Vec<u8> = (0..1537).map(|n| n as u8).collect();
        let dongle = Dongle::with(|sim| {
            sim.data.insert("/data.bin".into(), data.clone());
        });
        let (result, out, diagnostics) = script(
            &dongle,
            &["file", "get", "/data.bin", local.to_str().unwrap()],
            true,
        );
        result.unwrap();
        let files: Vec<_> = responses(&diagnostics)
            .into_iter()
            .filter_map(|response| match response.result {
                Some(p::response::Result::File(file)) => Some(file),
                _ => None,
            })
            .collect();
        assert_eq!(
            files,
            [p::FileData {
                path: "/data.bin".into(),
                offset: 0,
                size: data.len() as u64,
                data: data.clone(),
            }]
        );
        assert_eq!(fs::read(local).unwrap(), data);
        assert!(out.contains("1537 bytes"), "{out}");
    }

    #[test]
    fn file_put_handles_empty_files_and_chunk_boundaries_with_readback() {
        let dir = Directory::new("put");
        for size in [0, 512, 513, 1300] {
            let local = dir.0.join("upload.bin");
            let data: Vec<u8> = (0..size).map(|n| n as u8).collect();
            fs::write(&local, &data).unwrap();
            let dongle = Dongle::with(|sim| {
                sim.files.insert("/".into(), Vec::new());
                if size > 512 {
                    sim.data.insert("/data.bin".into(), vec![9; 1600]);
                }
            });
            let (result, out, diagnostics) = script(
                &dongle,
                &["file", "put", local.to_str().unwrap(), "/data.bin"],
                true,
            );
            result.unwrap();
            assert!(out.contains(&format!("{size} bytes")), "{out}");
            let sim = dongle.0.lock().unwrap();
            assert_eq!(sim.data["/data.bin"], data);
            assert!(!sim.data.contains_key("/.file-upload"));
            let mut written = Vec::new();
            let mut read = 0;
            for command in &sim.log {
                match command {
                    p::request::Command::WriteFile(file) => {
                        assert!(file.data.len() <= 512);
                        assert_eq!(file.offset, written.len() as u64);
                        written.extend_from_slice(&file.data);
                        assert_eq!(
                            file.path,
                            if size > 512 {
                                "/.file-upload"
                            } else {
                                "/data.bin"
                            }
                        );
                    }
                    p::request::Command::ReadFile(file) => {
                        assert_eq!(file.length, Some(512));
                        read += 1;
                    }
                    _ => {}
                }
            }
            assert_eq!(written, data);
            let readbacks: Vec<_> = responses(&diagnostics)
                .into_iter()
                .filter_map(|response| match response.result {
                    Some(p::response::Result::File(file)) => Some(file),
                    _ => None,
                })
                .collect();
            assert_eq!(readbacks.len(), read);
            let uploaded: Vec<_> = readbacks
                .iter()
                .filter(|file| file.path == "/data.bin")
                .flat_map(|file| file.data.iter().copied())
                .collect();
            assert_eq!(uploaded, data);
            assert_eq!(
                sim.log
                    .iter()
                    .any(|c| matches!(c, p::request::Command::MoveFile(_))),
                size > 512
            );
        }
    }

    #[test]
    fn file_put_stops_on_write_readback_and_move_errors_without_replacing_the_destination() {
        let dir = Directory::new("put-error");
        let local = dir.0.join("upload.bin");
        fs::write(&local, vec![7; 1200]).unwrap();
        for command in ["write_file", "read_file", "move_file"] {
            let dongle = Dongle::with(|sim| {
                sim.files.insert("/".into(), Vec::new());
                sim.data.insert("/data.bin".into(), vec![3; 700]);
                sim.refuse.insert(
                    command,
                    p::Error {
                        code: p::ErrorCode::StorageFailed as i32,
                        reason: p::ErrorReason::Storage as i32,
                        outcome: p::ErrorOutcome::Unknown as i32,
                        ..Default::default()
                    },
                );
            });
            let (result, out, diagnostics) = script(
                &dongle,
                &["file", "put", local.to_str().unwrap(), "/data.bin"],
                true,
            );
            let error = result.unwrap_err();
            assert!(error.message.contains("couldn't confirm"), "{error}");
            assert!(!out.contains("Uploaded"));
            assert!(responses(&diagnostics).iter().any(|response| matches!(
                &response.result,
                Some(p::response::Result::Error(error))
                    if error.outcome() == p::ErrorOutcome::Unknown
            )));
            assert_eq!(dongle.0.lock().unwrap().data["/data.bin"], vec![3; 700]);
            assert_eq!(dongle.sent().last(), Some(&command));
        }
    }

    #[test]
    fn file_put_preserves_existing_scratch_files_and_directories() {
        let dir = Directory::new("occupied");
        let local = dir.0.join("upload.bin");
        fs::write(&local, vec![7; 1200]).unwrap();
        for (directory, size) in [(false, 0), (false, 600), (true, 0)] {
            let dongle = Dongle::with(|sim| {
                sim.files.insert(
                    "/".into(),
                    vec![p::FileEntry {
                        name: ".file-upload".into(),
                        directory,
                        size,
                    }],
                );
                sim.data.insert("/data.bin".into(), vec![3; 700]);
                let path = if directory {
                    "/.file-upload/keep.bin"
                } else {
                    "/.file-upload"
                };
                sim.data.insert(path.into(), vec![9; size as usize]);
            });
            let before = dongle.0.lock().unwrap().data.clone();
            let (result, out, diagnostics) = script(
                &dongle,
                &["file", "put", local.to_str().unwrap(), "/data.bin"],
                true,
            );
            assert!(
                result.unwrap_err().message.contains("already exists"),
                "{diagnostics}"
            );
            assert!(!out.contains("Uploaded"));
            assert_eq!(dongle.0.lock().unwrap().data, before);
            assert_eq!(dongle.sent().last(), Some(&"list_files"));
            assert!(!dongle.sent().contains(&"write_file"));
        }
    }

    #[test]
    fn file_put_does_not_write_when_the_scratch_check_fails() {
        let dir = Directory::new("inventory-error");
        let local = dir.0.join("upload.bin");
        fs::write(&local, vec![7; 1200]).unwrap();
        let dongle = Dongle::with(|sim| {
            sim.data.insert("/data.bin".into(), vec![3; 700]);
            sim.refuse.insert(
                "list_files",
                p::Error {
                    code: p::ErrorCode::StorageFailed as i32,
                    outcome: p::ErrorOutcome::NoEffect as i32,
                    ..Default::default()
                },
            );
        });
        let (result, out, _) = script(
            &dongle,
            &["file", "put", local.to_str().unwrap(), "/data.bin"],
            true,
        );
        assert!(result.unwrap_err().message.contains("couldn't prepare"));
        assert!(!out.contains("Uploaded"));
        assert_eq!(dongle.0.lock().unwrap().data["/data.bin"], vec![3; 700]);
        assert!(!dongle.sent().contains(&"write_file"));
    }

    #[test]
    fn file_put_reports_an_acknowledged_upload_when_readback_fails() {
        let dir = Directory::new("readback-error");
        let local = dir.0.join("upload.bin");
        fs::write(&local, b"data").unwrap();
        let dongle = Dongle::with(|sim| {
            sim.refuse.insert(
                "read_file",
                p::Error {
                    code: p::ErrorCode::StorageFailed as i32,
                    outcome: p::ErrorOutcome::NoEffect as i32,
                    ..Default::default()
                },
            );
        });
        let (result, out, _) = script(
            &dongle,
            &["file", "put", local.to_str().unwrap(), "/data.bin"],
            true,
        );
        assert!(
            result
                .unwrap_err()
                .message
                .contains("acknowledged the upload, but its contents couldn't be verified")
        );
        assert!(!out.contains("Uploaded"));
        assert_eq!(dongle.0.lock().unwrap().data["/data.bin"], b"data");
        assert_eq!(
            dongle.sent().iter().filter(|&&c| c == "write_file").count(),
            1
        );
    }

    #[test]
    fn file_mutations_are_not_retried_after_losing_the_acknowledgement() {
        let dir = Directory::new("lost-acknowledgement");
        let local = dir.0.join("upload.bin");
        for (delayed, size) in [
            ("write_file", 3),
            ("write_file", 1200),
            ("move_file", 1200),
            ("delete_file", 0),
        ] {
            let data = vec![7; size];
            fs::write(&local, &data).unwrap();
            let dongle = Dongle::with(|sim| {
                sim.files.insert("/".into(), Vec::new());
                sim.data.insert("/data.bin".into(), vec![3; 700]);
                sim.slow.insert(delayed, Duration::from_secs(1));
            });
            let args = if delayed == "delete_file" {
                vec!["file", "delete", "/data.bin"]
            } else {
                vec!["file", "put", local.to_str().unwrap(), "/data.bin"]
            };
            let mut out = Vec::new();
            let mut diagnostics = Vec::new();
            let connect = dongle.connector();
            let result = Script {
                options: Options {
                    port: Some("simulated".into()),
                    args: args.into_iter().map(str::to_owned).collect(),
                    ..Default::default()
                },
                cancellation: Cancellation::default(),
                engine: Some(Arc::new(|_, _, _, _| panic!("no migration is expected"))),
            }
            .with_connector(
                Cursor::new(Vec::new()),
                &mut out,
                &mut diagnostics,
                move |port, handler| {
                    let connection = connect(port, handler)?;
                    connection.set_timeout(Some(Duration::from_millis(100)));
                    Ok(connection)
                },
            );
            let error = result.unwrap_err();
            assert!(error.message.contains("couldn't confirm"), "{error}");
            assert!(out.is_empty());
            assert_eq!(dongle.sent().last(), Some(&delayed));
            assert_eq!(dongle.sent().iter().filter(|&&c| c == delayed).count(), 1);
            let sim = dongle.0.lock().unwrap();
            match (delayed, size) {
                ("delete_file", _) => assert!(!sim.data.contains_key("/data.bin")),
                ("write_file", 1200) => {
                    assert_eq!(sim.data["/data.bin"], vec![3; 700]);
                    assert_eq!(sim.data["/.file-upload"], data[..512]);
                }
                _ => assert_eq!(sim.data["/data.bin"], data),
            }
        }
    }

    #[test]
    fn file_commands_validate_paths_and_capabilities_before_writing() {
        let dir = Directory::new("validation");
        let local = dir.0.join("upload.bin");
        fs::write(&local, b"data").unwrap();
        for path in [
            "/",
            "relative",
            "/a/../b",
            "/a//b",
            "/a/",
            "/.file-write-0123456789abcdef",
        ] {
            let dongle = Dongle::default();
            let (result, _, _) = script(
                &dongle,
                &["file", "put", local.to_str().unwrap(), path],
                false,
            );
            assert_eq!(result.unwrap_err().message, "invalid adapter file path");
            let (result, _, _) = script(&dongle, &["file", "delete", path], false);
            assert_eq!(result.unwrap_err().message, "invalid adapter file path");
            assert!(!dongle.sent().contains(&"write_file"));
            assert!(!dongle.sent().contains(&"delete_file"));
        }
        let dongle = Dongle::default();
        let (result, _, _) = script(&dongle, &["file", "delete", "/format.pb"], false);
        assert_eq!(
            result.unwrap_err().message,
            "the adapter's storage format file cannot be deleted"
        );
        assert!(!dongle.sent().contains(&"delete_file"));
        let dongle = Dongle::with(|sim| {
            sim.status.information.as_mut().unwrap().development_build = Some(false);
        });
        for args in [
            vec!["file", "put", local.to_str().unwrap(), "/data.bin"],
            vec!["file", "delete", "/data.bin"],
        ] {
            let (result, _, _) = script(&dongle, &args, false);
            assert_eq!(
                result.unwrap_err().message,
                "this adapter doesn't offer file access"
            );
        }
        assert!(!dongle.sent().contains(&"write_file"));
        assert!(!dongle.sent().contains(&"delete_file"));
        fs::write(&local, vec![7; 600]).unwrap();
        let dongle = Dongle::default();
        let (result, _, _) = script(
            &dongle,
            &["file", "put", local.to_str().unwrap(), "/.file-upload"],
            false,
        );
        assert_eq!(
            result.unwrap_err().message,
            "the upload destination is also the temporary transfer path"
        );
        assert!(!dongle.sent().contains(&"write_file"));
    }

    #[test]
    fn file_delete_reports_the_acknowledged_result() {
        let dongle = Dongle::with(|sim| {
            sim.data.insert("/data.bin".into(), vec![1, 2, 3]);
        });
        let (result, out, _) = script(&dongle, &["file", "delete", "/data.bin"], false);
        result.unwrap();
        assert_eq!(out, "Deleted /data.bin from the adapter.\n");
        assert!(!dongle.0.lock().unwrap().data.contains_key("/data.bin"));
    }

    #[test]
    fn concurrent_uploads_keep_their_chunks_and_destinations_separate() {
        let dir = Directory::new("concurrent");
        let local = [dir.0.join("first.bin"), dir.0.join("second.bin")];
        fs::write(&local[0], vec![1; 1200]).unwrap();
        fs::write(&local[1], vec![2; 1500]).unwrap();
        let dongle = Dongle::with(|sim| {
            sim.files.insert("/".into(), Vec::new());
            sim.slow.insert("write_file", Duration::from_millis(5));
        });
        let (send, events) = mpsc::channel();
        let controller =
            Controller::with_connector(move |event| send.send(event).unwrap(), dongle.connector());
        controller.open("simulated".into());
        while !matches!(
            events.recv_timeout(Duration::from_secs(5)).unwrap(),
            Event::Connection {
                phase: Phase::Ready,
                ..
            }
        ) {}
        for (local, path) in local.into_iter().zip(["/first.bin", "/second.bin"]) {
            controller.run(
                Command::FilePut {
                    local,
                    path: path.into(),
                },
                RunOptions::default(),
            );
        }
        let mut completed = 0;
        while completed < 2 {
            if let Event::Done { result, .. } = events.recv_timeout(Duration::from_secs(5)).unwrap()
            {
                result.unwrap();
                completed += 1;
            }
        }
        controller.close();
        while !matches!(
            events.recv_timeout(Duration::from_secs(5)).unwrap(),
            Event::Closed
        ) {}
        let sim = dongle.0.lock().unwrap();
        assert_eq!(sim.data["/first.bin"], vec![1; 1200]);
        assert_eq!(sim.data["/second.bin"], vec![2; 1500]);
        assert!(!sim.data.contains_key("/.file-upload"));
    }
}
