//! Application state shared by the desktop and terminal interfaces.
//!
//! Each adapter has one in-process controller. Actions stage edits or submit commands;
//! messages reconcile their results, discovery, reconnects and storage migrations.
//!
//! Construct [`Live`] with a [`Sink`] that delivers messages to the UI thread, then pass it to
//! [`App::new`]. Feed received messages to [`App::update`] and call [`App::tick`] every 500 ms.
//! The sink runs on worker threads and may block on a bounded channel; it must not mutate the
//! app directly. [`Live::with_backend`] replaces serial connections, discovery and migration
//! for tests or simulations.
//!
//! [`App::act`] handles [`Action`] values against the shown page or dialog. Set dialog text with
//! [`App::set_form`] before submitting it. Native setting controls use [`Action::SettingChoice`]
//! with typed values.
//!
//! [`App::adapters`], [`App::devices`] and [`App::state_of`] expose the current read model.
//! The `adapter_changes`, `device_values`, `settings_of`, `shown_value`, `marker` and
//! `shown_page` methods include staged edits and profile paging. Their locking and saving
//! helpers determine control availability. [`App::note`] returns results at their [`Spot`];
//! [`App::toast_text`] returns transient results belonging outside the shown page.
//!
//! [`App::hide`] closes dialogs and stops Add Device. [`App::quit`] closes sessions and cancels
//! migration; keep draining messages until [`App::done`] so every worker releases its port.
mod actions;
pub mod adapter;
pub mod add;
pub mod device;
pub mod fleet;
mod immediate;
mod input;
#[cfg(feature = "desktop")]
pub(crate) use immediate::unknown as write_outcome_unknown;
pub mod profiles;
pub mod settings;
pub mod words;
pub mod world;
use crate::settings::{Field as SettingField, Value};
use crate::{
    controller::{AdapterUpdate, Command, DeviceUpdate, Event, Notice, Outcome, Phase, Ticket},
    error::Error,
    migration,
    ui::text,
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
pub use fleet::{Fleet, Live, Sink, Slot};
pub use input::Input;
use profiles::ProfilePage;
use settings::Draft;
use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    sync::Arc,
    time::{Duration, Instant},
};
pub use world::{AdapterView, DeviceView, Storage};

/// Events produced by controllers, discovery and the migration worker.
pub enum Msg {
    Fleet(Slot, Box<Event>),
    Ports(Result<Vec<PortInfo>, String>),
    Migration(MigrationEvent),
    Migrated(MigrationResult<Migrated>),
    Interrupt,
}

/// Semantic emphasis that each interface renders in its own style.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    Accent,
    Dim,
    Warning,
    Success,
}

/// How often the attached adapters are listed.
const LIST_EVERY: Duration = Duration::from_secs(1);
/// How long an adapter that went away keeps its page while it may return, as across the USB
/// reconnect an interface allocation change causes.
pub(crate) const RECONNECT_HOLD: Duration = Duration::from_secs(15);
/// An unexpected session loss this soon after opening counts as a failure of its port.
pub(crate) const UNSTABLE: Duration = Duration::from_secs(10);
/// How long a failure stays in the status line.
const TOAST_FOR: Duration = Duration::from_secs(6);
/// How long a port that failed waits before it is tried again, doubling with each failure in a
/// row up to the most.
pub(crate) const RETRY_FIRST: Duration = Duration::from_secs(2);
pub(crate) const RETRY_MOST: Duration = Duration::from_secs(60);
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

/// A device preference on the Details tab.
pub use crate::controller::Toggle;

/// A control action against the shown page or dialog.
#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    Quit,
    Help,
    /// Lists the attached adapters again, including ports that failed to open.
    RefreshAdapters,
    /// Opens Add Device with the first ready adapter that has room.
    AddDevice,
    /// Opens Add Device with this adapter, and does nothing while it isn't ready.
    AddDeviceTo(String),
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
    /// Stages a configuration interface's profile. Zero clears it while the interface is off.
    InterfaceProfile(ConfigurationInterface, u32),
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
    /// Moves the layer at `from` to `to` on a device whose layers read `layers` when the move
    /// began. A move whose device or order no longer matches does nothing.
    LayerMove {
        adapter: String,
        device: u32,
        layers: Vec<u32>,
        from: usize,
        to: usize,
    },
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
    /// Selects the dialog's text field in interfaces with explicit focus.
    Field,
    ResetName,
    /// Chooses a profile in the profile picker; 0 chooses none.
    Pick(u32),
    PickPage(bool),
    /// Names a new profile from the profile picker, then returns to it.
    PickNewProfile,
    /// Lists the picker's profiles again after its listing failed.
    PickRetry,
    Confirm,
    /// Deletes a profile with the changes its confirmation shows, and no others.
    ProfileDeleteConfirm(crate::profiles::Deletion),

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
}

/// A small list of actions shown beside the control that opened it.
#[derive(Clone, Debug, PartialEq)]
pub struct Menu {
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

/// A command the application started, kept until its result.
#[derive(Clone, Debug)]
pub(crate) struct Job {
    pub(crate) slot: Slot,
    pub(crate) adapter: String,
    pub(crate) command: Command,
    pub(crate) kind: Kind,
}

#[derive(Clone, Debug)]
pub(crate) enum Kind {
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
    Answer {
        pairing: (Slot, Ticket),
        dismiss: bool,
    },
    CancelPairing,
    Restart,
    Bootloader,
}

/// When a failed port may be tried again.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Retry {
    pub(crate) at: Instant,
    pub(crate) count: u32,
}

/// A registered adapter: an opened port whose adapter answered.
pub(crate) struct Session {
    pub(crate) slot: Slot,
    pub(crate) port: String,
    pub(crate) id: String,
    pub(crate) opened: Instant,
    /// The original ending deadline bounds old devices while this session reloads them.
    pub(crate) retained_until: Option<Instant>,
    /// Opening finished without the adapter becoming usable; why.
    pub(crate) unready: Option<String>,
    pub(crate) list: ProfilePage,
    pub(crate) picker: ProfilePage,
    /// Profiles whose names were read once because nothing had named them.
    pub(crate) looked_up: HashSet<u32>,
    /// The saved devices have been listed in this session.
    pub(crate) listed: bool,
    pub(crate) storage: Storage,
}

/// A migration, which owns every adapter connection while it lasts.
pub(crate) struct Migration {
    pub(crate) id: String,
    /// The port the adapter was on, where the worker looks first.
    pub(crate) port: String,
    /// The adapter's status when its session closed.
    pub(crate) status: p::Status,
    pub(crate) stage: Stage,
    /// The worker has started; it exits before the migration ends.
    pub(crate) started: bool,
    pub(crate) waiting: bool,
    pub(crate) progress: Option<(u64, u64)>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Stage {
    /// Every session is closing; the worker starts once all have released their ports.
    Closing,
    Running,
    Cancelling,
    /// The worker failed: what to tell the user, and its reason.
    Failed(String, String),
}

impl Migration {
    pub(crate) fn storage(&self) -> Storage {
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
pub(crate) struct Held {
    pub(crate) port: String,
    pub(crate) status: p::Status,
    pub(crate) until: Instant,
    pub(crate) list: ProfilePage,
    pub(crate) picker: ProfilePage,
    pub(crate) profiles: BTreeMap<u32, p::Profile>,
}

/// An adapter the user disconnected; its port is not opened until Connect.
pub(crate) struct Off {
    /// Present while plugged in.
    pub(crate) port: Option<String>,
    pub(crate) status: p::Status,
    pub(crate) connecting: Option<Slot>,
    /// The port Connect opened.
    pub(crate) connect_port: String,
    /// Connect waits for the previous session to release the port.
    pub(crate) waiting: bool,
    pub(crate) error: Option<String>,
}

/// A port whose adapter refused the connection because Cordial doesn't support its protocol.
pub(crate) struct Refused {
    pub(crate) hello: Arc<NativeHello>,
    pub(crate) why: String,
    /// Refresh or Connect asked for the port to be opened again.
    pub(crate) retry: bool,
}

/// One settings save, while it runs and after it fails.
#[derive(Clone, Debug, Default)]
pub struct Submission {
    pub running: bool,
    /// The fields it sent.
    pub fields: Vec<SettingField>,
    /// Why it failed; None while running or after it succeeded.
    pub error: Option<String>,
    /// Its outcome is unknown, so its keys are neither saved nor failed.
    pub unknown: bool,
}

/// Session state, staged edits and the page and dialog currently shown.
pub struct App<F: Fleet> {
    pub(crate) fleet: F,
    pub(crate) done: bool,
    pub(crate) quitting: bool,

    pub(crate) listing: bool,
    pub(crate) list_due: Instant,
    /// The adapter list has arrived at least once.
    pub(crate) listed: bool,
    pub(crate) ports: Vec<PortInfo>,
    /// Ports that failed to open or whose session ended soon after opening, with when each may
    /// be tried again and how many times in a row it failed. A port leaves when it goes away.
    pub(crate) failed: BTreeMap<String, Retry>,
    /// Opened ports whose adapter hasn't answered yet.
    pub(crate) probes: HashMap<Slot, String>,
    pub(crate) sessions: Vec<Session>,
    pub(crate) held: BTreeMap<String, Held>,
    pub(crate) off: BTreeMap<String, Off>,
    /// Refused ports, listed until they go away. One is opened again only on Refresh or Connect.
    pub(crate) refused: BTreeMap<String, Refused>,
    /// Sessions closing, with the port each holds until it has closed.
    pub(crate) closing: HashMap<Slot, String>,

    /// Each session's view, as of the last refresh.
    pub(crate) states: HashMap<Slot, State>,
    pub(crate) adapters: Vec<AdapterView>,
    pub(crate) devices: Vec<DeviceView>,

    pub(crate) page: Page,
    /// The page's target has been shown, so its disappearing returns to the Overview.
    pub(crate) page_shown: bool,
    pub(crate) tab: Tab,

    pub(crate) immediate: Option<immediate::Immediate>,
    pub(crate) adapter_drafts: HashMap<String, AdapterUpdate>,
    pub(crate) device_drafts: HashMap<(String, u32), DeviceUpdate>,
    pub(crate) setting_drafts: HashMap<(String, u32), BTreeMap<SettingField, Draft>>,
    pub(crate) submissions: HashMap<(String, u32), Submission>,
    /// Why each device's settings couldn't be listed.
    pub(crate) settings_errors: HashMap<(String, u32), String>,
    pub(crate) notes: HashMap<Spot, String>,
    /// Each device's last connection state, to drop errors that belonged to an earlier one.
    pub(crate) device_states: HashMap<(String, u32), i32>,
    pub(crate) jobs: HashMap<(Slot, Ticket), Job>,
    pub(crate) toast: Option<(String, Instant)>,
    pub(crate) dialog: Option<Dialog>,
    /// The profile deletion last confirmed in its dialog.
    pub(crate) deleting: Option<profiles::Deleting>,
    /// A deletion's dialog stays open once the profile is deleted.
    pub(crate) keep_deleted: bool,
    /// The picker New Profile returns to once the profile it names is created or cancelled.
    pub(crate) pick_after_new: Option<PickFor>,
    pub(crate) menu: Option<Menu>,
    pub(crate) form: Input,
    /// The integer setting being typed, which holds the form.
    pub(crate) editing: Option<SettingField>,
    pub(crate) add: Option<AddDevice>,
    /// Adapters whose scan from a closed Add Device stops once its start is answered.
    pub(crate) scan_stops: HashSet<(Slot, Ticket)>,
    pub(crate) migration: Option<Migration>,
    /// Adapters whose migration the user cancelled; they migrate again only with Migrate.
    pub(crate) declined: HashSet<String>,
}

impl<F: Fleet> App<F> {
    pub fn new(fleet: F) -> Self {
        let mut m = Self {
            fleet,
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
            page: Page::Overview,
            page_shown: true,
            tab: Tab::Details,
            immediate: None,
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
            deleting: None,
            keep_deleted: false,
            pick_after_new: None,
            menu: None,
            form: Input::new(64),
            editing: None,
            add: None,
            scan_stops: HashSet::new(),
            migration: None,
            declined: HashSet::new(),
        };
        m.discover();
        m
    }

    pub fn quitting(&self) -> bool {
        self.quitting
    }

    /// Closes every session: a running scan stops and an unsaved pairing is cancelled. A running
    /// migration is cancelled, and the application finishes once its worker has exited.
    pub fn quit(&mut self) {
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
    pub(crate) fn close_all(&mut self) {
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

    pub(crate) fn check_done(&mut self) {
        if self.quitting && self.closing.is_empty() && !self.migrating() {
            self.done = true;
        }
    }

    /// Whether a migration's worker runs or is about to.
    pub fn migrating(&self) -> bool {
        self.migration
            .as_ref()
            .is_some_and(|m| !matches!(m.stage, Stage::Failed(..)))
    }

    /// Closes every session, stops discovery and migrates the adapter once every port is
    /// released.
    pub(crate) fn begin_migration(&mut self, id: &str) {
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
    pub(crate) fn start_migration(&mut self) {
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
    pub(crate) fn end_migration(&mut self, succeeded: bool) {
        let Some(m) = self.migration.take() else {
            return;
        };
        if !succeeded {
            self.declined.insert(m.id);
        }
        self.list_due = Instant::now();
    }

    pub(crate) fn migrate_adapter(&mut self, id: &str) {
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

    pub(crate) fn cancel_migration(&mut self) {
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

    pub(crate) fn migration_event(&mut self, event: MigrationEvent) {
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
    pub(crate) fn migrated(&mut self, result: MigrationResult<Migrated>) {
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

    pub(crate) fn close_slot(&mut self, slot: Slot) {
        let port = self.port_of(slot).unwrap_or_default();
        self.close_on(slot, port);
    }

    pub(crate) fn close_on(&mut self, slot: Slot, port: String) {
        self.fleet.close(slot);
        self.closing.insert(slot, port);
    }

    /// The port a slot was opened on, while the application knows it.
    pub(crate) fn port_of(&self, slot: Slot) -> Option<String> {
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
    pub fn animating(&self) -> bool {
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
    pub fn update(&mut self, msg: Msg) {
        self.refresh();
        match msg {
            Msg::Interrupt => self.quit(),
            Msg::Ports(result) => self.ports_listed(result),
            Msg::Migration(e) => self.migration_event(e),
            Msg::Migrated(result) => self.migrated(result),
            Msg::Fleet(slot, e) => self.fleet_event(slot, *e),
        }
        self.refresh();
        self.sync();
    }

    /// A timer tick: adapters are listed, spinners advance and held adapters expire.
    pub fn tick(&mut self) {
        self.refresh();
        self.sync();
    }

    /// Work that follows from the current state rather than from one message.
    pub(crate) fn sync(&mut self) {
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

    pub(crate) fn toast(&mut self, text: impl Into<String>) {
        self.toast = Some((text.into(), Instant::now()));
    }

    /// Lists the attached adapters when due.
    pub(crate) fn discover(&mut self) {
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
    pub(crate) fn refresh_adapters(&mut self) {
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

    pub(crate) fn ports_listed(&mut self, result: Result<Vec<PortInfo>, String>) {
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
        // A controller can finish reconnecting before its Ready event reaches the app.
        for session in &mut self.sessions {
            if let Some(st) = self.fleet.state(session.slot)
                && ports
                    .iter()
                    .any(|p| p.port == st.port && p.is_adapter(&session.id))
            {
                session.port = st.port;
            }
        }
        let present: BTreeSet<&str> = ports.iter().map(|p| p.port.as_str()).collect();
        let gone: Vec<Slot> =
            self.sessions
                .iter()
                .filter(|s| {
                    !present.contains(s.port.as_str())
                        && !self.fleet.state(s.slot).is_some_and(|st| {
                            st.pending_for(crate::controller::PROFILE_RECONNECT, 0)
                        })
                })
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
            let reconnecting = self.sessions.iter().any(|s| {
                self.fleet.state(s.slot).is_some_and(|st| {
                    st.pending_for(crate::controller::PROFILE_RECONNECT, 0)
                        && port.is_adapter(&st.status.id)
                })
            });
            if busy.contains(&port.port) || held_off || reconnecting {
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
    pub(crate) fn fail(&mut self, port: String) {
        let retry = self.failed.entry(port).or_insert(Retry {
            at: Instant::now(),
            count: 0,
        });
        retry.count = retry.count.saturating_add(1);
        let wait = RETRY_FIRST.saturating_mul(1 << retry.count.saturating_sub(1).min(5));
        retry.at = Instant::now() + wait.min(RETRY_MOST);
    }

    pub(crate) fn session(&self, slot: Slot) -> Option<&Session> {
        self.sessions.iter().find(|s| s.slot == slot)
    }

    pub(crate) fn session_mut(&mut self, slot: Slot) -> Option<&mut Session> {
        self.sessions.iter_mut().find(|s| s.slot == slot)
    }

    pub(crate) fn session_of(&self, id: &str) -> Option<&Session> {
        self.sessions.iter().find(|s| s.id == id)
    }

    pub(crate) fn session_of_mut(&mut self, id: &str) -> Option<&mut Session> {
        self.sessions.iter_mut().find(|s| s.id == id)
    }

    /// The adapter's session has listed its saved devices.
    pub(crate) fn devices_listed(&self, id: &str) -> bool {
        self.session_of(id).is_some_and(|s| s.listed)
    }

    /// A live session's view.
    pub fn state_of(&self, id: &str) -> Option<&State> {
        self.states.get(&self.session_of(id)?.slot)
    }

    pub(crate) fn fleet_event(&mut self, slot: Slot, e: Event) {
        match e {
            Event::Closed => {
                self.closing.remove(&slot);
                self.check_done();
                self.start_migration();
                self.retry_waiting();
            }
            Event::Connection { phase, port, .. } => {
                if matches!(phase, Phase::Ready)
                    && !port.is_empty()
                    && let Some(session) = self.session_mut(slot)
                {
                    session.port = port;
                }
                self.connection(slot, phase);
            }
            Event::Notice { notice, .. } => self.notice(slot, notice),
            Event::Progress {
                ticket, progress, ..
            } => {
                if let Some(job) = self.jobs.get(&(slot, ticket)).cloned() {
                    self.deletion_progress(&job, ticket, progress);
                }
            }
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
                    self.finish(job, ticket, result);
                }
            }
        }
    }

    pub(crate) fn connection(&mut self, slot: Slot, phase: Phase) {
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
                // A probe lost before its status names no adapter, so a held adapter keeps its
                // deadline.
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

    /// An opened port's adapter answered: it becomes a listed adapter, unless the same
    /// adapter is already open or was disconnected by the user.
    pub(crate) fn opened(&mut self, slot: Slot) {
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
    pub(crate) fn refuse(&mut self, port: String, hello: Arc<NativeHello>, error: &Error) {
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

    fn discard_held_on(&mut self, port: &str) {
        let info = self.ports.iter().find(|p| p.port == port);
        self.held
            .retain(|id, held| held.port != port && !info.is_some_and(|info| info.is_adapter(id)));
    }

    pub(crate) fn register(&mut self, slot: Slot, port: String, id: String) {
        let held = self.held.remove(&id);
        self.discard_held_on(&port);
        let (list, picker, retained_until) = match held {
            Some(h) => (h.list, h.picker, Some(h.until)),
            None => (ProfilePage::default(), ProfilePage::default(), None),
        };
        self.sessions.push(Session {
            slot,
            port,
            id,
            opened: Instant::now(),
            retained_until,
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
    pub(crate) fn ready(&mut self, slot: Slot) {
        let Some(s) = self.session_mut(slot) else {
            return;
        };
        s.unready = None;
        s.looked_up.clear();
        let id = s.id.clone();
        let Some(st) = self.fleet.state(slot) else {
            return;
        };
        self.immediate_readback(&id, None, slot);
        for d in &st.devices {
            self.immediate_device_readback(&id, d);
            self.list_settings(&id, d.id);
        }
        if crate::profiles::available(&st.status) {
            self.read_page(&id, false, false);
            if self.picker_open(&id) {
                self.read_page(&id, true, false);
            }
        }
    }

    /// Retains an adapter only within the deadline of its session's intentional-ending event.
    pub(crate) fn lose(&mut self, slot: Slot) {
        let Some(i) = self.sessions.iter().position(|s| s.slot == slot) else {
            return;
        };
        let s = self.sessions.remove(i);
        let st = self.fleet.state(slot).or_else(|| self.states.remove(&slot));
        self.states.remove(&slot);
        let ending = st.as_ref().and_then(|st| st.session_ending);
        let until = ending
            .map(|at| at + RECONNECT_HOLD)
            .filter(|until| *until > Instant::now());
        let (status, profiles) = st.map(|st| (st.status, st.profiles)).unwrap_or_default();
        if ending.is_none() && s.opened.elapsed() < UNSTABLE {
            self.fail(s.port.clone());
        } else {
            self.failed.remove(&s.port);
        }
        let (mut list, mut picker) = (s.list, s.picker);
        list.reading = false;
        picker.reading = false;
        if let Some(until) = until {
            self.held.insert(
                s.id.clone(),
                Held {
                    port: s.port.clone(),
                    status,
                    until,
                    list,
                    picker,
                    profiles,
                },
            );
        } else {
            self.held.remove(&s.id);
        }
        self.drop_session(&s.id, slot);
        self.close_on(slot, s.port);
    }

    /// Ends session-owned scans and dialogs. Native writes keep their completion jobs; uncertain
    /// outcomes wait for a fresh readback. Staged terminal forms keep their existing retry behavior.
    pub(crate) fn drop_session(&mut self, id: &str, slot: Slot) {
        if let Some(add) = &mut self.add {
            add.session_ended(id, slot, self.held.contains_key(id));
        }
        self.immediate_disconnect(id, slot);
        if self.immediate.is_none() {
            self.submissions.retain(|(a, _), _| a != id);
        }
        self.scan_stops.retain(|(s, _)| *s != slot);
        self.settings_errors.retain(|(a, _), _| a != id);
        // Deletions keep their frozen progress until their completion job reports its outcome.
        let of_adapter = match &self.dialog {
            Some(
                Dialog::Bootloader(a)
                | Dialog::Rename(a)
                | Dialog::Forget(a, _)
                | Dialog::ProfileName(a, _)
                | Dialog::Pick { adapter: a, .. },
            ) => a == id,
            Some(Dialog::ProfileDelete(a, _)) => a == id && !self.deletion_kept(id, slot),
            _ => false,
        };
        if of_adapter {
            self.deletion_lost(id);
            self.dialog = None;
            self.pick_after_new = None;
            self.menu = None;
        }
    }

    /// Closes an adapter's session and keeps it listed as disconnected while it stays plugged in.
    pub(crate) fn disconnect_adapter(&mut self, id: &str) {
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
    pub(crate) fn connect_adapter(&mut self, id: &str) {
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
    pub(crate) fn retry_waiting(&mut self) {
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
    pub(crate) fn connecting_off(&mut self, slot: Slot) -> Option<String> {
        let (id, off) = self
            .off
            .iter_mut()
            .find(|(_, o)| o.connecting == Some(slot))?;
        off.connecting = None;
        Some(id.clone())
    }

    /// A disconnected adapter's name, as messages name it.
    pub(crate) fn off_name(&self, id: &str) -> String {
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
    pub(crate) fn connect_failed(&mut self, id: &str, why: &str) {
        if let Some(off) = self.off.get_mut(id) {
            off.connecting = None;
            off.error = Some(why.to_owned());
        }
        if self.shown() != Page::Adapter(id.to_owned()) {
            self.toast(why);
        }
    }

    pub(crate) fn notice(&mut self, slot: Slot, notice: Notice) {
        let Some(id) = self.session(slot).map(|s| s.id.clone()) else {
            return;
        };
        let (event, first) = match notice {
            Notice::Event { event, first, .. } => (event, first),
            Notice::Response(p::Response {
                result: Some(p::response::Result::Profiles(list)),
                ..
            }) => return self.profiles_listed(&id, list),
            Notice::Response(p::Response {
                result: Some(p::response::Result::Device(device)),
                ..
            }) => return self.immediate_device_readback(&id, &device),
            _ => return,
        };
        let Some(kind) = event.kind else { return };
        match kind {
            p::event::Kind::Device(d) => {
                self.immediate_device_readback(&id, &d);
                if first && self.states.get(&slot).is_some_and(State::ready) {
                    self.list_settings(&id, d.id);
                }
            }
            p::event::Kind::DeviceRemoved(r) => {
                self.devices.retain(|d| d.adapter != id || d.d.id != r.id);
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
    pub(crate) fn list_settings(&mut self, id: &str, device: u32) {
        self.settings_errors.remove(&(id.to_owned(), device));
        self.run(
            id,
            Kind::SettingsList(device),
            Command::Settings(crate::controller::Target::Id(device)),
        );
    }

    /// Runs a command on an adapter's session; false when the adapter has no session.
    pub(crate) fn run(&mut self, id: &str, kind: Kind, command: Command) -> bool {
        self.run_job(id, kind, command).is_some()
    }

    /// Runs a command, returning its job's slot and ticket.
    pub(crate) fn run_job(
        &mut self,
        id: &str,
        kind: Kind,
        command: Command,
    ) -> Option<(Slot, Ticket)> {
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
    pub(crate) fn running(&self, id: &str, f: impl Fn(&Kind) -> bool) -> bool {
        self.jobs.values().any(|j| j.adapter == id && f(&j.kind))
    }

    pub(crate) fn finish(&mut self, job: Job, ticket: Ticket, result: Result<Outcome, Error>) {
        // Write acknowledgements can arrive after their session ends, including interface and
        // Gamepad Output changes that reconnect USB. Immediate writes retain their completion
        // jobs; device IDs are never reused. A pairing's result
        // likewise reports a bond saved before the session ended, and Add Device keeps only the
        // pairing it belongs to. A code rejection's acknowledgement also belongs to its original
        // pairing. A profile deletion keeps its confirmed outcome and reports partial failures.
        let current = self.session_of(&job.adapter).map(|s| s.slot) == Some(job.slot);
        let deletion = matches!(job.command, Command::ProfileDeletePlanned(_));
        if !current
            && !deletion
            && !(self.immediate.is_some() && immediate::is_write(&job.kind))
            && !matches!(
                job.kind,
                Kind::DeviceSave { .. } | Kind::Pair | Kind::Answer { dismiss: true, .. }
            )
        {
            return;
        }
        let id = job.adapter.clone();
        match &job.kind {
            Kind::Quiet => {}
            Kind::SettingsList(device) => {
                if result.is_ok() {
                    self.immediate_readback(&id, Some(*device), job.slot);
                }
                if let Err(e) = &result {
                    self.settings_errors
                        .insert((id, *device), words::read_failure(e));
                }
            }
            Kind::Page { .. } | Kind::Lookup => self.page_result(&job, &result),
            Kind::AdapterSave(_) | Kind::Rename => self.adapter_result(&job, &result),
            Kind::Device(..) | Kind::Forget(_) | Kind::DeviceSave { .. } => {
                self.device_result(&job, &result);
                if result.is_ok()
                    && let Kind::Device(device, Spot::SettingsNote(..)) = &job.kind
                {
                    self.immediate_settings_readback(&id, *device);
                }
            }
            Kind::SettingsSave { .. } => self.settings_result(&job, &result),
            Kind::Profile => self.profile_result(&job, ticket, &result),
            Kind::Scan | Kind::Pair | Kind::Answer { .. } | Kind::CancelPairing => {
                self.add_result(&job, &result)
            }
            Kind::Restart | Kind::Bootloader => {
                if let Err(e) = &result {
                    self.toast(words::failure(e));
                }
            }
        }
        if self.immediate.is_some() {
            self.immediate_result(&job, &result);
        }
    }

    /// Shows a page, as clicking it in the sidebar does. Another page starts on its Details tab
    /// with no local errors; staged changes stay.
    pub(crate) fn open(&mut self, page: Page) {
        if page != self.page {
            self.tab = Tab::Details;
            self.notes.clear();
            self.editing = None;
        }
        self.page = page;
        self.page_shown = false;
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

impl<F: Fleet> App<F> {
    /// Applies a control action and reconciles its resulting view.
    pub fn act(&mut self, action: Action) {
        self.action(action);
        self.refresh();
        self.sync();
    }

    pub fn show(&mut self, page: Page) {
        self.act(Action::Show(page));
    }

    pub fn set_tab(&mut self, tab: Tab) {
        self.act(Action::Tab(tab));
    }

    pub fn tab(&self) -> Tab {
        self.tab
    }
    pub fn done(&self) -> bool {
        self.done
    }
    pub fn adapters(&self) -> &[AdapterView] {
        &self.adapters
    }
    pub fn devices(&self) -> &[DeviceView] {
        &self.devices
    }
    pub fn dialog(&self) -> Option<&Dialog> {
        self.dialog.as_ref()
    }
    pub fn menu(&self) -> Option<&Menu> {
        self.menu.as_ref()
    }
    pub fn note(&self, spot: &Spot) -> Option<&str> {
        self.immediate_note(spot)
            .or_else(|| self.notes.get(spot).map(String::as_str))
    }
    pub fn toast_text(&self) -> Option<&str> {
        self.toast.as_ref().map(|(text, _)| text.as_str())
    }
    pub fn add(&self) -> Option<&AddDevice> {
        self.add.as_ref()
    }
    pub fn form(&self) -> &str {
        self.form.as_str()
    }
    pub fn form_limit(&self) -> usize {
        self.form.limit
    }

    /// Replaces the active dialog or integer setting's value.
    pub fn set_form(&mut self, text: &str) {
        self.form.set_value(text);
        self.notes.remove(&Spot::Dialog);
        if self.dialog == Some(Dialog::AddDevice) {
            self.add_form_changed();
        }
    }

    /// Whether the active pairing prompt accepts a typed code.
    pub fn pairing_code_input(&self) -> bool {
        matches!(
            self.pairing_prompt(),
            Some(crate::model::Prompt::EnterCode(_))
        )
    }

    pub fn pairing_prompt(&self) -> Option<crate::model::Prompt> {
        let pairing = self.add.as_ref()?.pairing.as_ref()?;
        self.states
            .get(&pairing.slot)?
            .pairing
            .as_ref()
            .filter(|p| p.candidate == pairing.candidate && crate::model::pairing_running(p))
            .and_then(crate::model::prompt)
    }

    /// Whether the active dialog accepts its text value.
    pub fn form_valid(&self) -> bool {
        match &self.dialog {
            Some(Dialog::Rename(_)) => crate::commands::adapter_name(self.form.as_str()).is_some(),
            Some(Dialog::ProfileName(..)) => crate::profiles::valid_name(self.form.as_str().trim()),
            Some(Dialog::AddDevice) => match self.pairing_prompt() {
                Some(crate::model::Prompt::EnterCode(kind)) => {
                    crate::commands::check_code(kind, self.form.as_str()).is_ok()
                }
                Some(crate::model::Prompt::ConfirmCode(_)) => true,
                _ => false,
            },
            _ => false,
        }
    }

    /// Closes transient UI and cancels an unsaved pairing and its scan.
    pub fn hide(&mut self) {
        self.commit_editing();
        if self.add.is_some() {
            self.close_add();
        }
        self.dialog = None;
        self.menu = None;
        self.notes.remove(&Spot::Dialog);
    }

    pub fn settings_error(&self, adapter: &str, device: u32) -> Option<&str> {
        self.settings_errors
            .get(&(adapter.to_owned(), device))
            .map(String::as_str)
    }

    pub fn submission(&self, adapter: &str, device: u32) -> Option<&Submission> {
        self.submissions.get(&(adapter.to_owned(), device))
    }
}

#[cfg(test)]
mod ending_tests {
    use super::*;
    use crate::sim::Desktop;
    use std::sync::mpsc::{self, Receiver};

    /// Holds cancellation before its controller worker can inspect the pairing state.
    struct GatedFleet {
        live: Live,
        cancel: std::cell::Cell<bool>,
    }

    impl Fleet for GatedFleet {
        fn open(&self, port: &str) -> Slot {
            self.live.open(port)
        }
        fn open_adapter(&self, port: &str, id: &str) -> Slot {
            self.live.open_adapter(port, id)
        }
        fn close(&self, slot: Slot) {
            self.live.close(slot);
        }
        fn run(&self, slot: Slot, command: Command) -> Ticket {
            if self.cancel.get() && matches!(command, Command::CancelPairing) {
                Ticket::MAX
            } else {
                self.live.run(slot, command)
            }
        }
        fn state(&self, slot: Slot) -> Option<State> {
            self.live.state(slot)
        }
        fn list_ports(&self) {
            self.live.list_ports();
        }
        fn migrate(&self, id: &str, port: &str) {
            self.live.migrate(id, port);
        }
        fn cancel_migration(&self) {
            self.live.cancel_migration();
        }
    }

    struct Harness {
        app: App<GatedFleet>,
        world: Desktop,
        rx: Receiver<Msg>,
        id: String,
        /// A job whose result is kept back in `kept` instead of applied.
        keep: Option<(Slot, Ticket)>,
        kept: Vec<Msg>,
    }

    impl Harness {
        fn new() -> Self {
            let world = Desktop::new(1);
            let id = world.ports()[0].serial.clone();
            let (tx, rx) = mpsc::channel();
            let app = App::new(GatedFleet {
                live: world.live(Arc::new(move |msg| {
                    let _ = tx.send(msg);
                })),
                cancel: std::cell::Cell::new(false),
            });
            let mut h = Self {
                app,
                world,
                rx,
                id,
                keep: None,
                kept: Vec::new(),
            };
            h.until(|h| h.app.adapter(&h.id).is_some_and(|a| a.ready));
            h.app.show(Page::Device(h.id.clone(), 1));
            h
        }

        fn until(&mut self, done: impl Fn(&Self) -> bool) {
            let deadline = Instant::now() + Duration::from_secs(5);
            while !done(self) {
                let msg = self
                    .rx
                    .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                    .expect("framed application did not reach the expected state");
                if let Msg::Fleet(slot, e) = &msg
                    && let Event::Done { ticket, .. } = &**e
                    && self.keep == Some((*slot, *ticket))
                {
                    self.kept.push(msg);
                    continue;
                }
                self.app.update(msg);
            }
        }

        /// Opens Add Device and starts pairing the candidate that shows a code until
        /// `complete_pairing`.
        fn pair(&mut self) {
            self.pair_candidate(3);
        }

        fn pair_candidate(&mut self, candidate: u32) {
            self.app.act(Action::AddDeviceTo(self.id.clone()));
            self.until(|h| {
                h.app.add().is_some_and(|a| !a.starting)
                    && h.app
                        .state_of(&h.id)
                        .unwrap()
                        .candidate(candidate)
                        .is_some()
            });
            self.app.act(Action::Pair(candidate));
            self.until(|h| h.app.pairing_prompt().is_some());
        }

        fn enter_code(&mut self, kind: p::CodeKind) {
            self.pair_candidate(1);
            self.world.adapters()[0].event(p::event::Kind::Pairing(p::Pairing {
                candidate: 1,
                step: Some(p::pairing::Step::EnterCode(p::EnterCode {
                    kind: kind as i32,
                })),
            }));
            self.until(|h| h.app.pairing_prompt() == Some(crate::model::Prompt::EnterCode(kind)));
        }

        /// Keeps the running pairing's result back until `release`.
        fn keep_pair_result(&mut self) {
            self.keep = self
                .app
                .jobs
                .iter()
                .find(|(_, j)| matches!(j.kind, Kind::Pair))
                .map(|(key, _)| *key);
            assert!(self.keep.is_some());
        }

        fn keep_answer_result(&mut self) {
            self.keep = self
                .app
                .jobs
                .iter()
                .find(|(_, j)| matches!(j.kind, Kind::Answer { .. }))
                .map(|(key, _)| *key);
            assert!(self.keep.is_some());
        }

        fn release(&mut self) {
            self.until(|h| !h.kept.is_empty());
            for msg in std::mem::take(&mut self.kept) {
                self.app.update(msg);
            }
        }

        fn pairing(&self) -> Option<&add::Pairing> {
            self.app.add()?.pairing.as_ref()
        }

        fn phase(&self) -> Option<add::Phase> {
            self.pairing().map(|p| p.phase)
        }

        fn expire(&mut self) {
            let past = Instant::now() - Duration::from_secs(1);
            self.app.held.get_mut(&self.id).unwrap().until = past;
            self.app.tick();
        }

        /// Saves the paired device without connecting it.
        fn pair_disconnected(&mut self) {
            self.world.adapters()[0]
                .0
                .lock()
                .unwrap()
                .pairings
                .get_mut(&3)
                .unwrap()
                .1
                .state = p::DeviceState::Disconnected as i32;
            self.pair();
            assert!(self.world.adapters()[0].complete_pairing());
            self.until(|h| h.phase() == Some(add::Phase::Paired));
        }

        fn set_paired(&self, f: impl FnOnce(&mut p::Device)) {
            let mut sim = self.world.adapters()[0].0.lock().unwrap();
            f(sim.devices.iter_mut().find(|d| d.id == 13).unwrap());
        }

        fn end(&mut self) {
            assert!(
                self.app
                    .state_of(&self.id)
                    .unwrap()
                    .session_ending
                    .is_none()
            );
            self.world.adapters()[0].event(p::event::Kind::SessionEnding(p::SessionEnding {
                reason: p::SessionEndingReason::ConfigurationChange as i32,
            }));
            // Background reads can fail on closure before the reader consumes the final frame.
            self.until(|h| h.app.state_of(&h.id).unwrap().session_ending.is_some());
            let received = self.app.state_of(&self.id).unwrap().session_ending.unwrap();
            self.world.adapters()[0].unplug();
            self.until(|h| h.app.session_of(&h.id).is_none());
            assert_eq!(self.app.held[&self.id].until, received + RECONNECT_HOLD);
        }

        fn reopen(&mut self) {
            self.until(|h| h.app.closing.is_empty());
            self.app.act(Action::RefreshAdapters);
        }

        fn close(mut self) {
            self.app.quit();
            self.until(|h| h.app.done());
            assert_eq!(self.world.adapters()[0].handles(), 0);
        }
    }

    #[test]
    fn deletion_results_belong_to_their_run_and_dialog() {
        for keep_deleted in [false, true] {
            let mut h = Harness::new();
            if keep_deleted {
                h.app.keep_deleted();
            }
            h.app.dialog = Some(Dialog::ProfileDelete(h.id.clone(), 1));
            let plan = h.app.deletion(&h.id, 1).unwrap();
            h.app.delete_profile(&h.id, &plan);
            let (key, job) = h
                .app
                .jobs
                .iter()
                .find(|(_, j)| matches!(j.command, Command::ProfileDeletePlanned(_)))
                .map(|(key, job)| (*key, job.clone()))
                .unwrap();
            h.keep = Some(key);
            h.until(|h| !h.kept.is_empty());
            let running = h.app.deletion_view(&h.id, 1);
            let dialog = h.app.dialog.clone();
            let success = Ok(Outcome::ProfileDeleted(p::Profile {
                id: 1,
                ..Default::default()
            }));
            let failure = Err(Error::new("the connection to the adapter closed"));
            for (slot, ticket) in [(key.0 + 1, key.1), (key.0, key.1 + 1)] {
                let stale = Job {
                    slot,
                    ..job.clone()
                };
                h.app.deletion_progress(
                    &stale,
                    ticket,
                    crate::controller::Progress {
                        done: 1,
                        waiting: true,
                    },
                );
                for result in [&success, &failure] {
                    h.app.profile_result(&stale, ticket, result);
                    assert_eq!(h.app.deletion_view(&h.id, 1), running);
                    assert_eq!(h.app.dialog, dialog);
                    assert!(h.app.note(&Spot::Dialog).is_none());
                }
            }
            h.app.dialog = Some(Dialog::ProfileName(h.id.clone(), None));
            let replacement = h.app.dialog.clone();
            h.app.profile_result(&job, key.1, &success);
            h.app.profile_result(&job, key.1, &failure);
            assert_eq!(h.app.dialog, replacement);
            assert!(h.app.note(&Spot::Dialog).is_none());
            h.release();
            assert_eq!(h.app.dialog, replacement);
            h.close();
        }
    }

    #[test]
    fn announced_replacements_do_not_accumulate_port_backoff() {
        let mut h = Harness::new();
        for _ in 0..5 {
            h.end();
            assert!(h.app.failed.is_empty());
            h.reopen();
            h.until(|h| h.app.adapter(&h.id).is_some_and(|a| a.ready));
        }
        for count in 1..=2 {
            let before = Instant::now();
            h.world.adapters()[0].unplug();
            h.until(|h| h.app.session_of(&h.id).is_none());
            let retry = &h.app.failed["simulated:1"];
            assert_eq!(retry.count, count);
            let wait = RETRY_FIRST * (1 << (count - 1));
            assert!(retry.at >= before + wait);
            assert!(retry.at <= Instant::now() + wait);
            h.reopen();
            h.until(|h| h.app.adapter(&h.id).is_some_and(|a| a.ready));
        }
        h.close();
    }

    #[test]
    fn an_expired_ending_event_still_does_not_count_as_a_port_failure() {
        let mut h = Harness::new();
        let slot = h.app.session_of(&h.id).unwrap().slot;
        h.app.states.get_mut(&slot).unwrap().session_ending =
            Some(Instant::now() - RECONNECT_HOLD - Duration::from_secs(1));
        h.app.fleet.close(slot);
        h.app.lose(slot);
        assert!(h.app.failed.is_empty());
        assert!(h.app.held.is_empty());
        assert!(h.app.session_of(&h.id).is_none());
        h.close();
    }

    #[test]
    fn a_framed_replacement_loss_before_status_keeps_the_original_deadline() {
        let mut h = Harness::new();
        h.end();
        let deadline = h.app.held[&h.id].until;
        h.world.adapters()[0].0.lock().unwrap().refuse.insert(
            "get_status",
            p::Error {
                code: p::ErrorCode::Busy as i32,
                ..Default::default()
            },
        );
        h.reopen();
        h.until(|h| h.app.failed.contains_key("simulated:1"));
        assert_eq!(h.app.held[&h.id].until, deadline);
        assert!(h.app.adapter(&h.id).is_some());
        assert!(!h.app.devices().is_empty());
        assert_eq!(h.app.shown(), Page::Device(h.id.clone(), 1));
        h.expire();
        assert!(h.app.adapter(&h.id).is_none());
        assert!(h.app.devices().is_empty());
        assert_eq!(h.app.shown(), Page::Overview);
        h.close();
    }

    #[test]
    fn confirmed_pairing_succeeds_without_a_connection() {
        let mut h = Harness::new();
        h.pair_disconnected();
        assert_eq!(h.phase(), Some(add::Phase::Paired));
        assert_eq!(h.pairing().unwrap().device, Some(13));
        assert!(h.pairing().unwrap().message.is_none());
        assert_eq!(
            h.app.device(&h.id, 13).unwrap().d.state(),
            p::DeviceState::Disconnected
        );
        h.app.act(Action::Done);
        assert!(h.app.dialog().is_none());
        assert_eq!(h.app.shown(), Page::Device(h.id.clone(), 13));
        assert!(!h.world.adapters()[0].sent().contains(&"cancel_pairing"));
        h.close();
    }

    #[test]
    fn submitting_a_pin_waits_for_the_confirmed_pairing_result() {
        let mut h = Harness::new();
        h.enter_code(p::CodeKind::Pin);
        h.keep_pair_result();
        h.app.set_form("123456");
        h.app.act(Action::Accept);
        h.until(|h| {
            !h.kept.is_empty() && !h.app.running(&h.id, |k| matches!(k, Kind::Answer { .. }))
        });
        assert_eq!(h.phase(), Some(add::Phase::Pairing));
        assert_eq!(h.pairing().unwrap().device, None);
        h.release();
        assert_eq!(h.phase(), Some(add::Phase::Paired));
        h.close();
    }

    #[test]
    fn cancelling_pairing_dismisses_before_acknowledgement() {
        for action in [Action::CancelPairing, Action::Cancel] {
            let mut h = Harness::new();
            h.pair();
            h.app.act(action);
            assert!(h.app.dialog().is_none());
            assert!(h.app.add().is_none());
            assert!(h.app.running(&h.id, |k| matches!(k, Kind::CancelPairing)));
            h.until(|h| {
                !h.app
                    .running(&h.id, |k| matches!(k, Kind::Pair | Kind::CancelPairing))
            });
            assert!(h.app.dialog().is_none());
            assert!(h.app.add().is_none());
            assert!(h.app.toast_text().is_none());
            assert!(h.app.note(&Spot::Dialog).is_none());
            h.close();
        }
    }

    #[test]
    fn reopening_waits_for_the_cancel_worker_after_the_original_pair_finishes() {
        let mut h = Harness::new();
        h.pair();
        h.app.fleet.cancel.set(true);
        h.app.act(Action::CancelPairing);
        assert!(h.app.dialog().is_none());
        assert!(h.world.adapters()[0].complete_pairing());
        h.until(|h| !h.app.running(&h.id, |k| matches!(k, Kind::Pair)));
        assert!(h.app.device(&h.id, 13).is_some());
        let pairing_commands = || {
            h.world.adapters()[0]
                .sent()
                .into_iter()
                .filter(|command| {
                    matches!(*command, "start_scan" | "start_pairing" | "cancel_pairing")
                })
                .collect::<Vec<_>>()
        };
        let sent = pairing_commands();
        h.app.act(Action::AddDeviceTo(h.id.clone()));
        h.app.act(Action::Pair(1));
        h.app.act(Action::ScanAgain);
        assert!(h.app.scan_waiting(&h.id));
        assert!(!h.app.add().unwrap().starting);
        assert!(h.pairing().is_none());
        assert_eq!(pairing_commands(), sent);

        let slot = h.app.session_of(&h.id).unwrap().slot;
        let job = h.app.jobs.remove(&(slot, Ticket::MAX)).unwrap();
        let mut quiet = job.clone();
        quiet.kind = Kind::Quiet;
        h.app.jobs.insert((slot, Ticket::MAX - 1), quiet);
        let ticket = h.app.fleet.live.run(slot, Command::CancelPairing);
        h.app.jobs.insert((slot, ticket), job);
        h.until(|h| h.app.add().is_some_and(|a| !a.starting && a.scan.is_some()));
        assert!(!h.app.scan_waiting(&h.id));
        assert!(h.app.toast_text().is_none());
        assert!(!h.world.adapters()[0].sent().contains(&"cancel_pairing"));
        h.app.act(Action::Pair(1));
        h.until(|h| h.app.pairing_prompt().is_some());
        assert_eq!(h.pairing().unwrap().candidate, 1);
        assert!(h.app.device(&h.id, 13).is_some());
        h.app.jobs.remove(&(slot, Ticket::MAX - 1));
        h.app.fleet.cancel.set(false);
        h.close();
    }

    #[test]
    fn closing_after_known_completion_is_silent_and_keeps_the_device() {
        for action in [Action::CancelPairing, Action::Cancel] {
            let mut h = Harness::new();
            h.pair();
            h.keep_pair_result();
            assert!(h.world.adapters()[0].complete_pairing());
            h.until(|h| !h.kept.is_empty());
            assert_eq!(h.phase(), Some(add::Phase::Pairing));
            h.app.act(action);
            assert!(h.app.dialog().is_none());
            assert!(h.app.add().is_none());
            h.until(|h| !h.app.running(&h.id, |k| matches!(k, Kind::CancelPairing)));
            h.release();
            assert!(h.app.dialog().is_none());
            assert!(h.app.add().is_none());
            assert!(h.app.toast_text().is_none());
            assert!(h.app.note(&Spot::Dialog).is_none());
            assert!(h.app.device(&h.id, 13).is_some());
            assert!(!h.world.adapters()[0].sent().contains(&"cancel_pairing"));
            h.close();
        }
    }

    #[test]
    fn cancellation_refusals_after_close_are_silent_and_keep_the_late_device() {
        for action in [Action::CancelPairing, Action::Cancel] {
            for code in [p::ErrorCode::Busy, p::ErrorCode::NoPrompt] {
                let mut h = Harness::new();
                h.pair();
                h.world.adapters()[0].0.lock().unwrap().refuse.insert(
                    "cancel_pairing",
                    p::Error {
                        code: code as i32,
                        ..Default::default()
                    },
                );
                h.app.act(action.clone());
                assert!(h.app.dialog().is_none());
                assert!(h.app.add().is_none());
                assert!(h.app.running(&h.id, |k| matches!(k, Kind::CancelPairing)));
                h.keep = h
                    .app
                    .jobs
                    .iter()
                    .find(|(_, j)| matches!(j.kind, Kind::CancelPairing))
                    .map(|(key, _)| *key);
                h.until(|h| !h.kept.is_empty());
                assert!(matches!(&h.kept[0], Msg::Fleet(_, event)
                    if matches!(&**event, Event::Done { result: Err(e), .. }
                        if e.dongle.as_ref().is_some_and(|error| error.code() == code))));
                h.keep_pair_result();
                assert!(h.world.adapters()[0].complete_pairing());
                h.until(|h| h.kept.len() == 2);
                assert!(h.app.running(&h.id, |k| matches!(k, Kind::CancelPairing)));
                let mut device = h.app.device(&h.id, 13).unwrap().d.clone();
                device.state = p::DeviceState::Disconnected as i32;
                h.world.adapters()[0].event(p::event::Kind::Device(device));
                h.until(|h| {
                    h.app
                        .device(&h.id, 13)
                        .is_some_and(|d| d.d.state() == p::DeviceState::Disconnected)
                });
                h.release();
                assert!(h.app.dialog().is_none());
                assert!(h.app.add().is_none());
                assert!(h.app.toast_text().is_none());
                assert!(h.app.note(&Spot::Dialog).is_none());
                assert_eq!(
                    h.app.device(&h.id, 13).unwrap().d.state(),
                    p::DeviceState::Disconnected
                );
                assert!(!h.world.adapters()[0].sent().contains(&"unpair_device"));
                assert_eq!(
                    h.world.adapters()[0]
                        .sent()
                        .iter()
                        .filter(|command| **command == "cancel_pairing")
                        .count(),
                    1
                );
                h.close();
            }
        }
    }

    #[test]
    fn a_pairing_saved_before_an_announced_end_connects_on_the_replacement() {
        let mut h = Harness::new();
        h.pair_disconnected();
        h.end();
        assert_eq!(h.app.dialog(), Some(&Dialog::AddDevice));
        assert_eq!(h.phase(), Some(add::Phase::Paired));
        assert!(h.app.adapter(&h.id).is_some());
        assert!(h.app.devices().iter().any(|d| d.d.id == 13));
        h.set_paired(|d| d.state = p::DeviceState::Connected as i32);
        h.reopen();
        h.until(|h| h.app.device(&h.id, 13).is_some_and(|d| d.connected()));
        assert_eq!(h.phase(), Some(add::Phase::Paired));
        h.app.act(Action::Done);
        assert!(h.app.dialog().is_none());
        assert_eq!(h.app.shown(), Page::Device(h.id.clone(), 13));
        h.close();
    }

    #[test]
    fn a_confirmed_pairing_stays_paired_across_an_announced_end() {
        let mut h = Harness::new();
        h.pair();
        assert!(h.world.adapters()[0].complete_pairing());
        h.until(|h| h.phase() == Some(add::Phase::Paired));
        h.end();
        assert_eq!(h.app.dialog(), Some(&Dialog::AddDevice));
        assert_eq!(h.phase(), Some(add::Phase::Paired));
        assert!(h.app.devices().iter().any(|d| d.d.id == 13));
        h.close();
    }

    #[test]
    fn a_pairing_result_after_an_announced_end_is_kept() {
        let mut h = Harness::new();
        h.pair();
        h.keep_pair_result();
        assert!(h.world.adapters()[0].complete_pairing());
        h.end();
        assert_eq!(h.app.dialog(), Some(&Dialog::AddDevice));
        assert_eq!(h.phase(), Some(add::Phase::Pairing));
        h.release();
        assert_eq!(h.phase(), Some(add::Phase::Paired));
        assert_eq!(h.pairing().unwrap().device, Some(13));
        h.reopen();
        h.until(|h| h.app.adapter(&h.id).is_some_and(|a| a.ready));
        assert_eq!(h.phase(), Some(add::Phase::Paired));
        h.close();
    }

    #[test]
    fn a_pairing_without_its_done_event_never_reports_success() {
        let mut h = Harness::new();
        h.pair();
        h.keep_pair_result();
        h.end();
        assert_eq!(h.phase(), Some(add::Phase::Pairing));
        h.release();
        assert!(h.app.dialog().is_none());
        assert!(h.app.add().is_none());
        h.close();
    }

    #[test]
    fn completed_pairing_finishes_without_a_device_id_in_production() {
        let mut h = Harness::new();
        h.pair();
        h.world.adapters()[0].event(p::event::Kind::Pairing(p::Pairing {
            candidate: 3,
            step: Some(p::pairing::Step::Error(p::Error {
                code: p::ErrorCode::StorageFailed as i32,
                reason: p::ErrorReason::Storage as i32,
                outcome: p::ErrorOutcome::Complete as i32,
                related: vec![p::Error {
                    code: p::ErrorCode::StorageFailed as i32,
                    outcome: p::ErrorOutcome::NoEffect as i32,
                    ..Default::default()
                }],
            })),
        }));
        let expected = if cfg!(debug_assertions) {
            add::Phase::Failed
        } else {
            add::Phase::Paired
        };
        h.until(|h| h.phase() == Some(expected));
        assert!(h.pairing().unwrap().device.is_none());
        assert_eq!(
            h.pairing().unwrap().message.is_some(),
            cfg!(debug_assertions)
        );
        assert!(!h.world.adapters()[0].sent().contains(&"cancel_pairing"));
        h.close();
    }

    #[test]
    fn a_pairing_error_after_an_announced_end_is_reported() {
        let mut h = Harness::new();
        h.pair();
        h.keep_pair_result();
        h.world.adapters()[0].event(p::event::Kind::Pairing(p::Pairing {
            candidate: 3,
            step: Some(p::pairing::Step::Error(p::Error {
                code: p::ErrorCode::Cancelled as i32,
                ..Default::default()
            })),
        }));
        h.until(|h| !h.kept.is_empty());
        h.end();
        h.release();
        assert_eq!(h.phase(), Some(add::Phase::Cancelled));
        h.close();
    }

    #[test]
    fn rejecting_pin_and_passkey_returns_to_selection_and_waits_for_the_pair_job() {
        for kind in [p::CodeKind::Pin, p::CodeKind::Passkey] {
            let mut h = Harness::new();
            h.enter_code(kind);
            h.keep_pair_result();
            let (slot, ticket) = h.keep.unwrap();
            let scans = h.world.adapters()[0]
                .sent()
                .iter()
                .filter(|c| **c == "start_scan")
                .count();
            h.app.act(Action::Reject);
            h.until(|h| h.pairing().is_none());
            assert_eq!(h.app.dialog(), Some(&Dialog::AddDevice));
            let add = h.app.add().unwrap();
            assert_eq!(add.adapter.as_deref(), Some(h.id.as_str()));
            assert!(add.error.is_none());
            assert!(add.scan.is_none());
            assert!(!add.starting);
            assert!(h.world.adapters()[0].sent().contains(&"reject_prompt"));
            assert!(!h.world.adapters()[0].sent().contains(&"cancel_pairing"));
            h.app.act(Action::Pair(1));
            h.app.act(Action::ScanAgain);
            assert!(h.pairing().is_none());
            h.app.act(Action::Cancel);
            h.app.act(Action::AddDeviceTo(h.id.clone()));
            assert!(h.pairing().is_none());
            assert!(!h.app.add().unwrap().starting);
            assert!(h.app.scan_waiting(&h.id));
            assert_eq!(
                h.world.adapters()[0]
                    .sent()
                    .iter()
                    .filter(|c| **c == "start_scan")
                    .count(),
                scans
            );
            h.release();
            h.until(|h| h.app.add().is_some_and(|a| !a.starting && a.scan.is_some()));
            assert!(!h.app.scan_waiting(&h.id));
            assert!(h.pairing().is_none());
            assert!(h.app.add().unwrap().error.is_none());
            h.app.act(Action::Pair(1));
            h.until(|h| h.app.pairing_prompt().is_some());
            h.app.update(Msg::Fleet(
                slot,
                Box::new(Event::Done {
                    session: 0,
                    ticket,
                    result: Err(Error::code(p::ErrorCode::Rejected, Some("pair start"))),
                }),
            ));
            assert_eq!(h.phase(), Some(add::Phase::Pairing));
            assert_eq!(h.pairing().unwrap().candidate, 1);
            assert!(h.pairing().unwrap().prompt_error.is_none());
            h.close();
        }
    }

    #[test]
    fn rejecting_code_waits_for_acknowledgement_when_the_pair_result_arrives_first() {
        let mut h = Harness::new();
        h.enter_code(p::CodeKind::Pin);
        h.app.act(Action::Reject);
        h.keep_answer_result();
        h.until(|h| !h.app.running(&h.id, |k| matches!(k, Kind::Pair)));
        assert_eq!(h.phase(), Some(add::Phase::Pairing));
        h.release();
        assert!(h.pairing().is_none());
        assert_eq!(h.app.dialog(), Some(&Dialog::AddDevice));
        h.close();
    }

    #[test]
    fn a_refused_code_rejection_keeps_the_prompt_and_actual_pairing_failure() {
        let mut h = Harness::new();
        h.enter_code(p::CodeKind::Pin);
        h.world.adapters()[0].0.lock().unwrap().refuse.insert(
            "reject_prompt",
            p::Error {
                code: p::ErrorCode::Busy as i32,
                ..Default::default()
            },
        );
        h.app.act(Action::Reject);
        h.until(|h| h.pairing().is_some_and(|p| p.prompt_error.is_some()));
        assert_eq!(h.phase(), Some(add::Phase::Pairing));
        h.world.adapters()[0].event(p::event::Kind::Pairing(p::Pairing {
            candidate: 1,
            step: Some(p::pairing::Step::Error(p::Error {
                code: p::ErrorCode::AuthFailed as i32,
                ..Default::default()
            })),
        }));
        h.until(|h| h.phase() == Some(add::Phase::Failed));
        assert!(h.pairing().unwrap().message.is_some());
        h.close();
    }

    #[test]
    fn a_pairing_error_stays_visible_when_its_prompt_disappears() {
        let mut h = Harness::new();
        h.enter_code(p::CodeKind::Pin);
        let mut pairing = h.pairing().unwrap().clone();
        pairing.prompt_error = Some("The adapter couldn't reject the pairing.".into());
        h.app.states.get_mut(&pairing.slot).unwrap().pairing = None;
        h.app.follow(pairing);
        assert_eq!(
            h.pairing().unwrap().prompt_error.as_deref(),
            Some("The adapter couldn't reject the pairing.")
        );
        h.close();
    }

    #[test]
    fn a_compare_answer_reports_that_its_prompt_is_gone() {
        for (action, command) in [
            (Action::Reject, "reject_prompt"),
            (Action::Accept, "accept_prompt"),
        ] {
            let mut h = Harness::new();
            h.pair_candidate(2);
            h.world.adapters()[0].0.lock().unwrap().refuse.insert(
                command,
                p::Error {
                    code: p::ErrorCode::NoPrompt as i32,
                    ..Default::default()
                },
            );
            h.app.act(action);
            h.until(|h| h.pairing().is_some_and(|p| p.prompt_error.is_some()));
            assert_eq!(h.phase(), Some(add::Phase::Pairing));
            let pairing = h.pairing().unwrap().clone();
            h.app.states.get_mut(&pairing.slot).unwrap().pairing = None;
            h.app.follow(pairing);
            assert!(h.pairing().unwrap().prompt_error.is_some());
            h.close();
        }
    }

    #[test]
    fn codes_differ_keeps_the_pairing_failure_result() {
        let mut h = Harness::new();
        h.pair_candidate(2);
        h.app.act(Action::Reject);
        h.until(|h| h.phase() == Some(add::Phase::Failed));
        assert!(h.pairing().unwrap().message.is_some());
        h.close();
    }

    #[test]
    fn a_failed_reject_acknowledgement_preserves_an_earlier_pairing_failure() {
        let mut h = Harness::new();
        h.enter_code(p::CodeKind::Passkey);
        h.world.adapters()[0].0.lock().unwrap().refuse.insert(
            "reject_prompt",
            p::Error {
                code: p::ErrorCode::Busy as i32,
                ..Default::default()
            },
        );
        h.app.act(Action::Reject);
        h.keep_answer_result();
        // The refusal leaves the prompt pending, so the device can still fail pairing.
        h.until(|h| !h.kept.is_empty());
        h.world.adapters()[0].event(p::event::Kind::Pairing(p::Pairing {
            candidate: 1,
            step: Some(p::pairing::Step::Error(p::Error {
                code: p::ErrorCode::AuthFailed as i32,
                ..Default::default()
            })),
        }));
        h.until(|h| !h.app.running(&h.id, |k| matches!(k, Kind::Pair)));
        assert_eq!(h.phase(), Some(add::Phase::Pairing));
        h.release();
        assert_eq!(h.phase(), Some(add::Phase::Failed));
        assert!(h.pairing().unwrap().message.is_some());
        h.close();
    }

    #[test]
    fn rejection_does_not_send_an_old_prompt_to_a_replacement_session() {
        let mut h = Harness::new();
        h.enter_code(p::CodeKind::Pin);
        h.keep_pair_result();
        h.end();
        h.reopen();
        h.until(|h| h.app.adapter(&h.id).is_some_and(|a| a.ready));
        h.app.act(Action::Reject);
        assert!(!h.world.adapters()[0].sent().contains(&"reject_prompt"));
        assert!(h.pairing().unwrap().prompt_error.is_some());
        assert_eq!(h.phase(), Some(add::Phase::Pairing));
        h.release();
        assert!(h.app.dialog().is_none());
        h.close();
    }

    #[test]
    fn a_reject_acknowledgement_after_reconnect_dismisses_its_original_pairing() {
        let mut h = Harness::new();
        h.enter_code(p::CodeKind::Pin);
        h.app.act(Action::Reject);
        h.keep_answer_result();
        h.until(|h| !h.app.running(&h.id, |k| matches!(k, Kind::Pair)));
        h.end();
        h.reopen();
        h.until(|h| h.app.adapter(&h.id).is_some_and(|a| a.ready));
        h.release();
        assert!(h.pairing().is_none());
        assert_eq!(h.app.dialog(), Some(&Dialog::AddDevice));
        assert!(h.app.add().unwrap().error.is_none());
        assert_eq!(
            h.world.adapters()[0]
                .sent()
                .iter()
                .filter(|c| **c == "reject_prompt")
                .count(),
            1
        );
        h.close();
    }

    #[test]
    fn an_expired_hold_closes_add_device() {
        let mut h = Harness::new();
        h.pair_disconnected();
        h.end();
        assert_eq!(h.app.dialog(), Some(&Dialog::AddDevice));
        h.expire();
        assert!(h.app.dialog().is_none());
        assert!(h.app.add().is_none());
        h.close();
    }

    #[test]
    fn an_unannounced_loss_closes_add_device() {
        let mut h = Harness::new();
        h.pair_disconnected();
        h.world.adapters()[0].unplug();
        h.until(|h| h.app.session_of(&h.id).is_none());
        assert!(h.app.held.is_empty());
        assert!(h.app.dialog().is_none());
        h.close();
    }

    #[test]
    fn cancelling_during_a_hold_ignores_the_late_result() {
        let mut h = Harness::new();
        h.pair();
        h.keep_pair_result();
        assert!(h.world.adapters()[0].complete_pairing());
        h.end();
        h.app.act(Action::CancelPairing);
        assert!(h.app.dialog().is_none());
        assert!(!h.world.adapters()[0].sent().contains(&"cancel_pairing"));
        h.release();
        assert!(h.app.add().is_none());
        h.close();
    }

    #[test]
    fn a_late_result_leaves_a_new_pairing_alone() {
        let mut h = Harness::new();
        h.pair();
        h.keep_pair_result();
        assert!(h.world.adapters()[0].complete_pairing());
        h.end();
        h.app.act(Action::Cancel);
        h.reopen();
        h.until(|h| h.app.adapter(&h.id).is_some_and(|a| a.ready));
        h.app.act(Action::AddDeviceTo(h.id.clone()));
        h.until(|h| {
            h.app.add().is_some_and(|a| !a.starting)
                && h.app.state_of(&h.id).unwrap().candidate(1).is_some()
        });
        h.app.act(Action::Pair(1));
        h.until(|h| h.app.pairing_prompt().is_some());
        h.release();
        let pairing = h.pairing().unwrap();
        assert_eq!(pairing.candidate, 1);
        assert_eq!(pairing.phase, add::Phase::Pairing);
        assert_eq!(pairing.device, None);
        h.close();
    }

    #[test]
    fn a_replacement_that_cannot_connect_keeps_the_confirmed_pairing() {
        let cases: [fn(&mut p::Device); 2] = [
            |d| {
                d.enabled = false;
                d.inactive = Some(p::InactiveReason::Disabled as i32);
                d.state = p::DeviceState::Disconnected as i32;
            },
            |d| {
                d.state = p::DeviceState::Disconnected as i32;
                d.errors = vec![p::Error {
                    code: p::ErrorCode::ConnectionFailed as i32,
                    ..Default::default()
                }];
            },
        ];
        for change in cases {
            let mut h = Harness::new();
            h.pair_disconnected();
            h.end();
            h.set_paired(change);
            h.reopen();
            h.until(|h| h.app.adapter(&h.id).is_some_and(|a| a.ready));
            assert_eq!(h.phase(), Some(add::Phase::Paired));
            assert!(h.pairing().unwrap().message.is_none());
            h.close();
        }
    }

    #[test]
    fn an_identified_replacement_without_its_own_event_does_not_inherit_a_hold() {
        let mut h = Harness::new();
        h.end();
        h.world.adapters()[0].0.lock().unwrap().status.ready = false;
        h.reopen();
        h.until(|h| h.app.session_of(&h.id).is_some());
        assert!(h.app.session_of(&h.id).unwrap().retained_until.is_some());
        assert!(h.app.state_of(&h.id).unwrap().session_ending.is_none());
        h.world.adapters()[0].unplug();
        h.until(|h| h.app.session_of(&h.id).is_none());
        assert!(h.app.held.is_empty());
        assert!(h.app.devices().is_empty());
        assert_eq!(h.app.shown(), Page::Overview);
        h.close();
    }

    #[test]
    fn a_framed_status_identity_mismatch_discards_old_pages() {
        let mut h = Harness::new();
        h.end();
        let replacement = "SIMULATED0000002";
        h.world.adapters()[0].0.lock().unwrap().status.id = replacement.into();
        h.reopen();
        h.until(|h| h.app.adapter(replacement).is_some_and(|a| a.ready));
        assert!(h.app.adapter(&h.id).is_none());
        assert!(h.app.held.is_empty());
        assert_eq!(h.app.shown(), Page::Overview);
        h.close();
    }

    #[test]
    fn a_framed_faulted_replacement_survives_the_old_data_deadline() {
        let mut h = Harness::new();
        h.end();
        let deadline = h.app.held[&h.id].until;
        {
            let mut sim = h.world.adapters()[0].0.lock().unwrap();
            sim.status.ready = false;
            sim.status.errors = vec![p::Error {
                code: p::ErrorCode::StorageFailed as i32,
                ..Default::default()
            }];
        }
        h.reopen();
        h.until(|h| h.app.session_of(&h.id).is_some());
        assert_eq!(
            h.app.session_of(&h.id).unwrap().retained_until,
            Some(deadline)
        );
        assert!(!h.app.devices().is_empty());
        assert_eq!(h.app.shown(), Page::Device(h.id.clone(), 1));
        h.app.session_of_mut(&h.id).unwrap().retained_until =
            Some(Instant::now() - Duration::from_secs(1));
        h.app.tick();
        assert!(h.app.devices().is_empty());
        assert_eq!(h.app.shown(), Page::Overview);
        let adapter = h.app.adapter(&h.id).unwrap();
        assert!(adapter.connected());
        assert!(!adapter.faults().is_empty());
        h.close();
    }
}
