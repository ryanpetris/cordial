//! Foreground, non-terminal command runner. No background service or reconnect loop.
use crate::{
    controller::{
        Cancellation, Command, Controller, Event, Notice, Outcome, Phase, RunOptions, SessionId,
        State, Ticket, Wait,
    },
    error::Error,
    ui::{
        command::{self, Line},
        text::{self, Filter},
    },
};
use cordial_client::{Connection, Received, serial};
use cordial_protocol::{self as p, event, message};
use std::{
    collections::BTreeMap,
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
        let (send, lines) = mpsc::sync_channel(1);
        thread::spawn(move || {
            let mut input = BufReader::new(input);
            loop {
                let mut line = String::new();
                let result = match input.by_ref().take(4097).read_line(&mut line) {
                    Ok(0) => Ok(None),
                    Ok(_) if line.len() > 4096 => {
                        Err(Error::new("command line exceeds 4096 bytes"))
                    }
                    Ok(_) => Ok(Some(line.trim_end_matches(['\r', '\n']).to_string())),
                    Err(error) => Err(error.into()),
                };
                let end = !matches!(result, Ok(Some(_)));
                if send.send(result).is_err() || end {
                    break;
                }
            }
        });
        let mut run = Run {
            script: self,
            controller: &controller,
            events: &events,
            lines: &lines,
            one_shot,
            foreground: None,
            pending: BTreeMap::new(),
            answered: None,
        };
        let result = run.run_loop(port, first, out, diagnostics);
        controller.close();
        let end = Instant::now() + Duration::from_secs(2);
        while Instant::now() < end {
            match events.recv_timeout(Duration::from_millis(25)) {
                Ok(Event::Closed) | Err(RecvTimeoutError::Disconnected) => break,
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
            Line::Help(_) => write_line(
                if self.options.json { diagnostics } else { out },
                &command::help(state),
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

struct Run<'a> {
    script: &'a Script,
    controller: &'a Controller,
    events: &'a Receiver<Event>,
    lines: &'a Receiver<Result<Option<String>, Error>>,
    one_shot: bool,
    foreground: Option<Ticket>,
    pending: BTreeMap<Ticket, (Command, Filter)>,
    /// The pairing step last answered, so one prompt takes one answer.
    answered: Option<p::Pairing>,
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

    fn start(&mut self, command: Command, filter: Filter, deadline: Option<Instant>) {
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
    }

    fn run_loop(
        &mut self,
        port: String,
        mut next: Option<Line>,
        out: &mut dyn Write,
        diagnostics: &mut dyn Write,
    ) -> Result<(), Error> {
        let direct = matches!(&next, Some(Line::Run(command)) if command.direct());
        let timeout = self.script.options.timeout;
        let deadline = (self.one_shot && !timeout.is_zero()).then(|| Instant::now() + timeout);
        let mut session: SessionId = self.controller.open(port);
        let mut prepared = false;
        let mut lost: Option<Error> = None;
        let mut starting: Option<Instant> = None;
        loop {
            if self.script.cancellation.cancelled() {
                return Err(Error::new("interrupted"));
            }
            if deadline.is_some_and(|d| Instant::now() >= d) {
                return Err(Error::new("operation timed out"));
            }
            if !prepared && starting.is_some_and(|d| Instant::now() >= d) {
                return Err(Error::new("the adapter didn't finish starting in time"));
            }
            if prepared && next.is_none() {
                let prompt = self.prompt();
                if self.foreground.is_none() || prompt.is_some() {
                    match self.lines.try_recv() {
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
            if prepared && let Some(line) = next.take() {
                let (line, filter) = match line {
                    Line::Devices(filter) => (Line::Run(Command::Devices), filter),
                    line => (line, Filter::All),
                };
                match line {
                    Line::Devices(_) => unreachable!(),
                    Line::Quit => return Ok(()),
                    Line::Select(port) => {
                        session = self.controller.open(port);
                        prepared = false;
                        starting = None;
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
                        // A one-shot scan without its own length scans for the timeout.
                        if let Command::Scan { seconds, .. } = &mut command
                            && *seconds == 0
                            && self.one_shot
                            && !timeout.is_zero()
                        {
                            *seconds = timeout
                                .as_secs_f64()
                                .ceil()
                                .clamp(1.0, f64::from(crate::commands::MAX_SCAN_SECONDS))
                                as u32;
                        }
                        self.start(
                            command,
                            filter,
                            deadline.map(|d| d + Duration::from_secs(1)),
                        );
                    }
                }
            }
            match self.events.recv_timeout(Duration::from_millis(20)) {
                Ok(Event::Connection {
                    session: id, phase, ..
                }) if id == session => match phase {
                    Phase::Opened if direct => prepared = true,
                    Phase::Ready => prepared = true,
                    Phase::Failed { open: true, .. } if prepared => {}
                    Phase::Failed { error, .. } => return Err(error),
                    // The command running reports its own outcome first.
                    Phase::Lost(error) => lost = Some(error),
                    Phase::Waiting => {
                        starting.get_or_insert(Instant::now() + STARTUP_LIMIT);
                        write_line(
                            if self.json() { diagnostics } else { out },
                            "Waiting for adapter readiness…",
                        )?
                    }
                    _ => {}
                },
                Ok(Event::Notice {
                    session: id,
                    notice,
                }) if id == session => {
                    let st = self.controller.state();
                    self.notice(&notice, st.as_ref(), out, diagnostics)?
                }
                Ok(Event::Done {
                    session: id,
                    ticket,
                    result,
                }) if id == session => {
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
                                let error = text::failure_text(&error);
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
