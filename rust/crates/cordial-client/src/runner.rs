//! Foreground, non-terminal command runner. No background service or reconnect loop.
use crate::{
    client::{Cancellation, Client, Error, Wait},
    controller::{
        Command, Controller, Event, Notice, OpenOptions, Outcome, Phase, RunOptions, SessionId,
        Ticket,
    },
    transport,
    ui::{
        command::{self, Line},
        text::{self, Filter},
    },
};
use std::{
    collections::BTreeMap,
    io::{BufRead, BufReader, Read, Write},
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
fn write_line(out: &mut dyn Write, line: &str) -> Result<(), Error> {
    if !line.is_empty() {
        writeln!(out, "{line}")?;
        out.flush()?;
    }
    Ok(())
}

pub struct Script {
    pub options: Options,
    pub cancellation: Cancellation,
}
impl Script {
    pub fn run(
        &self,
        input: impl Read + Send + 'static,
        out: &mut dyn Write,
        diagnostics: &mut dyn Write,
    ) -> Result<(), Error> {
        self.with_connector(input, out, diagnostics, Client::open)
    }
    /// Runs the same foreground loop against an in-memory adapter in tests.
    pub fn with_connector(
        &self,
        input: impl Read + Send + 'static,
        out: &mut dyn Write,
        diagnostics: &mut dyn Write,
        connect: impl Fn(&str, &Wait) -> Result<Client, Error> + Send + Sync + 'static,
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
                let ports = transport::ports()?;
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
        let result = self.run_loop(
            &controller,
            &events,
            &lines,
            port,
            first,
            one_shot,
            out,
            diagnostics,
        );
        controller.close();
        let end = Instant::now() + Duration::from_secs(2);
        while Instant::now() < end {
            match events.recv_timeout(Duration::from_millis(25)) {
                Ok(Event::Closed) => break,
                Ok(Event::Notice { notice, .. }) => {
                    let _ = self.notice(&notice, controller.state().as_ref(), out, diagnostics);
                }
                Err(RecvTimeoutError::Disconnected) => break,
                _ => {}
            }
        }
        result
    }
    fn local(
        &self,
        line: &Line,
        state: Option<&crate::controller::State>,
        out: &mut dyn Write,
        diagnostics: &mut dyn Write,
    ) -> Result<(), Error> {
        match line {
            Line::Help(_) => write_line(
                if self.options.json { diagnostics } else { out },
                &command::help(state),
            ),
            Line::List => {
                let ports = transport::ports()?;
                let value = if self.options.json {
                    text::terminal_json(&serde_json::to_string(&ports).unwrap())
                } else {
                    text::ports(&ports)
                };
                write_line(out, &value)
            }
            _ => Ok(()),
        }
    }
    fn notice(
        &self,
        notice: &Notice,
        state: Option<&crate::controller::State>,
        out: &mut dyn Write,
        diagnostics: &mut dyn Write,
    ) -> Result<(), Error> {
        if self.options.json
            && let Notice::Message { envelope, .. } = notice
            && !envelope.raw.is_empty()
        {
            return write_line(out, &text::terminal_json(&envelope.raw));
        }
        // Directory rows are results, printed as they arrive.
        if let Notice::StorageEntries { entries, .. } = notice {
            for entry in entries {
                let line = if self.options.json {
                    text::terminal_json(&serde_json::to_string(entry).unwrap())
                } else {
                    text::file_line(entry)
                };
                write_line(out, &line)?;
            }
            return Ok(());
        }
        if let Some(line) = text::notice(notice, state) {
            write_line(if self.options.json { diagnostics } else { out }, &line)?;
        }
        Ok(())
    }
    #[allow(clippy::too_many_arguments)]
    fn run_loop(
        &self,
        controller: &Controller,
        events: &Receiver<Event>,
        lines: &Receiver<Result<Option<String>, Error>>,
        port: String,
        mut next: Option<Line>,
        one_shot: bool,
        out: &mut dyn Write,
        diagnostics: &mut dyn Write,
    ) -> Result<(), Error> {
        let direct = matches!(&next, Some(Line::Run(command)) if command.direct());
        let deadline = if one_shot && !self.options.timeout.is_zero() {
            Some(Instant::now() + self.options.timeout)
        } else {
            None
        };
        let mut operation_deadline = deadline;
        let mut session = self.open(controller, port, direct, deadline);
        let mut prepared = false;
        let mut foreground: Option<Ticket> = None;
        let mut pending = BTreeMap::<Ticket, (Command, Filter)>::new();
        let mut lost: Option<Error> = None;
        loop {
            if self.cancellation.cancelled() {
                return Err(Error::new("interrupted"));
            }
            if operation_deadline.is_some_and(|d| Instant::now() >= d) {
                return Err(Error::new("operation timed out"));
            }
            if prepared
                && foreground.is_none()
                && let Some(error) = lost.take()
            {
                return Err(error);
            }
            if prepared && next.is_none() {
                let auth = controller
                    .state()
                    .and_then(|s| s.auth)
                    .filter(|a| !a.display)
                    .filter(|a| !pending.values().any(|(command, _)| matches!(command, Command::PairReply {request,prompt,..} if *request==a.request && *prompt==a.prompt.prompt_id)));
                if foreground.is_none() || auth.is_some() {
                    match lines.try_recv() {
                        Ok(Ok(Some(line))) => {
                            if let Some(auth) = auth
                                && !line.starts_with('/')
                                && !line.starts_with("pairing reply ")
                            {
                                let command = command::answer(&auth, &line).map_err(Error::new)?;
                                let ticket = controller.run(
                                    command.clone(),
                                    RunOptions {
                                        wait: self.wait(operation_deadline),
                                        ..Default::default()
                                    },
                                );
                                pending.insert(ticket, (command, Filter::All));
                            } else {
                                let words = command::split(line.strip_prefix('/').unwrap_or(&line))
                                    .map_err(Error::new)?;
                                if !words.is_empty() {
                                    next = Some(command::parse(&words).map_err(Error::new)?);
                                }
                            }
                        }
                        Ok(Ok(None)) | Err(mpsc::TryRecvError::Disconnected) => {
                            if auth.is_some() {
                                return Err(Error::new("pairing needs an answer before EOF"));
                            }
                            if foreground.is_none() {
                                return Ok(());
                            }
                        }
                        Ok(Err(error)) => return Err(error),
                        Err(mpsc::TryRecvError::Empty) => {}
                    }
                }
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
                        session = self.open(controller, port, false, deadline);
                        prepared = false;
                        foreground = None;
                        pending.clear();
                        lost = None;
                        if one_shot {
                            next = Some(Line::Quit);
                        }
                    }
                    Line::Help(_) | Line::List => {
                        self.local(&line, controller.state().as_ref(), out, diagnostics)?;
                        if one_shot && foreground.is_none() {
                            return Ok(());
                        }
                    }
                    Line::Run(command) => {
                        let mut duration = Duration::from_secs(10);
                        if one_shot
                            && matches!(command, Command::Scan(_))
                            && !self.options.timeout.is_zero()
                        {
                            duration = self.options.timeout;
                            operation_deadline =
                                Some(Instant::now() + duration + Duration::from_secs(1));
                        }
                        let ticket = controller.run(
                            command.clone(),
                            RunOptions {
                                one_shot,
                                scan_duration: duration,
                                wait: self.wait(operation_deadline),
                            },
                        );
                        pending.insert(ticket, (command, filter));
                        if foreground.is_none() {
                            foreground = Some(ticket);
                        }
                    }
                }
            }
            match events.recv_timeout(Duration::from_millis(20)) {
                Ok(Event::Connection {
                    session: id, phase, ..
                }) if id == session => match phase {
                    Phase::Opened if direct => prepared = true,
                    Phase::Ready => prepared = true,
                    Phase::Failed { error, .. } => return Err(error),
                    Phase::Lost(error) => {
                        // A reboot acknowledgment can already be queued before the USB port closes.
                        if pending
                            .values()
                            .any(|(c, _)| matches!(c, Command::Bootloader))
                        {
                            lost = Some(error);
                        } else {
                            return Err(error);
                        }
                    }
                    Phase::Waiting => write_line(
                        if self.options.json { diagnostics } else { out },
                        "Waiting for adapter readiness…",
                    )?,
                    _ => {}
                },
                Ok(Event::Notice {
                    session: id,
                    notice,
                }) if id == session => {
                    self.notice(&notice, controller.state().as_ref(), out, diagnostics)?
                }
                Ok(Event::Done {
                    session: id,
                    ticket,
                    result,
                }) if id == session => {
                    if let Some((command, filter)) = pending.remove(&ticket) {
                        let state = controller.state();
                        let dest: &mut dyn Write =
                            if self.options.json { diagnostics } else { out };
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
                            Err(failure) => {
                                let (partial, error) =
                                    text::failure_text(&command, &failure, state.as_ref());
                                write_line(dest, &partial)?;
                                if foreground == Some(ticket) {
                                    return Err(Error::new(error));
                                }
                                write_line(diagnostics, &format!("Error: {error}"))?;
                            }
                        }
                        if foreground == Some(ticket) {
                            foreground = None;
                            if one_shot {
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
    fn wait(&self, deadline: Option<Instant>) -> Wait {
        Wait {
            deadline,
            cancellation: self.cancellation.clone(),
        }
    }
    fn open(
        &self,
        controller: &Controller,
        port: String,
        direct: bool,
        deadline: Option<Instant>,
    ) -> SessionId {
        let deadline = Some(
            deadline.map_or(Instant::now() + Duration::from_secs(6), |d| {
                d.min(Instant::now() + Duration::from_secs(6))
            }),
        );
        // Each opening has its own cancellation; replacing it must not cancel the script.
        let options = OpenOptions {
            wait: Wait {
                deadline,
                cancellation: Cancellation::default(),
            },
            keep_unready: false,
        };
        if direct {
            controller.open_direct(port, options)
        } else {
            controller.open(port, options)
        }
    }
}
