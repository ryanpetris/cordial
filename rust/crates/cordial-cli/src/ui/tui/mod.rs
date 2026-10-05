//! The full-screen TUI. Every action is reachable with the mouse; keys are
//! alternatives, and only text entry needs typing. The pointer and Tab share
//! one highlight: hovering a control highlights it, leaving controls clears
//! it, and Tab continues from it. Hovering never activates a control.
mod activity;
mod files;
mod keys;
pub(crate) mod layout;
mod profiles;
mod settings;
mod staging;
#[cfg(test)]
mod tests;
mod view;

use crate::{
    commands::{self, STARTING},
    controller::{
        AdapterUpdate, Command, DeviceUpdate, Event, Outcome, Phase, SessionId, State, Target,
        Ticket,
    },
    error::Error,
    model::{self, Prompt},
    ui::{
        Backend, Msg,
        field::Field,
        reconnect::{self, Reconnect, Step},
        text,
    },
    view::Item,
};
use activity::LogEntry;
use cordial_client::serial::PortInfo;
use cordial_protocol::{
    self as p, CodeKind, ConfigurationInterface, Platform, Transport, value::Value,
};
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
    Adapters,
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
}

/// What a control does. Hits carry actions; the highlight names one.
#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    Quit,
    Help,
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
    /// Ends the session with the adapter and shows the adapters to choose from.
    AdapterDisconnect,
    RefreshPorts,
    Port(String),
    /// Selects a row of the device list.
    Select(Item),
    DeviceSettings,
    /// Opens the selected device's Diagnostics.
    Diagnostics,
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
    /// Enables or disables a transport.
    Transport(Transport, bool),
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
    /// Shows the adapter's Profiles view in place of its page.
    Profiles,
    /// Shows the selected device's Profiles view in place of its details.
    DeviceProfiles,
    /// Returns from a Profiles view.
    ProfilesBack,
    /// Shows the next page of profiles when true, else the previous one.
    ProfilePage(bool),
    /// Reads the shown page of profiles again after a failed read.
    ProfilesRetry,
    /// Stages turning a configuration interface on or off.
    InterfaceEnabled(ConfigurationInterface, bool),
    /// Opens the chooser of a configuration interface's profile.
    InterfaceChoose(ConfigurationInterface),
    /// Chooses a profile in the profile chooser; 0 chooses none.
    PickProfile(u32),
    ProfileNew,
    ProfileCopy(u32),
    ProfileDelete(u32),
    /// Creates or copies the profile named in the form.
    SaveProfileName,
    /// Opens the chooser of a profile to add to the selected device's layers.
    LayerAdd,
    /// Moves one of the selected device's layers earlier.
    LayerUp(usize),
    /// Moves one of the selected device's layers later.
    LayerDown(usize),
    LayerRemove(usize),
    /// Saves the adapter's staged changes.
    AdapterSave,
    /// Drops the adapter's staged changes.
    AdapterDiscard,
    /// Saves the selected device's staged details, without its layers.
    DeviceSave,
    /// Drops the selected device's staged details, without its layers.
    DeviceDiscard,
    /// Saves the selected device's staged layers.
    LayersSave,
    /// Drops the selected device's staged layers.
    LayersDiscard,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Dialog {
    Help,
    /// The selected device's warnings, HID++ details, security and identifiers.
    Diagnostics,
    Rename,
    Remove(u32),
    Bootloader,
    /// Replacing an existing local file with a download.
    Replace(files::Target),
    /// Naming a new profile, or a copy of this one.
    ProfileName(Option<u32>),
    ProfileDelete(u32),
    /// Confirming an adapter save that reconnects USB.
    SaveAdapter,
    /// Choosing a profile from one page of profiles.
    ProfilePick(PickFor),
}

/// What the profile chooser picks a profile for.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PickFor {
    /// A configuration interface's profile.
    Interface(ConfigurationInterface),
    /// A profile added to the end of the selected device's layers.
    Layer,
}

impl Dialog {
    /// A confirmation, which stays in front until answered.
    fn confirmation(&self) -> bool {
        matches!(
            self,
            Self::Remove(_)
                | Self::Bootloader
                | Self::Replace(_)
                | Self::ProfileDelete(_)
                | Self::SaveAdapter
        )
    }
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
    /// Reads profiles for the profile views, or a profile's name; reported only there.
    lookup: bool,
    /// Reads a page for the profile chooser rather than the adapter's Profiles view.
    picker: bool,
}
impl Job {
    fn new(command: Command) -> Self {
        Self {
            command,
            answer: false,
            save: None,
            load: false,
            lookup: false,
            picker: false,
        }
    }
}

/// An adapter save whose reply was cut short by the connection dropping, as the USB reconnect it
/// causes does. Once the adapter returns, its status shows whether the save was stored.
#[derive(Clone, Debug)]
struct Unsettled {
    ticket: Ticket,
    adapter: String,
    sent: AdapterUpdate,
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
    /// Why the open dialog's or pairing prompt's field was refused; cleared when it closes.
    form_err: String,
    /// The last failed adapter save, by adapter ID, shown on that adapter's page and Profiles
    /// view until its staged changes are changed, saved or discarded.
    adapter_err: Option<(String, String)>,
    /// Why a profile wasn't deleted, shown in the adapter's Profiles view until it closes.
    profile_err: String,
    /// Staged adapter changes, by adapter ID.
    adapter_drafts: HashMap<String, AdapterUpdate>,
    /// Staged device changes, by adapter and device ID.
    device_drafts: HashMap<String, DeviceUpdate>,
    /// The last failed save of a device's details, by adapter and device ID, and why.
    details_err: Option<(String, String)>,
    /// The last failed save of a device's layers, by adapter and device ID, and why.
    layers_err: Option<(String, String)>,
    selected: Option<Item>,
    /// The selected item whose Profiles view replaces its page.
    profiles_open: Option<Item>,
    /// The session was ended by Disconnect, so no Closed event follows a quit.
    closed: bool,
    logs: Vec<LogEntry>,
    /// Last reported state of saved devices, to describe changes.
    known: HashMap<u32, p::Device>,
    /// Candidate names for candidates no longer listed.
    names: HashMap<u32, String>,
    /// Candidates the activity reported this scan, and whether by name.
    found: HashMap<u32, bool>,
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
    adapter_scroll: usize,
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
    profile_page: profiles::ProfilePage,
    /// The profile chooser's own page.
    picker_page: profiles::ProfilePage,
    /// A save that reconnects USB, and then the wait for the adapter to return.
    reconnect: Option<Reconnect>,
    /// The selection and Profiles view to show again once the reconnected adapter is ready.
    resume: Option<(Option<Item>, Option<Item>)>,
    /// A save whose outcome is settled once the adapter returns.
    unsettled: Option<Unsettled>,
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
            adapter_err: None,
            profile_err: String::new(),
            adapter_drafts: HashMap::new(),
            device_drafts: HashMap::new(),
            details_err: None,
            layers_err: None,
            selected: None,
            profiles_open: None,
            closed: false,
            logs: Vec::new(),
            known: HashMap::new(),
            names: HashMap::new(),
            found: HashMap::new(),
            show_unnamed: false,
            event_width: 0,
            steps: HashMap::new(),
            news_seen: None,
            news_shown: None,
            news_shown_at: None,
            device_scroll: 0,
            adapter_scroll: 0,
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
            profile_page: profiles::ProfilePage::default(),
            picker_page: profiles::ProfilePage::default(),
            reconnect: None,
            resume: None,
            unsettled: None,
        };
        m.backend.list_ports();
        if !m.port.is_empty() {
            let port = m.port.clone();
            m.open(port);
        }
        m
    }

    /// Whether the device the Diagnostics dialog shows is connected, so it can be refreshed.
    pub(super) fn diagnosed_connected(&self) -> bool {
        self.state().is_some_and(
            |st| matches!(Self::find(&st, self.selected), (Some(d), _) if connected(d)),
        )
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
                .map(|p| p.candidate);
            st.candidates.retain(|c| {
                text::named(c) || !model::known_kinds(&c.kinds).is_empty() || pairing == Some(c.id)
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
        let Some(Item::Candidate(id)) = self.selected else {
            return;
        };
        if all.candidate(id).is_some() && st.candidate(id).is_none() {
            self.selected = None;
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
        if self.opening.is_some() || self.preparing || self.reconnecting() {
            Some("connecting")
        } else if self.session.is_none() && !self.listed {
            Some("loading")
        } else if self.chooser || self.session.is_none() {
            Some("chooser")
        } else {
            None
        }
    }

    /// Whether the adapter is expected back after a USB reconnect.
    pub(super) fn reconnecting(&self) -> bool {
        self.reconnect.as_ref().is_some_and(Reconnect::waiting)
    }

    /// Opens a port the user chose.
    fn open(&mut self, port: String) {
        if self.opening.is_some() || self.quitting {
            return;
        }
        self.reconnect = None;
        self.resume = None;
        // A save cut short on this same adapter is settled once it is ready again; one on
        // another adapter can't be told from here.
        let same = self.unsettled.as_ref().is_some_and(|u| {
            self.ports
                .iter()
                .any(|p| p.port == port && p.is_adapter(&u.adapter))
        });
        if !same {
            self.unsettled = None;
        }
        self.open_port(port);
    }

    fn open_port(&mut self, port: String) {
        if self.opening.is_some() || self.quitting {
            return;
        }
        self.session = None;
        self.startup = false; // Startup's one automatic open is spent.
        self.preparing = false;
        self.unready = None;
        self.waiting_since = None;
        self.port = port.clone();
        self.closed = false;
        self.selected = None;
        self.profiles_open = None;
        self.auth_key.clear();
        if self.dialog != Some(Dialog::Help) {
            // Help concerns no adapter, so it stays open.
            self.dialog = None;
            self.dialog_scroll = 0;
        }
        self.menu = None;
        self.page.reset();
        self.files.reset();
        self.profile_page = profiles::ProfilePage::default();
        self.picker_page = profiles::ProfilePage::default();
        self.form_focused = false; // Keys return to the view once the adapter opens.
        self.clear_errors();
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
        if self.closed {
            // Disconnect already closed the session.
            self.done = true;
            return;
        }
        self.backend.close();
    }

    /// Ends the session with the adapter and shows the adapters to choose from.
    fn disconnect(&mut self) {
        if self.session.is_none() || self.opening.is_some() {
            return;
        }
        self.backend.close();
        self.closed = true;
        self.session = None;
        self.preparing = false;
        self.unready = None;
        self.waiting_since = None;
        self.reconnect = None;
        self.resume = None;
        self.unsettled = None;
        self.selected = None;
        self.profiles_open = None;
        self.jobs.clear();
        if self.dialog != Some(Dialog::Help) {
            self.dialog = None;
            self.dialog_scroll = 0;
        }
        self.menu = None;
        self.page.reset();
        self.files.reset();
        self.profile_page = profiles::ProfilePage::default();
        self.picker_page = profiles::ProfilePage::default();
        self.form_focused = false;
        self.clear_errors();
        self.last_err = None;
        self.chooser = true;
        self.backend.list_ports();
    }

    /// Drops the results shown beside actions, which belong to the session that produced them.
    fn clear_errors(&mut self) {
        self.form_err.clear();
        self.adapter_err = None;
        self.profile_err.clear();
        self.details_err = None;
        self.layers_err = None;
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
        if reconnect::may_reconnect(&job.command)
            && let (Some(session), Some(st)) = (self.session, self.full_state())
        {
            self.reconnect = Some(Reconnect::new(session, ticket, &st.status));
        }
        self.jobs.insert(ticket, job);
    }

    /// The running pairing's prompt and its candidate; a lost adapter cannot
    /// answer, so the view stays usable.
    pub(super) fn auth(&self) -> Option<(u32, Prompt)> {
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
        self.sync_profiles();
        self.sync_reconnect();
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
            self.settle();
        }
        self.sync_auth();
        self.sync_settings();
        self.sync_profiles();
        self.sync_reconnect();
    }

    /// Lists the ports while waiting for the adapter to return after a USB reconnect, and
    /// reports its loss if it doesn't return in time.
    fn sync_reconnect(&mut self) {
        let Some(r) = &mut self.reconnect else { return };
        if r.lapsed() {
            // The connection stayed: the adapter's status shows what a cut-short save did.
            self.reconnect = None;
            self.settle();
            return;
        }
        // An open in progress finishes first.
        if self.quitting || self.opening.is_some() || self.preparing {
            return;
        }
        match r.step() {
            Step::Wait => {}
            Step::List => self.backend.list_ports(),
            Step::GiveUp(error) => {
                self.reconnect = None;
                let text = format!("Lost the adapter connection: {}", text::error_words(&error));
                self.note(activity::Kind::Bad, text);
                self.settle();
                if self.session.is_none() {
                    // A reopen replaced the lost session.
                    self.resume = None;
                    self.chooser = true;
                } else {
                    self.resume(false);
                }
            }
        }
    }

    /// Shows the selection and Profiles view from before a USB reconnect again; `read` reads
    /// the adapter's Profiles page again for a reopened session.
    fn resume(&mut self, read: bool) {
        let Some((selected, profiles)) = self.resume.take() else {
            return;
        };
        if self.selected.is_none() {
            self.selected = selected;
            self.profiles_open = profiles;
            if read && profiles == Some(Item::Adapter) {
                self.open_page(false);
            }
        }
    }

    fn ports_listed(&mut self, result: Result<Vec<PortInfo>, String>) {
        self.listed = true;
        if let Some(r) = self.reconnect.as_mut().filter(|r| r.waiting()) {
            // A failed listing is tried again.
            let found = r.listed(result.as_deref().unwrap_or_default());
            if let Ok(ports) = result {
                self.ports = ports;
                self.list_err = None;
            }
            if let Some(port) = found {
                self.open_port(port);
            }
            return;
        }
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
                self.profile_notice(&notice);
                self.sync_auth();
            }
            Event::Done {
                session,
                ticket,
                result,
            } => {
                if self.save_cut_short(session, ticket, &result) {
                    return;
                }
                if self.unsettled.as_ref().is_some_and(|u| u.ticket == ticket)
                    && Some(session) == self.session
                {
                    // The reply arrived after all, and is reported as usual.
                    self.unsettled = None;
                }
                let available = self.full_state().is_some_and(|st| st.available);
                if let Some(r) = &mut self.reconnect
                    && !r.done(ticket, &result, available)
                {
                    self.reconnect = None;
                }
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
                self.reconnect = None;
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
                self.resume(true);
                self.settle();
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
                self.settle();
            }
            Phase::Failed { open: false, .. }
                if (self.opening == Some(session) || self.session == Some(session))
                    && self.reconnecting() =>
            {
                // The returning adapter's port may not open at first.
                self.opening = None;
                self.session = None;
                self.preparing = false;
                if let Some(r) = &mut self.reconnect {
                    r.retry();
                }
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
                self.settle();
            }
            Phase::Lost(error)
                if self.session == Some(session)
                    && !self.quitting
                    && !self.preparing
                    && self
                        .reconnect
                        .as_mut()
                        .is_some_and(|r| r.lost(session, &error)) =>
            {
                // The expected loss after a save that reconnects USB: the adapter is reopened
                // once it returns, showing the page the save came from. Help stays open. A save
                // still without its reply is settled from the returning adapter's status.
                if let Some(ticket) = self.reconnect.as_ref().map(Reconnect::ticket) {
                    self.unsettle(ticket);
                }
                if self.dialog != Some(Dialog::Help) {
                    self.dialog = None;
                    self.dialog_scroll = 0;
                }
                self.resume = Some((self.selected, self.profiles_open));
                self.menu = None;
                self.files.reset();
            }
            Phase::Lost(error) if self.session == Some(session) && !self.quitting => {
                if self.preparing {
                    // Lost before it was ready: as a failed open.
                    return self.connection(session, Phase::Failed { error, open: false });
                }
                let text = format!("Lost the adapter connection: {}", text::error_words(&error));
                self.note(activity::Kind::Bad, text);
                self.files.reset();
                self.settle();
            }
            _ => {}
        }
    }

    /// Follows a save whose reply hasn't arrived through the reconnect that may cut it short.
    fn unsettle(&mut self, ticket: Ticket) {
        if self.unsettled.is_some() {
            return;
        }
        let (Some(job), Some(r)) = (self.jobs.get(&ticket), &self.reconnect) else {
            return;
        };
        if let Command::AdapterSave(sent) = &job.command {
            self.unsettled = Some(Unsettled {
                ticket,
                adapter: r.adapter().to_owned(),
                sent: sent.clone(),
            });
        }
    }

    /// Whether a save's failure was the connection cutting its reply short, as the USB reconnect
    /// it causes does, rather than the adapter refusing it. Such a save's outcome is settled
    /// once the adapter returns, so the failure isn't reported.
    fn save_cut_short(
        &mut self,
        session: SessionId,
        ticket: Ticket,
        result: &Result<Outcome, Error>,
    ) -> bool {
        let Err(e) = result else { return false };
        let followed = self
            .reconnect
            .as_ref()
            .is_some_and(|r| r.ticket() == ticket);
        let unsettled = self.unsettled.as_ref().is_some_and(|u| u.ticket == ticket);
        if !followed && !unsettled {
            return false;
        }
        let available = self.full_state().is_some_and(|st| st.available);
        let refused = e.dongle.is_some() && available && Some(session) == self.session;
        if refused && !unsettled && !self.reconnecting() {
            return false;
        }
        self.unsettle(ticket);
        if self.unsettled.is_none() {
            return false;
        }
        self.jobs.remove(&ticket);
        if let Some(r) = &mut self.reconnect {
            r.cut_short();
        }
        true
    }

    /// Reports a cut-short save from the adapter's status: saved when it shows every value sent,
    /// else with an unknown outcome. Settling without a ready adapter, as when it returns but
    /// fails to start, also reports the unknown outcome.
    fn settle(&mut self) {
        let Some(u) = self.unsettled.take() else {
            return;
        };
        self.jobs.remove(&u.ticket);
        // Until it is ready, an adapter reports only its firmware defaults.
        let st = self
            .full_state()
            .filter(|st| st.available && st.status.ready && st.status.id == u.adapter);
        match st {
            Some(st) if staging::adapter_pending(&u.sent, &st.status).is_empty() => {
                let job = Job::new(Command::AdapterSave(u.sent.clone()));
                let result = Ok(Outcome::AdapterSaved(st.status));
                self.staging_result(&job, &result);
                self.result_activity(&job, &result);
            }
            _ => {
                if self.state().is_some_and(|st| st.status.id == u.adapter) {
                    self.adapter_err = Some((u.adapter, text::SAVE_UNCONFIRMED.into()));
                }
                self.note(activity::Kind::Bad, text::SAVE_UNCONFIRMED_LOG.into());
            }
        }
    }

    /// Records a command's result.
    fn finish(&mut self, session: SessionId, job: Job, result: Result<Outcome, Error>) {
        if self.session != Some(session) {
            return;
        }
        if job.load {
            if let Command::Settings(Target::Id(id)) = &job.command {
                self.page.loading.remove(id);
                if *id == self.page.device {
                    self.page.load_err = result.err();
                }
            }
            return;
        }
        if job.lookup {
            self.lookup_result(&job, &result);
            return;
        }
        if let Err(e) = &result
            && job.answer
            && self.auth().is_some()
        {
            self.form_err = text::error_words(e);
        }
        if matches!(
            job.command,
            Command::ProfileCreate(..) | Command::ProfileCopy(..)
        ) && matches!(self.dialog, Some(Dialog::ProfileName(_)))
        {
            if let Err(e) = &result {
                self.form_err = text::capitalized(&text::error_words(e));
                self.form_focused = true;
            } else {
                self.dialog = None;
                self.form_focused = false;
            }
        }
        if let (Command::ProfileDelete(_), Err(e)) = (&job.command, &result) {
            self.profile_err = text::capitalized(&text::error_words(e));
        }
        if matches!(job.command, Command::Name(_)) && self.dialog == Some(Dialog::Rename) {
            if let Err(e) = &result {
                self.form_err = text::capitalized(&text::error_words(e));
                self.form_focused = true;
            } else {
                self.dialog = None;
                self.form_focused = false;
            }
        }
        self.settings_result(&job, &result);
        self.staging_result(&job, &result);
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
            Area::Adapters => &mut self.adapter_scroll,
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

    /// The saved device or candidate a row names, while the session holds it.
    pub(super) fn find(
        st: &State,
        item: Option<Item>,
    ) -> (Option<&p::Device>, Option<&p::Candidate>) {
        match item {
            Some(Item::Device(id)) => (st.device(id), None),
            Some(Item::Candidate(id)) => (None, st.candidate(id)),
            Some(Item::Adapter) | None => (None, None),
        }
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
            Action::Help | Action::Diagnostics => {
                if self.auth().is_some() || self.dialog.as_ref().is_some_and(Dialog::confirmation) {
                    return; // Prompts and confirmations stay in front until answered.
                }
                self.dialog = Some(if action == Action::Help {
                    Dialog::Help
                } else {
                    Dialog::Diagnostics
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
                    self.form_err.clear();
                    self.dialog_scroll = 0;
                } else if self.chooser && self.session.is_some() {
                    self.chooser = false;
                } else if self.page.device != 0 {
                    self.close_settings(); // Leaves the settings page; drafts are kept.
                } else if self.files.editing {
                    self.files.editing = false;
                } else if self.files.open {
                    self.close_files();
                } else if self.profiles_open.is_some() {
                    self.close_profiles(); // Drafts are kept.
                }
            }
            Action::Accept => self.answer(true),
            Action::Reject => self.answer(false),
            Action::Confirm => match self.dialog.take() {
                Some(Dialog::Remove(id)) => self.execute(Command::Unpair(Target::Id(id))),
                Some(Dialog::Bootloader) => self.execute(Command::Bootloader),
                Some(Dialog::Replace(target)) => self.confirm_replace(target),
                Some(other) if self.profiles_confirm(&other) => {}
                Some(other) if self.staging_confirm(&other) => {}
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
            Action::AdapterDisconnect => self.disconnect(),
            Action::Select(item) => self.select(item),
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
                    && let (Some(d), _) = Self::find(st, self.selected)
                {
                    let id = d.id;
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
                    if commands::adapter_name(&value).is_none() {
                        self.form_err = "Enter an adapter name of up to 64 bytes.".into();
                        return;
                    }
                    Some(value)
                };
                self.form_err.clear();
                self.form_focused = false;
                self.execute(Command::Name(name));
            }
            Action::Pair
            | Action::Connect
            | Action::Disconnect
            | Action::RefreshInfo
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
                if !self.files_action(&other)
                    && !self.profiles_action(&other)
                    && !self.staging_action(&other)
                {
                    self.settings_action(other);
                }
            }
        }
    }

    fn device_command(&mut self, action: Action, st: Option<&State>) {
        let Some(item) = self.selected else {
            self.note(activity::Kind::Info, "Select a device first.".into());
            return;
        };
        let found = st.map_or((None, None), |st| Self::find(st, Some(item)));
        let command = match (action, found) {
            // Shortcuts use the drawn actions; these also cover a click on an older frame's
            // button.
            (Action::Connect, (Some(d), _)) => {
                if let Some(why) = connect_blocked(d) {
                    let text = format!("{} can't connect now: {why}.", self.label(item));
                    self.note(activity::Kind::Info, text);
                    return;
                }
                let text = format!("Connecting {}…", self.label(item));
                self.note(activity::Kind::Info, text);
                Command::Connect(Target::Id(d.id))
            }
            (Action::Disconnect, (Some(d), _)) => Command::Disconnect(Target::Id(d.id)),
            (Action::RefreshInfo, (Some(d), _)) => {
                // One refresh at a time per device.
                if st.is_some_and(|st| pending_for(st, "device refresh", d.id)) {
                    return;
                }
                Command::Refresh(Target::Id(d.id))
            }
            (Action::Remove, (Some(d), _)) => {
                self.dialog = Some(Dialog::Remove(d.id));
                self.dialog_scroll = 0;
                return;
            }
            // Only a listed candidate pairs; a saved device is never paired from Saved.
            (Action::Pair, (_, Some(c))) => {
                if let Some(why) = st.and_then(|st| pair_blocked(st, c)) {
                    let text = format!("Can't pair {} now: {why}.", self.label(item));
                    self.note(activity::Kind::Info, text);
                    return;
                }
                let text = format!("Pairing {}…", self.label(item));
                self.note(activity::Kind::Info, text);
                Command::Pair(Target::Id(c.id))
            }
            // Hiding is local and never touches a saved device.
            (Action::Hide, (_, Some(c))) => {
                let text = format!("Hid {}", self.label(item));
                self.backend.hide_candidate(c.id);
                self.note(activity::Kind::Info, text);
                return;
            }
            _ => return,
        };
        self.execute(command);
    }

    /// Whether the adapter offers what an action sends. Actions that send
    /// nothing, such as Hide or opening a dialog, are always offered;
    /// readiness is checked separately where the control is drawn.
    pub(super) fn offers(&self, st: &State, action: &Action) -> bool {
        // Pair starts only from a Nearby candidate, never from Saved, even by
        // a key or an older frame's button.
        if *action == Action::Pair && !matches!(Self::find(st, self.selected), (None, Some(_))) {
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
/// has both enabled, else its one enabled transport, or none.
pub(super) fn scan_choices(st: &State) -> Vec<Option<Transport>> {
    match model::enabled_transports(&st.status)[..] {
        [] => Vec::new(),
        [only] => vec![Some(only)],
        _ => vec![None, Some(Transport::Ble), Some(Transport::Classic)],
    }
}

/// The running scan's name, from the transports it scans that are still enabled; disabling a
/// transport drops it from the scan. Until the scan's end arrives after every one of them is
/// disabled, the name is that of the transports it was started with.
pub(super) fn scanning_name(st: &State) -> &'static str {
    let requested: Vec<Transport> = st.scanning.iter().flatten().copied().collect();
    let enabled: Vec<Transport> = requested
        .iter()
        .copied()
        .filter(|t| model::transport_enabled(&st.status, *t) == Some(true))
        .collect();
    let named = if enabled.is_empty() {
        requested
    } else {
        enabled
    };
    match named[..] {
        [] => "",
        [only] => scan_name(Some(only)),
        _ => scan_name(None),
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
pub(super) fn pending_for(st: &State, command: &str, target: u32) -> bool {
    st.available && st.pending_for(command, target)
}

/// Why a saved device can't connect now, from its record.
pub(super) fn connect_blocked(d: &p::Device) -> Option<String> {
    let transport = d.transport();
    if model::inactive(d) == Some(p::InactiveReason::TransportDisabled) {
        return Some(text::transport_disabled(transport));
    }
    if d.blocked {
        return Some(text::inactive_words(p::InactiveReason::Blocked, transport));
    }
    if !d.enabled {
        return Some(text::inactive_words(p::InactiveReason::Disabled, transport));
    }
    model::inactive(d).map(|r| text::inactive_words(r, transport))
}

/// Why `c` can't start pairing now: storage is full, its transport is disabled, or another
/// pairing runs.
pub(super) fn pair_blocked(st: &State, c: &p::Candidate) -> Option<String> {
    if model::storage_full(&st.status) {
        return Some("storage is full; remove unused devices or saved settings".into());
    }
    if model::transport_disabled(&st.status, c.transport()) {
        return Some(text::transport_disabled(c.transport()));
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
