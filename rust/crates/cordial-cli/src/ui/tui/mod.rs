//! The full-screen TUI, laid out like the desktop application: a sidebar with the Overview, every
//! saved device of every attached adapter and the adapters themselves, and a page for the
//! selection with the same tabs, controls and words as the desktop application's. The TUI opens a
//! session with every attached adapter and follows adapters as they come and go.
//!
//! Every action is reachable with the mouse; keys are alternatives, and only text entry needs
//! typing. The pointer and Tab share one highlight: hovering a control highlights it, and Tab
//! continues from it. Hovering never activates a control.
mod adapter;
mod add;
mod device;
mod files;
mod fleet;
mod keys;
pub(crate) mod layout;
mod profiles;
mod render;
mod settings;
#[cfg(test)]
mod tests;
mod words;
mod world;

pub(crate) use fleet::Live;

use crate::settings::{Field as SettingField, Value};
use crate::{
    controller::{AdapterUpdate, Command, DeviceUpdate, Event, Notice, Outcome, Phase, Ticket},
    error::Error,
    migration,
    ui::{Msg, field::Field, text},
    view::State,
};
use add::AddDevice;
use cordial_client::{
    compatibility::NativeHello,
    migration::{Outcome as Migrated, Result as MigrationResult},
    serial::PortInfo,
};
use cordial_protocol::migration::{MigrationEvent, migration_event};
use cordial_protocol::{self as p, ConfigurationInterface, OutputMode, Platform, Transport};
use fleet::{Fleet, Slot};
use layout::Hit;
use profiles::ProfilePage;
use settings::Draft;
use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    sync::Arc,
    time::{Duration, Instant},
};
use world::{AdapterView, DeviceView, Storage};

pub const MIN_WIDTH: usize = 60;
pub const MIN_HEIGHT: usize = 16;

/// How often the attached adapters are listed.
const LIST_EVERY: Duration = Duration::from_secs(1);
/// How long an adapter that went away keeps its page while it may return, as across the USB
/// reconnect an interface allocation change causes.
const RECONNECT_HOLD: Duration = Duration::from_secs(15);
/// A session that ends this soon after opening counts as a failure of its port.
const UNSTABLE: Duration = Duration::from_secs(10);
/// How long a failure stays in the status line.
const TOAST_FOR: Duration = Duration::from_secs(6);
/// How long a port that failed waits before it is tried again, doubling with each failure in a
/// row up to the most.
const RETRY_FIRST: Duration = Duration::from_secs(2);
const RETRY_MOST: Duration = Duration::from_secs(60);
/// How long a scan from Add Device runs.
const SCAN_SECONDS: u32 = 30;

/// What the content area shows.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum Page {
    Overview,
    /// A saved device, by adapter ID and device ID.
    Device(String, u32),
    /// An adapter, by adapter ID.
    Adapter(String),
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Tab {
    Details,
    Settings,
    Profiles,
    Diagnostics,
}

impl Tab {
    pub fn label(self) -> &'static str {
        match self {
            Tab::Details => "Details",
            Tab::Settings => "Settings",
            Tab::Profiles => "Profiles",
            Tab::Diagnostics => "Diagnostics",
        }
    }
}

/// Regions that scroll with the wheel and keep their own Tab order.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Area {
    Sidebar,
    Main,
    Dialog,
}

/// A device preference on the Details tab.
pub use crate::controller::Toggle;

/// What a control does. Hits carry actions; the highlight names one.
#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    Quit,
    Help,
    /// Lists the attached adapters again, including ports that failed to open.
    RefreshAdapters,
    AddDevice,
    /// Shows a page from the sidebar.
    Open(Page),
    /// Shows a page from a link in the shown page.
    Show(Page),
    ConnectAdapter(String),
    DisconnectAdapter(String),
    /// Migrates an adapter's saved data, or tries a failed migration again.
    Migrate(String),
    CancelMigration,
    RenameAdapter(String),
    Tab(Tab),

    Platform(Platform),
    SwitchPlatform,
    /// Stages a transport's switch.
    Transport(Transport, bool),
    InterfaceEnabled(ConfigurationInterface, bool),
    InterfacePick(ConfigurationInterface),
    /// Shows the next page of profiles when true, else the previous one.
    ProfilePage(bool),
    ProfilesRetry,
    /// Lists the adapter's devices again, as after records couldn't be read.
    DevicesRetry,
    ProfileMenu(u32),
    ProfileNew,
    ProfileCopy(u32),
    ProfileDelete(u32),
    AdapterSave,
    AdapterDiscard,
    Files,
    Restart,
    Bootloader,

    DeviceToggle(Toggle, bool),
    /// Stages the device's gamepad output mode.
    GamepadOutput(OutputMode),
    Connect,
    Disconnect,
    Forget,
    DetailsSave,
    DetailsDiscard,
    LayerUp(usize),
    LayerDown(usize),
    LayerRemove(usize),
    LayerAdd,
    LayersSave,
    LayersDiscard,
    DiagnosticsRefresh,

    SettingBool(SettingField, bool),
    SettingChoice(SettingField, Value),
    /// Opens the choices of a setting with many.
    SettingSelect(SettingField),
    SettingStep(SettingField, i64),
    /// Types a number for an integer setting.
    SettingEdit(SettingField),
    /// Turns automatic free spin on or off.
    FreeSpin(SettingField, bool),
    /// Opens a setting's state menu.
    Marker(SettingField),
    Undo(SettingField),
    /// Stages saving the value the device reports.
    Keep(SettingField),
    ForgetSetting(SettingField),
    SettingsRefresh,
    SettingsReapply,
    SettingsReload,
    SettingsDiscard,
    SettingsSave,

    /// Closes the open dialog or menu.
    Cancel,
    /// The dialog's main action.
    Submit,
    /// Focuses the dialog's text field.
    Field,
    ResetName,
    /// Chooses a profile in the profile picker; 0 chooses none.
    Pick(u32),
    PickPage(bool),
    Confirm,

    /// Opens the Add Device adapter choices.
    AddAdapter,
    ChooseAdapter(String),
    ScanAgain,
    Pair(u32),
    ShowUnnamed(bool),
    CancelPairing,
    Accept,
    Reject,
    AddAnother,
    Done,

    FilesUp,
    FilesRefresh,
    FilesEntry(String),
    FilesDest,
    FilesDownload,
    FilesRetry,
}

/// What the profile picker chooses a profile for.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PickFor {
    Interface(ConfigurationInterface),
    /// A profile added to the end of a device's layers.
    Layer(u32),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Dialog {
    Help,
    Rename(String),
    Forget(String, u32),
    /// Naming a new profile, or a copy of this one.
    ProfileName(String, Option<u32>),
    ProfileDelete(String, u32),
    Pick {
        adapter: String,
        purpose: PickFor,
        chosen: u32,
    },
    AddDevice,
    Bootloader(String),
    Files(String),
    /// Replacing an existing local file with a download.
    Replace(String, files::Target),
}

/// A small list of actions shown beside the control that opened it.
#[derive(Clone, Debug, PartialEq)]
pub struct Menu {
    pub x: usize,
    pub y: usize,
    /// Each item's label, and its action; None when unavailable.
    pub items: Vec<(String, Option<Action>)>,
}

/// Where a failure is shown, next to the action that caused it.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum Spot {
    /// An adapter page's Save on Settings and Profiles.
    AdapterSave(String),
    /// A device's Details bar, for connect and disconnect.
    DeviceBar(String, u32),
    DetailsSave(String, u32),
    LayersSave(String, u32),
    SettingsNote(String, u32),
    Diagnostics(String, u32),
    /// The open dialog.
    Dialog,
}

/// A command the TUI started, kept until its result.
#[derive(Clone, Debug)]
pub(super) struct Job {
    slot: Slot,
    adapter: String,
    command: Command,
    kind: Kind,
}

#[derive(Clone, Debug)]
pub(super) enum Kind {
    /// A read whose failure needs no report.
    Quiet,
    SettingsList(u32),
    /// The profiles for the list, or the picker.
    Page {
        picker: bool,
    },
    Lookup,
    AdapterSave(AdapterUpdate),
    Rename,
    /// A device command whose failure shows at a spot.
    Device(u32, Spot),
    Forget(u32),
    DeviceSave {
        device: u32,
        sent: DeviceUpdate,
        /// The tab whose changes it saves.
        tab: Tab,
    },
    SettingsSave {
        device: u32,
        sent: Vec<(SettingField, Draft)>,
        /// The staged gamepad output is saved once this finishes.
        gamepad: bool,
    },
    /// Creating, copying or deleting a profile, from its dialog.
    Profile,
    Scan,
    Pair,
    Answer,
    CancelPairing,
    Files,
    FileGet,
    Restart,
    Bootloader,
}

/// When a failed port may be tried again.
#[derive(Clone, Copy, Debug)]
pub(super) struct Retry {
    at: Instant,
    count: u32,
}

/// A registered adapter: an opened port whose adapter answered.
pub(super) struct Session {
    slot: Slot,
    port: String,
    id: String,
    opened: Instant,
    /// Opening finished without the adapter becoming usable; why.
    unready: Option<String>,
    list: ProfilePage,
    picker: ProfilePage,
    /// Profiles whose names were read once because nothing had named them.
    looked_up: HashSet<u32>,
    /// The saved devices have been listed in this session.
    listed: bool,
    storage: Storage,
}

/// A migration, which owns every adapter connection while it lasts.
pub(super) struct Migration {
    id: String,
    /// The port the adapter was on, where the worker looks first.
    port: String,
    /// The adapter's status when its session closed.
    status: p::Status,
    stage: Stage,
    /// The worker has started; it exits before the migration ends.
    started: bool,
    waiting: bool,
    progress: Option<(u64, u64)>,
}

#[derive(Clone, Debug, PartialEq)]
enum Stage {
    /// Every session is closing; the worker starts once all have released their ports.
    Closing,
    Running,
    Cancelling,
    /// The worker failed: what to tell the user, and its reason.
    Failed(String, String),
}

impl Migration {
    pub(super) fn storage(&self) -> Storage {
        match &self.stage {
            Stage::Failed(text, reason) => Storage::Failed {
                text: text.clone(),
                reason: reason.clone(),
            },
            stage => Storage::Migrating {
                waiting: self.waiting,
                cancelling: *stage == Stage::Cancelling,
                progress: self.progress,
            },
        }
    }
}

/// An adapter that went away, kept while it may return.
pub(super) struct Held {
    status: p::Status,
    until: Instant,
    list: ProfilePage,
    picker: ProfilePage,
    profiles: BTreeMap<u32, p::Profile>,
}

/// An adapter the user disconnected; its port is not opened until Connect.
pub(super) struct Off {
    /// Present while plugged in.
    port: Option<String>,
    status: p::Status,
    connecting: Option<Slot>,
    /// The port Connect opened.
    connect_port: String,
    /// Connect waits for the previous session to release the port.
    waiting: bool,
    error: Option<String>,
}

/// A port whose adapter refused the connection because Cordial doesn't support its protocol.
pub(super) struct Refused {
    hello: Arc<NativeHello>,
    why: String,
    /// Refresh or Connect asked for the port to be opened again.
    retry: bool,
}

/// One settings save, while it runs and after it fails.
#[derive(Clone, Debug, Default)]
pub(super) struct Submission {
    pub running: bool,
    /// The fields it sent.
    pub fields: Vec<SettingField>,
    /// Why it failed; None while running or after it succeeded.
    pub error: Option<String>,
    /// Its outcome is unknown, so its keys are neither saved nor failed.
    pub unknown: bool,
}

/// The TUI's state; see `update` for how messages change it and `render` for how it is drawn.
pub(crate) struct Model<F: Fleet> {
    fleet: F,
    pub(crate) width: usize,
    pub(crate) height: usize,
    pub(crate) done: bool,
    quitting: bool,

    listing: bool,
    list_due: Instant,
    /// The adapter list has arrived at least once.
    listed: bool,
    ports: Vec<PortInfo>,
    /// Ports that failed to open or whose session ended soon after opening, with when each may
    /// be tried again and how many times in a row it failed. A port leaves when it goes away.
    failed: BTreeMap<String, Retry>,
    /// Opened ports whose adapter hasn't answered yet.
    probes: HashMap<Slot, String>,
    sessions: Vec<Session>,
    held: BTreeMap<String, Held>,
    off: BTreeMap<String, Off>,
    /// Refused ports, listed until they go away. One is opened again only on Refresh or Connect.
    refused: BTreeMap<String, Refused>,
    /// Sessions closing, with the port each holds until it has closed.
    closing: HashMap<Slot, String>,

    /// Each session's view, as of the last refresh.
    states: HashMap<Slot, State>,
    adapters: Vec<AdapterView>,
    devices: Vec<DeviceView>,
    /// The shown device as last listed, kept while its adapter reconnects.
    kept: Option<DeviceView>,

    page: Page,
    /// The page's target has been shown, so its disappearing returns to the Overview.
    page_shown: bool,
    tab: Tab,
    side_scroll: usize,
    main_scroll: usize,
    dialog_scroll: usize,
    /// Scroll the sidebar to the selection.
    reveal: bool,
    pub(super) hits: Vec<Hit>,
    /// Where each scrolling region was drawn: x, y, width and height.
    regions: Vec<(Area, (usize, usize, usize, usize))>,
    /// Where the page's body was drawn.
    body_area: (usize, usize, usize, usize),
    focus: Option<Action>,
    /// What the highlight belonged to; a change drops it.
    focus_ctx: String,
    hover: bool,
    px: usize,
    py: usize,
    armed: Option<Action>,
    /// The next frame highlights the first control of the page's body.
    enter_page: bool,

    adapter_drafts: HashMap<String, AdapterUpdate>,
    device_drafts: HashMap<(String, u32), DeviceUpdate>,
    setting_drafts: HashMap<(String, u32), BTreeMap<SettingField, Draft>>,
    submissions: HashMap<(String, u32), Submission>,
    /// Why each device's settings couldn't be listed.
    settings_errors: HashMap<(String, u32), String>,
    notes: HashMap<Spot, String>,
    /// Each device's last connection state, to drop errors that belonged to an earlier one.
    device_states: HashMap<(String, u32), i32>,
    jobs: HashMap<(Slot, Ticket), Job>,
    toast: Option<(String, Instant)>,
    dialog: Option<Dialog>,
    menu: Option<Menu>,
    form: Field,
    /// The dialog's text field has the keyboard.
    form_focused: bool,
    /// The integer setting being typed, which holds the form.
    editing: Option<SettingField>,
    add: Option<AddDevice>,
    /// Adapters whose scan from a closed Add Device stops once its start is answered.
    scan_stops: HashSet<(Slot, Ticket)>,
    files: files::Files,
    migration: Option<Migration>,
    /// Adapters whose migration the user cancelled; they migrate again only with Migrate.
    declined: HashSet<String>,
}

impl<F: Fleet> Model<F> {
    pub(crate) fn new(fleet: F) -> Self {
        let mut m = Self {
            fleet,
            width: 100,
            height: 30,
            done: false,
            quitting: false,
            listing: false,
            list_due: Instant::now(),
            listed: false,
            ports: Vec::new(),
            failed: BTreeMap::new(),
            probes: HashMap::new(),
            sessions: Vec::new(),
            held: BTreeMap::new(),
            off: BTreeMap::new(),
            refused: BTreeMap::new(),
            closing: HashMap::new(),
            states: HashMap::new(),
            adapters: Vec::new(),
            devices: Vec::new(),
            kept: None,
            page: Page::Overview,
            page_shown: true,
            tab: Tab::Details,
            side_scroll: 0,
            main_scroll: 0,
            dialog_scroll: 0,
            reveal: false,
            hits: Vec::new(),
            regions: Vec::new(),
            body_area: (0, 0, 0, 0),
            focus: None,
            focus_ctx: String::new(),
            hover: false,
            px: 0,
            py: 0,
            armed: None,
            enter_page: false,
            adapter_drafts: HashMap::new(),
            device_drafts: HashMap::new(),
            setting_drafts: HashMap::new(),
            submissions: HashMap::new(),
            settings_errors: HashMap::new(),
            notes: HashMap::new(),
            device_states: HashMap::new(),
            jobs: HashMap::new(),
            toast: None,
            dialog: None,
            menu: None,
            form: Field::new(64),
            form_focused: false,
            editing: None,
            add: None,
            scan_stops: HashSet::new(),
            files: files::Files::default(),
            migration: None,
            declined: HashSet::new(),
        };
        m.discover();
        m
    }

    pub(crate) fn quitting(&self) -> bool {
        self.quitting
    }

    /// Closes every session: a running scan stops and an unsaved pairing is cancelled. A running
    /// migration is cancelled, and the TUI ends once its worker has exited.
    pub(crate) fn quit(&mut self) {
        if self.quitting {
            return;
        }
        self.quitting = true;
        if let Some(m) = &mut self.migration {
            match m.stage {
                Stage::Running => {
                    m.stage = Stage::Cancelling;
                    self.fleet.cancel_migration();
                }
                Stage::Failed(..) => self.migration = None,
                _ => {}
            }
        }
        self.close_all();
        self.check_done();
    }

    /// Closes every session and every open in progress.
    fn close_all(&mut self) {
        let slots: Vec<Slot> = self
            .sessions
            .iter()
            .map(|s| s.slot)
            .chain(self.probes.keys().copied())
            .chain(self.off.values().filter_map(|o| o.connecting))
            .collect();
        for slot in slots {
            self.close_slot(slot);
        }
        for s in std::mem::take(&mut self.sessions) {
            self.drop_session(&s.id, s.slot);
        }
        self.probes.clear();
        for off in self.off.values_mut() {
            off.connecting = None;
            off.waiting = false;
        }
    }

    fn check_done(&mut self) {
        if self.quitting && self.closing.is_empty() && !self.migrating() {
            self.done = true;
        }
    }

    /// Whether a migration's worker runs or is about to.
    pub(crate) fn migrating(&self) -> bool {
        self.migration
            .as_ref()
            .is_some_and(|m| !matches!(m.stage, Stage::Failed(..)))
    }

    /// Closes every session, stops discovery and migrates the adapter once every port is
    /// released.
    fn begin_migration(&mut self, id: &str) {
        let Some(s) = self.session_of(id) else { return };
        let (port, slot) = (s.port.clone(), s.slot);
        let status = self
            .fleet
            .state(slot)
            .map(|st| st.status)
            .unwrap_or_default();
        self.declined.remove(id);
        self.migration = Some(Migration {
            id: id.to_owned(),
            port,
            status,
            stage: Stage::Closing,
            started: false,
            waiting: false,
            progress: None,
        });
        self.close_all();
        self.start_migration();
    }

    /// Starts the worker once every session has released its port.
    fn start_migration(&mut self) {
        let Some(m) = &mut self.migration else { return };
        if m.started || !self.closing.is_empty() {
            return;
        }
        if self.quitting {
            self.migration = None;
            return self.check_done();
        }
        if m.stage == Stage::Cancelling {
            return self.end_migration(false);
        }
        m.stage = Stage::Running;
        m.started = true;
        self.fleet.migrate(&m.id, &m.port);
    }

    /// Ends the migration and lets the adapters open again. Unless the migration succeeded, the
    /// adapter migrates again only with Migrate.
    fn end_migration(&mut self, succeeded: bool) {
        let Some(m) = self.migration.take() else {
            return;
        };
        if !succeeded {
            self.declined.insert(m.id);
        }
        self.list_due = Instant::now();
    }

    fn migrate_adapter(&mut self, id: &str) {
        match &mut self.migration {
            Some(m) if m.id == id && matches!(m.stage, Stage::Failed(..)) => {
                m.stage = Stage::Running;
                m.waiting = false;
                m.progress = None;
                self.fleet.migrate(&m.id, &m.port);
            }
            Some(_) => {}
            None => {
                if self
                    .session_of(id)
                    .is_some_and(|s| s.storage == Storage::Required)
                {
                    self.begin_migration(id);
                }
            }
        }
    }

    fn cancel_migration(&mut self) {
        let Some(m) = &mut self.migration else { return };
        match m.stage {
            Stage::Running => {
                m.stage = Stage::Cancelling;
                self.fleet.cancel_migration();
            }
            Stage::Closing => m.stage = Stage::Cancelling,
            Stage::Cancelling => {}
            Stage::Failed(..) => self.end_migration(false),
        }
    }

    fn migration_event(&mut self, event: MigrationEvent) {
        let Some(m) = &mut self.migration else { return };
        match event.event {
            Some(migration_event::Event::Waiting(_)) => m.waiting = true,
            Some(migration_event::Event::Progress(p)) => {
                m.waiting = false;
                m.progress = (p.total_bytes > 0).then_some((p.completed_bytes, p.total_bytes));
            }
            _ => {}
        }
    }

    /// The worker exited with its result.
    fn migrated(&mut self, result: MigrationResult<Migrated>) {
        let Some(m) = &mut self.migration else { return };
        if self.quitting {
            self.migration = None;
            return self.check_done();
        }
        match result {
            Ok(Migrated::Success) => self.end_migration(true),
            // A cancellation the user asked for ends the migration however the worker ended.
            Ok(Migrated::Cancelled) | Err(_) if m.stage == Stage::Cancelling => {
                self.end_migration(false)
            }
            Ok(Migrated::Cancelled) => self.end_migration(false),
            Err(e) => {
                let what = if e.outcome_unknown {
                    migration::UNKNOWN
                } else {
                    migration::FAILED
                };
                m.stage = Stage::Failed(text::sentence(what), e.reason);
                m.waiting = false;
            }
        }
    }

    fn close_slot(&mut self, slot: Slot) {
        let port = self.port_of(slot).unwrap_or_default();
        self.close_on(slot, port);
    }

    fn close_on(&mut self, slot: Slot, port: String) {
        self.fleet.close(slot);
        self.closing.insert(slot, port);
    }

    /// The port a slot was opened on, while the TUI knows it.
    fn port_of(&self, slot: Slot) -> Option<String> {
        self.session(slot)
            .map(|s| s.port.clone())
            .or_else(|| self.probes.get(&slot).cloned())
            .or_else(|| {
                self.off
                    .values()
                    .find(|o| o.connecting == Some(slot))
                    .map(|o| o.connect_port.clone())
            })
    }

    /// Whether a spinner is on screen, which needs faster frames.
    pub(crate) fn animating(&self) -> bool {
        !self.jobs.is_empty()
            || self.migrating()
            || self.adapters.iter().any(|a| {
                a.storage == Storage::Normal
                    && (a.conn == world::Conn::Connecting || a.connected() && !a.ready && !a.failed)
            })
            || self.states.values().any(|st| {
                st.scanning.is_some() || st.devices.iter().any(|d| d.state != 0 && d.state != 2)
            })
    }

    /// Applies one message.
    pub(crate) fn update(&mut self, msg: Msg) {
        // The pointer moving changes only the highlight, which the next frame works out.
        let moved = matches!(
            &msg,
            Msg::Mouse(m) if matches!(
                m.kind,
                ratatui::crossterm::event::MouseEventKind::Moved
                    | ratatui::crossterm::event::MouseEventKind::Drag(_)
            )
        );
        if moved {
            if let Msg::Mouse(m) = msg {
                self.mouse(m);
            }
            return;
        }
        self.refresh();
        match msg {
            Msg::Interrupt => self.quit(),
            Msg::Resize(w, h) => {
                self.width = usize::from(w).max(1);
                self.height = usize::from(h).max(1);
                self.armed = None;
                self.menu = None;
                self.unhover();
            }
            Msg::Ports(result) => self.ports_listed(result),
            Msg::Migration(e) => self.migration_event(e),
            Msg::Migrated(result) => self.migrated(result),
            Msg::Fleet(slot, e) => self.fleet_event(slot, *e),
            Msg::Controller(_) => {}
            Msg::Mouse(m) => self.mouse(m),
            Msg::Key(k) => self.key(k),
            Msg::Paste(text) => self.paste(&text),
        }
        self.refresh();
        self.sync();
    }

    /// A timer tick: adapters are listed, spinners advance and held adapters expire.
    pub(crate) fn tick(&mut self) {
        self.refresh();
        self.sync();
    }

    /// Work that follows from the current state rather than from one message.
    fn sync(&mut self) {
        self.discover();
        self.sync_profiles();
        self.sync_add();
        if self
            .toast
            .as_ref()
            .is_some_and(|(_, at)| at.elapsed() >= TOAST_FOR)
        {
            self.toast = None;
        }
    }

    fn toast(&mut self, text: impl Into<String>) {
        self.toast = Some((text.into(), Instant::now()));
    }

    /// Lists the attached adapters when due.
    fn discover(&mut self) {
        if self.quitting
            || self.listing
            || self.migration.is_some()
            || Instant::now() < self.list_due
        {
            return;
        }
        self.listing = true;
        self.fleet.list_ports();
    }

    /// Lists the attached adapters now, trying ports that failed or were refused again at once.
    /// A migration owns the ports, so refused ports aren't tried again for it.
    fn refresh_adapters(&mut self) {
        for retry in self.failed.values_mut() {
            retry.at = Instant::now();
        }
        if self.migration.is_none() {
            for refused in self.refused.values_mut() {
                refused.retry = true;
            }
        }
        self.list_due = Instant::now();
        self.discover();
    }

    fn ports_listed(&mut self, result: Result<Vec<PortInfo>, String>) {
        self.listing = false;
        self.list_due = Instant::now() + LIST_EVERY;
        self.listed = true;
        // A migration owns the ports until it ends.
        if self.migration.is_some() {
            return;
        }
        let Ok(ports) = result else {
            // A failed listing changes nothing and is tried again.
            return;
        };
        let present: BTreeSet<&str> = ports.iter().map(|p| p.port.as_str()).collect();
        let gone: Vec<Slot> = self
            .sessions
            .iter()
            .filter(|s| !present.contains(s.port.as_str()))
            .map(|s| s.slot)
            .collect();
        for slot in gone {
            self.lose(slot);
        }
        self.failed
            .retain(|port, _| present.contains(port.as_str()));
        self.refused
            .retain(|port, _| present.contains(port.as_str()));
        for (id, off) in &mut self.off {
            let port = ports
                .iter()
                .find(|p| holds(id, off.port.as_deref(), p))
                .map(|p| p.port.clone());
            if port != off.port {
                off.port = port;
                off.error = None;
            }
        }
        if self.quitting {
            return;
        }
        self.retry_waiting();
        let now = Instant::now();
        let busy: BTreeSet<String> = self
            .sessions
            .iter()
            .map(|s| s.port.clone())
            .chain(self.probes.values().cloned())
            .chain(self.closing.values().cloned())
            .chain(
                self.off
                    .values()
                    .filter(|o| o.connecting.is_some())
                    .map(|o| o.connect_port.clone()),
            )
            .chain(
                self.failed
                    .iter()
                    .filter(|(_, r)| r.at > now)
                    .map(|(p, _)| p.clone()),
            )
            .chain(
                self.refused
                    .iter()
                    .filter(|(_, r)| !r.retry)
                    .map(|(p, _)| p.clone()),
            )
            .collect();
        for port in &ports {
            let held_off = self
                .off
                .iter()
                .any(|(id, o)| holds(id, o.port.as_deref(), port));
            if busy.contains(&port.port) || held_off {
                continue;
            }
            if let Some(refused) = self.refused.get_mut(&port.port) {
                refused.retry = false;
            }
            let slot = self.fleet.open(&port.port);
            self.probes.insert(slot, port.port.clone());
        }
        self.ports = ports;
    }

    /// Puts off trying a port again, longer after each failure in a row.
    fn fail(&mut self, port: String) {
        let retry = self.failed.entry(port).or_insert(Retry {
            at: Instant::now(),
            count: 0,
        });
        retry.count = retry.count.saturating_add(1);
        let wait = RETRY_FIRST.saturating_mul(1 << retry.count.saturating_sub(1).min(5));
        retry.at = Instant::now() + wait.min(RETRY_MOST);
    }

    fn session(&self, slot: Slot) -> Option<&Session> {
        self.sessions.iter().find(|s| s.slot == slot)
    }

    fn session_mut(&mut self, slot: Slot) -> Option<&mut Session> {
        self.sessions.iter_mut().find(|s| s.slot == slot)
    }

    fn session_of(&self, id: &str) -> Option<&Session> {
        self.sessions.iter().find(|s| s.id == id)
    }

    fn session_of_mut(&mut self, id: &str) -> Option<&mut Session> {
        self.sessions.iter_mut().find(|s| s.id == id)
    }

    /// The adapter's session has listed its saved devices.
    fn devices_listed(&self, id: &str) -> bool {
        self.session_of(id).is_some_and(|s| s.listed)
    }

    /// A live session's view.
    pub(super) fn state_of(&self, id: &str) -> Option<&State> {
        self.states.get(&self.session_of(id)?.slot)
    }

    fn fleet_event(&mut self, slot: Slot, e: Event) {
        match e {
            Event::Closed => {
                self.closing.remove(&slot);
                self.check_done();
                self.start_migration();
                self.retry_waiting();
            }
            Event::Connection { phase, .. } => self.connection(slot, phase),
            Event::Notice { notice, .. } => self.notice(slot, notice),
            Event::Done { ticket, result, .. } => {
                if let Some(job) = self.jobs.remove(&(slot, ticket)) {
                    if self.scan_stops.remove(&(slot, ticket)) {
                        // The scan of a closed Add Device stops once it has started.
                        let current = self.session_of(&job.adapter).map(|s| s.slot) == Some(slot);
                        if result.is_ok() && current {
                            self.run(&job.adapter, Kind::Quiet, Command::ScanStop);
                        }
                        return;
                    }
                    self.finish(job, result);
                }
            }
        }
    }

    fn connection(&mut self, slot: Slot, phase: Phase) {
        match phase {
            Phase::Opened => self.opened(slot),
            Phase::Migrate | Phase::Blocked(_) | Phase::Unsupported(_) => {
                self.opened(slot);
                let Some(s) = self.session_mut(slot) else {
                    return;
                };
                s.storage = match phase {
                    Phase::Blocked(e) => {
                        Storage::Blocked(words::failure_in(&e, words::Scope::Other))
                    }
                    Phase::Unsupported(e) => Storage::Unsupported(words::failure(&e)),
                    _ => Storage::Required,
                };
                let id = s.id.clone();
                if s.storage == Storage::Required
                    && !self.declined.contains(&id)
                    && self.migration.is_none()
                    && !self.quitting
                {
                    self.begin_migration(&id);
                }
            }
            Phase::Waiting => {}
            Phase::Ready => self.ready(slot),
            Phase::Failed { error, open: true } => {
                if let Some(s) = self.session_mut(slot) {
                    s.unready = Some(words::failure(&error));
                }
            }
            Phase::Failed { error, open: false } => {
                if let Some(port) = self.probes.remove(&slot) {
                    self.close_on(slot, port.clone());
                    match &error.native_hello {
                        Some(hello) => self.refuse(port, hello.clone(), &error),
                        None => {
                            self.refused.remove(&port);
                            self.fail(port);
                        }
                    }
                } else if let Some(port) = self.port_of(slot)
                    && let Some(id) = self.connecting_off(slot)
                {
                    self.close_on(slot, port);
                    let why = if error.native_hello.is_some() {
                        words::failure(&error)
                    } else {
                        words::open_failed(&self.off_name(&id))
                    };
                    self.connect_failed(&id, &why);
                }
            }
            Phase::Other => {
                if let Some(port) = self.port_of(slot)
                    && let Some(id) = self.connecting_off(slot)
                {
                    self.close_on(slot, port);
                    let why = words::other_adapter(&self.off_name(&id));
                    self.connect_failed(&id, &why);
                }
            }
            Phase::Lost(_) => {
                if let Some(port) = self.probes.remove(&slot) {
                    self.refused.remove(&port);
                    self.fail(port.clone());
                    self.close_on(slot, port);
                } else if let Some(port) = self.port_of(slot)
                    && let Some(id) = self.connecting_off(slot)
                {
                    self.close_on(slot, port);
                    let why = words::open_failed(&self.off_name(&id));
                    self.connect_failed(&id, &why);
                } else {
                    self.lose(slot);
                }
            }
        }
    }

    /// An opened port's adapter answered: it becomes an adapter of the TUI, unless the same
    /// adapter is already open or was disconnected by the user.
    fn opened(&mut self, slot: Slot) {
        let id = self
            .fleet
            .state(slot)
            .map(|st| st.status.id.clone())
            .unwrap_or_default();
        if let Some(port) = self.probes.remove(&slot) {
            self.refused.remove(&port);
            // A refused adapter's page follows it once it answers.
            if !id.is_empty() && self.page == Page::Adapter(port.clone()) {
                self.page = Page::Adapter(id.clone());
            }
            if id.is_empty() || self.session_of(&id).is_some() {
                self.fail(port.clone());
                return self.close_on(slot, port);
            }
            if let Some(off) = self.off.get_mut(&id) {
                // Found on another port, as after being plugged in again: it stays disconnected.
                off.port = Some(port.clone());
                return self.close_on(slot, port);
            }
            return self.register(slot, port, id);
        }
        // The controller has checked that the adapter is the one Connect opened its port for.
        let Some((key, port)) = self
            .off
            .iter()
            .find(|(_, o)| o.connecting == Some(slot))
            .map(|(k, o)| (k.clone(), o.connect_port.clone()))
        else {
            return;
        };
        self.off.remove(&key);
        self.register(slot, port, id);
    }

    /// Lists a port whose adapter refused the connection. An adapter held on the port, as its
    /// USB serial number suggests, is no longer expected back; its page shows the refusal.
    fn refuse(&mut self, port: String, hello: Arc<NativeHello>, error: &Error) {
        if let Some(info) = self.ports.iter().find(|p| p.port == port) {
            let ids: Vec<String> = self
                .held
                .keys()
                .filter(|id| info.is_adapter(id))
                .cloned()
                .collect();
            for id in ids {
                self.held.remove(&id);
                if self.page == Page::Adapter(id) {
                    self.page = Page::Adapter(port.clone());
                }
            }
        }
        self.refused.insert(
            port,
            Refused {
                hello,
                why: words::failure(error),
                retry: false,
            },
        );
    }

    fn register(&mut self, slot: Slot, port: String, id: String) {
        let held = self.held.remove(&id);
        let (list, picker) = match held {
            Some(h) => (h.list, h.picker),
            None => (ProfilePage::default(), ProfilePage::default()),
        };
        self.sessions.push(Session {
            slot,
            port,
            id,
            opened: Instant::now(),
            unready: None,
            list,
            picker,
            looked_up: HashSet::new(),
            listed: false,
            storage: Storage::Normal,
        });
    }

    /// The adapter became ready and its saved devices are loaded: reads each device's settings
    /// and the profiles.
    fn ready(&mut self, slot: Slot) {
        let Some(s) = self.session_mut(slot) else {
            return;
        };
        s.unready = None;
        s.looked_up.clear();
        let id = s.id.clone();
        let Some(st) = self.fleet.state(slot) else {
            return;
        };
        for d in &st.devices {
            self.list_settings(&id, d.id);
        }
        if crate::profiles::available(&st.status) {
            self.read_page(&id, false, false);
            if self.picker_open(&id) {
                self.read_page(&id, true, false);
            }
        }
    }

    /// A session ended on its own, or its port went away. Its adapter keeps its page for a while
    /// in case it returns; one that ended soon after opening isn't opened again automatically.
    fn lose(&mut self, slot: Slot) {
        let Some(i) = self.sessions.iter().position(|s| s.slot == slot) else {
            return;
        };
        let s = self.sessions.remove(i);
        let st = self.states.remove(&slot).or_else(|| self.fleet.state(slot));
        let (status, profiles) = st.map(|st| (st.status, st.profiles)).unwrap_or_default();
        if s.opened.elapsed() < UNSTABLE {
            self.fail(s.port.clone());
        } else {
            self.failed.remove(&s.port);
        }
        let (mut list, mut picker) = (s.list, s.picker);
        list.reading = false;
        picker.reading = false;
        self.held.insert(
            s.id.clone(),
            Held {
                status,
                until: Instant::now() + RECONNECT_HOLD,
                list,
                picker,
                profiles,
            },
        );
        self.drop_session(&s.id, slot);
        self.close_on(slot, s.port);
    }

    /// Drops what belonged to a session that ended: its scan and pairing in Add Device, and its
    /// settings saves.
    fn drop_session(&mut self, id: &str, slot: Slot) {
        if let Some(add) = &mut self.add {
            add.session_ended(slot);
        }
        self.submissions.retain(|(a, _), _| a != id);
        self.scan_stops.retain(|(s, _)| *s != slot);
        self.settings_errors.retain(|(a, _), _| a != id);
        // Its dialogs close with it; their requests can't finish.
        let of_adapter = match &self.dialog {
            Some(
                Dialog::Files(a)
                | Dialog::Replace(a, _)
                | Dialog::Bootloader(a)
                | Dialog::Rename(a)
                | Dialog::Forget(a, _)
                | Dialog::ProfileName(a, _)
                | Dialog::ProfileDelete(a, _)
                | Dialog::Pick { adapter: a, .. },
            ) => a == id,
            _ => false,
        };
        if of_adapter {
            self.dialog = None;
            self.form_focused = false;
            self.menu = None;
        }
    }

    /// Closes an adapter's session and keeps it listed as disconnected while it stays plugged in.
    fn disconnect_adapter(&mut self, id: &str) {
        let Some(i) = self.sessions.iter().position(|s| s.id == id) else {
            return;
        };
        let s = self.sessions.remove(i);
        let status = self
            .states
            .remove(&s.slot)
            .map(|st| st.status)
            .unwrap_or_default();
        self.off.insert(
            s.id.clone(),
            Off {
                port: Some(s.port.clone()),
                status,
                connecting: None,
                connect_port: String::new(),
                waiting: false,
                error: None,
            },
        );
        self.drop_session(&s.id, s.slot);
        self.close_on(s.slot, s.port);
    }

    /// Opens a disconnected adapter again, or a refused port on the next listing.
    fn connect_adapter(&mut self, id: &str) {
        if self.refused.contains_key(id) && self.migration.is_some() {
            return self.toast(text::sentence(migration::BUSY));
        }
        if let Some(refused) = self.refused.get_mut(id) {
            refused.retry = true;
            self.list_due = Instant::now();
            return self.discover();
        }
        let Some(off) = self.off.get(id) else {
            return;
        };
        if off.connecting.is_some() || off.waiting || self.quitting {
            return;
        }
        if self.migration.is_some() {
            return self.connect_failed(id, &text::sentence(migration::BUSY));
        }
        let Some(port) = off.port.clone() else {
            return self.connect_failed(id, words::NOT_PLUGGED_IN);
        };
        if self.closing.values().any(|p| *p == port) {
            // The previous session must release the port first.
            if let Some(off) = self.off.get_mut(id) {
                off.waiting = true;
                off.error = None;
            }
            return;
        }
        let slot = self.fleet.open_adapter(&port, id);
        if let Some(off) = self.off.get_mut(id) {
            off.connecting = Some(slot);
            off.connect_port = port;
            off.error = None;
        }
    }

    /// Tries each Connect that waits for a closing session again: it opens the adapter's port
    /// once no session holds it, or reports that the adapter went away.
    fn retry_waiting(&mut self) {
        let waiting: Vec<String> = self
            .off
            .iter()
            .filter(|(_, o)| o.waiting)
            .map(|(id, _)| id.clone())
            .collect();
        for id in waiting {
            if let Some(off) = self.off.get_mut(&id) {
                off.waiting = false;
            }
            self.connect_adapter(&id);
        }
    }

    /// The disconnected adapter a Connect opened `slot` for; that attempt is over.
    fn connecting_off(&mut self, slot: Slot) -> Option<String> {
        let (id, off) = self
            .off
            .iter_mut()
            .find(|(_, o)| o.connecting == Some(slot))?;
        off.connecting = None;
        Some(id.clone())
    }

    /// A disconnected adapter's name, as messages name it.
    fn off_name(&self, id: &str) -> String {
        let name = self
            .off
            .get(id)
            .map(|o| words::clean(&o.status.name))
            .unwrap_or_default();
        if name.is_empty() {
            "the adapter".to_owned()
        } else {
            name
        }
    }

    /// Reports a failed Connect beside the adapter's Connect, and in the status line when that
    /// isn't shown.
    fn connect_failed(&mut self, id: &str, why: &str) {
        if let Some(off) = self.off.get_mut(id) {
            off.connecting = None;
            off.error = Some(why.to_owned());
        }
        if self.shown() != Page::Adapter(id.to_owned()) {
            self.toast(why);
        }
    }

    fn notice(&mut self, slot: Slot, notice: Notice) {
        let Some(id) = self.session(slot).map(|s| s.id.clone()) else {
            return;
        };
        let (event, first) = match notice {
            Notice::Event { event, first, .. } => (event, first),
            Notice::Response(p::Response {
                result: Some(p::response::Result::Profiles(list)),
                ..
            }) => return self.profiles_listed(&id, list),
            _ => return,
        };
        let Some(kind) = event.kind else { return };
        match kind {
            p::event::Kind::Device(d) if first => {
                if self.states.get(&slot).is_some_and(State::ready) {
                    self.list_settings(&id, d.id);
                }
            }
            p::event::Kind::DeviceRemoved(r) => {
                let key = (id.clone(), r.id);
                self.device_drafts.remove(&key);
                self.setting_drafts.remove(&key);
                self.submissions.remove(&key);
                self.settings_errors.remove(&key);
            }
            p::event::Kind::Profile(_) | p::event::Kind::ProfileRemoved(_) => {
                self.profile_event(&id, &kind);
            }
            _ => {}
        }
    }

    /// Reads a device's settings list.
    fn list_settings(&mut self, id: &str, device: u32) {
        self.settings_errors.remove(&(id.to_owned(), device));
        self.run(
            id,
            Kind::SettingsList(device),
            Command::Settings(crate::controller::Target::Id(device)),
        );
    }

    /// Runs a command on an adapter's session; false when the adapter has no session.
    fn run(&mut self, id: &str, kind: Kind, command: Command) -> bool {
        self.run_job(id, kind, command).is_some()
    }

    /// Runs a command, returning its job's slot and ticket.
    fn run_job(&mut self, id: &str, kind: Kind, command: Command) -> Option<(Slot, Ticket)> {
        let slot = self.session_of(id).map(|s| s.slot)?;
        let ticket = self.fleet.run(slot, command.clone());
        self.jobs.insert(
            (slot, ticket),
            Job {
                slot,
                adapter: id.to_owned(),
                command,
                kind,
            },
        );
        Some((slot, ticket))
    }

    /// Whether a job of this adapter matches.
    fn running(&self, id: &str, f: impl Fn(&Kind) -> bool) -> bool {
        self.jobs.values().any(|j| j.adapter == id && f(&j.kind))
    }

    fn finish(&mut self, job: Job, result: Result<Outcome, Error>) {
        // A result from a session that has since ended belongs to nothing shown now, except a
        // device save's: device IDs are never reused, and a save that reconnects USB, such as a
        // Gamepad Output change, may answer only after its session ended.
        let current = self.session_of(&job.adapter).map(|s| s.slot) == Some(job.slot);
        if !current && !matches!(job.kind, Kind::DeviceSave { .. }) {
            return;
        }
        let id = job.adapter.clone();
        match &job.kind {
            Kind::Quiet => {}
            Kind::SettingsList(device) => {
                if let Err(e) = &result {
                    self.settings_errors
                        .insert((id, *device), words::read_failure(e));
                }
            }
            Kind::Page { .. } | Kind::Lookup => self.page_result(&job, &result),
            Kind::AdapterSave(_) | Kind::Rename => self.adapter_result(&job, &result),
            Kind::Device(..) | Kind::Forget(_) | Kind::DeviceSave { .. } => {
                self.device_result(&job, &result)
            }
            Kind::SettingsSave { .. } => self.settings_result(&job, &result),
            Kind::Profile => self.profile_result(&job, &result),
            Kind::Scan | Kind::Pair | Kind::Answer | Kind::CancelPairing => {
                self.add_result(&job, &result)
            }
            Kind::Restart => {
                if let Err(e) = &result {
                    self.toast(words::failure(e));
                }
            }
            Kind::Files | Kind::FileGet | Kind::Bootloader => self.files_result(&job, &result),
        }
    }

    /// Shows a page, as clicking it in the sidebar does. Another page starts on its Details tab
    /// with no local errors; staged changes stay.
    fn open(&mut self, page: Page) {
        if page != self.page {
            self.tab = Tab::Details;
            self.main_scroll = 0;
            self.notes.clear();
            self.editing = None;
            self.form_focused = false;
        }
        self.page = page;
        self.page_shown = false;
        self.reveal = true;
    }

    fn paste(&mut self, text: &str) {
        if self.files.editing {
            self.files.dest_err.clear();
            self.files.dest.insert(text);
        } else if self.form_focused || self.editing.is_some() {
            if matches!(self.dialog, Some(Dialog::AddDevice)) {
                self.add_form_changed();
            }
            self.form.insert(text);
        }
    }
}

/// Whether a listed port is a disconnected adapter's: by the adapter ID that begins its USB
/// serial number, or by its path where no serial number can be read.
fn holds(id: &str, path: Option<&str>, port: &PortInfo) -> bool {
    if port.serial.is_empty() {
        path == Some(port.port.as_str())
    } else {
        port.is_adapter(id)
    }
}

/// How long to wait for the next message before a tick.
pub fn tick_interval(animating: bool) -> Duration {
    if animating {
        Duration::from_millis(120)
    } else {
        Duration::from_millis(500)
    }
}

/// A spinner frame for the current time.
pub(super) fn spinner() -> char {
    const FRAMES: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis());
    FRAMES[(ms / 100 % 10) as usize]
}
