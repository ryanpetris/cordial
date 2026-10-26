//! The interactive shell: complete command output goes to terminal
//! scrollback above an editable prompt, with history, completion and
//! bluetoothctl-style commands. Commands run while others are pending, and
//! notifications print without disturbing a partially typed line. During a
//! pairing prompt the next line is the answer, and `/COMMAND` runs a command.
use crate::{
    commands::STARTING,
    controller::{Command, Event, Outcome, Phase, SessionId, State, Ticket},
    error::Error,
    migration::{self, Gate},
    model::{self, Prompt},
    ui::{
        Backend, Interrupter, Live, Msg, UiError, UiOptions,
        command::{self, Line},
        field::Field,
        reconnect::{Reconnect, Step},
        term::{self, Input, Modes},
        text::{self, Filter, safe},
    },
};
use cordial_client::{
    migration::{Outcome as Migrated, Result as MigrationResult},
    serial::PortInfo,
};
use cordial_protocol::migration::{MigrationEvent, migration_event};
use ratatui::crossterm::{
    cursor,
    event::{KeyCode, KeyEvent, KeyModifiers},
    queue,
    terminal::{self as crossterm_terminal, Clear, ClearType},
};
use std::{
    collections::{HashMap, HashSet},
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
    /// The session it was sent to. A replaced session's result is still reported, without the
    /// state of the session that replaced it.
    session: Option<SessionId>,
}

/// A migration, which owns every adapter connection while it lasts.
struct Migration {
    id: String,
    /// The port the adapter was on, where the worker looks first.
    port: String,
    stage: Stage,
    /// The worker has started; it exits before the migration ends.
    started: bool,
    /// The worker waits for the adapter to return.
    waiting: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Stage {
    /// The session is closing; the worker starts once it has released its port.
    Closing,
    Running,
    Cancelling,
    /// The worker failed. Nothing opens until the user cancels or migrates again.
    Failed,
}

const MIGRATING: &str = "Migrating the adapter's saved data…";
const MIGRATED: &str = "Migrated the adapter's saved data.";
const MIGRATION_CANCELLED: &str = "Migration cancelled.";

pub(crate) struct Model<B: Backend> {
    backend: B,
    session: Option<SessionId>,
    opening: Option<SessionId>,
    port: String,
    /// The startup adapter list may open its only adapter.
    startup: bool,
    /// The next adapter list is printed, as `adapter list` asked.
    listing: bool,
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
    /// The wait for the adapter to return after its connection dropped.
    reconnect: Option<Reconnect>,
    migration: Option<Migration>,
    /// Adapters whose migration the user cancelled; they migrate again only with migrate.
    declined: HashSet<String>,
}

impl<B: Backend> Model<B> {
    pub(crate) fn new(backend: B, port: Option<String>) -> Self {
        let mut m = Self {
            backend,
            session: None,
            opening: None,
            port: String::new(),
            startup: port.is_none(),
            listing: false,
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
            reconnect: None,
            migration: None,
            declined: HashSet::new(),
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

    /// Opens a port the user chose, replacing any open in progress.
    fn open(&mut self, port: String) {
        self.reconnect = None;
        self.open_port(port, None);
    }

    /// Opens a port, for the adapter with ID `id` when given.
    fn open_port(&mut self, port: String, id: Option<String>) {
        if self.quitting {
            return;
        }
        self.session = None;
        self.startup = false;
        self.preparing = false;
        self.port = port.clone();
        self.auth_key.clear();
        self.opening = Some(match id {
            Some(id) => self.backend.open_adapter(port, id),
            None => self.backend.open(port),
        });
    }

    /// Closes the session. A running migration is cancelled first, and the shell ends once its
    /// worker has exited.
    pub(crate) fn quit(&mut self) {
        if self.quitting {
            return;
        }
        self.quitting = true;
        match &mut self.migration {
            None => self.backend.close(),
            Some(m) if m.stage == Stage::Running => {
                m.stage = Stage::Cancelling;
                self.backend.cancel_migration();
            }
            Some(m) if m.stage == Stage::Failed => {
                self.migration = None;
                self.done = true;
            }
            Some(_) => {}
        }
    }

    /// Whether a migration owns the adapter connections.
    pub(crate) fn migrating(&self) -> bool {
        self.migration.is_some()
    }

    /// Closes the session and migrates its adapter once the port is released.
    fn begin_migration(&mut self, id: String) {
        self.declined.remove(&id);
        self.migration = Some(Migration {
            id,
            port: self.port.clone(),
            stage: Stage::Closing,
            started: false,
            waiting: false,
        });
        self.session = None;
        self.opening = None;
        self.preparing = false;
        self.reconnect = None;
        self.backend.close();
        self.log(MIGRATING);
    }

    fn start_worker(&mut self) {
        if let Some(m) = &mut self.migration {
            m.stage = Stage::Running;
            m.started = true;
            m.waiting = false;
            self.backend.migrate(m.id.clone(), m.port.clone());
        }
    }

    /// Ends the migration and opens its adapter again once it is found. Unless the migration
    /// succeeded, the adapter migrates again only with migrate.
    fn resume(&mut self, succeeded: bool) {
        let Some(m) = self.migration.take() else {
            return;
        };
        if !succeeded {
            self.declined.insert(m.id.clone());
        }
        self.port = m.port;
        self.reconnect = Reconnect::new(&m.id, &Error::new("the adapter didn't return"));
    }

    /// `migrate`: migrates the adapter, or tries a failed migration again.
    fn migrate_command(&mut self) {
        if let Some(m) = &self.migration {
            if m.stage == Stage::Failed {
                self.log(MIGRATING);
                self.start_worker();
            } else {
                self.log(format!("Error: {}", migration::BUSY));
            }
            return;
        }
        let Some(st) = self.state() else {
            return self
                .log("Error: no adapter selected; use adapter list and adapter select PORT");
        };
        match migration::gate(&st.status) {
            Gate::Assist => self.begin_migration(st.status.id),
            Gate::Normal => self.log("The adapter's saved data doesn't need migration."),
            Gate::Blocked(e) => self.log(format!("Error: {}", safe(&text::blocked_line(&e)))),
            Gate::Unsupported(e) => self.log(format!("Error: {}", safe(&e.message))),
        }
    }

    /// `migrate cancel`: stops the migration, or ends a failed one and resumes.
    fn cancel_command(&mut self) {
        let Some(m) = &mut self.migration else {
            return self.log("Error: no migration is running");
        };
        match m.stage {
            Stage::Running => {
                m.stage = Stage::Cancelling;
                self.backend.cancel_migration();
            }
            Stage::Closing => m.stage = Stage::Cancelling,
            Stage::Cancelling => {}
            Stage::Failed => self.cancelled(),
        }
    }

    fn migration_event(&mut self, event: MigrationEvent) {
        let Some(m) = &mut self.migration else { return };
        match event.event {
            Some(migration_event::Event::Waiting(_)) if !m.waiting => {
                m.waiting = true;
                self.log("Waiting for the adapter…");
            }
            Some(migration_event::Event::Progress(_)) => m.waiting = false,
            _ => {}
        }
    }

    /// The worker exited with its result.
    fn migrated(&mut self, result: MigrationResult<Migrated>) {
        let Some(m) = &mut self.migration else { return };
        if self.quitting {
            self.migration = None;
            self.done = true;
            return;
        }
        match result {
            Ok(Migrated::Success) => {
                self.log(MIGRATED);
                self.resume(true);
            }
            // A cancellation the user asked for ends the migration however the worker ended.
            Ok(Migrated::Cancelled) => self.cancelled(),
            Err(_) if m.stage == Stage::Cancelling => self.cancelled(),
            Err(e) => {
                m.stage = Stage::Failed;
                m.waiting = false;
                self.log(format!("Error: {}", safe(&migration::failure_text(&e))));
            }
        }
    }

    fn cancelled(&mut self) {
        self.log(MIGRATION_CANCELLED);
        self.resume(false);
    }

    fn run(&mut self, command: Command, filter: Option<Filter>) {
        let ticket = self.backend.run(command.clone());
        self.jobs.insert(
            ticket,
            Job {
                command,
                filter,
                session: self.session,
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
        self.sync_reconnect();
    }

    /// How long the shell waits for a message before a tick: shorter while the adapter is
    /// expected back after a USB reconnect.
    fn tick_interval(&self) -> Duration {
        if self.reconnect.is_some() {
            Duration::from_millis(100)
        } else {
            Duration::from_secs(1)
        }
    }

    /// Lists the ports while waiting for the adapter to return after a USB reconnect, and
    /// reports its loss if it doesn't return in time.
    fn sync_reconnect(&mut self) {
        let Some(r) = &mut self.reconnect else { return };
        // An open in progress finishes first.
        if self.quitting || self.opening.is_some() {
            return;
        }
        match r.step() {
            Step::Wait => {}
            Step::List => self.backend.list_ports(),
            Step::GiveUp(error) => {
                self.reconnect = None;
                self.log(format!(
                    "Adapter unavailable: {}",
                    safe(&text::error_line(&error))
                ));
            }
        }
    }

    pub(crate) fn update(&mut self, msg: Msg) {
        match msg {
            Msg::Interrupt => self.quit(),
            Msg::Resize(..) | Msg::Mouse(_) | Msg::Fleet(..) => {}
            Msg::Paste(text) => {
                if self.answering() {
                    self.form.insert(&text);
                } else {
                    self.input.insert(&text);
                }
            }
            Msg::Ports(result) => self.ports_listed(result),
            Msg::Migration(e) => self.migration_event(e),
            Msg::Migrated(result) => self.migrated(result),
            Msg::Controller(e) => self.controller(*e),
            Msg::Key(k) => self.key(k),
        }
        self.sync_reconnect();
    }

    fn ports_listed(&mut self, result: Result<Vec<PortInfo>, String>) {
        // A failed listing is tried again.
        if let Some(r) = &mut self.reconnect
            && let Some(port) = r.listed(result.as_deref().unwrap_or_default())
            && self.opening.is_none()
        {
            let id = r.id.clone();
            self.open_port(port, Some(id));
        }
        if std::mem::take(&mut self.listing) {
            match &result {
                Ok(ports) => self.log(text::ports(ports)),
                Err(e) => self.log(format!("Error: {}", safe(e))),
            }
        }
        if !std::mem::take(&mut self.startup) {
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
            Event::Closed => match &self.migration {
                Some(m) if !m.started => {
                    if self.quitting {
                        self.migration = None;
                        self.done = true;
                    } else if m.stage == Stage::Cancelling {
                        self.cancelled();
                    } else {
                        self.start_worker();
                    }
                }
                Some(_) => {}
                None if self.quitting => self.done = true,
                None => {}
            },
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
                if let Some(job) = self.jobs.remove(&ticket) {
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
                self.reconnect = None;
                self.session = Some(session);
                self.preparing = true;
                let port = safe(&self.port);
                self.log(format!("Connected to {port}. Use help for commands."));
            }
            Phase::Migrate | Phase::Blocked(_) | Phase::Unsupported(_)
                if self.opening == Some(session) =>
            {
                self.opening = None;
                if self.quitting {
                    return;
                }
                self.reconnect = None;
                self.session = Some(session);
                let port = safe(&self.port);
                self.log(format!("Connected to {port}."));
                match phase {
                    Phase::Blocked(e) => {
                        return self.log(format!("Error: {}", safe(&text::blocked_line(&e))));
                    }
                    Phase::Unsupported(e) => {
                        return self.log(format!("Error: {}", safe(&e.message)));
                    }
                    _ => {}
                }
                let id = self.state().map(|st| st.status.id).unwrap_or_default();
                if self.declined.contains(&id) {
                    self.log(format!("Error: {}", migration::REQUIRED));
                } else {
                    self.begin_migration(id);
                }
            }
            Phase::Ready if self.session == Some(session) => self.preparing = false,
            Phase::Failed {
                open: false,
                error: Error {
                    native_hello: None, ..
                },
            }
            | Phase::Other
                if (self.opening == Some(session) || self.session == Some(session))
                    && self.reconnect.is_some() =>
            {
                // The returning adapter's port may not open at first, or may briefly answer
                // for another adapter.
                self.opening = None;
                self.session = None;
                self.preparing = false;
                if let Some(r) = &mut self.reconnect {
                    r.retry();
                }
            }
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
                        "Error: {line}. adapter status, adapter restart, file list, file get and adapter bootloader still work; use adapter select to retry."
                    ));
                } else {
                    // An adapter that refused the connection isn't awaited any longer.
                    self.reconnect = None;
                    self.session = None;
                    self.log(format!("Open failed: {line}"));
                }
            }
            // The adapter may be reconnecting USB: it is reopened once it returns.
            Phase::Lost(error) if self.session == Some(session) && !self.quitting => {
                self.preparing = false;
                let id = self.state().map(|st| st.status.id).unwrap_or_default();
                self.reconnect = Reconnect::new(&id, &error);
                if self.reconnect.is_none() {
                    self.log(format!(
                        "Adapter unavailable: {}",
                        safe(&text::error_line(&error))
                    ));
                }
            }
            _ => {}
        }
    }

    fn finish(&mut self, job: Job, result: Result<Outcome, Error>) {
        let st = self.state().filter(|_| job.session == self.session);
        match result {
            Ok(outcome) => {
                let text = match (job.filter, &st) {
                    (Some(filter), Some(st)) => text::devices(st, filter),
                    _ => text::outcome(&job.command, &outcome, st.as_ref()),
                };
                self.log(text);
            }
            Err(error) => self.log(format!(
                "Error: {}",
                text::failure_text(&job.command, &error)
            )),
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
        if let Some(m) = &self.migration
            && !matches!(
                parsed,
                Line::Quit | Line::Help(_) | Line::Migrate | Line::MigrateCancel
            )
        {
            let why = if m.stage == Stage::Failed {
                format!(
                    "{}. Use migrate to try again or migrate cancel to stop",
                    migration::FAILED
                )
            } else {
                migration::BUSY.to_owned()
            };
            return self.log(format!("Error: {why}"));
        }
        let (command, filter) = match parsed {
            Line::Quit => return self.quit(),
            Line::Migrate => return self.migrate_command(),
            Line::MigrateCancel => return self.cancel_command(),
            Line::Select(port) => return self.open(port),
            Line::Help(topic) => {
                return self.log(command::help_on(self.state().as_ref(), topic.as_deref()));
            }
            Line::List => {
                self.listing = true;
                return self.backend.list_ports();
            }
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
        if let Some(m) = &self.migration {
            lines.push(
                match m.stage {
                    Stage::Failed => "Needs Attention",
                    _ if m.waiting => "Waiting for Adapter",
                    _ => "Migrating…",
                }
                .to_owned(),
            );
        }
        if self.state().is_some_and(|st| {
            st.available && !st.status.ready && matches!(migration::gate(&st.status), Gate::Normal)
        }) {
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
                    candidate,
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
            // A migration's worker is always waited for.
            if model.quitting && !model.migrating() {
                let since = *closing.get_or_insert_with(Instant::now);
                if since.elapsed() > CLOSE_LIMIT {
                    break;
                }
            }
            match self.rx.recv_timeout(model.tick_interval()) {
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
    use crate::{
        controller::Subject,
        ui::{
            command::tests as fixture,
            fake::{Call, Fake},
        },
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
                candidate: 2,
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
            app.m.output[1].starts_with("1  Keyboard  ble  connected  trusted"),
            "{}",
            app.m.output[1]
        );
        assert!(
            !app.m.output[1]
                .lines()
                .any(|l| l.trim_start().starts_with("2  ")),
            "only connected devices"
        );
        app.typed("scan start up");
        assert_eq!(
            app.m.output.last().unwrap(),
            "Error: invalid arguments for scan start; use help"
        );
        app.typed("device connect 1");
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
        app.typed("device get 2");
        app.m.update(Msg::Paste("pair ".into()));
        app.key(KeyCode::Up, KeyModifiers::NONE);
        assert_eq!(app.m.input.value(), "device get 2");
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
                "Pair 2 enter_passkey . /COMMAND runs another command.",
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
            st.status.information = None;
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
            "adapter status, adapter restart, file list, file get and adapter bootloader still work; use adapter select to retry."
        ));
    }

    impl App {
        fn connection(&mut self, session: SessionId, phase: Phase) {
            self.m.update(Msg::Controller(Box::new(Event::Connection {
                session,
                port: "/dev/ttyACM0".into(),
                phase,
            })));
        }
        fn calls(&self) -> Vec<Call> {
            self.fake.calls.borrow().clone()
        }
        /// Turns VIA on with profile 2, then drops the connection as the adapter does when it
        /// reconnects USB.
        fn save_and_lose_usb(&mut self) {
            self.fake.state.borrow_mut().as_mut().unwrap().status.id = "0123456789ABCDEF".into();
            fixture::with_profiles(&mut self.fake.state.borrow_mut().as_mut().unwrap().status);
            self.typed("adapter set interface via on 2");
            assert_eq!(self.runs().len(), 1, "{:?}", self.m.output);
            let mut saved = self.fake.state.borrow().as_ref().unwrap().status.clone();
            saved.configuration_interfaces[0].enabled = true;
            saved.configuration_interfaces[0].profile = 2;
            self.done(Ok(Outcome::Interface(
                p::ConfigurationInterface::Via,
                saved,
            )));
            self.lose_usb();
        }
        /// Drops the connection as the adapter does when it reconnects USB.
        fn lose_usb(&mut self) {
            self.fake.state.borrow_mut().as_mut().unwrap().available = false;
            self.fake.calls.borrow_mut().clear();
            self.m.output.clear();
            let session = self.m.session.unwrap();
            self.connection(session, Phase::Lost(Error::new("unplugged")));
        }
        /// Advertises HID gamepad output, with device 2's saved gamepad output choice.
        fn offer_hid(&mut self, saved: Option<p::OutputMode>) {
            let mut st = self.fake.state.borrow_mut();
            let st = st.as_mut().unwrap();
            st.status.id = "0123456789ABCDEF".into();
            st.status.output_support = vec![p::OutputSupport {
                role: p::Role::Gamepad as i32,
                modes: vec![p::OutputModeSupport {
                    mode: p::OutputMode::Hid as i32,
                    resources: Vec::new(),
                }],
            }];
            st.devices[1].output_preferences = saved
                .map(|mode| p::OutputPreference {
                    role: p::Role::Gamepad as i32,
                    mode: mode as i32,
                })
                .into_iter()
                .collect();
        }
    }

    fn mouse() -> Subject {
        Subject {
            id: 2,
            name: "Mouse".into(),
        }
    }

    #[test]
    fn a_gamepad_output_change_reopens_the_adapter_when_it_returns() {
        let set = Outcome::Device {
            subject: mouse(),
            device: fixture::state().devices[1].clone(),
        };
        for (saved, line, outcome) in [
            (None, "device set 2 gamepad-output hid", set),
            // Unpairing a device can remove its interfaces.
            (
                Some(p::OutputMode::Hid),
                "device unpair 2",
                Outcome::Unpaired(mouse()),
            ),
        ] {
            let mut app = App::new();
            app.offer_hid(saved);
            app.typed(line);
            app.done(Ok(outcome));
            app.lose_usb();
            assert!(app.m.output.is_empty(), "{line}: {:?}", app.m.output);
            assert_eq!(app.calls(), [Call::List]);
            app.m.update(Msg::Ports(Ok(vec![PortInfo {
                port: "/dev/ttyACM1".into(),
                serial: "0123456789ABCDEF".into(),
            }])));
            assert_eq!(
                app.calls().last(),
                Some(&Call::OpenAdapter(
                    "/dev/ttyACM1".into(),
                    "0123456789ABCDEF".into()
                ))
            );
            let session = app.fake.next.get();
            app.connection(session, Phase::Opened);
            assert!(app.m.reconnect.is_none());
            assert_eq!(app.m.session, Some(session));
        }
    }

    #[test]
    fn a_gamepad_output_change_that_doesnt_return_is_reported_unavailable() {
        let mut app = App::new();
        app.offer_hid(None);
        app.typed("device set 2 gamepad-output hid");
        // The connection dropped before the reply, so the save may have been stored.
        app.lose_usb();
        app.done(Err(Error::new("the connection to the adapter closed")));
        assert!(app.m.reconnect.is_some());
        app.m.output.clear();
        app.m.reconnect.as_mut().unwrap().expire();
        app.m.tick();
        assert_eq!(app.m.output, ["Adapter unavailable: unplugged"]);
        assert!(app.m.reconnect.is_none());
    }

    fn adapter(port: &str) -> PortInfo {
        PortInfo {
            port: port.into(),
            serial: "ADAPTER".into(),
        }
    }

    #[test]
    fn an_adapter_that_reconnects_usb_by_itself_is_reopened() {
        // The adapter reconnects USB when a device connects, without any save.
        let mut app = App::new();
        let old = app.m.session;
        app.lose_usb();
        assert!(app.m.output.is_empty(), "{:?}", app.m.output);
        assert_eq!(app.calls(), [Call::List]);
        assert_eq!(app.m.tick_interval(), Duration::from_millis(100));
        // Another adapter's port is left alone.
        app.m.update(Msg::Ports(Ok(vec![PortInfo {
            port: "/dev/ttyACM2".into(),
            serial: "OTHER".into(),
        }])));
        assert_eq!(app.calls(), [Call::List]);
        app.m.reconnect.as_mut().unwrap().hurry();
        app.m.tick();
        app.m.update(Msg::Ports(Ok(vec![adapter("/dev/ttyACM1")])));
        assert_eq!(
            app.calls().last(),
            Some(&Call::OpenAdapter("/dev/ttyACM1".into(), "ADAPTER".into()))
        );
        let session = app.fake.next.get();
        // The port may not open at first.
        app.connection(
            session,
            Phase::Failed {
                error: Error::new("busy"),
                open: false,
            },
        );
        assert!(app.m.output.is_empty(), "{:?}", app.m.output);
        app.m.reconnect.as_mut().unwrap().hurry();
        app.m.tick();
        app.m.update(Msg::Ports(Ok(vec![adapter("/dev/ttyACM1")])));
        let session = app.fake.next.get();
        {
            let mut st = app.fake.state.borrow_mut();
            let st = st.as_mut().unwrap();
            st.session = session;
            st.available = true;
        }
        app.connection(session, Phase::Opened);
        assert_ne!(app.m.session, old);
        assert_eq!(app.m.session, Some(session));
        assert!(app.m.reconnect.is_none());
        assert!(
            app.m.preparing,
            "the new session reads its status and devices"
        );
        app.connection(session, Phase::Ready);
        assert!(!app.m.preparing);
        assert_eq!(
            app.m.output,
            ["Connected to /dev/ttyACM1. Use help for commands."]
        );
        // A later loss is followed again.
        app.lose_usb();
        assert!(app.m.reconnect.is_some());
    }

    #[test]
    fn an_adapter_that_isnt_recognized_or_doesnt_return_is_reported_unavailable() {
        let mut app = App::new();
        app.fake
            .state
            .borrow_mut()
            .as_mut()
            .unwrap()
            .status
            .id
            .clear();
        app.lose_usb();
        assert_eq!(app.m.output, ["Adapter unavailable: unplugged"]);
        assert!(app.calls().is_empty());
        assert!(app.m.reconnect.is_none());

        let mut app = App::new();
        app.lose_usb();
        app.m.update(Msg::Ports(Err("no access".into())));
        assert!(app.m.output.is_empty(), "{:?}", app.m.output);
        app.m.reconnect.as_mut().unwrap().expire();
        app.m.tick();
        assert_eq!(app.m.output, ["Adapter unavailable: unplugged"]);
        assert!(app.m.reconnect.is_none());
        assert_eq!(app.m.tick_interval(), Duration::from_secs(1));
        // A late listing opens nothing.
        app.m.update(Msg::Ports(Ok(vec![adapter("/dev/ttyACM1")])));
        assert_eq!(app.calls(), [Call::List]);
    }

    #[test]
    fn the_user_can_list_select_or_quit_while_the_adapter_is_awaited() {
        let mut app = App::new();
        app.lose_usb();
        app.typed("adapter list");
        app.m.update(Msg::Ports(Ok(vec![adapter("/dev/ttyACM1")])));
        assert_eq!(
            app.m.output,
            ["> adapter list", "/dev/ttyACM1  ADAPTER"],
            "the listing is printed as well as followed"
        );
        assert_eq!(
            app.calls().last(),
            Some(&Call::OpenAdapter("/dev/ttyACM1".into(), "ADAPTER".into()))
        );

        // Choosing another port ends the wait, so the adapter isn't reopened over it.
        let mut app = App::new();
        app.lose_usb();
        app.typed("adapter select /dev/ttyUSB0");
        assert!(app.m.reconnect.is_none());
        app.m.update(Msg::Ports(Ok(vec![adapter("/dev/ttyACM1")])));
        assert_eq!(app.calls(), [Call::List, Call::Open("/dev/ttyUSB0".into())]);
        assert_eq!(app.m.output, ["> adapter select /dev/ttyUSB0"]);
        let session = app.fake.next.get();
        app.connection(
            session,
            Phase::Failed {
                error: Error::new("no such port"),
                open: false,
            },
        );
        assert_eq!(app.m.output.last().unwrap(), "Open failed: no such port");

        let mut app = App::new();
        app.lose_usb();
        app.key(KeyCode::Char('d'), KeyModifiers::CONTROL);
        app.m.update(Msg::Ports(Ok(vec![adapter("/dev/ttyACM1")])));
        app.m.reconnect.as_mut().unwrap().hurry();
        app.m.tick();
        assert_eq!(app.calls(), [Call::List, Call::Close]);
    }

    impl App {
        /// Reports the result of the command sent with `ticket` to session `session`.
        fn done_for(&mut self, session: SessionId, ticket: Ticket, result: Result<Outcome, Error>) {
            self.m.update(Msg::Controller(Box::new(Event::Done {
                session,
                ticket,
                result,
            })));
        }
        /// Lists the adapter on another port, which starts reopening it.
        fn adapter_returns(&mut self) -> SessionId {
            self.m.update(Msg::Ports(Ok(vec![adapter("/dev/ttyACM1")])));
            assert_eq!(self.last_open().as_deref(), Some("/dev/ttyACM1"));
            self.fake.next.get()
        }
        fn last_open(&self) -> Option<String> {
            self.calls().into_iter().rev().find_map(|c| match c {
                Call::Open(port) | Call::OpenAdapter(port, _) => Some(port),
                _ => None,
            })
        }
    }

    fn closed() -> Result<Outcome, Error> {
        Err(Error::new("the connection to the adapter closed"))
    }

    const UNCONFIRMED: &str = "Error: the adapter couldn't confirm whether the change was saved";

    #[test]
    fn a_command_cut_short_by_the_loss_reports_its_failure_and_isnt_sent_again() {
        let mut app = App::new();
        app.typed("device set 2 trusted off");
        app.lose_usb();
        app.done(closed());
        assert_eq!(app.m.output, [UNCONFIRMED]);
        app.m.update(Msg::Ports(Ok(vec![adapter("/dev/ttyACM1")])));
        let session = app.fake.next.get();
        app.connection(session, Phase::Opened);
        app.connection(session, Phase::Ready);
        assert!(app.runs().is_empty(), "{:?}", app.calls());
    }

    #[test]
    fn only_a_save_cut_short_reports_an_unconfirmed_outcome() {
        let mut app = App::new();
        app.typed("device get 2");
        app.done(closed());
        assert_eq!(
            app.m.output.last().unwrap(),
            "Error: the connection to the adapter closed"
        );
        app.typed("device set 2 trusted off");
        app.done(Err(Error::new("the adapter didn't respond in time")));
        assert_eq!(app.m.output.last().unwrap(), UNCONFIRMED);
        app.typed("device set 2 trusted off");
        app.done(Err(Error::code(ErrorCode::Busy, Some("device set"))));
        assert!(
            app.m
                .output
                .last()
                .unwrap()
                .starts_with("Error: busy: the adapter is busy"),
            "{:?}",
            app.m.output
        );
    }

    #[test]
    fn a_result_that_arrives_after_the_adapter_is_reopened_is_reported_once() {
        let saved = || {
            Ok(Outcome::Device {
                subject: mouse(),
                device: fixture::state().devices[1].clone(),
            })
        };
        // The result arrives while the adapter is being reopened, or once it has been.
        for reopened in [false, true] {
            let mut app = App::new();
            let old = app.m.session.unwrap();
            app.typed("device set 2 trusted off");
            let save = *app.m.jobs.keys().max().unwrap();
            app.typed("device get 2");
            let read = *app.m.jobs.keys().max().unwrap();
            app.lose_usb();
            let session = app.adapter_returns();
            if reopened {
                app.connection(session, Phase::Opened);
                app.m.output.clear();
            }
            app.done_for(old, save, saved());
            assert_eq!(app.m.output.len(), 1, "{reopened}: {:?}", app.m.output);
            assert!(
                app.m.output[0].contains("Mouse"),
                "{reopened}: {:?}",
                app.m.output
            );
            app.done_for(old, save, saved());
            app.done_for(old, read, closed());
            assert_eq!(
                app.m.output[1..],
                ["Error: the connection to the adapter closed"],
                "{reopened}"
            );
            // A ticket the shell didn't send is ignored.
            app.done_for(old, 999, saved());
            assert_eq!(app.m.output.len(), 2, "{reopened}: {:?}", app.m.output);
            assert!(app.runs().is_empty(), "{reopened}: {:?}", app.calls());
            if !reopened {
                app.connection(session, Phase::Opened);
                assert_eq!(app.m.session, Some(session));
            }
        }

        // A save cut off by the loss reports that its outcome is unknown.
        let mut app = App::new();
        let old = app.m.session.unwrap();
        app.typed("device set 2 trusted off");
        let save = *app.m.jobs.keys().max().unwrap();
        app.lose_usb();
        let session = app.adapter_returns();
        app.connection(session, Phase::Opened);
        app.m.output.clear();
        app.done_for(old, save, closed());
        assert_eq!(app.m.output, [UNCONFIRMED]);
    }

    #[test]
    fn selecting_a_port_while_the_adapter_is_reopened_opens_that_port() {
        let mut app = App::new();
        app.lose_usb();
        let reopening = app.adapter_returns();
        app.typed("adapter select /dev/ttyUSB0");
        assert_eq!(app.last_open().as_deref(), Some("/dev/ttyUSB0"));
        assert!(app.m.reconnect.is_none());
        let selected = app.fake.next.get();
        assert_ne!(selected, reopening);
        // The replaced open finishing or failing changes nothing.
        app.connection(reopening, Phase::Opened);
        app.connection(
            reopening,
            Phase::Failed {
                error: Error::new("busy"),
                open: false,
            },
        );
        assert_eq!(app.m.session, None);
        assert_eq!(app.m.opening, Some(selected));
        app.connection(selected, Phase::Opened);
        assert_eq!(app.m.session, Some(selected));
        assert_eq!(
            app.m.output,
            [
                "> adapter select /dev/ttyUSB0",
                "Connected to /dev/ttyUSB0. Use help for commands."
            ]
        );
        // The adapter isn't looked for again.
        app.m.update(Msg::Ports(Ok(vec![adapter("/dev/ttyACM1")])));
        assert_eq!(app.last_open().as_deref(), Some("/dev/ttyUSB0"));
    }

    #[test]
    fn a_save_that_reconnects_usb_reopens_the_adapter_when_it_returns() {
        let mut app = App::new();
        app.save_and_lose_usb();
        assert!(app.m.output.is_empty(), "{:?}", app.m.output);
        assert_eq!(app.calls(), [Call::List]);
        // A listing without it isn't printed, and is tried again.
        app.m.update(Msg::Ports(Ok(Vec::new())));
        assert!(app.m.output.is_empty(), "{:?}", app.m.output);
        app.m.reconnect.as_mut().unwrap().hurry();
        app.m.tick();
        assert_eq!(app.calls(), [Call::List, Call::List]);
        app.m.update(Msg::Ports(Ok(vec![PortInfo {
            port: "/dev/ttyACM1".into(),
            serial: "0123456789ABCDEF-vial:f64c2b3c".into(),
        }])));
        assert_eq!(
            app.calls().last(),
            Some(&Call::OpenAdapter(
                "/dev/ttyACM1".into(),
                "0123456789ABCDEF".into()
            ))
        );
        let session = app.fake.next.get();
        app.connection(session, Phase::Opened);
        assert!(app.m.reconnect.is_none());
        assert_eq!(app.m.session, Some(session));
        assert_eq!(
            app.m.output,
            ["Connected to /dev/ttyACM1. Use help for commands."]
        );
    }

    #[test]
    fn an_adapter_that_doesnt_return_is_reported_unavailable() {
        let mut app = App::new();
        app.save_and_lose_usb();
        app.m.reconnect.as_mut().unwrap().expire();
        app.m.tick();
        assert_eq!(app.m.output, ["Adapter unavailable: unplugged"]);
        assert!(app.m.reconnect.is_none());
    }

    /// A shell whose adapter reports that its saved data needs migration.
    fn needing_migration() -> App {
        let fake = Fake::default();
        let m = Model::new(fake.clone(), Some("/dev/ttyACM0".into()));
        let mut app = App { m, fake };
        let mut st = fixture::state();
        st.session = app.fake.next.get();
        st.status.id = "0123456789ABCDEF".into();
        st.status.ready = false;
        st.loaded = false;
        st.status.errors = vec![p::Error {
            code: p::ErrorCode::StorageMigrationRequired as i32,
            reason: p::ErrorReason::Storage as i32,
            outcome: p::ErrorOutcome::NoEffect as i32,
            ..Default::default()
        }];
        *app.fake.state.borrow_mut() = Some(st);
        app.fake.calls.borrow_mut().clear();
        app.connection(app.fake.next.get(), Phase::Migrate);
        app
    }

    fn migration_event(event: migration_event::Event) -> Msg {
        Msg::Migration(MigrationEvent { event: Some(event) })
    }

    fn failed() -> Msg {
        Msg::Migrated(Err(cordial_client::migration::Error {
            reason: "the adapter refused a write".into(),
            outcome_unknown: false,
        }))
    }

    #[test]
    fn migration_starts_only_once_the_session_has_released_its_port() {
        let mut app = needing_migration();
        assert_eq!(app.calls(), [Call::Close]);
        assert!(app.m.output.iter().any(|l| l == MIGRATING));
        app.m.update(Msg::Controller(Box::new(Event::Closed)));
        assert_eq!(
            app.calls(),
            [
                Call::Close,
                Call::Migrate("0123456789ABCDEF".into(), "/dev/ttyACM0".into())
            ]
        );
        app.m
            .update(migration_event(migration_event::Event::Waiting(
                Default::default(),
            )));
        assert_eq!(app.m.prompt(80).0[0], "Waiting for Adapter");
        // Nothing opens or runs while it migrates, however long it waits.
        app.fake.calls.borrow_mut().clear();
        for line in [
            "adapter status",
            "file list /",
            "adapter select /dev/ttyACM1",
            "adapter list",
        ] {
            app.typed(line);
        }
        app.m.tick();
        assert!(app.calls().is_empty(), "{:?}", app.calls());
        assert!(app.m.output.iter().any(|l| l.contains(migration::BUSY)));
        app.m.update(Msg::Migrated(Ok(Migrated::Success)));
        assert!(app.m.output.iter().any(|l| l == MIGRATED));
        assert!(!app.m.migrating());
        // The adapter is found again by its ID.
        assert_eq!(app.calls(), [Call::List]);
    }

    #[test]
    fn a_failed_migration_keeps_every_port_closed_until_cancelled() {
        let mut app = needing_migration();
        app.m.update(Msg::Controller(Box::new(Event::Closed)));
        app.m.update(failed());
        assert!(app.m.migrating());
        assert_eq!(app.m.prompt(80).0[0], "Needs Attention");
        app.fake.calls.borrow_mut().clear();
        app.typed("adapter status");
        app.m.tick();
        assert!(app.calls().is_empty());
        // Migrate tries again without opening anything.
        app.typed("migrate");
        assert_eq!(
            app.calls(),
            [Call::Migrate(
                "0123456789ABCDEF".into(),
                "/dev/ttyACM0".into()
            )]
        );
        app.m.update(failed());
        app.fake.calls.borrow_mut().clear();
        app.typed("migrate cancel");
        assert!(!app.m.migrating());
        assert_eq!(app.calls(), [Call::List]);
        // The adapter returns still needing migration, and waits for migrate.
        app.m.update(Msg::Ports(Ok(vec![PortInfo {
            port: "/dev/ttyACM0".into(),
            serial: "0123456789ABCDEF".into(),
        }])));
        let session = app.fake.next.get();
        app.fake.calls.borrow_mut().clear();
        app.fake.state.borrow_mut().as_mut().unwrap().session = session;
        app.connection(session, Phase::Migrate);
        assert!(app.calls().is_empty(), "{:?}", app.calls());
        assert!(app.m.output.iter().any(|l| l.contains(migration::REQUIRED)));
        app.typed("migrate");
        assert_eq!(app.calls(), [Call::Close]);
    }

    #[test]
    fn quitting_cancels_the_migration_and_waits_for_the_worker() {
        let mut app = needing_migration();
        app.m.update(Msg::Controller(Box::new(Event::Closed)));
        app.fake.calls.borrow_mut().clear();
        app.key(KeyCode::Char('c'), KeyModifiers::CONTROL);
        app.m.update(Msg::Interrupt);
        assert_eq!(app.calls(), [Call::CancelMigration]);
        assert!(!app.m.done);
        app.m
            .update(migration_event(migration_event::Event::Cancelled(
                Default::default(),
            )));
        assert!(!app.m.done);
        app.m.update(Msg::Migrated(Ok(Migrated::Cancelled)));
        assert!(app.m.done);
        assert_eq!(app.calls(), [Call::CancelMigration]);
    }

    #[test]
    fn cancelling_does_not_migrate_again_by_itself() {
        let mut app = needing_migration();
        app.m.update(Msg::Controller(Box::new(Event::Closed)));
        app.typed("migrate cancel");
        assert_eq!(app.calls().last(), Some(&Call::CancelMigration));
        // Still suspended until the worker exits.
        app.fake.calls.borrow_mut().clear();
        app.typed("adapter status");
        assert!(app.calls().is_empty());
        app.m.update(Msg::Migrated(Ok(Migrated::Cancelled)));
        assert_eq!(app.calls(), [Call::List]);
        assert!(app.m.declined.contains("0123456789ABCDEF"));
    }

    #[test]
    fn a_failure_reported_after_cancel_ends_the_migration() {
        let mut app = needing_migration();
        app.m.update(Msg::Controller(Box::new(Event::Closed)));
        app.fake.calls.borrow_mut().clear();
        // The worker failed before it saw the cancellation; its result arrives after.
        app.typed("migrate cancel");
        assert_eq!(app.calls(), [Call::CancelMigration]);
        assert!(app.m.migrating());
        app.m.update(failed());
        assert!(!app.m.migrating());
        assert!(app.m.output.iter().any(|l| l == MIGRATION_CANCELLED));
        assert!(app.m.declined.contains("0123456789ABCDEF"));
        // The adapter is found again by its ID, and another adapter on its port is passed over.
        assert_eq!(app.calls(), [Call::CancelMigration, Call::List]);
        app.m.update(Msg::Ports(Ok(vec![PortInfo {
            port: "/dev/ttyACM0".into(),
            serial: "0123456789ABCDEF".into(),
        }])));
        assert_eq!(
            app.calls().last(),
            Some(&Call::OpenAdapter(
                "/dev/ttyACM0".into(),
                "0123456789ABCDEF".into()
            ))
        );
        app.connection(app.fake.next.get(), Phase::Other);
        assert_eq!(app.m.session, None);
        assert!(app.m.reconnect.is_some());
    }

    #[test]
    fn an_unsupported_adapter_says_why() {
        let fake = Fake::default();
        let mut app = App {
            m: Model::new(fake.clone(), Some("/dev/ttyACM0".into())),
            fake,
        };
        let session = app.fake.next.get();
        app.connection(
            session,
            Phase::Unsupported(Error::new(migration::UNSUPPORTED)),
        );
        assert_eq!(app.m.session, Some(session));
        assert!(app.m.output.iter().any(|l| l.contains("Update Cordial")));
        assert!(!app.m.migrating());
    }

    #[test]
    fn a_blocked_adapter_shows_its_error_and_doesnt_migrate() {
        let fake = Fake::default();
        let mut app = App {
            m: Model::new(fake.clone(), Some("/dev/ttyACM0".into())),
            fake,
        };
        let mut st = fixture::state();
        st.session = app.fake.next.get();
        st.status.ready = false;
        st.status.errors = vec![
            p::Error {
                code: p::ErrorCode::StorageMigrationRequired as i32,
                reason: p::ErrorReason::Storage as i32,
                outcome: p::ErrorOutcome::NoEffect as i32,
                ..Default::default()
            },
            p::Error {
                code: p::ErrorCode::StorageFailed as i32,
                reason: p::ErrorReason::Storage as i32,
                related: vec![p::Error {
                    code: p::ErrorCode::StorageFull as i32,
                    ..Default::default()
                }],
                ..Default::default()
            },
        ];
        let error = Error::dongle(st.status.errors[1].clone(), None);
        *app.fake.state.borrow_mut() = Some(st);
        app.fake.calls.borrow_mut().clear();
        app.connection(app.fake.next.get(), Phase::Blocked(error.clone()));
        assert!(!app.m.migrating());
        assert!(app.calls().is_empty(), "{:?}", app.calls());
        let line = "Error: the adapter couldn't read or write its saved data; also (the adapter's storage is full)".to_owned();
        assert_eq!(app.m.output.last(), Some(&line));
        app.typed("migrate");
        assert_eq!(app.m.output.last(), Some(&line));
        assert!(app.calls().is_empty(), "{:?}", app.calls());
        // Restart runs without readiness or migration.
        app.typed("adapter restart");
        assert_eq!(app.calls(), [Call::Run("Restart".into())]);
    }

    const NEWER: &str =
        "Open failed: this adapter requires a newer version of Cordial. Update Cordial to connect";

    fn newer() -> Error {
        let hello = cordial_client::compatibility::NativeHello {
            hello: p::AdapterHello {
                protocol_version: Some(p::CURRENT_PROTOCOL_VERSION + 1),
                ..Default::default()
            },
            bytes: Vec::new(),
        };
        Error::from_client(cordial_client::Error::Incompatible(hello), "adapter status")
    }

    #[test]
    fn a_selected_adapter_that_refuses_says_why() {
        let mut app = App::new();
        app.typed("adapter select /dev/ttyACM1");
        let session = app.fake.next.get();
        app.connection(
            session,
            Phase::Failed {
                error: newer(),
                open: false,
            },
        );
        assert_eq!(app.m.output.last().unwrap(), NEWER);
        assert_eq!(app.m.session, None);
    }

    #[test]
    fn an_adapter_that_returns_refusing_is_no_longer_awaited() {
        let mut app = App::new();
        app.lose_usb();
        app.m.update(Msg::Ports(Ok(vec![adapter("/dev/ttyACM1")])));
        let session = app.fake.next.get();
        app.connection(
            session,
            Phase::Failed {
                error: newer(),
                open: false,
            },
        );
        assert_eq!(app.m.output, [NEWER]);
        assert!(app.m.reconnect.is_none());
        app.fake.calls.borrow_mut().clear();
        app.m.tick();
        app.m.update(Msg::Ports(Ok(vec![adapter("/dev/ttyACM1")])));
        assert!(app.calls().is_empty(), "{:?}", app.calls());
    }
}
