//! The full-screen TUI. Every action is reachable with the mouse; keys are
//! alternatives, and only text entry needs typing. The pointer and Tab share
//! one highlight: hovering a control highlights it, leaving controls clears
//! it, and Tab continues from it. Hovering never activates a control.
mod activity;
mod files;
mod keys;
pub(crate) mod layout;
mod settings;
#[cfg(test)]
mod tests;
mod view;

use crate::{
    commands::{self, STARTING},
    controller::{Command, Event, Outcome, Phase, SessionId, State, Ticket, Toggle},
    error::Error,
    model::{self, Prompt},
    ui::{Backend, Msg, field::Field, text},
};
use activity::LogEntry;
use cordial_client::serial::PortInfo;
use cordial_protocol::{self as p, CodeKind, Platform, Transport, value::Value};
use layout::Hit;
use ratatui::crossterm::event::{KeyEvent, MouseButton, MouseEvent, MouseEventKind};
use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

/// How long the TUI shows a starting adapter as connecting before showing it
/// as not ready, with what still works.
const STARTUP_GRACE: Duration = Duration::from_secs(10);

pub const MIN_WIDTH: usize = 40;
pub const MIN_HEIGHT: usize = 10;

/// Scrollable panes.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Area {
    Devices,
    Details,
    Events,
    Dialog,
    SetList,
    Editor,
    Files,
    Transfer,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Menu {
    Scan,
    Adapter,
}

/// What a control does. Hits carry actions; the highlight names one.
#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    Quit,
    Help,
    /// The Adapter settings dialog.
    AdapterSettings,
    Menu(Menu),
    /// A pane that receives wheel scrolling; never a click target.
    Wheel(Area),
    /// A border arrow; true scrolls up.
    Scroll(Area, bool),
    Reopen,
    /// Focuses the pairing-code field.
    Input,
    CancelDialog,
    Accept,
    Reject,
    Confirm,
    Adapters,
    RefreshPorts,
    Port(String),
    Device(String),
    DeviceSettings,
    /// Asks the selected connected device for current information.
    RefreshInfo,
    /// Scans one transport, or every supported one.
    Scan(Option<Transport>),
    ScanOff,
    Refresh,
    CancelPairing,
    Pair,
    Connect,
    Disconnect,
    Enable,
    Disable,
    Trust,
    Untrust,
    Block,
    Unblock,
    Remove,
    Hide,
    /// Lists nearby devices with neither a name nor a known kind, or not.
    ShowUnnamed(bool),
    Hidpp(bool),
    Platform(Platform),
    Rename,
    SaveName,
    ResetName,
    Bootloader,
    SettingsBack,
    SettingsReload,
    SettingsRefresh,
    Category(&'static str),
    Setting(String),
    Draft(String, Value),
    /// SmartShift On or Off, separate from Min and Max, which can set the
    /// same values, so each control has one highlight.
    Switch(String, Value),
    Step(String, i64),
    /// Drops the setting's staged change.
    Undo(String),
    /// Stages saving the value the device reports.
    Keep(String),
    /// Stages forgetting the saved value.
    Forget(String),
    /// Saves every staged change of the page's device.
    SaveAll,
    /// Drops every staged change of the page's device.
    Discard,
    FilesOpen,
    FilesClose,
    FilesUp,
    FilesRefresh,
    /// A directory row opens it; a file row selects it.
    FilesEntry(String),
    /// Focuses the local destination field.
    FilesDest,
    FilesDownload,
    FilesRetry,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Dialog {
    Help,
    /// Adapter settings.
    Settings,
    Rename,
    Remove(String),
    Bootloader,
    /// Replacing an existing local file with a download.
    Replace(files::Target),
}

/// A command started from the TUI, kept until its result.
#[derive(Clone, Debug)]
pub(super) struct Job {
    command: Command,
    /// A pairing answer; its error belongs in the prompt.
    answer: bool,
    /// The settings page's Save of these keys.
    save: Option<Vec<String>>,
    /// Loads the settings page's list.
    load: bool,
}
impl Job {
    fn new(command: Command) -> Self {
        Self {
            command,
            answer: false,
            save: None,
            load: false,
        }
    }
}

/// The TUI's state; see `update` for how messages change it and `render` for
/// how it is drawn.
pub(crate) struct Model<B: Backend> {
    backend: B,
    session: Option<SessionId>,
    opening: Option<SessionId>,
    quitting: bool,
    /// Waiting for readiness and the saved devices.
    preparing: bool,
    /// The adapter stayed open without becoming ready; why.
    unready: Option<String>,
    /// Since when the adapter reports that it is starting.
    waiting_since: Option<Instant>,
    /// The startup adapter list may open its only adapter.
    startup: bool,
    /// An adapter list arrived; until then the TUI shows it loading.
    listed: bool,
    list_err: Option<String>,
    chooser: bool,
    pub(super) done: bool,
    port: String,
    ports: Vec<PortInfo>,
    pub(super) width: usize,
    pub(super) height: usize,
    form: Field,
    form_focused: bool,
    form_err: String,
    selected: String,
    logs: Vec<LogEntry>,
    /// Last reported state, to describe changes.
    known: HashMap<String, p::Device>,
    /// Last known warning list of each device, to report additions.
    known_warnings: HashMap<String, Vec<p::DeviceWarning>>,
    /// Candidate names for devices no longer listed.
    names: HashMap<String, String>,
    /// Candidates the activity reported this scan, and whether by name.
    found: HashMap<String, bool>,
    /// Nearby devices with neither a name nor a known kind are listed; off in
    /// each new TUI session.
    show_unnamed: bool,
    /// Activity text width at the last draw.
    event_width: usize,
    /// Border-arrow scroll step per pane.
    steps: HashMap<Area, usize>,
    /// Footer failures up to here were dismissed by a key.
    news_seen: Option<Instant>,
    /// The failure the footer drew, and since when.
    news_shown: Option<Instant>,
    news_shown_at: Option<Instant>,
    device_scroll: usize,
    detail_scroll: usize,
    event_scroll: usize,
    dialog_scroll: usize,
    dialog: Option<Dialog>,
    menu: Option<Menu>,
    scan_label: &'static str,
    auth_key: String,
    pub(super) hits: Vec<Hit>,
    armed: Option<Action>,
    /// The highlighted control, and what it belonged to.
    focus: Option<Action>,
    focus_ctx: String,
    /// The pointer's last reported cell, and whether it placed the highlight.
    px: usize,
    py: usize,
    hover: bool,
    /// Scroll the device list to the selection.
    reveal: bool,
    last_err: Option<String>,
    jobs: HashMap<Ticket, Job>,
    page: settings::Page,
    files: files::Files,
}

impl<B: Backend> Model<B> {
    pub(crate) fn new(backend: B, port: Option<String>) -> Self {
        let startup = port.is_none();
        let mut m = Self {
            backend,
            session: None,
            opening: None,
            quitting: false,
            preparing: false,
            unready: None,
            waiting_since: None,
            startup,
            listed: false,
            list_err: None,
            chooser: startup,
            done: false,
            port: port.unwrap_or_default(),
            ports: Vec::new(),
            width: 80,
            height: 24,
            form: Field::new(16),
            form_focused: false,
            form_err: String::new(),
            selected: String::new(),
            logs: Vec::new(),
            known: HashMap::new(),
            known_warnings: HashMap::new(),
            names: HashMap::new(),
            found: HashMap::new(),
            show_unnamed: false,
            event_width: 0,
            steps: HashMap::new(),
            news_seen: None,
            news_shown: None,
            news_shown_at: None,
            device_scroll: 0,
            detail_scroll: 0,
            event_scroll: 0,
            dialog_scroll: 0,
            dialog: None,
            menu: None,
            scan_label: "",
            auth_key: String::new(),
            hits: Vec::new(),
            armed: None,
            focus: None,
            focus_ctx: String::new(),
            px: 0,
            py: 0,
            hover: false,
            reveal: false,
            last_err: None,
            jobs: HashMap::new(),
            page: settings::Page::default(),
            files: files::Files::default(),
        };
        m.backend.list_ports();
        if !m.port.is_empty() {
            let port = m.port.clone();
            m.open(port);
        }
        m
    }

    /// The session's current state, while it is the one shown, without the
    /// nearby devices the unnamed filter hides. Everything the TUI draws,
    /// selects and acts on reads this, so hidden devices take no part.
    pub(super) fn state(&self) -> Option<State> {
        let mut st = self.full_state()?;
        if !self.show_unnamed {
            // A device being paired stays listed so its pairing can be followed.
            let pairing = st
                .pairing
                .as_ref()
                .filter(|p| model::pairing_running(p))
                .map(|p| p.candidate.clone());
            st.candidates.retain(|c| {
                text::named(c)
                    || !matches!(c.kind(), p::Kind::Unknown)
                    || pairing.as_ref() == Some(&c.id)
            });
        }
        Some(st)
    }

    /// The session's current state, including every nearby device.
    fn full_state(&self) -> Option<State> {
        let session = self.session?;
        self.backend.state().filter(|s| s.session == session)
    }

    /// Deselects a nearby device the unnamed filter now hides, such as after
    /// turning Show Unnamed Devices off or once its pairing ends, so no
    /// details or actions stay for it.
    pub(super) fn drop_hidden_selection(&mut self) {
        let (Some(all), Some(st)) = (self.full_state(), self.state()) else {
            return;
        };
        let id = &self.selected;
        let hidden = all.candidate(id).is_some() && st.candidate(id).is_none();
        if hidden {
            self.selected.clear();
            self.detail_scroll = 0;
        }
    }

    /// How many nearby devices the unnamed filter hides now.
    pub(super) fn unnamed_hidden(&self) -> usize {
        let all = self.full_state().map_or(0, |st| st.candidates.len());
        let shown = self.state().map_or(0, |st| st.candidates.len());
        all.saturating_sub(shown)
    }

    /// Whether a spinner is on screen, which needs faster frames.
    pub(crate) fn animating(&self) -> bool {
        if matches!(self.gate(), Some("loading" | "connecting")) {
            return true;
        }
        self.state().is_some_and(|st| {
            st.available
                && (!st.pending.is_empty()
                    || st.scanning.is_some()
                    || st.pairing.as_ref().is_some_and(model::pairing_running))
        })
    }

    /// The screen shown instead of the main view: finding adapters,
    /// connecting until the adapter is ready and its saved devices are loaded,
    /// or choosing one. The main view shows only a prepared adapter.
    pub(super) fn gate(&self) -> Option<&'static str> {
        if self.opening.is_some() || self.preparing {
            Some("connecting")
        } else if self.session.is_none() && !self.listed {
            Some("loading")
        } else if self.chooser || self.session.is_none() {
            Some("chooser")
        } else {
            None
        }
    }

    fn open(&mut self, port: String) {
        if self.opening.is_some() || self.quitting {
            return;
        }
        self.session = None;
        self.startup = false; // Startup's one automatic open is spent.
        self.preparing = false;
        self.unready = None;
        self.waiting_since = None;
        self.port = port.clone();
        self.selected.clear();
        self.auth_key.clear();
        if self.dialog != Some(Dialog::Help) {
            // Help concerns no adapter, so it stays open.
            self.dialog = None;
            self.dialog_scroll = 0;
        }
        self.menu = None;
        self.page.reset();
        self.files.reset();
        self.form_focused = false; // Keys return to the view once the adapter opens.
        self.opening = Some(self.backend.open(port));
    }

    pub(crate) fn quitting(&self) -> bool {
        self.quitting
    }

    pub(crate) fn quit(&mut self) {
        if self.quitting {
            return;
        }
        self.quitting = true;
        self.backend.close();
    }

    fn running(&self, f: impl Fn(&Command) -> bool) -> bool {
        self.jobs.values().any(|j| f(&j.command))
    }

    fn renaming(&self) -> bool {
        self.running(|c| matches!(c, Command::Name(_)))
    }

    fn execute(&mut self, command: Command) {
        self.execute_job(Job::new(command));
    }

    fn execute_job(&mut self, job: Job) {
        // Until the saved devices load, commands would act on an empty list.
        if self.preparing && !job.command.direct() {
            let session = self.session.unwrap_or_default();
            self.finish(session, job, Err(Error::new(STARTING)));
            return;
        }
        let ticket = self.backend.run(job.command.clone());
        self.jobs.insert(ticket, job);
    }

    /// The running pairing's prompt and its candidate; a lost adapter cannot
    /// answer, so the view stays usable.
    pub(super) fn auth(&self) -> Option<(String, Prompt)> {
        let st = self.state().filter(|st| st.available)?;
        let pairing = st.pairing.filter(model::pairing_running)?;
        let prompt = model::prompt(&pairing)?;
        Some((pairing.candidate, prompt))
    }

    fn answer(&mut self, accept: bool) {
        let Some((_, prompt)) = self.auth() else {
            return;
        };
        let mut job = Job::new(if accept {
            Command::Accept(None)
        } else {
            Command::Reject
        });
        job.answer = true;
        if accept && let Prompt::EnterCode(kind) = prompt {
            let value = self.form.value();
            if let Err(text) = commands::check_code(kind, &value) {
                let session = self.session.unwrap_or_default();
                self.finish(session, job, Err(Error::new(text)));
                return;
            }
            job.command = Command::Accept(Some(value));
        }
        self.execute_job(job);
    }

    /// Follows the active pairing prompt: a new prompt closes menus and
    /// dialogs, clears the code field and focuses it for entry.
    pub(super) fn sync_auth(&mut self) {
        let a = self.auth();
        let key = a
            .as_ref()
            .map_or_else(String::new, |(c, p)| format!("{c}/{p:?}"));
        if key == self.auth_key {
            return;
        }
        self.auth_key = key;
        self.armed = None;
        self.form_err.clear();
        self.form.reset();
        self.form.limit = 16;
        self.form_focused = false;
        if let Some((_, prompt)) = a {
            self.dialog = None;
            self.menu = None;
            self.dialog_scroll = 0;
            self.form_focused = matches!(prompt, Prompt::EnterCode(_));
        }
    }

    /// Applies one message. `done` is set once the TUI should exit.
    pub(crate) fn update(&mut self, msg: Msg) {
        match msg {
            Msg::Interrupt => self.quit(),
            Msg::Resize(w, h) => {
                self.width = usize::from(w).max(1);
                self.height = usize::from(h).max(1);
                self.armed = None;
                self.unhover();
            }
            Msg::Ports(result) => self.ports_listed(result),
            Msg::Controller(e) => self.controller(*e),
            Msg::Mouse(m) => self.mouse(m),
            Msg::Key(k) => self.key(k),
            Msg::Paste(text) => {
                if self.files.editing {
                    self.files.dest_err.clear();
                    self.files.dest.insert(&text);
                } else if self.form_focused {
                    self.form_err.clear();
                    self.form.insert(&text);
                }
            }
        }
        self.sync_settings();
    }

    /// A timer tick: spinners advance, and an adapter that keeps starting is
    /// shown as not ready.
    pub(crate) fn tick(&mut self) {
        if self.preparing
            && self
                .waiting_since
                .is_some_and(|t| t.elapsed() >= STARTUP_GRACE)
        {
            self.preparing = false;
            self.unready = Some(STARTING.into());
        }
        self.sync_auth();
        self.sync_settings();
    }

    fn ports_listed(&mut self, result: Result<Vec<PortInfo>, String>) {
        self.listed = true;
        match result {
            Ok(ports) => {
                self.ports = ports;
                self.list_err = None;
            }
            Err(e) => {
                self.ports.clear();
                self.note(
                    activity::Kind::Warn,
                    format!("Couldn't list adapters: {}", text::display(&e)),
                );
                self.list_err = Some(e);
            }
        }
        // Only the startup list opens an adapter by itself, and only when it
        // finds exactly one. Opening or listing again first ends that chance.
        let startup = std::mem::take(&mut self.startup);
        if startup && self.list_err.is_none() && self.ports.len() == 1 {
            let port = self.ports[0].port.clone();
            self.open(port);
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
                if Some(session) != self.session {
                    return;
                }
                self.notice_activity(&notice);
                self.sync_auth();
            }
            Event::Done {
                session,
                ticket,
                result,
            } => {
                if Some(session) == self.session && self.files_done(ticket, &result) {
                    return;
                }
                if let Some(job) = self.jobs.remove(&ticket) {
                    self.finish(session, job, result);
                }
            }
        }
    }

    fn connection(&mut self, session: SessionId, phase: Phase) {
        let couldnt = |m: &Self, e: &Error| {
            format!(
                "Couldn't open {}: {}",
                text::display(&m.port),
                text::error_words(e)
            )
        };
        match phase {
            Phase::Opened if self.opening == Some(session) => {
                self.opening = None;
                if self.quitting {
                    return; // Closing already covers this session.
                }
                self.session = Some(session);
                self.chooser = false;
                self.last_err = None;
                self.dialog_scroll = 0;
                self.preparing = true;
                self.reset_activity();
                let text = format!("Connected to {}", text::display(&self.port));
                self.note(activity::Kind::Good, text);
            }
            Phase::Waiting if self.session == Some(session) => {
                self.waiting_since.get_or_insert_with(Instant::now);
            }
            Phase::Ready if self.session == Some(session) && !self.quitting => {
                self.preparing = false;
                self.unready = None;
                self.waiting_since = None;
                self.remember();
            }
            Phase::Failed { error, open: true } if self.session == Some(session) => {
                // Open but not ready, as when its storage failed: files, status
                // and bootloader still work.
                self.preparing = false;
                let words = text::error_words(&error);
                self.note(
                    activity::Kind::Bad,
                    format!("The adapter isn't ready: {words}"),
                );
                self.unready = Some(words);
            }
            Phase::Failed { error, .. }
                if self.opening == Some(session) || self.session == Some(session) =>
            {
                // The TUI shows only a prepared adapter, so a failure returns
                // to the chooser without trying again.
                self.opening = None;
                self.session = None;
                self.preparing = false;
                if self.quitting {
                    return;
                }
                let text = couldnt(self, &error);
                self.note(activity::Kind::Bad, text);
                self.last_err = Some(text::error_words(&error));
                self.chooser = true;
            }
            Phase::Lost(error) if self.session == Some(session) && !self.quitting => {
                if self.preparing {
                    // Lost before it was ready: as a failed open.
                    return self.connection(session, Phase::Failed { error, open: false });
                }
                let text = format!("Lost the adapter connection: {}", text::error_words(&error));
                self.note(activity::Kind::Bad, text);
                self.files.reset();
            }
            _ => {}
        }
    }

    /// Records a command's result.
    fn finish(&mut self, session: SessionId, job: Job, result: Result<Outcome, Error>) {
        if self.session != Some(session) {
            return;
        }
        if job.load {
            if let Command::Settings(id) = &job.command {
                self.page.loading.remove(id);
                if *id == self.page.device {
                    self.page.load_err = result.err();
                }
            }
            return;
        }
        if let Err(e) = &result {
            if job.answer && self.auth().is_some() {
                self.form_err = text::error_words(e);
            }
            if matches!(job.command, Command::Platform(_)) && self.dialog == Some(Dialog::Settings)
            {
                // Shown beside the options, as well as in the activity.
                self.form_err = text::error_words(e);
            }
        }
        if matches!(job.command, Command::Name(_)) && self.dialog == Some(Dialog::Rename) {
            if let Err(e) = &result {
                self.form_err = text::error_words(e);
                self.form_focused = true;
            } else {
                self.dialog = None;
                self.form_focused = false;
            }
        }
        self.settings_result(&job, &result);
        self.result_activity(&job, &result);
        self.sync_auth();
    }

    fn mouse(&mut self, m: MouseEvent) {
        self.px = usize::from(m.column);
        self.py = usize::from(m.row);
        match m.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                self.armed = self.target(self.px, self.py);
                self.focus = None;
                if self.armed.is_none() && self.menu.is_some() {
                    self.menu = None; // Clicking outside an open menu dismisses it.
                }
            }
            MouseEventKind::Up(MouseButton::Left) => {
                // Only a release on the pressed control activates it.
                let target = self.target(self.px, self.py);
                let armed = self.armed.take();
                if let Some(target) = target
                    && Some(&target) == armed.as_ref()
                {
                    self.action(target);
                }
            }
            MouseEventKind::Moved | MouseEventKind::Drag(_) => {
                self.hover = true;
                self.hovered();
            }
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                let up = m.kind == MouseEventKind::ScrollUp;
                let Some(area) = self.area(self.px, self.py) else {
                    return;
                };
                let scroll = self.scroll(area);
                // Events count from the newest line.
                *scroll = if up != (area == Area::Events) {
                    scroll.saturating_sub(1)
                } else {
                    *scroll + 1
                };
            }
            _ => {}
        }
    }

    fn scroll(&mut self, area: Area) -> &mut usize {
        match area {
            Area::Devices => &mut self.device_scroll,
            Area::Details => &mut self.detail_scroll,
            Area::Events => &mut self.event_scroll,
            Area::Dialog => &mut self.dialog_scroll,
            Area::SetList => &mut self.page.list_scroll,
            Area::Editor => &mut self.page.editor_scroll,
            Area::Files => &mut self.files.list_scroll,
            Area::Transfer => &mut self.files.panel_scroll,
        }
    }

    fn key(&mut self, k: KeyEvent) {
        let key = keys::name(&k);
        self.news_seen = self.news_shown; // A key dismisses only a failure already drawn.
        self.hover = false; // Keys take the highlight from the pointer,
        self.armed = None; // and a held button's release does nothing.
        if key == "ctrl+c" || key == "ctrl+d" {
            // From any page, menu, dialog, prompt or field.
            self.quit();
            return;
        }
        if self.full_key(&key) {
            return;
        }
        if self.files.editing {
            self.files.dest_err.clear();
            self.files.dest.key(&k);
        } else if self.form_focused {
            self.form_err.clear();
            self.form.key(&k);
        }
    }

    /// The control under a cell; scroll regions only receive wheels.
    fn target(&self, x: usize, y: usize) -> Option<Action> {
        self.hits
            .iter()
            .find(|h| {
                y == h.y && x >= h.x && x < h.x + h.w && !matches!(h.action, Action::Wheel(_))
            })
            .map(|h| h.action.clone())
    }
    fn area(&self, x: usize, y: usize) -> Option<Area> {
        self.hits.iter().find_map(|h| match h.action {
            Action::Wheel(area) if y == h.y && x >= h.x && x < h.x + h.w => Some(area),
            _ => None,
        })
    }

    pub(super) fn find<'a>(
        st: &'a State,
        id: &str,
    ) -> (Option<&'a p::Device>, Option<&'a p::Candidate>) {
        if let Some(d) = st.device(id) {
            return (Some(d), None);
        }
        (None, st.candidate(id))
    }

    pub(super) fn action(&mut self, action: Action) {
        // While an adapter opens, only Quit and Help, which acts on none, apply.
        let help = action == Action::Help
            || self.dialog == Some(Dialog::Help)
                && matches!(
                    action,
                    Action::CancelDialog | Action::Scroll(Area::Dialog, _)
                );
        if self.quitting || self.opening.is_some() && !help {
            if action == Action::Quit {
                self.quit();
            }
            return;
        }
        // Every control, key and shortcut passes here: what the adapter
        // doesn't offer does nothing, even from an older frame.
        if let Some(st) = self.state()
            && !self.offers(&st, &action)
        {
            return;
        }
        if let Action::Menu(menu) = action {
            self.menu = if self.menu == Some(menu) {
                None
            } else {
                Some(menu)
            };
            return;
        }
        if self.menu.take().is_some() && action == Action::CancelDialog {
            return;
        }
        let st = self.state();
        match action {
            Action::Quit => self.quit(),
            Action::Reopen => {
                let port = self.port.clone();
                self.open(port);
            }
            Action::Input => {
                if self.dialog == Some(Dialog::Rename) && self.renaming() {
                    return;
                }
                self.focus = None;
                self.form_focused = true;
            }
            Action::Help | Action::AdapterSettings => {
                if self.auth().is_some()
                    || matches!(
                        self.dialog,
                        Some(Dialog::Remove(_) | Dialog::Bootloader | Dialog::Replace(_))
                    )
                {
                    return; // Prompts and confirmations stay in front until answered.
                }
                self.dialog = Some(if action == Action::Help {
                    Dialog::Help
                } else {
                    Dialog::Settings
                });
                self.dialog_scroll = 0;
                self.form_err.clear();
                self.form_focused = false;
            }
            Action::CancelDialog => {
                if let Some((_, prompt)) = self.auth() {
                    if matches!(prompt, Prompt::ShowCode(..)) {
                        self.execute(Command::CancelPairing);
                    } else {
                        self.answer(false);
                    }
                } else if self.dialog.is_some() {
                    self.dialog = None;
                    self.form_focused = false;
                    self.dialog_scroll = 0;
                } else if self.chooser && self.session.is_some() {
                    self.chooser = false;
                } else if !self.page.device.is_empty() {
                    self.close_settings(); // Leaves the settings page; drafts are kept.
                } else if self.files.editing {
                    self.files.editing = false;
                } else if self.files.open {
                    self.close_files();
                }
            }
            Action::Accept => self.answer(true),
            Action::Reject => self.answer(false),
            Action::Confirm => match self.dialog.take() {
                Some(Dialog::Remove(id)) => self.execute(Command::Unpair(id)),
                Some(Dialog::Bootloader) => self.execute(Command::Bootloader),
                Some(Dialog::Replace(target)) => self.confirm_replace(target),
                other => self.dialog = other,
            },
            Action::Adapters => {
                self.startup = false;
                self.chooser = true;
                self.dialog_scroll = 0;
                self.backend.list_ports();
            }
            Action::RefreshPorts => {
                self.startup = false;
                self.backend.list_ports();
            }
            Action::Port(port) => self.open(port),
            Action::Device(id) => {
                self.selected = id;
                self.detail_scroll = 0;
            }
            Action::Scroll(area, up) => {
                // Border arrows move by less than a pane's height so no line
                // is skipped.
                let step = self.steps.get(&area).copied().unwrap_or(1).max(1);
                let scroll = self.scroll(area);
                *scroll = if up != (area == Area::Events) {
                    scroll.saturating_sub(step)
                } else {
                    *scroll + step
                };
            }
            Action::DeviceSettings => {
                if let Some(st) = &st
                    && let (Some(d), _) = Self::find(st, &self.selected)
                {
                    let id = d.id.clone();
                    self.open_settings(id);
                }
            }
            Action::Scan(transport) => {
                self.scan_label = scan_name(transport);
                self.execute(Command::Scan {
                    transports: transport.into_iter().collect(),
                    seconds: 0,
                });
            }
            Action::ScanOff => self.execute(Command::ScanStop),
            Action::Refresh => self.execute(Command::Devices),
            Action::CancelPairing => self.execute(Command::CancelPairing),
            Action::Hidpp(on) => {
                let Some(st) = st else { return };
                let (Some(d), _) = Self::find(&st, &self.selected) else {
                    return;
                };
                // Nothing is sent for the saved value, or while a change is
                // pending or the device's settings work runs.
                if !settings::settings_busy(&st, d, self.saving(&d.id)).is_empty()
                    || model::hidpp_enabled(d) == on
                {
                    return;
                }
                self.execute(Command::Set(d.id.clone(), Toggle::Hidpp, on));
            }
            Action::Rename => {
                let Some(st) = st else { return };
                if !can_set_platform(&st) || self.renaming() {
                    return;
                }
                self.dialog = Some(Dialog::Rename);
                self.dialog_scroll = 0;
                self.form.limit = 64;
                self.form.set_value(&st.status.name);
                self.form_focused = true;
                self.form_err.clear();
            }
            Action::SaveName | Action::ResetName => {
                let Some(st) = st else { return };
                if self.dialog != Some(Dialog::Rename) || !can_set_platform(&st) || self.renaming()
                {
                    return;
                }
                let name = if action == Action::ResetName {
                    None
                } else {
                    let value = self.form.value();
                    if !commands::valid_name(&value) {
                        self.form_err = "Invalid adapter name".into();
                        return;
                    }
                    Some(value)
                };
                self.form_err.clear();
                self.form_focused = false;
                self.execute(Command::Name(name));
            }
            Action::Platform(p) => {
                let Some(st) = st else { return };
                if !can_set_platform(&st)
                    || self.running(|c| matches!(c, Command::Platform(_)))
                    || st.status.platform() == p
                {
                    return;
                }
                self.form_err.clear();
                self.execute(Command::Platform(p));
            }
            Action::Pair
            | Action::Connect
            | Action::Disconnect
            | Action::RefreshInfo
            | Action::Enable
            | Action::Disable
            | Action::Trust
            | Action::Untrust
            | Action::Block
            | Action::Unblock
            | Action::Remove
            | Action::Hide => self.device_command(action, st.as_ref()),
            Action::ShowUnnamed(on) => {
                self.show_unnamed = on;
                self.drop_hidden_selection();
                // The option keeps the highlight across the selection change.
                self.focus_ctx = self.focus_context();
                self.reveal = true;
            }
            Action::Bootloader => {
                self.dialog = Some(Dialog::Bootloader);
                self.dialog_scroll = 0;
            }
            other => {
                if !self.files_action(&other) {
                    self.settings_action(other);
                }
            }
        }
    }

    fn device_command(&mut self, action: Action, st: Option<&State>) {
        if self.selected.is_empty() {
            self.note(activity::Kind::Info, "Select a device first.".into());
            return;
        }
        let id = self.selected.clone();
        let command = match action {
            // Only a listed Nearby candidate pairs; offers() already refused
            // anything else, and a saved device is never paired from Saved.
            Action::Pair if !matches!(st.map(|st| Self::find(st, &id)), Some((None, Some(_)))) => {
                return;
            }
            // Shortcuts use the drawn actions; these also cover a click on
            // an older frame's button.
            Action::Connect
                if let Some(why) = st
                    .and_then(|st| Self::find(st, &id).0)
                    .and_then(connect_blocked) =>
            {
                let text = format!("{} can't connect now: {why}.", self.label(&id));
                self.note(activity::Kind::Info, text);
                return;
            }
            Action::Pair if let Some(why) = st.and_then(pair_blocked) => {
                let text = format!("Can't pair {} now: {why}.", self.label(&id));
                self.note(activity::Kind::Info, text);
                return;
            }
            Action::Pair => {
                let text = format!("Pairing {}…", self.label(&id));
                self.note(activity::Kind::Info, text);
                Command::Pair(id)
            }
            Action::Connect => {
                let text = format!("Connecting {}…", self.label(&id));
                self.note(activity::Kind::Info, text);
                Command::Connect(id)
            }
            Action::Disconnect => Command::Disconnect(id),
            Action::RefreshInfo => {
                // One refresh at a time per device.
                if st.is_some_and(|st| pending_for(st, "device refresh", &id)) {
                    return;
                }
                Command::Refresh(id)
            }
            Action::Enable => Command::Set(id, Toggle::Enabled, true),
            Action::Disable => Command::Set(id, Toggle::Enabled, false),
            Action::Trust => Command::Set(id, Toggle::Trusted, true),
            Action::Untrust => Command::Set(id, Toggle::Trusted, false),
            Action::Block => Command::Set(id, Toggle::Blocked, true),
            Action::Unblock => Command::Set(id, Toggle::Blocked, false),
            Action::Remove => {
                self.dialog = Some(Dialog::Remove(id));
                self.dialog_scroll = 0;
                return;
            }
            _ => {
                // Hiding is local and never touches a saved device.
                if let Some((None, Some(_))) = st.map(|st| Self::find(st, &id)) {
                    let text = format!("Hid {}", self.label(&id));
                    self.backend.hide_candidate(&id);
                    self.note(activity::Kind::Info, text);
                }
                return;
            }
        };
        self.execute(command);
    }

    /// Whether the adapter offers what an action sends. Actions that send
    /// nothing, such as Hide or opening a dialog, are always offered;
    /// readiness is checked separately where the control is drawn.
    pub(super) fn offers(&self, st: &State, action: &Action) -> bool {
        // Pair starts only from a Nearby candidate, never from Saved, even by
        // a key or an older frame's button.
        if *action == Action::Pair && !matches!(Self::find(st, &self.selected), (None, Some(_))) {
            return false;
        }
        let command = match action {
            Action::Menu(Menu::Scan) => return scan_choices(st).len() > 1,
            Action::Scan(t) => return scan_choices(st).contains(t),
            Action::ScanOff => return !scan_choices(st).is_empty(),
            Action::Bootloader => Command::Bootloader,
            Action::FilesOpen
            | Action::FilesUp
            | Action::FilesRefresh
            | Action::FilesEntry(_)
            | Action::FilesDownload
            | Action::FilesRetry => Command::Files("/".into()),
            _ => return true,
        };
        commands::unsupported(st, &command).is_none()
    }
}

/// The scans offered: both transports and each alone when the adapter
/// supports both, else its one transport, or none.
pub(super) fn scan_choices(st: &State) -> Vec<Option<Transport>> {
    match model::transports(&st.status)[..] {
        [] => Vec::new(),
        [only] => vec![Some(only)],
        _ => vec![None, Some(Transport::Ble), Some(Transport::Classic)],
    }
}

pub(super) fn scan_name(t: Option<Transport>) -> &'static str {
    match t {
        None => "BLE + Classic",
        Some(Transport::Ble) => "BLE",
        Some(_) => "Classic",
    }
}

/// Whether this session runs a command, for a target or any; a lost
/// session's commands never complete.
pub(super) fn pending_for(st: &State, command: &str, target: &str) -> bool {
    st.available && st.pending_for(command, target)
}

/// Why a saved device can't connect now, from its record.
pub(super) fn connect_blocked(d: &p::Device) -> Option<String> {
    if d.blocked {
        return Some(text::inactive_words(p::InactiveReason::Blocked).into());
    }
    if !d.enabled {
        return Some(text::inactive_words(p::InactiveReason::Disabled).into());
    }
    model::inactive(d).map(|r| text::inactive_words(r).into())
}

/// Why no pairing can start now: storage is full or another pairing runs.
pub(super) fn pair_blocked(st: &State) -> Option<String> {
    if model::storage_full(&st.status) {
        return Some("storage is full; remove unused devices or saved settings".into());
    }
    if st.pairing.as_ref().is_some_and(model::pairing_running) {
        return Some("another pairing is in progress".into());
    }
    None
}

/// Whether the adapter name and platform can be changed now. Until the adapter is ready,
/// the reported platform is only the firmware default.
pub(super) fn can_set_platform(st: &State) -> bool {
    st.available && st.status.ready
}

pub(super) fn connected(d: &p::Device) -> bool {
    model::connected(d)
}

/// The code kind a prompt asks to type, for its wording.
pub(super) fn code_kind(prompt: &Prompt) -> CodeKind {
    match prompt {
        Prompt::EnterCode(k) | Prompt::ShowCode(k, _) => *k,
        Prompt::ConfirmCode(_) => CodeKind::Passkey,
    }
}

pub(crate) fn tick_interval(animating: bool) -> Duration {
    if animating {
        Duration::from_millis(100)
    } else {
        Duration::from_secs(1)
    }
}
