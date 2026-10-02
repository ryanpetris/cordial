//! The interactive shell: complete command output goes to terminal
//! scrollback above an editable prompt, with history, completion and
//! bluetoothctl-style commands. Commands run while others are pending, and
//! notifications print without disturbing a partially typed line. During a
//! pairing prompt the next line is the answer, and `/COMMAND` runs a command.
use crate::{
    commands::STARTING,
    controller::{Command, Event, Outcome, Phase, SessionId, State, Ticket},
    error::Error,
    model::{self, Prompt},
    ui::{
        Backend, Interrupter, Live, Msg, UiError, UiOptions,
        command::{self, Line},
        field::Field,
        term::{self, Input, Modes},
        text::{self, Filter, safe},
    },
};
use cordial_client::serial::PortInfo;
use ratatui::crossterm::{
    cursor,
    event::{KeyCode, KeyEvent, KeyModifiers},
    queue,
    terminal::{self as crossterm_terminal, Clear, ClearType},
};
use std::{
    collections::HashMap,
    io::{self, Write},
    sync::mpsc::{Receiver, RecvTimeoutError, SyncSender},
    time::{Duration, Instant},
};

const PROMPT: &str = "cordial> ";
const FORM_PROMPT: &str = "> ";
const HISTORY: usize = 200;
/// The longest the shell waits for its session cleanup after quit.
const CLOSE_LIMIT: Duration = Duration::from_secs(5);

/// A command typed in the shell, kept until its result.
struct Job {
    command: Command,
    /// `device list` lists with this filter once refreshed.
    filter: Option<Filter>,
    /// The session it was sent to; results of a replaced session are dropped.
    session: Option<SessionId>,
}

pub(crate) struct Model<B: Backend> {
    backend: B,
    session: Option<SessionId>,
    opening: Option<SessionId>,
    port: String,
    /// The startup adapter list may open its only adapter.
    startup: bool,
    preparing: bool,
    quitting: bool,
    pub(crate) done: bool,
    input: Field,
    form: Field,
    auth_key: String,
    history: Vec<String>,
    history_index: usize,
    history_draft: String,
    completion: Option<(String, usize)>,
    jobs: HashMap<Ticket, Job>,
    /// Text for scrollback, printed above the prompt before the next draw.
    pub(crate) output: Vec<String>,
}

impl<B: Backend> Model<B> {
    pub(crate) fn new(backend: B, port: Option<String>) -> Self {
        let mut m = Self {
            backend,
            session: None,
            opening: None,
            port: String::new(),
            startup: port.is_none(),
            preparing: false,
            quitting: false,
            done: false,
            input: Field::new(1024),
            form: Field::new(1024),
            auth_key: String::new(),
            history: Vec::new(),
            history_index: 0,
            history_draft: String::new(),
            completion: None,
            jobs: HashMap::new(),
            output: Vec::new(),
        };
        match port {
            Some(port) => m.open(port),
            None => m.backend.list_ports(),
        }
        m
    }

    fn log(&mut self, text: impl Into<String>) {
        let text = text.into();
        if !text.is_empty() {
            self.output.push(text);
        }
    }

    fn state(&self) -> Option<State> {
        let session = self.session?;
        self.backend.state().filter(|s| s.session == session)
    }

    /// The pairing prompt typed lines answer, if any.
    fn prompt_open(&self) -> Option<Prompt> {
        self.state().as_ref().and_then(command::answerable)
    }

    /// Whether typed lines answer a pairing prompt.
    fn answering(&self) -> bool {
        self.prompt_open().is_some()
    }

    fn open(&mut self, port: String) {
        if self.opening.is_some() || self.quitting {
            return;
        }
        self.session = None;
        self.startup = false;
        self.preparing = false;
        self.port = port.clone();
        self.auth_key.clear();
        self.opening = Some(self.backend.open(port));
    }

    pub(crate) fn quit(&mut self) {
        if !self.quitting {
            self.quitting = true;
            self.backend.close();
        }
    }

    fn run(&mut self, command: Command, filter: Option<Filter>) {
        let ticket = self.backend.run(command.clone());
        let session = self.session;
        self.jobs.insert(
            ticket,
            Job {
                command,
                filter,
                session,
            },
        );
    }

    /// Follows the active pairing prompt: a new one clears the answer field.
    fn sync_auth(&mut self) {
        let key = self
            .state()
            .and_then(|st| st.pairing)
            .filter(model::answerable)
            .map_or_else(String::new, |p| format!("{p:?}"));
        if key != self.auth_key {
            self.auth_key = key;
            self.form.reset();
        }
    }

    pub(crate) fn tick(&mut self) {
        self.sync_auth();
    }

    pub(crate) fn update(&mut self, msg: Msg) {
        match msg {
            Msg::Interrupt => self.quit(),
            Msg::Resize(..) | Msg::Mouse(_) => {}
            Msg::Paste(text) => {
                if self.answering() {
                    self.form.insert(&text);
                } else {
                    self.input.insert(&text);
                }
            }
            Msg::Ports(result) => self.ports_listed(result),
            Msg::Controller(e) => self.controller(*e),
            Msg::Key(k) => self.key(k),
        }
    }

    fn ports_listed(&mut self, result: Result<Vec<PortInfo>, String>) {
        if !std::mem::take(&mut self.startup) {
            match result {
                Ok(ports) => self.log(text::ports(&ports)),
                Err(e) => self.log(format!("Error: {}", safe(&e))),
            }
            return;
        }
        // Only the startup list opens an adapter by itself, and only when it
        // finds exactly one.
        match result {
            Ok(ports) if ports.len() == 1 => self.open(ports[0].port.clone()),
            Ok(_) => {}
            Err(e) => self.log(format!("Adapter enumeration: {}", safe(&e))),
        }
    }

    fn controller(&mut self, e: Event) {
        match e {
            Event::Closed => {
                if self.quitting {
                    self.done = true;
                }
            }
            Event::Connection { session, phase, .. } => self.connection(session, phase),
            Event::Notice { session, notice } => {
                if Some(session) == self.session {
                    if let Some(line) = text::notice(&notice, self.state().as_ref()) {
                        self.log(line);
                    }
                    self.sync_auth();
                }
            }
            Event::Done { ticket, result, .. } => {
                if let Some(job) = self.jobs.remove(&ticket)
                    && job.session == self.session
                {
                    self.finish(job, result);
                    self.sync_auth();
                }
            }
        }
    }

    fn connection(&mut self, session: SessionId, phase: Phase) {
        match phase {
            Phase::Opened if self.opening == Some(session) => {
                self.opening = None;
                if self.quitting {
                    return;
                }
                self.session = Some(session);
                self.preparing = true;
                let port = safe(&self.port);
                self.log(format!("Connected to {port}. Use help for commands."));
            }
            Phase::Ready if self.session == Some(session) => self.preparing = false,
            Phase::Failed { error, open } => {
                if self.opening == Some(session) {
                    self.opening = None;
                } else if self.session != Some(session) {
                    return;
                }
                self.preparing = false;
                let line = safe(&text::error_line(&error));
                if open {
                    // The session stays for diagnosis and recovery.
                    self.log(format!(
                        "Error: {line}. adapter status, file list, file get and adapter bootloader still work; use adapter select to retry."
                    ));
                } else {
                    self.session = None;
                    self.log(format!("Open failed: {line}"));
                }
            }
            Phase::Lost(error) if self.session == Some(session) && !self.quitting => {
                self.preparing = false;
                self.log(format!(
                    "Adapter unavailable: {}",
                    safe(&text::error_line(&error))
                ));
            }
            _ => {}
        }
    }

    fn finish(&mut self, job: Job, result: Result<Outcome, Error>) {
        let st = self.state();
        match result {
            Ok(outcome) => {
                let text = match (job.filter, &st) {
                    (Some(filter), Some(st)) => text::devices(st, filter),
                    _ => text::outcome(&job.command, &outcome, st.as_ref()),
                };
                self.log(text);
            }
            Err(error) => self.log(format!("Error: {}", text::failure_text(&error))),
        }
    }

    fn key(&mut self, k: KeyEvent) {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        match k.code {
            KeyCode::Char('c') if ctrl => return self.quit(),
            KeyCode::Char('d') if ctrl => return self.quit(),
            KeyCode::Enter => return self.submit(),
            _ => {}
        }
        if self.answering() {
            self.form.key(&k);
            return;
        }
        match k.code {
            KeyCode::Up if !self.history.is_empty() => {
                if self.history_index == self.history.len() {
                    self.history_draft = self.input.value();
                }
                self.history_index = self.history_index.saturating_sub(1);
                self.input
                    .set_value(&self.history[self.history_index].clone());
            }
            KeyCode::Down => {
                self.history_index = (self.history_index + 1).min(self.history.len());
                let value = self
                    .history
                    .get(self.history_index)
                    .cloned()
                    .unwrap_or_else(|| self.history_draft.clone());
                self.input.set_value(&value);
            }
            KeyCode::Tab => self.complete(),
            _ => {
                self.completion = None;
                self.input.key(&k);
            }
        }
    }

    /// Cycles through completions of the line as it was before the first Tab.
    fn complete(&mut self) {
        let (base, index) = self
            .completion
            .take()
            .unwrap_or_else(|| (self.input.value(), 0));
        let choices = command::complete(&base, self.state().as_ref());
        if !choices.is_empty() {
            self.input.set_value(&choices[index % choices.len()]);
        }
        self.completion = Some((base, index + 1));
    }

    /// Commands the adapter doesn't offer are never sent.
    fn offered(&self, command: Command) -> Result<Command, String> {
        match self.state() {
            Some(st) => command::offer(command, &st),
            None => Ok(command),
        }
    }

    /// Runs a command the adapter offers, else says why not.
    fn send(&mut self, command: Command) {
        match self.offered(command) {
            Ok(command) => self.run(command, None),
            Err(e) => self.log(format!("Error: {}", safe(&e))),
        }
    }

    fn submit(&mut self) {
        let mut line = self.input.value();
        if let Some(prompt) = self.prompt_open() {
            line = self.form.value();
            if !line.starts_with('/') && !line.starts_with("pair ") {
                match command::answer(&prompt, &line) {
                    Ok(reply) => self.send(reply),
                    Err(e) => self.log(format!("Error: {}", safe(&e))),
                }
                return;
            }
            line = line.trim_start_matches('/').to_owned();
            self.form.reset();
        } else {
            self.input.reset();
        }
        let args = match command::split(&line) {
            Ok(args) if args.is_empty() => return,
            Ok(args) => args,
            Err(e) => return self.log(e),
        };
        self.history.push(line.clone());
        if self.history.len() > HISTORY {
            self.history.remove(0);
        }
        self.history_index = self.history.len();
        self.completion = None;
        self.log(format!("> {}", safe(&line)));
        self.execute(&args);
    }

    fn execute(&mut self, args: &[String]) {
        let parsed = match command::parse(args) {
            Ok(line) => line,
            Err(e) => return self.log(format!("Error: {}", safe(&e))),
        };
        let (command, filter) = match parsed {
            Line::Quit => return self.quit(),
            Line::Select(port) => return self.open(port),
            Line::Help(_) => return self.log(command::help(self.state().as_ref())),
            Line::List => return self.backend.list_ports(),
            Line::Devices(filter) => (Command::Devices, Some(filter)),
            Line::Run(command) => (command, None),
        };
        let command = match self.offered(command) {
            Ok(command) => command,
            Err(e) => return self.log(format!("Error: {}", safe(&e))),
        };
        // Until the saved devices load, commands would act on an empty list.
        if self.preparing && !command.direct() {
            return self.log(format!("Error: {STARTING}"));
        }
        self.run(command, filter);
    }

    /// The lines below the scrollback: status lines, then the prompt, and the
    /// cursor's column on the prompt.
    pub(crate) fn prompt(&mut self, width: usize) -> (Vec<String>, usize) {
        let mut lines = Vec::new();
        if self.opening.is_some() {
            lines.push("Opening adapter…".to_owned());
        }
        if self
            .state()
            .is_some_and(|st| st.available && !st.status.ready)
        {
            lines.push("Waiting for adapter…".to_owned());
        }
        if self.quitting {
            lines.push("Closing control session…".to_owned());
        }
        let candidate = self
            .state()
            .and_then(|st| st.pairing)
            .map(|p| p.candidate)
            .unwrap_or_default();
        let (prompt, field) = match self.prompt_open() {
            Some(open) => {
                lines.push(format!(
                    "Pair {} {} {}. /COMMAND runs another command.",
                    safe(&candidate),
                    text::prompt_token(&open),
                    safe(text::prompt_value(&open).unwrap_or(""))
                ));
                (FORM_PROMPT, &mut self.form)
            }
            None => (PROMPT, &mut self.input),
        };
        let room = width.saturating_sub(prompt.len() + 1).max(1);
        let (visible, column) = field.view(room);
        for line in &mut lines {
            *line = crate::ui::tui::layout::truncate_str(line, width.saturating_sub(1).max(1));
        }
        lines.push(format!("{prompt}{visible}"));
        (lines, prompt.len() + column)
    }
}

/// Draws the prompt area and prints scrollback above it.
struct Screen {
    /// The prompt row the cursor is on, counted from the area's first row.
    cursor_row: u16,
}
impl Screen {
    fn clear(&mut self, out: &mut impl Write) -> io::Result<()> {
        queue!(out, cursor::MoveToColumn(0))?;
        if self.cursor_row > 0 {
            queue!(out, cursor::MoveUp(self.cursor_row))?;
        }
        queue!(out, Clear(ClearType::FromCursorDown))?;
        self.cursor_row = 0;
        Ok(())
    }

    fn draw<B: Backend>(&mut self, model: &mut Model<B>, out: &mut impl Write) -> io::Result<()> {
        self.clear(out)?;
        for text in model.output.drain(..) {
            // Raw mode needs explicit carriage returns.
            out.write_all(text.replace('\n', "\r\n").as_bytes())?;
            out.write_all(b"\r\n")?;
        }
        let width = crossterm_terminal::size().map_or(80, |(w, _)| usize::from(w));
        let (lines, column) = model.prompt(width);
        out.write_all(lines.join("\r\n").as_bytes())?;
        self.cursor_row = (lines.len() - 1) as u16;
        queue!(out, cursor::MoveToColumn(column as u16))?;
        out.flush()
    }
}

/// The interactive shell for a terminal.
pub struct Shell {
    port: Option<String>,
    tx: SyncSender<Msg>,
    rx: Receiver<Msg>,
    interrupted: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

pub fn shell(options: UiOptions) -> (Shell, Interrupter) {
    let (tx, rx, interrupter, interrupted) = term::channel();
    (
        Shell {
            port: options.port,
            tx,
            rx,
            interrupted,
        },
        interrupter,
    )
}

impl Shell {
    /// Runs until quit, exit, Ctrl+C, Ctrl+D, SIGINT or SIGTERM. Closing the adapter's port ends
    /// a running scan and an unsaved pairing.
    pub fn run(self) -> Result<(), UiError> {
        let modes = Modes::enter(false)?;
        let input = Input::spawn(self.tx.clone());
        let mut model = Model::new(Live::new(self.tx.clone()), self.port);
        let mut out = io::stdout();
        let mut screen = Screen { cursor_row: 0 };
        let mut closing: Option<Instant> = None;
        loop {
            if self.interrupted.load(std::sync::atomic::Ordering::Acquire) && !model.quitting {
                model.update(Msg::Interrupt);
            }
            screen.draw(&mut model, &mut out)?;
            if model.done {
                break;
            }
            if model.quitting {
                let since = *closing.get_or_insert_with(Instant::now);
                if since.elapsed() > CLOSE_LIMIT {
                    break;
                }
            }
            match self.rx.recv_timeout(Duration::from_secs(1)) {
                Ok(msg) => {
                    model.update(msg);
                    while let Ok(msg) = self.rx.try_recv() {
                        model.update(msg);
                    }
                }
                Err(RecvTimeoutError::Timeout) => model.tick(),
                Err(RecvTimeoutError::Disconnected) => break,
            }
        }
        drop(input);
        screen.clear(&mut out)?;
        out.flush()?;
        drop(modes);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::{
        command::tests as fixture,
        fake::{Call, Fake},
    };
    use cordial_protocol::{self as p, CodeKind, ErrorCode};

    struct App {
        m: Model<Fake>,
        fake: Fake,
    }
    impl App {
        fn new() -> Self {
            let fake = Fake::default();
            let mut m = Model::new(fake.clone(), Some("/dev/ttyACM0".into()));
            let session = fake.next.get();
            let mut st = fixture::state();
            st.session = session;
            *fake.state.borrow_mut() = Some(st);
            for phase in [Phase::Opened, Phase::Ready] {
                m.update(Msg::Controller(Box::new(Event::Connection {
                    session,
                    port: "/dev/ttyACM0".into(),
                    phase,
                })));
            }
            fake.calls.borrow_mut().clear();
            m.output.clear();
            Self { m, fake }
        }
        fn typed(&mut self, line: &str) {
            self.m.update(Msg::Paste(line.into()));
            self.key(KeyCode::Enter, KeyModifiers::NONE);
        }
        fn key(&mut self, code: KeyCode, modifiers: KeyModifiers) {
            self.m.update(Msg::Key(KeyEvent::new(code, modifiers)));
        }
        fn runs(&self) -> Vec<String> {
            self.fake
                .calls
                .borrow()
                .iter()
                .filter_map(|c| match c {
                    Call::Run(r) => Some(r.clone()),
                    _ => None,
                })
                .collect()
        }
        fn done(&mut self, result: Result<Outcome, Error>) {
            let ticket = *self.m.jobs.keys().max().unwrap();
            let session = self.m.session.unwrap();
            self.m.update(Msg::Controller(Box::new(Event::Done {
                session,
                ticket,
                result,
            })));
        }
        fn pairing(&mut self, step: p::pairing::Step) {
            self.fake.state.borrow_mut().as_mut().unwrap().pairing = Some(p::Pairing {
                candidate: "c_2".into(),
                step: Some(step),
            });
            self.m.tick();
        }
    }

    #[test]
    fn commands_echo_and_print_results() {
        let mut app = App::new();
        app.typed("device list Connected");
        assert_eq!(app.m.output, ["> device list Connected"]);
        assert_eq!(app.runs(), ["Devices"]);
        app.done(Ok(Outcome::Devices));
        assert_eq!(app.m.output.len(), 2, "{:?}", app.m.output);
        assert!(
            app.m.output[1].starts_with("d_1  Keyboard  ble  connected  trusted"),
            "{}",
            app.m.output[1]
        );
        assert!(!app.m.output[1].contains("d_2"), "only connected devices");
        app.typed("scan start up");
        assert_eq!(
            app.m.output.last().unwrap(),
            "Error: invalid arguments for scan start; use help"
        );
        app.typed("device connect d_1");
        app.done(Err(Error::code(ErrorCode::Busy, Some("device connect"))));
        assert!(
            app.m
                .output
                .last()
                .unwrap()
                .starts_with("Error: busy: the adapter is busy")
        );
        app.typed("help");
        assert!(app.m.output.last().unwrap().starts_with("adapter list "));
    }

    #[test]
    fn history_and_completion() {
        let mut app = App::new();
        app.typed("adapter status");
        app.typed("device get d_2");
        app.m.update(Msg::Paste("pair ".into()));
        app.key(KeyCode::Up, KeyModifiers::NONE);
        assert_eq!(app.m.input.value(), "device get d_2");
        app.key(KeyCode::Up, KeyModifiers::NONE);
        assert_eq!(app.m.input.value(), "adapter status");
        app.key(KeyCode::Down, KeyModifiers::NONE);
        app.key(KeyCode::Down, KeyModifiers::NONE);
        assert_eq!(app.m.input.value(), "pair ", "draft restored");
        app.key(KeyCode::Tab, KeyModifiers::NONE);
        assert_eq!(app.m.input.value(), "pair start ");
        app.key(KeyCode::Tab, KeyModifiers::NONE);
        assert_eq!(app.m.input.value(), "pair accept ");
    }

    #[test]
    fn ctrl_c_quits_from_typed_input_and_from_a_pairing_prompt() {
        let mut app = App::new();
        app.m.update(Msg::Paste("unfinished".into()));
        app.key(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert!(app.m.quitting);
        let mut app = App::new();
        app.pairing(p::pairing::Step::EnterCode(p::EnterCode {
            kind: CodeKind::Passkey as i32,
        }));
        app.key(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert!(app.m.quitting);
        let mut app = App::new();
        app.key(KeyCode::Char('d'), KeyModifiers::CONTROL);
        assert!(app.m.quitting);
    }

    #[test]
    fn pairing_prompt_takes_an_answer_or_a_command() {
        let mut app = App::new();
        app.pairing(p::pairing::Step::EnterCode(p::EnterCode {
            kind: CodeKind::Passkey as i32,
        }));
        let (lines, _) = app.m.prompt(80);
        assert_eq!(
            lines,
            [
                "Pair c_2 enter_passkey . /COMMAND runs another command.",
                "> "
            ]
        );
        app.typed("12");
        assert_eq!(
            app.m.output.last().unwrap(),
            "Error: passkey must contain exactly six digits"
        );
        app.m.form.reset();
        app.typed("/adapter status");
        assert_eq!(app.runs(), ["Status"]);
        app.typed("042731");
        assert_eq!(app.runs().last().unwrap(), "Accept(Some(\"042731\"))");
    }

    #[test]
    fn unoffered_commands_are_refused_locally() {
        let mut app = App::new();
        {
            let mut st = app.fake.state.borrow_mut();
            let st = st.as_mut().unwrap();
            st.status.info.clear();
            st.status.transports.remove(0);
        }
        app.typed("file list /");
        assert_eq!(
            app.m.output.last().unwrap(),
            "Error: this adapter doesn't offer file access"
        );
        app.typed("scan start classic");
        assert_eq!(
            app.m.output.last().unwrap(),
            "Error: this adapter does not support the requested scan"
        );
        app.typed("help");
        let help = app.m.output.last().unwrap();
        assert!(help.contains("scan start [ble] [SECONDS]") && !help.contains("\nfile list"));
        app.m.update(Msg::Paste("fil".into()));
        app.key(KeyCode::Tab, KeyModifiers::NONE);
        assert_eq!(
            app.m.input.value(),
            "fil",
            "no completion for unoffered commands"
        );
        assert!(app.runs().is_empty());
    }

    #[test]
    fn commands_wait_for_saved_devices() {
        let fake = Fake::default();
        let mut m = Model::new(fake.clone(), Some("/dev/ttyACM0".into()));
        let session = fake.next.get();
        m.update(Msg::Controller(Box::new(Event::Connection {
            session,
            port: "p".into(),
            phase: Phase::Opened,
        })));
        m.update(Msg::Paste("device list".into()));
        m.update(Msg::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)));
        assert_eq!(m.output.last().unwrap(), &format!("Error: {STARTING}"));
        m.update(Msg::Paste("adapter status".into()));
        m.update(Msg::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)));
        assert!(fake.calls.borrow().contains(&Call::Run("Status".into())));
        m.update(Msg::Controller(Box::new(Event::Connection {
            session,
            port: "p".into(),
            phase: Phase::Failed {
                error: Error::new("adapter not ready"),
                open: true,
            },
        })));
        assert!(m.output.last().unwrap().ends_with(
            "adapter status, file list, file get and adapter bootloader still work; use adapter select to retry."
        ));
    }
}
