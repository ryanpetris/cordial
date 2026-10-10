//! Serial command handling and asynchronous Bluetooth operations.
//!
//! The application is driven by two loops that take turns. The priority loop handles what input
//! needs right now: radio events, connection setup with its profile loads, link timers and output,
//! and HID forwarding (`event`, `operate`). The secondary loop handles everything that can wait:
//! client commands, events, configuration editors and background storage work (`begin`,
//! `proceed`, `save`, `work`). Most calls the secondary loop makes read or write about one record,
//! and one that writes may count free space again, so the priority loop usually waits for at most
//! that.
//!
//! Records nothing needs on flash at once, such as a configuration editor's edits, discovered
//! layouts and first-connection setup, are saved once keyboard and mouse input pauses
//! ([`crate::deferred`]), one per call. Everything waiting is saved in one call before USB
//! enumerates again, before the bootloader and when an editor is released, and every dirty record
//! before free space is checked for a new profile, a pairing or a new setting. Saves that commands
//! promise, and bonds, are written before the command responds.
//!
//! A command that reads many records, such as a listing, proceeds one record per `proceed`, and no
//! other secondary work runs until it has responded. A listing answers one page; a page of a list
//! held in memory is answered in one step. Every other command validates, acts and responds at
//! once. Work that outlives a command (scanning, pairing, matching a pairing's bond with a saved
//! device, an unpair waiting for its link to close, settings jobs) lives here and reports its
//! progress through events. Adapter, device, profile, scan and pairing events carry the complete
//! current state of one thing; settings, warnings and rules events carry what changed since the
//! client last listed or was told. Changed things are marked dirty and written one frame at a
//! time when the serial output is free.
use alloc::{boxed::Box, collections::VecDeque, string::String, vec::Vec};

use cordial_protocol::{self as p, event::Kind as Ev, request::Command, response::Result as R};

use crate::{
    bluetooth::{Bluetooth, Event},
    control::Session,
    deferred::{self, Record},
    devices::{self, Backoff, Peer, Policies, Policy},
    interfaces::{self, Interface},
    link::LinkId,
    manager::Manager,
    model::{
        errors::{DeviceWarning, ErrorCode as Error},
        identifiers::{ConnectionState, Transport},
        link::{DeviceKind, PromptMethod},
        settings::SettingKey,
    },
    profiles,
    settings::{Catalog, Change},
    storage::{Preferences, RecordStore},
    wire,
};

pub struct Build {
    /// Development firmware: the development commands are available.
    pub development: bool,
    pub version: &'static str,
    /// Hardware configuration the firmware was built for.
    pub board: &'static str,
    pub default_adapter_name: &'static str,
    pub adapter_id: String,
    /// Bytes of memory for loaded profiles, or `None` on a board without profile support.
    pub profile_memory_budget: Option<u32>,
    /// Production composition supplies None and does not link the entry function.
    pub bootloader: Option<Bootloader>,
}
pub struct Bootloader {
    pub enter: fn() -> !,
}

struct Candidate {
    id: u32,
    peer: Peer,
    address: Peer,
    kind: DeviceKind,
    name: Box<str>,
    rssi: Option<i32>,
    dirty: bool,
}

struct Scan {
    token: u64,
    classic: bool,
    ble: bool,
    deadline: u64,
}

struct Prompt {
    method: PromptMethod,
    deadline: u64,
    answered: bool,
}

struct Pair {
    candidate: u32,
    link: Option<LinkId>,
    address: Peer,
    expected: Option<Peer>,
    /// A native bond to remove once the attempt's link has closed.
    cleanup: Option<Peer>,
    deadline: u64,
    name: Box<str>,
    /// Set once the attempt is ending, with the error it ends with.
    cancelling: Option<Error>,
    prompt: Option<Prompt>,
    /// The session that started the attempt has ended, so its remaining events go nowhere.
    quiet: bool,
    /// The stack's bonds have been synced since the attempt started, so its link can open.
    synced: bool,
    /// The saved device with the candidate's identity, found when the attempt started.
    saved: Option<u64>,
    /// The bond the stack has saved for the attempt, waiting to be matched with a saved device.
    bonding: Option<Bonding>,
}

/// A pairing's new bond. Saved device records are read one per step in the secondary loop to find
/// the device with its identity, such as a device that paired again from a private address.
struct Bonding {
    identity: Peer,
    scan: Records,
    /// A resident device or the saved device the attempt started with has been looked up.
    direct: bool,
}

/// Saved records of one kind, visited in ascending ID order one step at a time.
struct Records {
    kind: u8,
    after: u64,
    page: VecDeque<u64>,
    end: bool,
}
/// What one step of a scan did.
enum Visit {
    /// The next saved ID, to read in the same step.
    Record(u64),
    /// Read the next page of IDs.
    Page,
    /// Every record has been visited.
    Done,
}
/// Saved IDs a scan reads from the record directory at a time.
const SCAN_PAGE: usize = 16;
impl Records {
    fn new(kind: u8) -> Self {
        Self {
            kind,
            after: 0,
            page: VecDeque::new(),
            end: false,
        }
    }
    async fn next<S: RecordStore>(&mut self, store: &mut S) -> Result<Visit, Error> {
        if let Some(id) = self.page.pop_front() {
            self.after = id;
            return Ok(Visit::Record(id));
        }
        if self.end {
            return Ok(Visit::Done);
        }
        let ids = store
            .record_ids(self.kind, self.after, SCAN_PAGE)
            .await
            .map_err(|_| Error::StorageFailed)?;
        self.end = ids.len() < SCAN_PAGE;
        self.page = ids.into();
        Ok(Visit::Page)
    }
}

/// One page of a device or profile listing, read one record per step.
struct Listing {
    /// The last ID the listing has passed.
    cursor: u64,
    /// IDs of the current page still to read.
    ids: VecDeque<u64>,
    /// IDs remain after the current page.
    more: bool,
    /// A page has been read.
    read: bool,
}
enum Turn {
    Read(u64),
    Paged,
    /// The response is complete; whether nothing follows it.
    Finished(bool),
}
impl Listing {
    fn new(after: u32) -> Self {
        Self {
            cursor: after.into(),
            ids: VecDeque::new(),
            more: false,
            read: false,
        }
    }
    /// The next step of the listing. Pages that hold nothing to list are passed over until one
    /// does or the records end; `found` says whether the response holds anything yet.
    async fn turn<S: RecordStore>(
        &mut self,
        kind: u8,
        size: usize,
        found: bool,
        store: &mut S,
    ) -> Result<Turn, Error> {
        if let Some(id) = self.ids.pop_front() {
            self.cursor = id;
            return Ok(Turn::Read(id));
        }
        if self.read && (found || !self.more) {
            return Ok(Turn::Finished(!self.more));
        }
        let mut ids = store
            .record_ids(kind, self.cursor, size + 1)
            .await
            .map_err(|_| Error::StorageFailed)?;
        self.more = ids.len() > size;
        ids.truncate(size);
        self.ids = ids.into();
        self.read = true;
        Ok(Turn::Paged)
    }
}

/// A command that reads many records, waiting for its next step.
enum Pending {
    Devices(Listing, p::DeviceList),
    Profiles(Listing, p::ProfileList),
    /// A page of a disconnected device's saved settings, once its policy has been read.
    Settings(Policy, Option<p::SettingRef>),
    /// A page of a profile's rules, once its record has been read.
    Rules(u32, Option<profiles::Usage>),
    Files(FileListing),
    /// Deleting a profile once no saved device refers to it.
    DeleteProfile(u64, Records),
    /// Starting a pairing once the saved device with the candidate's identity, if any, is found.
    Pairing(PairingStart, Records),
}
struct PairingStart {
    candidate: u32,
    peer: Peer,
    address: Peer,
    name: Box<str>,
}

/// A page of a directory listing. Entries are read one per step, keeping the first ones by name
/// after `after`, whatever order the store gives them in.
struct FileListing {
    path: String,
    after: String,
    index: usize,
    /// At most one more entry than a page holds, in ascending name order.
    page: Vec<p::FileEntry>,
}

/// Background storage work that takes several steps.
enum Work {
    /// Making enabled devices that fit in the stack resident, then syncing the stack's bonds when
    /// the resident set changed.
    Fill {
        scan: Records,
        changed: bool,
    },
    Bonds(BondSync),
    /// Removing the references to a lost profile, then deleting it.
    LostProfile(u64, Option<Records>),
}
/// Syncing the stack's bonds with the resident entries.
enum BondSync {
    /// Reading which bonds the stack holds.
    Inventory,
    /// Removing those no resident entry or link uses.
    Forget(Vec<Peer>),
    /// Loading each resident entry's saved bond, from this slot on.
    Import(usize),
}

/// Background settings work a device is waiting for.
#[derive(Clone, Copy, Default)]
struct Jobs {
    /// Re-read device information, before `read`.
    information: bool,
    /// Re-read settings.
    read: bool,
    apply: bool,
}

/// The settings and warnings of one device that a client may hold.
#[derive(Default)]
struct Reported {
    id: u64,
    /// Settings, as [`key_bit`]s.
    keys: u32,
    warnings: Vec<DeviceWarning>,
}

/// A pending event that reads records, by its kind and its device or profile ID. Each backs off
/// on its own, so one event's success never shortens another's grown backoff.
#[derive(Clone, Copy, PartialEq, Eq)]
enum EventRead {
    Device(u64),
    Settings(u64),
    Profile(u64),
    Rules(u64),
}

/// The bit standing for setting `key` in a set of settings.
fn key_bit(key: SettingKey) -> u32 {
    SettingKey::ALL
        .iter()
        .position(|k| *k == key)
        .map_or(0, |i| 1 << i)
}
/// Every setting.
const ALL_KEYS: u32 = u32::MAX;
const _: () = assert!(SettingKey::ALL.len() <= 32);

/// The profile a configuration interface's editor uses, loaded while the editor sends packets.
/// Edits change the loaded table at once; they are handed to storage as one change once the
/// editor pauses.
pub(crate) struct Editor {
    pub interface: Interface,
    pub profile: u64,
    pub map: profiles::Map,
    pub last: u64,
    /// When the first edit not yet handed to storage was made, and when the last one was.
    pub edited: Option<(u64, u64)>,
    /// How the last write of the edits straight from the table, made when there was no memory
    /// to hand them over, failed.
    pub failure: Option<crate::storage::Error>,
    /// The backoff of those writes.
    pub retry: devices::Backoff,
}

const DEVICE: u8 = 1;
const SETTINGS: u8 = 2;
const WARNINGS: u8 = 4;
const PROFILE: u8 = 1;
const RULES: u8 = 2;

/// Entries in one page of a list held in memory. Pages of records read from flash use the
/// page sizes of their records.
const SETTINGS_PAGE: usize = 8;
const WARNINGS_PAGE: usize = 16;
const RULES_PAGE: usize = 32;
const FEATURES_PAGE: usize = 32;
const FILES_PAGE: usize = 16;

const PAIR_TIMEOUT_MS: u64 = 120_000;
const PROMPT_TIMEOUT_MS: u64 = 30_000;
const CONNECT_TIMEOUT_MS: u64 = 30_000;
const PAGE_BUSY_MS: u64 = 1000;
const SCAN_DEFAULT_SECONDS: u32 = 10;
const SCAN_MAX_SECONDS: u32 = 60;
/// An editor's profile is released after this long without a packet.
pub const EDITOR_IDLE_MS: u64 = 5000;
/// An editor's edits are handed to storage this long after its last edit packet,
pub const EDITOR_BATCH_MS: u64 = 500;
/// and at least this often while edit packets keep arriving.
pub const EDITOR_BATCH_MAX_MS: u64 = 2000;

pub struct Application {
    pub manager: Manager,
    pub serial: Session,
    build: Build,
    candidates: Vec<Candidate>,
    candidate_seq: u32,
    scan: Option<Scan>,
    scan_seq: u64,
    radio_scan: Option<(u64, bool, bool)>,
    truncated: bool,
    pair: Option<Pair>,
    pairing: Option<p::Pairing>,
    pairing_dirty: bool,
    scan_done: Option<p::ScanDone>,
    /// Devices whose unpair waits for their link to close.
    unpairing: Vec<u64>,
    jobs: Vec<(u64, Jobs)>,
    /// Pending device, settings and warnings events, by device ID.
    dirty: Vec<(u64, u8)>,
    /// Settings whose state may have changed since the last settings event, by device ID, as
    /// [`key_bit`]s.
    touched: Vec<(u64, u32)>,
    /// What the client may hold of the settings and warnings of each device with a connection,
    /// from listings and events since the session started. An entry stays until the events that
    /// follow the end of the connection have been written, so it is bounded by the connections
    /// and nothing is kept for other devices.
    reported: Vec<Reported>,
    removed: VecDeque<u64>,
    adapter_dirty: bool,
    /// The store generation free space was last counted at for the status. Writes change the
    /// generation, and a later background step counts again.
    counted: Option<u64>,
    /// Pending profile record and rules events, by profile ID.
    profile_dirty: Vec<(u64, u8)>,
    /// Rule inputs that may have changed since the last rules event, by profile ID.
    touched_rules: Vec<(u64, Vec<profiles::Usage>)>,
    profile_removed: VecDeque<u64>,
    pub(crate) editor: Option<Editor>,
    pub usb_reconnect: bool,
    /// The readiness last reported, so any change sends an adapter event.
    reported_ready: bool,
    /// The profile memory in use last reported.
    reported_memory: usize,
    /// Loaded profiles were released since devices that did not fit last tried again.
    profiles_released: bool,
    /// The dirty entry whose events go first next time, so devices take turns.
    next_dirty: usize,
    /// The dirty profile entry whose events go first next time, so profiles take turns.
    next_profile: usize,
    /// Scan results go before device events next time, so neither starves the other.
    scan_turn: bool,
    reboot_at: Option<u64>,
    /// Records saved once input pauses.
    unsaved: deferred::Dirty,
    lost_retry: Backoff,
    /// Pending events whose reads failed, each with its own backoff. An entry stays only while
    /// its event is pending.
    event_retries: Vec<(EventRead, Backoff)>,
    /// Filling the resident set and syncing the stack's bonds.
    bonds_retry: Backoff,
    /// The command waiting for its next step.
    pending: Option<Pending>,
    /// Background work in progress. It pauses while commands run.
    work: Option<Work>,
    /// The slot the next profile load retry starts from.
    retry_from: usize,
    /// Memory was released before the current pass of profile load retries started.
    retry_released: bool,
    /// Background work goes before the next event.
    maintenance_turn: bool,
}

fn failure(code: Error) -> p::Error {
    wire::error(code, None, false)
}

fn bad_args() -> p::Error {
    failure(Error::InvalidArgs)
}

fn capacity(reason: p::CapacityReason) -> p::Error {
    wire::error(Error::Capacity, Some(reason), false)
}

fn wire_usage(usage: profiles::Usage) -> p::Usage {
    p::Usage {
        usage_page: profiles::page(usage).into(),
        usage: profiles::id(usage).into(),
    }
}
/// The internal form of a wire usage; `None` when it is not a 16-bit page and usage.
fn usage(usage: &p::Usage) -> Option<profiles::Usage> {
    Some(profiles::usage(
        u16::try_from(usage.usage_page).ok()?,
        u16::try_from(usage.usage).ok()?,
    ))
    .filter(|u| *u != 0)
}
fn ranges(ranges: &[profiles::Range]) -> Vec<p::UsageRange> {
    ranges
        .iter()
        .map(|r| p::UsageRange {
            collection: (r.collection != 0).then(|| wire_usage(r.collection)),
            usage_page: r.page.into(),
            min: r.min.into(),
            max: r.max.into(),
        })
        .collect()
}
fn wire_rule(rule: profiles::Rule) -> p::ProfileRule {
    use p::profile_rule::{Effect, Output, Remap, Scale};
    p::ProfileRule {
        input: Some(wire_usage(rule.input)),
        effect: Some(match rule.effect {
            profiles::Effect::Remap(outputs) => Effect::Remap(Remap {
                outputs: outputs
                    .into_iter()
                    .map(|o| Output {
                        usage: Some(wire_usage(o.usage)),
                        collection: Some(wire_usage(o.collection)),
                    })
                    .collect(),
            }),
            profiles::Effect::Scale(numerator, denominator) => Effect::Scale(Scale {
                numerator,
                denominator,
            }),
        }),
    }
}
/// The internal form of a wire rule, before it is normalized.
fn rule(value: &p::ProfileRule) -> Option<profiles::Rule> {
    use p::profile_rule::Effect;
    let input = usage(value.input.as_ref()?)?;
    let effect = match value.effect.as_ref()? {
        Effect::Remap(remap) => profiles::Effect::Remap(
            remap
                .outputs
                .iter()
                .map(|o| {
                    Some(profiles::Output {
                        usage: usage(o.usage.as_ref()?)?,
                        collection: match &o.collection {
                            Some(c) => usage(c)?,
                            None => 0,
                        },
                    })
                })
                .collect::<Option<_>>()?,
        ),
        Effect::Scale(scale) => profiles::Effect::Scale(scale.numerator, scale.denominator),
    };
    Some(profiles::Rule { input, effect })
}
/// One page of the settings `catalog` holds, after `after`, and whether nothing follows it.
fn settings_page(catalog: &Catalog, after: Option<&p::SettingRef>) -> (Vec<p::Setting>, bool) {
    let mut settings = wire::settings(catalog);
    if let Some(after) = after {
        let after = (after.integration, after.key.as_bytes());
        settings.retain(|s| wire::setting_order(s) > after);
    }
    let end = settings.len() <= SETTINGS_PAGE;
    settings.truncate(SETTINGS_PAGE);
    (settings, end)
}

fn wire_interface(interface: Interface) -> p::ConfigurationInterface {
    match interface {
        Interface::Via => p::ConfigurationInterface::Via,
        Interface::Vial => p::ConfigurationInterface::Vial,
    }
}

impl Application {
    pub fn new(build: Build) -> Self {
        let mut manager = Manager::default();
        manager.profile_budget = build.profile_memory_budget.map(|b| b as usize);
        Self {
            manager,
            serial: Session::new(),
            build,
            candidates: Vec::new(),
            candidate_seq: 0,
            scan: None,
            scan_seq: 0,
            radio_scan: None,
            truncated: false,
            pair: None,
            pairing: None,
            pairing_dirty: false,
            scan_done: None,
            unpairing: Vec::new(),
            jobs: Vec::new(),
            dirty: Vec::new(),
            touched: Vec::new(),
            reported: Vec::new(),
            removed: VecDeque::new(),
            adapter_dirty: false,
            counted: None,
            profile_dirty: Vec::new(),
            touched_rules: Vec::new(),
            profile_removed: VecDeque::new(),
            editor: None,
            usb_reconnect: false,
            reported_ready: false,
            reported_memory: 0,
            profiles_released: false,
            next_dirty: 0,
            next_profile: 0,
            scan_turn: false,
            reboot_at: None,
            unsaved: deferred::Dirty::default(),
            lost_retry: Backoff::default(),
            event_retries: Vec::new(),
            bonds_retry: Backoff::default(),
            pending: None,
            work: None,
            retry_from: 0,
            retry_released: false,
            maintenance_turn: false,
        }
    }

    fn adapter_name(&self) -> &str {
        self.manager
            .preference
            .name
            .as_deref()
            .unwrap_or(self.build.default_adapter_name)
    }

    pub fn status<B: Bluetooth>(&self, radio: &B) -> p::Status {
        let supported = radio.capabilities();
        let enabled = self.manager.capabilities(radio);
        let mut info = Vec::new();
        let mut fact = |key: &str, value: p::value::Value| {
            info.push(p::Info {
                key: key.into(),
                value: Some(p::Value { value: Some(value) }),
            });
        };
        fact(
            p::keys::FIRMWARE_VERSION,
            p::value::Value::Text(self.build.version.into()),
        );
        fact(
            p::keys::BOARD_NAME,
            p::value::Value::Text(self.build.board.into()),
        );
        if self.development() {
            fact(p::keys::BUILD_DEVELOPMENT, p::value::Value::Bool(true));
        }
        if self.manager.storage_ready && (self.manager.storage_full() || self.manager.unsaved_full)
        {
            fact(p::keys::STORAGE_FULL, p::value::Value::Bool(true));
        }
        let profile_support = self.manager.profile_budget.map(|budget| p::ProfileSupport {
            remap_inputs: ranges(&profiles::REMAP_INPUTS),
            scale_inputs: ranges(&profiles::SCALE_INPUTS),
            remap_outputs: ranges(&profiles::REMAP_OUTPUTS),
            memory_budget: budget as u32,
            max_remap_outputs: profiles::MAX_REMAP_OUTPUTS as u32,
            max_layers: profiles::MAX_LAYERS as u32,
            memory_used: self.manager.profiles.used() as u32,
        });
        let configuration_interfaces = if self.manager.profiles_supported() {
            Interface::ALL
                .into_iter()
                .map(|interface| {
                    let saved = interfaces::preference(
                        &self.manager.preference.configuration_interfaces,
                        interface,
                    );
                    p::ConfigurationInterfaceSupport {
                        interface: wire_interface(interface) as i32,
                        enabled: saved.enabled,
                        profile: saved.profile.unwrap_or(0) as u32,
                        conflicts: interface
                            .conflicts()
                            .iter()
                            .map(|c| wire_interface(*c) as i32)
                            .collect(),
                    }
                })
                .collect()
        } else {
            Vec::new()
        };
        p::Status {
            id: self.build.adapter_id.clone(),
            name: self.adapter_name().into(),
            platform: wire::platform(self.manager.preference.host_platform) as i32,
            ready: self.manager.radio_ready && self.manager.storage_ready,
            transports: devices::Transports::ALL
                .into_iter()
                .filter(|t| supported.supports(*t))
                .map(|t| p::TransportSupport {
                    transport: wire::transport(t) as i32,
                    max_enabled: self
                        .manager
                        .storage_ready
                        .then(|| self.manager.max_enabled(t) as u32),
                    enabled: Some(enabled.supports(t)),
                })
                .collect(),
            info,
            profile_support,
            configuration_interfaces,
        }
    }

    fn development(&self) -> bool {
        cfg!(feature = "development") && self.build.development
    }

    /// Marks events to send for device `id`. A settings event covers every setting.
    fn mark(&mut self, id: u64, bits: u8) {
        if bits & SETTINGS != 0 {
            self.touch(id, ALL_KEYS);
        }
        match self.dirty.iter_mut().find(|(d, _)| *d == id) {
            Some((_, b)) => *b |= bits,
            None => self.dirty.push((id, bits)),
        }
    }

    /// Marks a settings event for the settings `keys` of device `id`.
    fn touch(&mut self, id: u64, keys: u32) {
        match self.touched.iter_mut().find(|(d, _)| *d == id) {
            Some((_, k)) => *k |= keys,
            None => self.touched.push((id, keys)),
        }
        match self.dirty.iter_mut().find(|(d, _)| *d == id) {
            Some((_, b)) => *b |= SETTINGS,
            None => self.dirty.push((id, SETTINGS)),
        }
    }

    /// Whether resident device `id` has a connection.
    fn live(&self, id: u64) -> bool {
        self.manager
            .find(id)
            .and_then(|s| self.manager.devices[s].as_ref())
            .is_some_and(|d| d.live.is_some())
    }

    /// What the client may hold of device `id`, while it is tracked. Tracking starts only for a
    /// device with a connection.
    fn reported(&mut self, id: u64) -> Option<&mut Reported> {
        let index = match self.reported.iter().position(|r| r.id == id) {
            Some(index) => index,
            None if self.live(id) => {
                self.reported.push(Reported {
                    id,
                    ..Reported::default()
                });
                self.reported.len() - 1
            }
            None => return None,
        };
        Some(&mut self.reported[index])
    }

    /// The devices whose settings and warnings are tracked for change events.
    pub fn tracked_devices(&self) -> usize {
        self.reported.len()
    }

    /// Stops tracking devices that have no connection and no pending events.
    fn release_reported(&mut self) {
        let mut i = 0;
        while i < self.reported.len() {
            let id = self.reported[i].id;
            if self.live(id) || self.dirty.iter().any(|(d, _)| *d == id) {
                i += 1;
            } else {
                self.reported.swap_remove(i);
            }
        }
    }

    /// Forgets the pending events of a device that is gone.
    fn forget_device(&mut self, id: u64) {
        self.dirty.retain(|(d, _)| *d != id);
        self.event_retries
            .retain(|(r, _)| *r != EventRead::Device(id) && *r != EventRead::Settings(id));
        self.touched.retain(|(d, _)| *d != id);
        self.reported.retain(|r| r.id != id);
    }

    fn mark_slot(&mut self, slot: usize, bits: u8) {
        if let Some(id) = self
            .manager
            .devices
            .get(slot)
            .and_then(|d| d.as_ref())
            .map(|d| d.id)
        {
            self.mark(id, bits);
        }
    }

    pub(crate) fn mark_profile(&mut self, id: u64, bits: u8) {
        match self.profile_dirty.iter_mut().find(|(p, _)| *p == id) {
            Some((_, b)) => *b |= bits,
            None => self.profile_dirty.push((id, bits)),
        }
    }

    /// Marks a rules event for the rules of profile `id` whose inputs are `inputs`.
    fn touch_rules(&mut self, id: u64, inputs: impl IntoIterator<Item = profiles::Usage>) {
        let mut inputs = inputs.into_iter().peekable();
        if inputs.peek().is_none() {
            return;
        }
        let index = match self.touched_rules.iter().position(|(p, _)| *p == id) {
            Some(index) => index,
            None => {
                self.touched_rules.push((id, Vec::new()));
                self.touched_rules.len() - 1
            }
        };
        let touched = &mut self.touched_rules[index].1;
        touched.extend(inputs);
        touched.sort_unstable();
        touched.dedup();
        self.mark_profile(id, RULES);
    }

    fn reply(&mut self, result: Option<R>) {
        self.serial.respond(p::Response { result });
    }

    fn fail(&mut self, error: p::Error) {
        self.serial.fail(error);
    }

    pub(crate) fn storage_error(&mut self, error: crate::storage::Error) -> p::Error {
        if error == crate::storage::Error::Unknown {
            self.manager.fail_storage();
            self.manager.write_uncertain = true;
            self.adapter_dirty = true;
        }
        match error {
            crate::storage::Error::Full => capacity(p::CapacityReason::Storage),
            crate::storage::Error::Missing => failure(Error::NotFound),
            error => wire::error(
                Error::StorageFailed,
                None,
                error == crate::storage::Error::Unknown,
            ),
        }
    }

    /// The saved policy of device `id` and its resident slot. A connected device's own policy is
    /// used once read. A device whose record turns out to be lost is deleted.
    async fn policy_of<S: RecordStore, B: Bluetooth>(
        &mut self,
        id: u32,
        store: &mut S,
        radio: &mut B,
    ) -> Result<(Policy, Option<usize>), p::Error> {
        self.policy_record(id, store, radio)
            .await
            .map(|(policy, slot, _)| (policy, slot))
    }

    /// As `policy_of`, with the bond of the saved record when it was read.
    async fn policy_record<S: RecordStore, B: Bluetooth>(
        &mut self,
        id: u32,
        store: &mut S,
        radio: &mut B,
    ) -> Result<(Policy, Option<usize>, Option<crate::bonds::Bond>), p::Error> {
        let id = u64::from(id);
        if id == 0 {
            return Err(failure(Error::NotFound));
        }
        let slot = self.manager.find(id);
        if let Some(policy) = slot
            .and_then(|s| self.manager.devices[s].as_ref())
            .and_then(|d| d.live.as_ref())
            .and_then(|l| l.policy.clone())
        {
            return Ok((policy, slot, None));
        }
        if !self.manager.storage_ready {
            return Err(failure(Error::RadioUnavailable));
        }
        match (Policies { store }).load_record(id).await {
            Ok((policy, bond)) => Ok((policy, slot, Some(bond))),
            Err(crate::storage::Error::Missing) => Err(failure(Error::NotFound)),
            Err(crate::storage::Error::Corrupt) => {
                let _ = self.manager.lose(id, store, radio).await;
                Err(failure(Error::NotFound))
            }
            Err(_) => Err(failure(Error::StorageFailed)),
        }
    }

    async fn device_result<S: RecordStore, B: Bluetooth>(
        &mut self,
        id: u32,
        store: &mut S,
        radio: &mut B,
    ) {
        match self.policy_of(id, store, radio).await {
            Ok((policy, slot)) => {
                let device = wire::device(&self.manager, &policy, slot);
                self.reply(Some(R::Device(device)));
            }
            Err(e) => self.fail(e),
        }
    }

    /// Saves one connected device's first-connection setup progress, not while a connection
    /// waits for its first input. A failed save leaves the remaining steps to the device's next
    /// connection. `write_uncertain` describes the last requested write, so these background
    /// saves leave it as they found it. Returns whether it saved.
    async fn setup<S: RecordStore, B: Bluetooth>(
        &mut self,
        store: &mut S,
        radio: &mut B,
        now: u64,
    ) -> bool {
        if !self.manager.storage_ready || self.manager.starting(now) {
            return false;
        }
        if let Some((index, slot, policy, setup)) = self.manager.setup() {
            if !self.write_allowed(now) {
                return false;
            }
            let uncertain = self.manager.write_uncertain;
            let saved = self.manager.save_policy(policy, None, store, radio).await;
            self.manager.write_uncertain = uncertain;
            if saved.is_ok() {
                if let Some(live) = self.manager.devices[slot]
                    .as_mut()
                    .and_then(|d| d.live.as_mut())
                {
                    live.setup = setup;
                }
                self.mark_slot(slot, DEVICE);
            } else if let Some(c) = &mut self.manager.connections[index] {
                c.setup_failed = true;
            }
            return true;
        }
        false
    }

    /// Answers one request completely, taking every step of a command that reads many records.
    /// For an owner that runs the application from one loop.
    pub async fn dispatch<S: RecordStore, B: Bluetooth>(
        &mut self,
        request: p::Request,
        store: &mut S,
        radio: &mut B,
        now: u64,
    ) {
        self.begin(request, store, radio, now).await;
        while self.command_pending() {
            self.proceed(store, radio, now).await;
        }
    }

    /// Whether a command is waiting for its next step. No other request is read, and no other
    /// secondary work runs, until it has responded.
    pub fn command_pending(&self) -> bool {
        self.pending.is_some()
    }

    /// Takes the next step of the command in progress: reads one record, or responds.
    pub async fn proceed<S: RecordStore, B: Bluetooth>(
        &mut self,
        store: &mut S,
        radio: &mut B,
        now: u64,
    ) {
        // Storage can stop being ready between steps, such as after a write in the priority loop
        // fails. The command then fails as it would have at its start, instead of answering with
        // records left out.
        if self.pending.is_some() && !self.manager.storage_ready {
            self.pending = None;
            return self.fail(failure(Error::RadioUnavailable));
        }
        match self.pending.take() {
            None => {}
            Some(Pending::Devices(listing, list)) => {
                self.list_devices(listing, list, store, radio).await
            }
            Some(Pending::Profiles(listing, list)) => {
                self.list_profiles(listing, list, store, radio).await
            }
            Some(Pending::Settings(policy, after)) => {
                match self.saved_catalog(&policy, store).await {
                    Ok(catalog) => {
                        let page = settings_page(&catalog, after.as_ref());
                        self.reply_settings(policy.id, page);
                    }
                    Err(e) => self.fail(e),
                }
            }
            Some(Pending::Rules(id, after)) => match self.rules_page(id.into(), after, store).await
            {
                Ok(rules) => {
                    let mut page: Vec<p::ProfileRule> = rules.into_iter().map(wire_rule).collect();
                    let end = page.len() <= RULES_PAGE;
                    page.truncate(RULES_PAGE);
                    self.reply(Some(R::ProfileRules(p::ProfileRules {
                        profile: id,
                        rules: page,
                        end,
                    })));
                }
                Err(e) => self.fail(e),
            },
            Some(Pending::Files(listing)) => self.list_files(listing, store).await,
            Some(Pending::DeleteProfile(id, scan)) => self.delete_profile(id, scan, store).await,
            Some(Pending::Pairing(start, scan)) => {
                self.find_pairing_device(start, scan, store, radio, now)
                    .await
            }
        }
    }

    /// Starts answering one request. A command that reads many records answers after its further
    /// steps (`proceed`); every other command answers now. The session reads the next request only
    /// after this response has been written.
    pub async fn begin<S: RecordStore, B: Bluetooth>(
        &mut self,
        request: p::Request,
        store: &mut S,
        radio: &mut B,
        now: u64,
    ) {
        let Some(command) = request.command else {
            return self.fail(failure(Error::UnknownCommand));
        };
        let development = self.development();
        let profiles = self.manager.profiles_supported();
        match command {
            Command::GetStatus(_) => {
                if self.manager.storage_ready {
                    // A failed size read keeps the last estimate.
                    if let Ok(bytes) = store.available().await {
                        self.manager.available_bytes = bytes;
                    }
                }
                let status = self.status(radio);
                self.reply(Some(R::Status(status)));
            }
            Command::ListProfiles(args) if profiles => {
                if !self.manager.storage_ready {
                    return self.fail(failure(Error::RadioUnavailable));
                }
                self.pending = Some(Pending::Profiles(
                    Listing::new(args.after),
                    p::ProfileList::default(),
                ));
            }
            Command::GetProfile(args) if profiles => {
                match self.profile_record(args.profile, store, radio).await {
                    Ok(profile) => self.reply(Some(R::Profile(profile))),
                    Err(e) => self.fail(e),
                }
            }
            Command::CreateProfile(args) if profiles => {
                self.create_profile(&args.name, None, store, radio, now)
                    .await
            }
            Command::CopyProfile(args) if profiles => {
                self.create_profile(&args.name, Some(args.profile), store, radio, now)
                    .await
            }
            Command::DeleteProfile(args) if profiles => {
                self.start_delete_profile(args.profile, store, radio).await
            }
            Command::ListProfileRules(args) if profiles => {
                let after = match &args.after {
                    None => None,
                    Some(after) => match usage(after) {
                        Some(after) => Some(after),
                        None => return self.fail(bad_args()),
                    },
                };
                match self.profile_record(args.profile, store, radio).await {
                    Ok(_) => self.pending = Some(Pending::Rules(args.profile, after)),
                    Err(e) => self.fail(e),
                }
            }
            Command::SetProfileRules(args) if profiles => {
                self.set_profile_rules(args, store, now).await
            }
            Command::ListProfiles(_)
            | Command::GetProfile(_)
            | Command::CreateProfile(_)
            | Command::CopyProfile(_)
            | Command::DeleteProfile(_)
            | Command::ListProfileRules(_)
            | Command::SetProfileRules(_) => self.fail(failure(Error::UnknownCommand)),
            Command::SetAdapter(args) => self.set_adapter(args, store, radio, now).await,
            Command::EnterBootloader(_) => self.bootloader(now),
            Command::StartScan(args) => self.start_scan(args, radio, now),
            Command::StopScan(_) => {
                self.stop_scan(radio);
                self.reply(None);
            }
            Command::StartPairing(args) => self.start_pairing(args, radio),
            Command::AcceptPrompt(args) => self.answer(true, Some(args.value), radio, now),
            Command::RejectPrompt(_) => self.answer(false, None, radio, now),
            Command::CancelPairing(_) => {
                self.stop_pairing(Error::Cancelled, radio);
                self.reply(None);
            }
            Command::ListDevices(args) => {
                if !self.manager.storage_ready {
                    return self.fail(failure(Error::RadioUnavailable));
                }
                self.pending = Some(Pending::Devices(
                    Listing::new(args.after),
                    p::DeviceList::default(),
                ));
            }
            Command::GetDevice(args) => self.device_result(args.device, store, radio).await,
            Command::SetDevice(args) => self.set_device(args, store, radio).await,
            Command::ConnectDevice(args) => self.connect(args.device, store, radio, now).await,
            Command::DisconnectDevice(args) => {
                if let Some(slot) = self.manager.find(args.device.into()) {
                    self.manager.disconnect(slot, radio).ok();
                    self.mark(args.device.into(), DEVICE);
                }
                self.device_result(args.device, store, radio).await
            }
            Command::UnpairDevice(args) => self.unpair(args.device, store, radio).await,
            Command::RefreshDevice(args) => self.refresh(args.device, store, radio, now).await,
            Command::ListWarnings(args) => match self.policy_of(args.device, store, radio).await {
                Ok((policy, slot)) => {
                    let mut warnings = self.warnings_of(slot);
                    if let Some(after) = &args.after {
                        let after = wire::warning_order(after);
                        warnings.retain(|w| wire::warning_order(&wire::warning(w)) > after);
                    }
                    let end = warnings.len() <= WARNINGS_PAGE;
                    warnings.truncate(WARNINGS_PAGE);
                    if let Some(reported) = self.reported(policy.id) {
                        for w in &warnings {
                            if !reported.warnings.contains(w) {
                                reported.warnings.push(*w);
                            }
                        }
                    }
                    let mut list = wire::warnings(policy.id, &warnings);
                    list.end = end;
                    self.reply(Some(R::Warnings(list)));
                }
                Err(e) => self.fail(e),
            },
            Command::ListSettings(args) => {
                let (policy, slot) = match self.policy_of(args.device, store, radio).await {
                    Ok(found) => found,
                    Err(e) => return self.fail(e),
                };
                match self.live_catalog(slot) {
                    Some(catalog) => {
                        // The catalog is borrowed from the device; the page is built from it.
                        let page = settings_page(catalog, args.after.as_ref());
                        self.reply_settings(policy.id, page);
                    }
                    None => self.pending = Some(Pending::Settings(policy, args.after)),
                }
            }
            Command::SetSettings(args) => self.change_settings(args, store, radio, now).await,
            Command::ListFeatures(args) if development => {
                let after = args
                    .after
                    .as_ref()
                    .map(|a| (a.integration, a.index))
                    .unwrap_or_default();
                let features = self
                    .manager
                    .find(args.device.into())
                    .and_then(|s| self.manager.devices[s].as_ref())
                    .and_then(|d| d.live.as_ref())
                    .map(|live| {
                        live.catalog
                            .features()
                            .iter()
                            .enumerate()
                            .filter(|(index, _)| {
                                args.after.is_none()
                                    || (p::IntegrationKind::Hidpp as i32, *index as u32) > after
                            })
                            .take(FEATURES_PAGE + 1)
                            .map(|(index, f)| p::Feature {
                                integration: p::IntegrationKind::Hidpp as i32,
                                supported: f.supported(),
                                detail: Some(p::feature::Detail::Hidpp(p::HidppFeature {
                                    index: index as u32,
                                    id: f.id.0.into(),
                                    version: f.version.0.into(),
                                    flags: f.flags.0.into(),
                                })),
                            })
                            .collect::<Vec<_>>()
                    });
                match features {
                    Some(mut features) => {
                        let end = features.len() <= FEATURES_PAGE;
                        features.truncate(FEATURES_PAGE);
                        self.reply(Some(R::Features(p::FeatureList { features, end })))
                    }
                    None => match self.policy_of(args.device, store, radio).await {
                        Ok(_) => self.reply(Some(R::Features(p::FeatureList {
                            features: Vec::new(),
                            end: true,
                        }))),
                        Err(e) => self.fail(e),
                    },
                }
            }
            Command::ListFiles(args) if development => {
                if !crate::model::storage_path(&args.path) {
                    return self.fail(bad_args());
                }
                self.pending = Some(Pending::Files(FileListing {
                    path: args.path,
                    after: args.after,
                    index: 0,
                    page: Vec::new(),
                }));
            }
            Command::ReadFile(args) if development => self.read_file(&args.path, store).await,
            Command::ListFeatures(_) | Command::ListFiles(_) | Command::ReadFile(_) => {
                self.fail(failure(Error::UnknownCommand))
            }
        }
    }

    /// One step of a page of saved devices in ascending ID order.
    async fn list_devices<S: RecordStore, B: Bluetooth>(
        &mut self,
        mut listing: Listing,
        mut list: p::DeviceList,
        store: &mut S,
        radio: &mut B,
    ) {
        use p::device_list_entry::Entry;
        let found = !list.entries.is_empty();
        match listing.turn(2, devices::PAGE_SIZE, found, store).await {
            Err(code) => return self.fail(failure(code)),
            Ok(Turn::Finished(end)) => {
                list.end = end;
                return self.reply(Some(R::Devices(list)));
            }
            Ok(Turn::Paged) => {}
            Ok(Turn::Read(id)) => match self.policy_of(id as u32, store, radio).await {
                Ok((policy, slot)) => list.entries.push(p::DeviceListEntry {
                    entry: Some(Entry::Device(wire::device(&self.manager, &policy, slot))),
                }),
                Err(e) if e.code == p::ErrorCode::StorageFailed as i32 => {
                    list.entries.push(p::DeviceListEntry {
                        entry: Some(Entry::Unreadable(id as u32)),
                    })
                }
                // A lost record is removed and left out.
                Err(_) => {}
            },
        }
        self.pending = Some(Pending::Devices(listing, list));
    }

    /// The settings of device `id`: the readings of its connection and its saved values, or the
    /// saved values alone while it is not connected.
    async fn device_settings<S: RecordStore, B: Bluetooth>(
        &mut self,
        id: u32,
        store: &mut S,
        radio: &mut B,
    ) -> Result<Vec<p::Setting>, p::Error> {
        let (policy, slot) = self.policy_of(id, store, radio).await?;
        if let Some(catalog) = self.live_catalog(slot) {
            return Ok(wire::settings(catalog));
        }
        let catalog = self.saved_catalog(&policy, store).await?;
        Ok(wire::settings(&catalog))
    }

    /// The settings catalog of the connection of the device in `slot`, once it has read the
    /// device's policy.
    fn live_catalog(&self, slot: Option<usize>) -> Option<&Catalog> {
        slot.and_then(|s| self.manager.devices[s].as_ref())
            .and_then(|d| d.live.as_ref())
            .filter(|l| l.policy.is_some())
            .map(|l| &l.catalog)
    }

    /// The current warnings of the device in `slot`, in listing order.
    fn warnings_of(&self, slot: Option<usize>) -> Vec<DeviceWarning> {
        let warnings = slot
            .and_then(|s| self.manager.devices[s].as_ref())
            .and_then(|d| d.live.as_ref())
            .map_or(&[][..], |l| l.warnings.as_slice());
        wire::sorted_warnings(warnings)
    }

    /// Responds with a page of device `id`'s settings, which the client then holds.
    fn reply_settings(&mut self, id: u64, (settings, end): (Vec<p::Setting>, bool)) {
        let keys = settings
            .iter()
            .filter_map(|s| wire::parse_setting_key(&s.key))
            .fold(0, |keys, key| keys | key_bit(key));
        if let Some(reported) = self.reported(id) {
            reported.keys |= keys;
        }
        self.reply(Some(R::Settings(p::DeviceSettings {
            device: id as u32,
            settings,
            end,
        })));
    }

    /// A catalog holding a disconnected device's saved settings.
    async fn saved_catalog<S: RecordStore>(
        &mut self,
        policy: &Policy,
        store: &mut S,
    ) -> Result<Catalog, p::Error> {
        let preferences = match (Preferences {
            store,
            device: policy.id,
        })
        .load_all()
        .await
        {
            Ok(preferences) => preferences,
            Err(crate::storage::Error::Corrupt) => Vec::new(),
            Err(_) => return Err(failure(Error::StorageFailed)),
        };
        let mut catalog = Catalog::default();
        // Saved settings that no longer decode are left out; the next connection removes them.
        let _ = catalog.restore_preferences(preferences);
        catalog.connection(false, policy.hidpp_enabled());
        Ok(catalog)
    }

    async fn profile_record<S: RecordStore, B: Bluetooth>(
        &mut self,
        id: u32,
        store: &mut S,
        _radio: &mut B,
    ) -> Result<p::Profile, p::Error> {
        let id = u64::from(id);
        if id == 0 {
            return Err(failure(Error::NotFound));
        }
        if !self.manager.storage_ready {
            return Err(failure(Error::RadioUnavailable));
        }
        match profiles::metadata(store, id).await {
            Ok(meta) => Ok(p::Profile {
                id: id as u32,
                name: meta.name,
                roles: wire::roles(self.pending_roles(id).unwrap_or(meta.roles).0),
            }),
            Err(crate::storage::Error::Corrupt) => {
                self.lost_profile(id);
                Err(failure(Error::NotFound))
            }
            Err(error) => Err(self.storage_error(error)),
        }
    }

    /// The roles summary of profile `id` when RAM is ahead of its saved record: that of rules
    /// edited or waiting to be saved, or a summary waiting to be saved.
    fn pending_roles(&self, id: u64) -> Option<devices::Roles> {
        self.editor
            .as_ref()
            .filter(|e| e.profile == id && e.edited.is_some())
            .map(|e| &e.map)
            .or_else(|| self.unsaved.map(id))
            .map(|map| devices::Roles(map.borrow().roles()))
            .or_else(|| self.unsaved.roles(id))
    }

    /// Queues the cleanup of a profile whose saved files are undecodable.
    fn lost_profile(&mut self, id: u64) {
        let cleaning = matches!(self.work, Some(Work::LostProfile(lost, _)) if lost == id);
        if !cleaning && !self.manager.lost_profiles.contains(&id) {
            self.manager.lost_profiles.push(id);
        }
    }

    /// One step of a page of saved profiles in ascending ID order.
    async fn list_profiles<S: RecordStore, B: Bluetooth>(
        &mut self,
        mut listing: Listing,
        mut list: p::ProfileList,
        store: &mut S,
        radio: &mut B,
    ) {
        use p::profile_list_entry::Entry;
        let found = !list.entries.is_empty();
        match listing
            .turn(profiles::METADATA, profiles::PAGE_SIZE, found, store)
            .await
        {
            Err(code) => return self.fail(failure(code)),
            Ok(Turn::Finished(end)) => {
                list.end = end;
                return self.reply(Some(R::Profiles(list)));
            }
            Ok(Turn::Paged) => {}
            Ok(Turn::Read(id)) => match self.profile_record(id as u32, store, radio).await {
                Ok(profile) => list.entries.push(p::ProfileListEntry {
                    entry: Some(Entry::Profile(profile)),
                }),
                Err(e) if e.code == p::ErrorCode::StorageFailed as i32 => {
                    list.entries.push(p::ProfileListEntry {
                        entry: Some(Entry::Unreadable(id as u32)),
                    })
                }
                Err(_) => {}
            },
        }
        self.pending = Some(Pending::Profiles(listing, list));
    }

    /// Profile `id`'s table in RAM, which its users share. A table waiting to be saved, or held
    /// by an editor, stays loaded: a loaded entry is forgotten only once nothing holds its table,
    /// or when its profile is deleted, which drops those too.
    pub(crate) fn loaded(&self, id: u64) -> Option<profiles::Map> {
        self.manager.profiles.get(id)
    }

    /// Up to one more than a page of profile `id`'s rules for inputs above `after`: from its table
    /// in RAM when there is one, otherwise from the saved file, read only as far as the page.
    async fn rules_page<S: RecordStore>(
        &mut self,
        id: u64,
        after: Option<profiles::Usage>,
        store: &mut S,
    ) -> Result<Vec<profiles::Rule>, p::Error> {
        if let Some(map) = self.loaded(id) {
            return Ok(map.borrow().after(after).take(RULES_PAGE + 1).collect());
        }
        match profiles::saved_page(store, id, after, RULES_PAGE + 1).await {
            Ok(rules) => Ok(rules),
            Err(crate::storage::Error::Corrupt) => {
                self.lost_profile(id);
                Err(failure(Error::StorageFailed))
            }
            Err(error) => Err(self.storage_error(error)),
        }
    }

    /// The rules of profile `id`: a copy of its table in RAM when there is one, otherwise the
    /// saved one.
    async fn rules<S: RecordStore>(
        &mut self,
        id: u64,
        store: &mut S,
    ) -> Result<profiles::Rules, p::Error> {
        if let Some(map) = self.loaded(id) {
            return Ok(map.borrow().clone());
        }
        match profiles::rules(store, id).await {
            Ok(rules) => Ok(rules),
            Err(crate::storage::Error::Corrupt) => {
                self.lost_profile(id);
                Err(failure(Error::StorageFailed))
            }
            Err(error) => Err(self.storage_error(error)),
        }
    }

    async fn create_profile<S: RecordStore, B: Bluetooth>(
        &mut self,
        name: &str,
        source: Option<u32>,
        store: &mut S,
        radio: &mut B,
        now: u64,
    ) {
        if !profiles::name_valid(name) {
            return self.fail(bad_args());
        }
        let rules = match source {
            None => profiles::Rules::default(),
            Some(source) => {
                if let Err(e) = self.profile_record(source, store, radio).await {
                    return self.fail(e);
                }
                match self.rules(source.into(), store).await {
                    Ok(rules) => rules,
                    Err(e) => return self.fail(e),
                }
            }
        };
        // Free space is checked with every deferred save on flash.
        if let Err(e) = self.save_for_admission(store, now).await {
            return self.fail(e);
        }
        match profiles::create(store, name, &rules).await {
            Ok((id, _)) => {
                self.mark_profile(id, PROFILE);
                self.reply(Some(R::ProfileCreated(p::ProfileCreated {
                    profile: id as u32,
                })));
            }
            Err(error) => {
                let error = self.storage_error(error);
                self.fail(error)
            }
        }
    }

    /// Starts deleting profile `id`: refused while a configuration interface refers to it, then
    /// each saved device's layers are checked, one per step, before it is deleted.
    async fn start_delete_profile<S: RecordStore, B: Bluetooth>(
        &mut self,
        id: u32,
        store: &mut S,
        radio: &mut B,
    ) {
        if let Err(e) = self.profile_record(id, store, radio).await {
            return self.fail(e);
        }
        let id = u64::from(id);
        if self
            .manager
            .preference
            .configuration_interfaces
            .iter()
            .any(|p| p.profile == Some(id))
        {
            return self.fail(failure(Error::InUse));
        }
        self.pending = Some(Pending::DeleteProfile(id, Records::new(2)));
    }

    /// One step of deleting profile `id`: checks one saved device's layers, enabled or not, or
    /// deletes the profile once none refers to it.
    ///
    /// The check stays valid across steps. No other request is read and no background work runs
    /// until this command responds, and the priority loop never adds a reference: pairing saves a
    /// new device without layers and keeps a re-paired device's saved layers. Background work
    /// paused between its own steps, such as cleaning up a lost profile, only removes references.
    async fn delete_profile<S: RecordStore>(&mut self, id: u64, mut scan: Records, store: &mut S) {
        match scan.next(store).await {
            Err(code) => return self.fail(failure(code)),
            Ok(Visit::Page) => {}
            Ok(Visit::Record(device)) => match (Policies { store }).load(device).await {
                Ok(policy) if policy.profiles.contains(&id) => {
                    return self.fail(failure(Error::InUse));
                }
                Ok(_) | Err(crate::storage::Error::Missing | crate::storage::Error::Corrupt) => {}
                Err(_) => return self.fail(failure(Error::StorageFailed)),
            },
            Ok(Visit::Done) => {
                if let Err(error) = profiles::remove(store, id).await {
                    let error = self.storage_error(error);
                    return self.fail(error);
                }
                self.removed_profile(id);
                return self.reply(None);
            }
        }
        self.pending = Some(Pending::DeleteProfile(id, scan));
    }

    fn removed_profile(&mut self, id: u64) {
        // An editor still holding the profile, kept because its edits could not be handed over,
        // must not write its rules file again.
        if self.editor.as_ref().is_some_and(|e| e.profile == id) {
            self.editor = None;
        }
        self.manager.profiles.forget(id);
        self.unsaved.forget_profile(id);
        self.writes_recovered();
        self.profile_dirty.retain(|(p, _)| *p != id);
        self.event_retries
            .retain(|(r, _)| *r != EventRead::Profile(id) && *r != EventRead::Rules(id));
        self.touched_rules.retain(|(p, _)| *p != id);
        if !self.profile_removed.contains(&id) {
            self.profile_removed.push_back(id);
        }
    }

    /// Applies rule changes to profile `id`, saves them and publishes them to every user of the
    /// profile. A loaded table is changed in place of a copy: the new table is merged from it and
    /// replaces it once saved, and the saved file then holds any edits not yet saved too.
    pub(crate) async fn change_rules<S: RecordStore>(
        &mut self,
        id: u64,
        changes: Vec<profiles::Change>,
        store: &mut S,
        now: u64,
    ) -> Result<(), p::Error> {
        let Some(budget) = self.manager.profile_budget else {
            return Err(failure(Error::UnknownCommand));
        };
        if !self.manager.storage_ready {
            return Err(failure(Error::RadioUnavailable));
        }
        let mut meta = match profiles::metadata(store, id).await {
            Ok(meta) => meta,
            Err(crate::storage::Error::Corrupt) => {
                self.lost_profile(id);
                return Err(failure(Error::NotFound));
            }
            Err(error) => return Err(self.storage_error(error)),
        };
        // What listings report now, which the saved change may change.
        let reported = self.pending_roles(id).unwrap_or(meta.roles);
        let loaded = self.loaded(id);
        let changed = match &loaded {
            Some(map) => map.borrow().changed(changes),
            None => self.rules(id, store).await?.changed(changes),
        };
        let (rules, inputs) = changed.map_err(|_| failure(Error::Capacity))?;
        // Rules already in RAM but not yet on flash are saved, so the response holds for them too.
        let unsaved = self.unsaved.map(id).is_some()
            || self
                .editor
                .as_ref()
                .is_some_and(|e| e.profile == id && e.edited.is_some());
        if inputs.is_empty() && !unsaved {
            return Ok(());
        }
        let memory = rules.memory();
        if memory > budget || !self.manager.profiles.fits(id, memory, budget) {
            return Err(capacity(p::CapacityReason::ProfileMemory));
        }
        if let Err(error) = profiles::save_rules(store, id, &rules).await {
            return Err(self.storage_error(error));
        }
        let roles = devices::Roles(rules.roles());
        // The saved rules are the edit; the roles summary follows them.
        if let Some(map) = loaded {
            *map.borrow_mut() = rules;
        }
        self.unsaved.rules_saved(id);
        self.writes_recovered();
        if let Some(editor) = self.editor.as_mut().filter(|e| e.profile == id) {
            editor.edited = None;
            editor.failure = None;
            editor.retry.succeeded();
        }
        self.retry_users(id);
        self.touch_rules(id, inputs);
        self.save_roles(id, &mut meta, roles, store, now).await;
        if roles != reported {
            self.mark_profile(id, PROFILE);
        }
        Ok(())
    }

    /// Applies an editor's rule changes to the loaded table `map` of profile `id` without writing
    /// it: the editor hands its edits to storage once it pauses. `reset` forgets every rule first.
    pub(crate) fn edit_rules(
        &mut self,
        id: u64,
        map: &profiles::Map,
        reset: bool,
        changes: Vec<profiles::Change>,
        now: u64,
    ) -> Result<(), ()> {
        let budget = self.manager.profile_budget.ok_or(())?;
        let (rules, inputs) = if reset {
            let inputs: Vec<profiles::Usage> = map.borrow().inputs().collect();
            let (rules, mut more) = profiles::Rules::default()
                .changed(changes)
                .map_err(|_| ())?;
            more.retain(|input| inputs.binary_search(input).is_err());
            let mut inputs = inputs;
            inputs.extend(more);
            inputs.sort_unstable();
            (rules, inputs)
        } else {
            map.borrow().changed(changes).map_err(|_| ())?
        };
        if inputs.is_empty() {
            return Ok(());
        }
        let memory = rules.memory();
        if memory > budget || !self.manager.profiles.fits(id, memory, budget) {
            return Err(());
        }
        // Listings report the roles of the edited rules, so a change of them is a profile event.
        if rules.roles() != map.borrow().roles() {
            self.mark_profile(id, PROFILE);
        }
        *map.borrow_mut() = rules;
        self.retry_users(id);
        self.touch_rules(id, inputs);
        if let Some(editor) = self.editor.as_mut().filter(|e| e.profile == id) {
            let first = editor.edited.map_or(now, |(first, _)| first);
            editor.edited = Some((first, now));
        }
        Ok(())
    }

    /// Saves `roles` as profile `id`'s roles summary when it changed. A failed save is repaired
    /// once input pauses.
    async fn save_roles<S: RecordStore>(
        &mut self,
        id: u64,
        meta: &mut profiles::Metadata,
        roles: devices::Roles,
        store: &mut S,
        now: u64,
    ) {
        // These roles replace any summary still waiting to be saved.
        self.unsaved.saved(&Record::Roles(id, roles));
        if roles == meta.roles {
            return;
        }
        meta.roles = roles;
        match profiles::save_metadata(store, id, meta).await {
            Ok(()) => self.mark_profile(id, PROFILE),
            Err(error) => {
                if error == crate::storage::Error::Unknown {
                    let _ = self.storage_error(error);
                }
                // Without memory to list the repair, the summary is corrected at the next save
                // of the profile's rules, and listings report the saved one until then.
                let _ = self.unsaved.mark(Record::Roles(id, roles), now);
            }
        }
    }

    /// Connected devices whose profiles are not loaded and whose layers include profile `id`
    /// try loading them again at the next background step.
    fn retry_users(&mut self, id: u64) {
        for d in self.manager.devices.iter_mut().flatten() {
            if d.layers.contains(&id)
                && let Some(live) = d.live.as_mut()
                && live.profile_error.is_some()
            {
                live.profile_retry = Some((0, devices::RETRY_DELAY_MS));
                self.profiles_released = true;
            }
        }
    }

    async fn set_profile_rules<S: RecordStore>(
        &mut self,
        args: p::SetProfileRules,
        store: &mut S,
        now: u64,
    ) {
        if args.changes.is_empty() {
            return self.fail(bad_args());
        }
        let mut changes = Vec::new();
        for change in &args.changes {
            use p::profile_rule_change::Change as C;
            let change = match &change.change {
                Some(C::Rule(value)) => {
                    let Some(rule) = rule(value) else {
                        return self.fail(bad_args());
                    };
                    match rule.normalized() {
                        Ok(rule) => profiles::Change::Set(rule),
                        Err(_) => return self.fail(bad_args()),
                    }
                }
                Some(C::Forget(reference)) => {
                    let Some(input) = reference.input.as_ref().and_then(usage) else {
                        return self.fail(bad_args());
                    };
                    profiles::Change::Forget(input)
                }
                None => return self.fail(bad_args()),
            };
            changes.push(change);
        }
        if args.profile == 0 {
            return self.fail(failure(Error::NotFound));
        }
        // `change_rules` reads the profile's metadata, which also confirms that it exists.
        match self
            .change_rules(args.profile.into(), changes, store, now)
            .await
        {
            Ok(_) => self.reply(None),
            Err(e) => self.fail(e),
        }
    }

    /// One step of cleaning up lost profile `id`; `scan` is `None` at the first step. When only
    /// its rules file is undecodable, the file is removed and the profile stays, empty. Otherwise
    /// every reference to it is removed and it is deleted: a configuration interface that refers
    /// to it is cleared and disabled, then each saved device's layers lose it, one device per step.
    /// Returns the scan to continue with, or `None` once the cleanup is complete.
    ///
    /// Commands can run between steps, but none can add a reference meanwhile: the profile record
    /// is missing or undecodable, so every command that would refer to it fails. Each device's
    /// policy is read, changed and saved in one step, so a save between steps is not undone.
    async fn clean_lost_profile<S: RecordStore, B: Bluetooth>(
        &mut self,
        id: u64,
        scan: Option<Records>,
        store: &mut S,
        radio: &mut B,
        now: u64,
    ) -> Result<Option<Records>, Error> {
        let Some(mut scan) = scan else {
            match profiles::metadata(store, id).await {
                Ok(mut meta) => {
                    let empty = profiles::Rules::default();
                    profiles::save_rules(store, id, &empty)
                        .await
                        .map_err(|_| Error::StorageFailed)?;
                    // What listings report now, and a table in RAM holding the rules a client may
                    // have listed.
                    let reported = self.pending_roles(id).unwrap_or(meta.roles);
                    let map = self.loaded(id);
                    self.unsaved.rules_saved(id);
                    // The saved empty rules replace an editor's edits too.
                    if let Some(editor) = self.editor.as_mut().filter(|e| e.profile == id) {
                        editor.edited = None;
                        editor.failure = None;
                        editor.retry.succeeded();
                    }
                    self.writes_recovered();
                    let loaded: Vec<profiles::Usage> = match &map {
                        Some(map) => map.borrow().inputs().collect(),
                        None => Vec::new(),
                    };
                    if let Some(map) = map {
                        *map.borrow_mut() = empty;
                    }
                    self.retry_users(id);
                    self.touch_rules(id, loaded);
                    self.save_roles(id, &mut meta, devices::Roles(0), store, now)
                        .await;
                    if reported != devices::Roles(0) {
                        self.mark_profile(id, PROFILE);
                    }
                    return Ok(None);
                }
                Err(crate::storage::Error::Missing | crate::storage::Error::Corrupt) => {}
                Err(_) => return Err(Error::StorageFailed),
            }
            let mut preference = self.manager.preference.clone();
            let enabled = interfaces::enabled(&preference.configuration_interfaces);
            for entry in &mut preference.configuration_interfaces {
                if entry.profile == Some(id) {
                    entry.profile = None;
                    entry.enabled = false;
                }
            }
            preference
                .configuration_interfaces
                .retain(|p| p.enabled || p.profile.is_some());
            if preference != self.manager.preference {
                self.manager.adapter(preference, store).await?;
                self.usb_reconnect |=
                    interfaces::enabled(&self.manager.preference.configuration_interfaces)
                        != enabled;
                self.adapter_dirty = true;
            }
            return Ok(Some(Records::new(2)));
        };
        match scan.next(store).await? {
            Visit::Page => {}
            Visit::Record(device) => {
                let (mut policy, bond) = match (Policies { store }).load_record(device).await {
                    Ok(record) => record,
                    Err(crate::storage::Error::Missing | crate::storage::Error::Corrupt) => {
                        return Ok(Some(scan));
                    }
                    Err(_) => return Err(Error::StorageFailed),
                };
                if policy.profiles.contains(&id) {
                    policy.profiles.retain(|p| *p != id);
                    if self.manager.find(device).is_some() {
                        self.manager
                            .save_policy(policy, Some(&bond), store, radio)
                            .await?;
                    } else {
                        // Nothing is resident for this device, so only its record changes. It
                        // takes no room in the stack, whether or not it is enabled.
                        match (Policies { store }).save_with(&policy, &bond).await {
                            Ok(()) | Err(crate::storage::Error::Missing) => {}
                            Err(_) => return Err(Error::StorageFailed),
                        }
                    }
                    self.mark(device, DEVICE);
                }
            }
            Visit::Done => {
                profiles::remove(store, id)
                    .await
                    .map_err(|_| Error::StorageFailed)?;
                self.removed_profile(id);
                return Ok(None);
            }
        }
        Ok(Some(scan))
    }

    async fn set_adapter<S: RecordStore, B: Bluetooth>(
        &mut self,
        args: p::SetAdapter,
        store: &mut S,
        radio: &mut B,
        now: u64,
    ) {
        if !self.manager.storage_ready {
            return self.fail(failure(Error::RadioUnavailable));
        }
        let mut preference = self.manager.preference.clone();
        let name = match args.name.as_deref() {
            None => self.manager.preference.name.clone(),
            Some("") => None,
            Some(value) => match crate::model::adapter_name(value) {
                Some(name) => Some(name.into()),
                None => return self.fail(bad_args()),
            },
        };
        let platform = match args.platform.map(p::Platform::try_from) {
            None => self.manager.preference.host_platform,
            Some(Ok(platform)) => wire::host_platform(platform),
            Some(Err(_)) => return self.fail(bad_args()),
        };
        let mut transports = self.manager.preference.transports;
        for update in &args.transports {
            let transport = match p::Transport::try_from(update.transport) {
                Ok(p::Transport::Classic) => Transport::Classic,
                Ok(p::Transport::Ble) => Transport::Ble,
                Ok(p::Transport::Unspecified) => return self.fail(bad_args()),
                Err(_) => return self.fail(failure(Error::UnsupportedTransport)),
            };
            if !radio.capabilities().supports(transport) {
                return self.fail(failure(Error::UnsupportedTransport));
            }
            if let Some(enabled) = update.enabled {
                transports.set(transport, enabled);
            }
        }
        // Each profile is read once, however many updates name it.
        let mut found: Vec<u32> = Vec::new();
        for update in &args.configuration_interfaces {
            let interface = match p::ConfigurationInterface::try_from(update.interface) {
                Ok(p::ConfigurationInterface::Via) => Interface::Via,
                Ok(p::ConfigurationInterface::Vial) => Interface::Vial,
                Ok(p::ConfigurationInterface::Unspecified) => return self.fail(bad_args()),
                Err(_) => return self.fail(failure(Error::UnsupportedTransport)),
            };
            if !self.manager.profiles_supported() {
                return self.fail(failure(Error::UnsupportedTransport));
            }
            let mut entry = interfaces::preference(&preference.configuration_interfaces, interface);
            if let Some(enabled) = update.enabled {
                entry.enabled = enabled;
            }
            match update.profile {
                None => {}
                Some(0) => entry.profile = None,
                Some(id) => {
                    if !found.contains(&id) {
                        if let Err(e) = self.profile_record(id, store, radio).await {
                            return self.fail(e);
                        }
                        found.push(id);
                    }
                    entry.profile = Some(id.into());
                }
            }
            interfaces::set(&mut preference.configuration_interfaces, entry);
        }
        let saved = &preference.configuration_interfaces;
        if saved.iter().any(|p| p.enabled && p.profile.is_none()) {
            return self.fail(bad_args());
        }
        if !interfaces::valid(saved) {
            return self.fail(failure(Error::UnsupportedTransport));
        }
        let before = &self.manager.preference.configuration_interfaces;
        let reconnect = interfaces::enabled(saved) != interfaces::enabled(before)
            || saved
                .iter()
                .filter(|p| p.enabled)
                .any(|p| interfaces::preference(before, p.interface).profile != p.profile);
        if !self.manager.storage_ready {
            return self.fail(failure(Error::RadioUnavailable));
        }
        let platform_changed = platform != self.manager.preference.host_platform;
        let changed: Vec<Transport> = devices::Transports::ALL
            .into_iter()
            .filter(|t| transports.contains(*t) != self.manager.preference.transports.contains(*t))
            .collect();
        preference.name = name;
        preference.host_platform = platform;
        preference.transports = transports;
        if preference != self.manager.preference {
            if let Err(code) = self.manager.adapter(preference, store).await {
                let uncertain = self.manager.write_uncertain;
                return self.fail(wire::error(code, None, uncertain));
            }
            self.adapter_dirty = true;
            self.usb_reconnect |= reconnect;
            if reconnect {
                self.drop_editor();
            }
            if platform_changed {
                for slot in 0..self.manager.devices.len() {
                    if self.manager.devices[slot]
                        .as_ref()
                        .is_some_and(|d| d.state == ConnectionState::Connected && d.hidpp_enabled)
                    {
                        self.mark_slot(slot, DEVICE | SETTINGS);
                    }
                }
            }
            if !changed.is_empty() {
                self.apply_transports(&changed, radio, now);
            }
        }
        self.reply(None);
    }

    /// Applies saved changes to the enabled transports, all supported by the
    /// radio. Disabling a transport closes its links, ends a pairing over it as
    /// an unsupported transport would, and drops it from a running scan, ending
    /// the scan when nothing is left. Saved devices of a disabled transport
    /// become inactive, and eligible again once it is enabled: the background
    /// fill reads their records one per step.
    fn apply_transports<B: Bluetooth>(&mut self, changed: &[Transport], radio: &mut B, now: u64) {
        let enabled = self.manager.preference.transports;
        for &transport in changed.iter().filter(|t| !enabled.contains(**t)) {
            if self
                .pair
                .as_ref()
                .is_some_and(|p| p.address.transport == transport)
            {
                self.stop_pairing(Error::UnsupportedTransport, radio);
            }
            let links: Vec<LinkId> = self
                .manager
                .connections
                .iter()
                .flatten()
                .filter(|c| c.peer.transport == transport)
                .map(|c| c.id)
                .collect();
            for link in links {
                self.close(link, None, radio);
            }
            if let Some(scan) = &mut self.scan {
                match transport {
                    Transport::Classic => scan.classic = false,
                    Transport::Ble => scan.ble = false,
                }
                if !scan.classic && !scan.ble {
                    self.stop_scan(radio);
                }
            }
        }
        self.manager.retire_disabled_transports();
        for slot in 0..self.manager.devices.len() {
            if self.manager.devices[slot]
                .as_ref()
                .is_some_and(|d| changed.contains(&d.peer.transport))
            {
                self.mark_slot(slot, DEVICE);
            }
        }
        // The fill follows the request at once, whatever earlier failures delayed.
        self.manager.vacated = true;
        self.bonds_retry.succeeded();
        for &transport in changed {
            if let Err(error) = radio.set_transport(transport, enabled.contains(transport)) {
                self.radio_failed(error, now);
                return;
            }
        }
    }

    fn bootloader(&mut self, now: u64) {
        if !self.development() || self.build.bootloader.is_none() {
            return self.fail(failure(Error::UnknownCommand));
        }
        if self.pair.is_some()
            || self
                .manager
                .connections
                .iter()
                .flatten()
                .any(|c| c.device.is_none())
        {
            return self.fail(failure(Error::Busy));
        }
        self.reply(None);
        self.serial.stop_commands();
        for source in 0..devices::ACTIVE_CONNECTIONS {
            self.manager.forward.remove(source);
        }
        self.reboot_at = Some(now.saturating_add(250));
    }

    fn start_scan<B: Bluetooth>(&mut self, args: p::StartScan, radio: &mut B, now: u64) {
        if args.transports.is_empty() {
            return self.fail(bad_args());
        }
        let seconds = match args.seconds {
            0 => SCAN_DEFAULT_SECONDS,
            s if s <= SCAN_MAX_SECONDS => s,
            _ => return self.fail(bad_args()),
        };
        // Requested transports that are unsupported or disabled are left out.
        let caps = self.manager.capabilities(radio);
        let (mut classic, mut ble) = (false, false);
        for transport in &args.transports {
            match p::Transport::try_from(*transport) {
                Ok(p::Transport::Classic) => classic |= caps.classic,
                Ok(p::Transport::Ble) => ble |= caps.ble,
                _ => {}
            }
        }
        if !classic && !ble {
            return self.fail(failure(Error::UnsupportedTransport));
        }
        if !self.manager.radio_ready {
            return self.fail(failure(Error::RadioUnavailable));
        }
        let Some(token) = self.scan_seq.checked_add(1) else {
            return self.fail(failure(Error::InternalError));
        };
        if let Err(e) = radio.scan(token, classic, ble) {
            return self.fail(failure(e));
        }
        self.scan_seq = token;
        self.radio_scan = Some((token, classic, ble));
        // A new scan invalidates earlier candidates, except one a pairing already captured.
        let pairing = self.pair.as_ref().map(|p| p.candidate);
        self.candidates.retain(|c| Some(c.id) == pairing);
        self.truncated = false;
        self.scan_done = None;
        self.scan = Some(Scan {
            token,
            classic,
            ble,
            deadline: now.saturating_add(u64::from(seconds) * 1000),
        });
        self.reply(None);
    }

    fn stop_scan<B: Bluetooth>(&mut self, radio: &mut B) {
        if let Some(scan) = self.scan.take() {
            let _ = radio.scan(scan.token, false, false);
            self.radio_scan = None;
            self.scan_done = Some(p::ScanDone {
                count: self.candidates.len() as u32,
                truncated: self.truncated,
            });
        }
    }

    /// Starts a pairing with scan candidate `args.candidate`. Saved device records are read one
    /// per step to find whether the candidate is a saved device.
    fn start_pairing<B: Bluetooth>(&mut self, args: p::StartPairing, radio: &mut B) {
        if self.pair.is_some() {
            return self.fail(failure(Error::Busy));
        }
        let Some(c) = self.candidates.iter().find(|c| c.id == args.candidate) else {
            return self.fail(failure(Error::NotFound));
        };
        let start = PairingStart {
            candidate: args.candidate,
            peer: c.peer,
            address: c.address,
            name: c.name.clone(),
        };
        if !self.manager.storage_ready || !self.manager.radio_ready {
            return self.fail(failure(Error::RadioUnavailable));
        }
        if !self
            .manager
            .capabilities(radio)
            .supports(start.peer.transport)
            || radio.bond_capacity(start.peer.transport) == 0
        {
            return self.fail(failure(Error::UnsupportedTransport));
        }
        self.pending = Some(Pending::Pairing(start, Records::new(2)));
    }

    /// One step of starting a pairing: reads one saved device record, or starts the attempt once
    /// the saved device with the candidate's identity is found or every record has been read.
    async fn find_pairing_device<S: RecordStore, B: Bluetooth>(
        &mut self,
        start: PairingStart,
        mut scan: Records,
        store: &mut S,
        radio: &mut B,
        now: u64,
    ) {
        // A candidate whose address is a saved identity is that device. A blocked device is not
        // in the stack, so it is recognized from its saved identity.
        let saved = match scan.next(store).await {
            Err(code) => return self.fail(failure(code)),
            Ok(Visit::Page) => None,
            Ok(Visit::Record(id)) => match (Policies { store }).load(id).await {
                Ok(policy) if policy.peer == start.peer && policy.blocked => {
                    return self.fail(failure(Error::Blocked));
                }
                Ok(policy) if policy.peer == start.peer => Some(Some(policy.id)),
                Ok(_) | Err(crate::storage::Error::Missing | crate::storage::Error::Corrupt) => {
                    None
                }
                Err(_) => return self.fail(failure(Error::StorageFailed)),
            },
            Ok(Visit::Done) => Some(None),
        };
        match saved {
            Some(saved) => self.pair_with(start, saved, store, radio, now).await,
            None => self.pending = Some(Pending::Pairing(start, scan)),
        }
    }

    async fn pair_with<S: RecordStore, B: Bluetooth>(
        &mut self,
        start: PairingStart,
        saved: Option<u64>,
        store: &mut S,
        radio: &mut B,
        now: u64,
    ) {
        // Readiness can change between steps; nothing else that matters here can.
        if !self.manager.storage_ready || !self.manager.radio_ready {
            return self.fail(failure(Error::RadioUnavailable));
        }

        let slot = self.manager.peer(start.peer);
        match store.available().await {
            Ok(bytes) => self.manager.available_bytes = bytes,
            Err(_) => return self.fail(failure(Error::StorageFailed)),
        }
        if self.manager.storage_full() {
            self.adapter_dirty = true;
            return self.fail(capacity(p::CapacityReason::Storage));
        }
        // The selected device's link and background setup can free a slot. Keep
        // unrelated established input links intact.
        let closing: Vec<LinkId> = self
            .manager
            .connections
            .iter()
            .flatten()
            .filter(|c| (slot.is_some() && c.device == slot) || c.runtime.is_none())
            .map(|c| c.id)
            .collect();
        if self.manager.connections.iter().all(Option::is_some) && closing.is_empty() {
            return self.fail(capacity(p::CapacityReason::Connections));
        }
        for link in closing {
            self.close(link, None, radio);
        }
        self.pair = Some(Pair {
            candidate: start.candidate,
            link: None,
            address: start.address,
            expected: saved.is_some().then_some(start.peer),
            cleanup: None,
            deadline: now.saturating_add(PAIR_TIMEOUT_MS),
            name: start.name,
            cancelling: None,
            prompt: None,
            quiet: false,
            synced: false,
            saved,
            bonding: None,
        });
        self.set_pairing(
            start.candidate,
            p::pairing::Step::Connecting(p::PairingConnecting {}),
        );
        self.reply(None);
    }

    /// Reports how a finished attempt ended, unless its session has ended.
    fn pairing_ended(&mut self, pair: &Pair, step: p::pairing::Step) {
        if !pair.quiet {
            self.set_pairing(pair.candidate, step);
        }
    }

    fn set_pairing(&mut self, candidate: u32, step: p::pairing::Step) {
        if self.pair.as_ref().is_some_and(|p| p.quiet) {
            return;
        }
        self.pairing = Some(p::Pairing {
            candidate,
            step: Some(step),
        });
        self.pairing_dirty = true;
    }

    fn answer<B: Bluetooth>(
        &mut self,
        accept: bool,
        value: Option<String>,
        radio: &mut B,
        now: u64,
    ) {
        let Some(pair) = &mut self.pair else {
            return self.fail(failure(Error::StalePrompt));
        };
        let (Some(link), None, Some(prompt)) = (pair.link, pair.cancelling, &mut pair.prompt)
        else {
            return self.fail(failure(Error::StalePrompt));
        };
        if prompt.answered || prompt.method.display() || now >= prompt.deadline {
            return self.fail(failure(Error::StalePrompt));
        }
        let value = value.filter(|v| !v.is_empty());
        let action = if accept {
            crate::model::link::PairAction::Accept
        } else {
            crate::model::link::PairAction::Reject
        };
        let entry = matches!(
            prompt.method,
            PromptMethod::EnterPasskey | PromptMethod::EnterPin
        );
        if !prompt.method.valid_reply(
            action,
            if entry && accept {
                value.as_deref()
            } else {
                None
            },
        ) {
            return self.fail(bad_args());
        }
        prompt.answered = true;
        let method = prompt.method;
        self.reply(None);
        let result = radio.pair_reply(link, method, accept, value.as_deref());
        if let Err(e) = result {
            self.stop_pairing(e, radio);
        } else if !accept {
            self.stop_pairing(Error::AuthenticationRejected, radio);
        }
    }

    /// Ends the pairing attempt that has not saved its bond, closing its link.
    fn stop_pairing<B: Bluetooth>(&mut self, reason: Error, radio: &mut B) {
        let Some(pair) = &mut self.pair else { return };
        if pair.cancelling.is_some() {
            return;
        }
        pair.cancelling = Some(reason);
        pair.prompt = None;
        if let Some(link) = pair.link {
            // A user cancellation is a request result, not a link/profile failure.
            self.close(link, (reason != Error::Cancelled).then_some(reason), radio);
        }
    }

    async fn set_device<S: RecordStore, B: Bluetooth>(
        &mut self,
        args: p::SetDevice,
        store: &mut S,
        radio: &mut B,
    ) {
        if !self.manager.storage_ready {
            return self.fail(failure(Error::RadioUnavailable));
        }
        let (original, slot, bond) = match self.policy_record(args.device, store, radio).await {
            Ok(found) => found,
            Err(e) => return self.fail(e),
        };
        let mut policy = original.clone();
        let mut setup = slot
            .and_then(|s| self.manager.devices[s].as_ref())
            .and_then(|d| d.live.as_ref())
            .map(|l| l.setup);
        if let Some(layers) = &args.profiles {
            if !self.manager.profiles_supported() {
                return self.fail(failure(Error::UnsupportedTransport));
            }
            if layers.profiles.len() > profiles::MAX_LAYERS || layers.profiles.contains(&0) {
                return self.fail(bad_args());
            }
            for &id in &layers.profiles {
                if let Err(e) = self.profile_record(id, store, radio).await {
                    return self.fail(e);
                }
            }
            policy.profiles = layers.profiles.iter().map(|&id| id.into()).collect();
        }
        for update in &args.integrations {
            match p::IntegrationKind::try_from(update.kind) {
                Ok(p::IntegrationKind::Hidpp) => {}
                Ok(p::IntegrationKind::Unspecified) => return self.fail(bad_args()),
                Err(_) => return self.fail(failure(Error::UnsupportedTransport)),
            }
            if let Some(enabled) = update.enabled {
                // A user choice settles setup's HID++ detection.
                policy.set_hidpp(enabled);
                if let Some(setup) = &mut setup {
                    setup.hidpp = true;
                }
                policy.setup_pending &= !setup.is_some_and(|s| s.complete());
            }
        }
        if let Some(enabled) = args.enabled {
            policy.enabled = enabled;
        }
        if let Some(trusted) = args.trusted {
            policy.trusted = trusted;
        }
        if let Some(blocked) = args.blocked {
            policy.blocked = blocked;
        }
        if !self.manager.storage_ready {
            return self.fail(failure(Error::RadioUnavailable));
        }
        if policy == original {
            return self.reply(None);
        }
        if self.pair.is_some() {
            // An unresolved pairing may still reveal this identity after native
            // bonding; its storage reservation must not be spent meanwhile.
            return self.fail(failure(Error::Busy));
        }
        let id = policy.id;
        let hidpp = policy.hidpp_enabled();
        if let Err(code) = self
            .manager
            .save_policy(policy, bond.as_ref(), store, radio)
            .await
        {
            if code == Error::NotFound {
                // The device's record was lost; it is deleted as at startup.
                let _ = self.manager.lose(id, store, radio).await;
            }
            let error = if code == Error::Capacity {
                capacity(p::CapacityReason::Enabled)
            } else {
                wire::error(code, None, self.manager.write_uncertain)
            };
            return self.fail(error);
        }
        if let Some(setup) = setup
            && let Some(live) = self
                .manager
                .find(id)
                .and_then(|s| self.manager.devices[s].as_mut())
                .and_then(|d| d.live.as_mut())
        {
            live.setup = setup;
        }
        // Without the integration, a disconnected device's settings show no saved state.
        let integration = original.hidpp_enabled() != hidpp;
        self.mark(
            id,
            if integration {
                DEVICE | SETTINGS
            } else {
                DEVICE
            },
        );
        self.reply(None);
    }

    async fn connect<S: RecordStore, B: Bluetooth>(
        &mut self,
        id: u32,
        store: &mut S,
        radio: &mut B,
        now: u64,
    ) {
        let (policy, slot) = match self.policy_of(id, store, radio).await {
            Ok(found) => found,
            Err(e) => return self.fail(e),
        };
        if !self.manager.storage_ready || !self.manager.radio_ready {
            return self.fail(failure(Error::RadioUnavailable));
        }
        if self.pair.is_some() {
            return self.fail(failure(Error::Busy));
        }
        let Some(slot) = slot else {
            // Only resident devices connect; say why this one is not.
            return self.fail(if policy.blocked {
                failure(Error::Blocked)
            } else if !policy.enabled {
                failure(Error::Disabled)
            } else if !self
                .manager
                .capabilities(radio)
                .supports(policy.peer.transport)
            {
                failure(Error::UnsupportedTransport)
            } else {
                capacity(p::CapacityReason::Enabled)
            });
        };
        let d = self.manager.devices[slot].as_ref().unwrap();
        let layout = if d.state == ConnectionState::Disconnected {
            crate::layouts::load(store, d.id, d.peer.transport).await
        } else {
            None
        };
        match self.manager.connect(
            slot,
            true,
            now.saturating_add(CONNECT_TIMEOUT_MS),
            layout.as_ref(),
            radio,
        ) {
            Ok(_) => {
                self.mark(id.into(), DEVICE);
                self.device_result(id, store, radio).await;
            }
            Err(Error::Capacity) => self.fail(capacity(p::CapacityReason::Connections)),
            Err(e) => self.fail(failure(e)),
        }
    }

    async fn unpair<S: RecordStore, B: Bluetooth>(
        &mut self,
        id: u32,
        store: &mut S,
        radio: &mut B,
    ) {
        let (policy, slot) = match self.policy_of(id, store, radio).await {
            Ok(found) => found,
            Err(e) => return self.fail(e),
        };
        if self.pair.is_some() {
            return self.fail(failure(Error::Busy));
        }
        if !self.manager.storage_ready {
            return self.fail(failure(Error::RadioUnavailable));
        }
        if let Some(slot) = slot {
            self.manager.disconnect(slot, radio).ok();
            if self.manager.link_for(slot).is_some() {
                self.manager.devices[slot].as_mut().unwrap().deleting = true;
                if !self.unpairing.contains(&policy.id) {
                    self.unpairing.push(policy.id);
                }
                self.mark(policy.id, DEVICE);
                return self.reply(None);
            }
        }
        match self.remove(policy.id, policy.peer, store, radio).await {
            Ok(()) => self.reply(None),
            Err(code) => {
                let uncertain = self.manager.write_uncertain;
                self.fail(wire::error(code, None, uncertain));
            }
        }
    }

    async fn remove<S: RecordStore, B: Bluetooth>(
        &mut self,
        id: u64,
        peer: Peer,
        store: &mut S,
        radio: &mut B,
    ) -> Result<(), Error> {
        self.manager.unpair(id, peer, store, radio).await?;
        self.forget_device(id);
        self.removed.push_back(id);
        self.adapter_dirty = true;
        Ok(())
    }

    async fn refresh<S: RecordStore, B: Bluetooth>(
        &mut self,
        id: u32,
        store: &mut S,
        radio: &mut B,
        now: u64,
    ) {
        if let Err(e) = self.policy_of(id, store, radio).await {
            return self.fail(e);
        }
        let Some(slot) = self.manager.find(id.into()) else {
            return self.fail(failure(Error::NotConnected));
        };
        let Some(link) = self.manager.link_for(slot).filter(|_| {
            self.manager.devices[slot].as_ref().unwrap().state == ConnectionState::Connected
        }) else {
            return self.fail(failure(Error::NotConnected));
        };
        radio.refresh_info(link).ok();
        let job = self.job(id.into());
        job.information = true;
        job.read = true;
        self.start_jobs(now);
        self.reply(None);
    }

    fn job(&mut self, id: u64) -> &mut Jobs {
        let index = match self.jobs.iter().position(|(d, _)| *d == id) {
            Some(i) => i,
            None => {
                self.jobs.push((id, Jobs::default()));
                self.jobs.len() - 1
            }
        };
        &mut self.jobs[index].1
    }

    /// Starts waiting settings work on devices whose settings engine is idle. Work for a device that
    /// is not connected waits for its next connection, which applies saved settings anyway.
    fn start_jobs(&mut self, now: u64) {
        let mut i = 0;
        while i < self.jobs.len() {
            let id = self.jobs[i].0;
            let Some(slot) = self.manager.find(id) else {
                self.jobs.swap_remove(i);
                continue;
            };
            let Some(link) = self.manager.link_for(slot) else {
                self.jobs.swap_remove(i);
                continue;
            };
            let c = self.manager.connections[link.slot as usize]
                .as_mut()
                .unwrap();
            let d = self.manager.devices[slot].as_mut().unwrap();
            let hidpp = d.hidpp_enabled;
            let (Some(runtime), Some(live)) = (
                c.runtime.as_mut().filter(|_| !c.closing),
                d.live.as_mut().filter(|l| l.policy.is_some()),
            ) else {
                i += 1;
                continue;
            };
            if runtime.busy() {
                i += 1;
                continue;
            }
            let jobs = &mut self.jobs[i].1;
            if jobs.information {
                if hidpp {
                    runtime.start_information(&mut live.catalog, now).ok();
                }
                jobs.information = false;
            }
            if !runtime.busy() {
                if jobs.read {
                    runtime
                        .start_settings(&mut live.catalog, false, None, false, now)
                        .ok();
                    jobs.read = false;
                } else if jobs.apply {
                    if hidpp {
                        runtime
                            .start_settings(&mut live.catalog, true, None, false, now)
                            .ok();
                    }
                    jobs.apply = false;
                }
            }
            if !jobs.information && !jobs.read && !jobs.apply {
                self.jobs.swap_remove(i);
            } else {
                i += 1;
            }
        }
    }

    async fn change_settings<S: RecordStore, B: Bluetooth>(
        &mut self,
        args: p::SetSettings,
        store: &mut S,
        radio: &mut B,
        now: u64,
    ) {
        let (policy, slot) = match self.policy_of(args.device, store, radio).await {
            Ok(found) => found,
            Err(e) => return self.fail(e),
        };
        if args.changes.is_empty() {
            return self.fail(bad_args());
        }
        let mut parsed: Vec<Change> = Vec::new();
        for change in &args.changes {
            if change.integration != p::IntegrationKind::Hidpp as i32 {
                return self.fail(failure(Error::NotFound));
            }
            let Some(key) = wire::parse_setting_key(&change.key) else {
                return self.fail(failure(Error::NotFound));
            };
            use p::setting_change::Change as C;
            let value = match &change.change {
                Some(C::Value(value)) => match wire::setting_value(key.kind(), value) {
                    Some(value) => Some(value),
                    None => return self.fail(bad_args()),
                },
                Some(C::Forget(_)) => None,
                None => return self.fail(bad_args()),
            };
            // Changes apply in order; a later change to the same setting replaces an earlier one.
            parsed.retain(|c| c.key != key);
            parsed.push(Change { key, value });
        }
        // Free space is checked with every deferred save on flash.
        if let Err(e) = self.save_for_admission(store, now).await {
            return self.fail(e);
        }
        let mut prefs = Preferences {
            store,
            device: policy.id,
        };
        let live = slot
            .and_then(|s| self.manager.devices[s].as_mut())
            .and_then(|d| d.live.as_mut())
            .filter(|l| l.policy.is_some());
        let result = match live {
            Some(live) => live.catalog.change(&parsed, &mut prefs).await,
            None => {
                let mut catalog = match self.saved_catalog(&policy, prefs.store).await {
                    Ok(catalog) => catalog,
                    Err(e) => return self.fail(e),
                };
                let mut prefs = Preferences {
                    store,
                    device: policy.id,
                };
                catalog.change(&parsed, &mut prefs).await
            }
        };
        if let Err(error) = result {
            if error == crate::settings::Error::StorageUnknown {
                self.manager.fail_storage();
                self.adapter_dirty = true;
            }
            return self.fail(wire::error(
                error.code(),
                None,
                error == crate::settings::Error::StorageUnknown,
            ));
        }
        let keys = parsed.iter().fold(0, |keys, c| keys | key_bit(c.key));
        self.touch(policy.id, keys);
        self.job(policy.id).apply = true;
        self.reply(None);
    }

    /// One step of a page of a directory's entries in ascending name order: reads one entry, or
    /// responds once every entry has been read.
    async fn list_files<S: RecordStore>(&mut self, mut listing: FileListing, store: &mut S) {
        match store.file_entry(&listing.path, listing.index).await {
            Ok(Some(entry)) => {
                listing.index += 1;
                if entry.name > listing.after {
                    let at = listing.page.partition_point(|e| e.name < entry.name);
                    listing.page.insert(
                        at,
                        p::FileEntry {
                            name: entry.name,
                            directory: entry.kind == crate::storage::FileType::Directory,
                            size: entry.size as u64,
                        },
                    );
                    listing.page.truncate(FILES_PAGE + 1);
                }
                self.pending = Some(Pending::Files(listing));
            }
            Ok(None) => {
                let mut entries = listing.page;
                let end = entries.len() <= FILES_PAGE;
                entries.truncate(FILES_PAGE);
                self.reply(Some(R::Files(p::FileList { entries, end })));
            }
            Err(crate::storage::Error::Missing) => self.fail(failure(Error::NotFound)),
            Err(_) => self.fail(failure(Error::StorageFailed)),
        }
    }

    async fn read_file<S: RecordStore>(&mut self, path: &str, store: &mut S) {
        if !crate::model::storage_path(path) {
            return self.fail(bad_args());
        }
        let mut data = Vec::new();
        let mut chunk = [0; 512];
        loop {
            match store.file_read(path, data.len() as u32, &mut chunk).await {
                Ok(0) => break,
                Ok(n) => data.extend_from_slice(&chunk[..n]),
                Err(crate::storage::Error::Missing) => return self.fail(failure(Error::NotFound)),
                Err(_) => return self.fail(failure(Error::StorageFailed)),
            }
        }
        self.reply(Some(R::File(p::FileData { data })));
    }

    fn close<B: Bluetooth>(&mut self, link: LinkId, error: Option<Error>, radio: &mut B) {
        let slot = self.manager.connection(link).and_then(|c| c.device);
        self.manager.close(link, error, radio);
        if let Some(slot) = slot {
            self.mark_slot(slot, DEVICE | SETTINGS);
        }
    }

    fn end_session<B: Bluetooth>(&mut self, radio: &mut B) {
        self.stop_scan(radio);
        self.stop_pairing(Error::Cancelled, radio);
        if let Some(pair) = &mut self.pair {
            pair.quiet = true;
        }
        self.candidates = Vec::new();
    }

    /// Follows the serial port opening and closing. Ending a session stops a running scan and
    /// cancels a pairing that has not saved its bond; a new session starts with nothing to report.
    pub fn session<B: Bluetooth>(&mut self, active: bool, radio: &mut B) {
        if self.serial.session(active) {
            self.end_session(radio);
        }
        self.dirty.clear();
        self.event_retries.clear();
        self.touched.clear();
        self.reported.clear();
        self.removed.clear();
        self.profile_dirty.clear();
        self.touched_rules.clear();
        self.profile_removed.clear();
        self.adapter_dirty = false;
        self.pairing_dirty = false;
        self.pairing = None;
        self.scan_done = None;
    }
}

impl Application {
    /// Backends deliver one owned event at a time, after returning from vendor callbacks.
    pub async fn event<S: RecordStore, B: Bluetooth>(
        &mut self,
        event: Event,
        store: &mut S,
        radio: &mut B,
        now: u64,
    ) {
        if self.reboot_at.is_some() {
            return;
        }
        // Input and the other frequent events are handled without waiting. Connection and
        // pairing changes read or write records; their state lives on the heap while they run, so
        // the priority loop's task does not keep room for it.
        if let Some(event) = self.event_now(event, radio, now) {
            Box::pin(self.event_later(event, store, radio, now)).await;
        }
    }

    /// Handles an event that needs no storage. Returns any other event.
    fn event_now<B: Bluetooth>(&mut self, event: Event, radio: &mut B, now: u64) -> Option<Event> {
        match event {
            Event::Failed(error) | Event::Restarting(error) => self.radio_failed(error, now),
            Event::Found {
                scan,
                peer,
                address,
                connectable,
                kind,
                name,
                rssi,
            } => {
                if connectable
                    && self.radio_scan.is_some_and(|(token, _, _)| token == scan)
                    && let Some(slot) = self.manager.peer(peer)
                {
                    self.manager.devices[slot].as_mut().unwrap().seen(now);
                }
                let address = address?;
                let scanning = self.scan.as_ref().is_some_and(|s| {
                    s.token == scan
                        && if peer.transport == Transport::Classic {
                            s.classic
                        } else {
                            s.ble
                        }
                });
                if !scanning {
                    return None;
                }
                let name = devices::display_name(name.as_bytes());
                let rssi = rssi.filter(|r| (-127..=20).contains(r)).map(i32::from);
                if let Some(c) = self.candidates.iter_mut().find(|c| c.peer == peer) {
                    c.address = address;
                    if !name.is_empty() && c.name != name {
                        c.name = name;
                        c.dirty = true;
                    }
                    if kind != DeviceKind::Unknown && c.kind != kind {
                        c.kind = kind;
                        c.dirty = true;
                    }
                    if c.rssi != rssi {
                        c.rssi = rssi;
                        c.dirty = true;
                    }
                } else if self.candidates.len() == devices::SCAN_CANDIDATES {
                    self.truncated = true;
                } else {
                    let Some(seq) = self.candidate_seq.checked_add(1) else {
                        self.truncated = true;
                        return None;
                    };
                    self.candidate_seq = seq;
                    self.candidates.push(Candidate {
                        id: seq,
                        peer,
                        address,
                        kind,
                        name,
                        rssi,
                        dirty: true,
                    });
                }
            }
            Event::Prompt {
                link,
                method,
                value,
            } => {
                let current = self
                    .pair
                    .as_ref()
                    .is_some_and(|p| p.link == Some(link) && p.cancelling.is_none());
                if !current {
                    self.manager
                        .close(link, Some(Error::AuthenticationFailed), radio);
                    return None;
                }
                let pair = self.pair.as_ref().unwrap();
                let numeric = matches!(
                    method,
                    PromptMethod::ConfirmPasskey | PromptMethod::DisplayPasskey
                );
                let valid = if numeric {
                    value
                        .as_ref()
                        .is_some_and(|v| v.len() == 6 && v.bytes().all(|b| b.is_ascii_digit()))
                } else {
                    !value.as_ref().is_some_and(|v| {
                        v.len() > 16 || !v.bytes().all(|b| (0x20..=0x7e).contains(&b))
                    })
                };
                if now >= pair.deadline
                    || pair.prompt.as_ref().is_some_and(|p| !p.answered)
                    || !valid
                {
                    self.stop_pairing(Error::AuthenticationFailed, radio);
                    return None;
                }
                let deadline = pair.deadline.min(now.saturating_add(PROMPT_TIMEOUT_MS));
                let candidate = pair.candidate;
                let step = match method {
                    PromptMethod::EnterPasskey | PromptMethod::EnterPin => {
                        p::pairing::Step::EnterCode(p::EnterCode {
                            kind: if method == PromptMethod::EnterPin {
                                p::CodeKind::Pin
                            } else {
                                p::CodeKind::Passkey
                            } as i32,
                        })
                    }
                    PromptMethod::ConfirmPasskey => p::pairing::Step::ConfirmCode(p::ConfirmCode {
                        passkey: value.as_deref().unwrap_or_default().into(),
                    }),
                    PromptMethod::DisplayPasskey | PromptMethod::DisplayPin => {
                        p::pairing::Step::ShowCode(p::ShowCode {
                            kind: if method == PromptMethod::DisplayPin {
                                p::CodeKind::Pin
                            } else {
                                p::CodeKind::Passkey
                            } as i32,
                            value: value.as_deref().unwrap_or_default().into(),
                        })
                    }
                };
                self.pair.as_mut().unwrap().prompt = Some(Prompt {
                    method,
                    deadline,
                    answered: method.display(),
                });
                self.set_pairing(candidate, step);
            }
            Event::Security { link, security } => {
                if let Some(slot) = self.manager.security(link, security) {
                    self.mark_slot(slot, DEVICE);
                }
            }
            Event::Input(report) => {
                let slot = self.manager.connection(report.link).and_then(|c| c.device);
                match self.manager.input(&report, now) {
                    Ok(true) => {
                        if let Some(slot) = slot {
                            self.mark_slot(slot, DEVICE);
                        }
                    }
                    Err(e) => self.close(report.link, Some(e), radio),
                    _ => {}
                }
            }
            Event::Written { id, result } => match self.manager.written(id, result, now) {
                Ok(Some(slot)) => self.mark_slot(slot, DEVICE),
                Err(e) => self.close(id.link, Some(e), radio),
                _ => {}
            },
            Event::Information {
                link,
                uuid,
                instance,
                success,
                bytes,
            } => {
                if let Some(slot) = self
                    .manager
                    .connection(link)
                    .filter(|c| !c.closing && c.runtime.is_some())
                    .and_then(|c| c.device)
                    && let Some(live) = self.manager.devices[slot]
                        .as_mut()
                        .and_then(|d| d.live.as_mut())
                {
                    let info = &mut live.catalog.info;
                    if success {
                        crate::info::standard(info, uuid, instance, &bytes);
                    } else {
                        crate::info::standard_failed(info, uuid, instance);
                    }
                }
            }
            Event::Read {
                id,
                report_type,
                result,
            } => {
                if let Some(c) = self
                    .manager
                    .connections
                    .get_mut(id.link.slot as usize)
                    .and_then(Option::as_mut)
                    .filter(|c| c.id == id.link && !c.closing)
                    && let Some(slot) = c.device
                    && let Some(link) = &mut c.runtime
                    && let Some(live) = self.manager.devices[slot]
                        .as_mut()
                        .and_then(|d| d.live.as_mut())
                {
                    link.report_read_complete(
                        id,
                        report_type,
                        result.as_ref().map_err(|e| *e),
                        &mut live.catalog,
                        now,
                    );
                }
            }
            event => return Some(event),
        }
        None
    }

    /// Handles an event that reads or writes records.
    async fn event_later<S: RecordStore, B: Bluetooth>(
        &mut self,
        event: Event,
        store: &mut S,
        radio: &mut B,
        now: u64,
    ) {
        match event {
            Event::Ready => self.ready(store, radio).await,
            Event::Incoming { attempt, peer } => {
                if self.pair.is_some() {
                    let _ = radio.incoming(attempt, None, None);
                    return;
                }
                let layout = match self.manager.admits(peer, now) {
                    Some(slot) => {
                        let id = self.manager.devices[slot].as_ref().unwrap().id;
                        crate::layouts::load(store, id, peer.transport).await
                    }
                    None => None,
                };
                if let Ok(Some(slot)) =
                    self.manager
                        .incoming(attempt, peer, now, layout.as_ref(), radio)
                {
                    self.mark_slot(slot, DEVICE);
                }
            }
            Event::Bonded { link, identity } => self.bonded(link, identity, radio, now),
            Event::Connected {
                link,
                descriptors,
                max_output,
                layout,
            } => match self.manager.connected(link, descriptors, max_output, now) {
                Ok(Some(slot)) => {
                    // The device's profiles load before its first input; nothing else does.
                    self.manager.load_profiles(slot, store).await;
                    self.mark_slot(slot, DEVICE | SETTINGS | WARNINGS);
                    if let Some(layout) = layout {
                        let c = self.manager.connection_mut(link).unwrap();
                        c.maps = Some(crate::layouts::maps(&layout));
                        c.layout = Some(layout);
                    }
                }
                Ok(None) => {}
                Err(e) => {
                    // The backend could not use the saved layout.
                    if layout.is_some()
                        && let Some(slot) = self.manager.connection(link).and_then(|c| c.device)
                    {
                        self.remove_layout(slot, store).await;
                    }
                    self.close(link, Some(e), radio);
                }
            },
            Event::Layout {
                link,
                descriptors,
                layout,
            } => {
                let maps = crate::layouts::maps(&layout);
                let Some((slot, unchanged)) = self
                    .manager
                    .connection(link)
                    .filter(|c| !c.closing && c.runtime.is_some())
                    .and_then(|c| Some((c.device?, c.maps == Some(maps))))
                else {
                    return;
                };
                // Changed report characteristics alone leave the parsed maps as they are. The
                // layout is saved in the background.
                if unchanged {
                    self.manager.connection_mut(link).unwrap().layout = Some(layout);
                    return;
                }
                match self.manager.relayout(link, descriptors) {
                    Ok(Some(slot)) => {
                        self.manager.load_profiles(slot, store).await;
                        let c = self.manager.connection_mut(link).unwrap();
                        c.maps = Some(maps);
                        c.layout = Some(layout);
                        self.mark_slot(slot, DEVICE | SETTINGS | WARNINGS);
                    }
                    Ok(None) => {}
                    Err(e) => {
                        self.remove_layout(slot, store).await;
                        self.close(link, Some(e), radio);
                    }
                }
            }
            Event::Disconnected { link, error } => {
                let ended = self
                    .manager
                    .connection(link)
                    .and_then(|c| Some((c.device?, c.error.or(error))));
                self.dropped(link, error, now);
                // A device whose HID layout could not be used is discovered again next time.
                if let Some((slot, Some(Error::UnsupportedHid))) = ended {
                    self.remove_layout(slot, store).await;
                }
                // The stack's bonds are synced in the background.
                self.manager.bonds_pending = true;
            }
            _ => {}
        }
    }

    /// The radio has started: loads every saved record, or brings the restarted radio in line
    /// with the loaded ones.
    async fn ready<S: RecordStore, B: Bluetooth>(&mut self, store: &mut S, radio: &mut B) {
        self.manager.radio_ready = true;
        // A change the backend could not apply before it restarted is applied now.
        let ready = if self.manager.storage_ready {
            match self.manager.apply_transports(radio) {
                Ok(()) => self.manager.sync_bonds(store, radio).await,
                Err(error) => Err(error),
            }
        } else {
            self.manager.load(store, radio).await
        };
        match ready {
            // Loading resolves nothing of a write whose outcome is unknown, so storage stays not
            // ready until that write is.
            Ok(()) => self.writes_recovered(),
            Err(Error::StorageFailed) => self.manager.fail_storage(),
            Err(_) => self.manager.radio_ready = false,
        }
        self.adapter_dirty = true;
    }

    /// The radio failed or is restarting: ends the scan and every link, and reports readiness.
    fn radio_failed(&mut self, error: Error, now: u64) {
        if self.reboot_at.is_some() {
            return;
        }
        if self.scan.take().is_some() {
            self.radio_scan = None;
            self.scan_done = Some(p::ScanDone {
                count: self.candidates.len() as u32,
                truncated: self.truncated,
            });
        }
        for slot in 0..devices::ACTIVE_CONNECTIONS {
            if let Some(id) = self.manager.connections[slot].as_ref().map(|c| c.id) {
                self.dropped(id, Some(error), now);
            }
        }
        self.manager.radio_ready = false;
        if error == Error::StorageFailed {
            self.manager.fail_storage();
        }
        self.adapter_dirty = true;
    }

    /// A failed save leaves the next connection to discover the device again.
    async fn save_layout<S: RecordStore>(
        &mut self,
        slot: usize,
        layout: &crate::bluetooth::Layout,
        store: &mut S,
    ) {
        if !self.manager.storage_ready {
            return;
        }
        let Some(d) = self.manager.devices[slot].as_ref() else {
            return;
        };
        let (id, transport) = (d.id, d.peer.transport);
        let full = self.manager.storage_full();
        let saved = crate::layouts::save(store, id, transport, layout).await;
        self.refresh_available(store).await;
        // Whole-block allocation can take more than the file's size. A layout never takes
        // the room kept for pairing another device.
        if saved && self.manager.storage_full() {
            crate::layouts::remove(store, id).await;
            self.refresh_available(store).await;
        }
        self.adapter_dirty |= full != self.manager.storage_full();
    }

    /// Whether a connection holds a discovered layout to save.
    fn layout_pending(&self) -> bool {
        self.manager
            .connections
            .iter()
            .flatten()
            .any(|c| c.layout.is_some() && c.device.is_some())
    }

    /// Saves the next discovered layout a connection holds, or every one when not `one`.
    async fn save_layouts<S: RecordStore>(&mut self, one: bool, store: &mut S) {
        for index in 0..devices::ACTIVE_CONNECTIONS {
            let Some(c) = self.manager.connections[index].as_mut() else {
                continue;
            };
            let (Some(slot), Some(layout)) = (c.device, c.layout.take()) else {
                continue;
            };
            self.save_layout(slot, &layout, store).await;
            if one {
                return;
            }
        }
    }

    /// For background work with a write to make: whether it may write at `now`, which is once
    /// input pauses.
    fn write_allowed(&mut self, now: u64) -> bool {
        let starting = self.manager.starting(now);
        self.unsaved.wait(now, self.manager.last_input, starting)
    }

    /// Hands the editor's edits to storage as one change of its profile's rules, once the editor
    /// has paused or has been editing for long enough, or at once when `now` is `None`. Returns
    /// `false` when edits are left with the editor because there is no memory to list them; they
    /// are handed over at a later try.
    fn hand_over(&mut self, now: Option<u64>) -> bool {
        let Some(editor) = &mut self.editor else {
            return true;
        };
        let Some((first, last)) = editor.edited else {
            return true;
        };
        if now.is_some_and(|now| {
            now.saturating_sub(last) < EDITOR_BATCH_MS
                && now.saturating_sub(first) < EDITOR_BATCH_MAX_MS
        }) {
            return true;
        }
        let (id, map) = (editor.profile, editor.map.clone());
        let roles = devices::Roles(map.borrow().roles());
        let (rules, summary) = (Record::Rules(id, map), Record::Roles(id, roles));
        // Records already listed need no room; listed rules are this same table.
        let listed = self.unsaved.lists(&rules);
        let needed = usize::from(!listed) + usize::from(!self.unsaved.lists(&summary));
        if !self.unsaved.reserve(needed) && !listed {
            return false;
        }
        editor.edited = None;
        let failure = editor.failure.take();
        editor.retry.succeeded();
        // The edits became unsaved when they were made.
        let marked = self.unsaved.mark(rules.clone(), first);
        debug_assert!(marked);
        if let Some(failure) = failure {
            self.unsaved.set_failure(&rules, failure);
        }
        // Without room for it, the roles summary is listed once the rules are written.
        let _ = self.unsaved.mark(summary, first);
        true
    }

    /// Releases the editor's profile, handing its edits to storage first. An editor whose edits
    /// cannot be handed over yet is kept, so they are not lost.
    pub(crate) fn drop_editor(&mut self) {
        if self.hand_over(None) {
            self.editor = None;
        }
    }

    /// Whether anything waits to be saved: an editor's edits, a dirty record or a discovered
    /// layout.
    pub fn has_unsaved(&self) -> bool {
        self.editor.as_ref().is_some_and(|e| e.edited.is_some())
            || !self.unsaved.is_empty()
            || self.layout_pending()
    }

    /// Hands an editor's edits to storage once it pauses, and writes the next dirty record once
    /// it is due. Returns whether it wrote.
    pub async fn save<S: RecordStore>(&mut self, store: &mut S, now: u64) -> bool {
        let listed = self.hand_over(Some(now));
        // A failed save that made storage not ready is still retried.
        if !(self.manager.storage_ready || self.manager.unsaved_unready) {
            return false;
        }
        let starting = self.manager.starting(now);
        // Edits there is no memory to list are written straight from the editor, on the timing
        // of a dirty record: their longest wait counts from the first edit, or from the end of
        // the backoff of a failed write.
        if !listed
            && self.editor.as_ref().is_some_and(|e| {
                e.edited.is_some_and(|(first, _)| {
                    let since = first.max(e.retry.at());
                    deferred::allowed(since, now, self.manager.last_input, starting)
                })
            })
            && self.save_editor(store, now, false).await
        {
            return true;
        }
        let Some(record) = self.unsaved.next(now, self.manager.last_input, starting) else {
            return false;
        };
        let _ = self.write(record, store, now).await;
        true
    }

    /// Saves everything waiting at once, whatever input is doing: before USB enumerates again,
    /// before the bootloader and when an editor is released.
    pub async fn save_all<S: RecordStore>(&mut self, store: &mut S, now: u64) {
        self.save_records(store, now).await;
        if self.manager.storage_ready {
            self.save_layouts(false, store).await;
        }
    }

    /// Saves an editor's edits and every dirty record at once, whatever input is doing, as before
    /// free space is checked for new data. Each dirty record is tried once; one that fails stays
    /// dirty and is retried after its backoff. Layouts are left to wait, since a layout never
    /// takes the room admission keeps.
    async fn save_records<S: RecordStore>(&mut self, store: &mut S, now: u64) {
        let listed = self.hand_over(None);
        if !(self.manager.storage_ready || self.manager.unsaved_unready) {
            return;
        }
        if !listed {
            self.save_editor(store, now, true).await;
        }
        self.unsaved.start_flush();
        while let Some(record) = self.unsaved.next_flushed() {
            let _ = self.write(record, store, now).await;
        }
    }

    /// Writes the editor's edits, which there was no memory to list, straight from its table,
    /// once its own backoff allows, or at once when `forced`. Edits that cannot be written stay
    /// with the editor, which tracks the outcome as a listed record would. Returns whether it
    /// wrote.
    pub(crate) async fn save_editor<S: RecordStore>(
        &mut self,
        store: &mut S,
        now: u64,
        forced: bool,
    ) -> bool {
        let Some(editor) = self
            .editor
            .as_ref()
            .filter(|e| e.edited.is_some() && (forced || e.retry.due(now)))
        else {
            return false;
        };
        let record = Record::Rules(editor.profile, editor.map.clone());
        let result = self.write(record, store, now).await;
        if let Some(editor) = self.editor.as_mut() {
            match result {
                Ok(()) => {
                    editor.edited = None;
                    editor.failure = None;
                    editor.retry.succeeded();
                }
                Err(error) => {
                    editor.failure = Some(error);
                    editor.retry.failed_up_to(now, deferred::RULES_RETRY_MAX_MS);
                }
            }
        }
        self.writes_recovered();
        true
    }

    /// Saves everything [`Self::save_records`] does before free space is counted to admit new
    /// data. Fails as storage not being ready, before or after the saves, so nothing is admitted
    /// once a save has made it so.
    async fn save_for_admission<S: RecordStore>(
        &mut self,
        store: &mut S,
        now: u64,
    ) -> Result<(), p::Error> {
        if self.manager.storage_ready {
            self.save_records(store, now).await;
        }
        if self.manager.storage_ready {
            Ok(())
        } else {
            Err(failure(Error::RadioUnavailable))
        }
    }

    /// Writes one dirty record. A rules file that cannot be written, which holds edits an editor
    /// has been told are made, is retried with its backoff, and the failure of its last try is
    /// reported ([`Self::writes_recovered`]). A failure that is not reported leaves storage ready,
    /// so the profile can still be changed, overwritten or deleted. Roles summaries are repaired
    /// without being reported, as when they are saved at once.
    ///
    /// After a write whose outcome is unknown, a failed retry ends that only when the file is then
    /// read: it holds the new rules, which counts as saved, or other contents, which leaves the
    /// definite failure. A retry that fails before reaching the file, or whose file cannot be
    /// read, keeps the outcome unknown. Returns how the write ended.
    async fn write<S: RecordStore>(
        &mut self,
        record: Record,
        store: &mut S,
        now: u64,
    ) -> Result<(), crate::storage::Error> {
        use crate::storage::Error as E;
        let unknown = self.unsaved.failure(&record) == Some(E::Unknown)
            || matches!(&record, Record::Rules(id, _)
                if self.editor.as_ref().is_some_and(|e| e.profile == *id
                    && e.failure == Some(E::Unknown)));
        let result = match &record {
            Record::Rules(id, map) => {
                let (bytes, roles) = {
                    let rules = map.borrow();
                    (rules.saved(), devices::Roles(rules.roles()))
                };
                let bytes = match bytes {
                    Ok(bytes) => bytes,
                    Err(error) => {
                        let error = if unknown { E::Unknown } else { error };
                        self.write_failed(&record, error, now);
                        return Err(error);
                    }
                };
                let saved = match profiles::write_rules(store, *id, bytes.as_deref()).await {
                    Err(error) if unknown && error != E::Unknown => {
                        match profiles::rules_hold(store, *id, bytes.as_deref()).await {
                            Ok(true) => Ok(()),
                            Ok(false) => Err(error),
                            Err(_) => Err(E::Unknown),
                        }
                    }
                    saved => saved,
                };
                // The roles summary that follows describes the rules just written. One that could
                // not be listed with the rules is listed now; without memory for it, listings
                // report the saved summary until the next save of the rules.
                if saved.is_ok() {
                    if self.unsaved.lists(&Record::Roles(*id, roles)) {
                        self.unsaved.follow_rules(*id, roles);
                    } else if !self.unsaved.mark(Record::Roles(*id, roles), now) {
                        self.mark_profile(*id, PROFILE);
                    }
                }
                saved
            }
            Record::Roles(id, roles) => match profiles::metadata(store, *id).await {
                Ok(meta) if meta.roles == *roles => Ok(()),
                Ok(mut meta) => {
                    meta.roles = *roles;
                    let saved = profiles::save_metadata(store, *id, &meta).await;
                    if saved.is_ok() {
                        self.mark_profile(*id, PROFILE);
                    }
                    saved
                }
                // A profile that is gone needs nothing; one that is undecodable is lost.
                Err(E::Missing) => Ok(()),
                Err(E::Corrupt) => {
                    self.lost_profile(*id);
                    Ok(())
                }
                Err(error) => Err(error),
            },
            Record::DeviceRoles(id, roles) => match (Policies { store }).load_record(*id).await {
                Ok((mut policy, bond)) if policy.roles != *roles => {
                    policy.roles = *roles;
                    Policies { store }.save_with(&policy, &bond).await
                }
                // A record that is gone or lost is dealt with when it is next read.
                Ok(_) | Err(E::Missing | E::Corrupt) => Ok(()),
                Err(error) => Err(error),
            },
        };
        match result {
            Ok(()) => {
                self.unsaved.saved(&record);
                self.writes_recovered();
            }
            Err(error) => self.write_failed(&record, error, now),
        }
        result
    }

    fn write_failed(&mut self, record: &Record, error: crate::storage::Error, now: u64) {
        self.unsaved.failed(record, error, now);
        self.writes_recovered();
    }

    /// Reports the failures of the last tries of the dirty rules files: storage is full while one
    /// found the filesystem full, and not ready while one ended with an unknown outcome. Other
    /// failures, including running out of memory for a file's buffer, are only retried.
    fn writes_recovered(&mut self) {
        use crate::storage::Error as E;
        let editor = |error| {
            self.editor
                .as_ref()
                .is_some_and(|e| e.failure == Some(error))
        };
        let full = self.unsaved.rules_failed(E::Full) || editor(E::Full);
        if full != self.manager.unsaved_full {
            self.manager.unsaved_full = full;
            self.adapter_dirty = true;
        }
        let unknown = self.unsaved.rules_failed(E::Unknown) || editor(E::Unknown);
        if unknown && self.manager.storage_ready {
            self.manager.fail_storage();
            self.manager.unsaved_unready = true;
            self.adapter_dirty = true;
        } else if !unknown && core::mem::take(&mut self.manager.unsaved_unready) {
            // Storage that another failure has made not ready since stays so: that failure
            // cleared `unsaved_unready`.
            self.manager.storage_ready = true;
            self.adapter_dirty = true;
        }
    }

    /// Counts free space for the status. A failed count keeps the last estimate until the next
    /// write.
    async fn refresh_available<S: RecordStore>(&mut self, store: &mut S) {
        let full = self.manager.storage_full();
        self.counted = store.generation().await.ok();
        if let Ok(bytes) = store.available().await {
            self.manager.available_bytes = bytes;
        }
        self.adapter_dirty |= full != self.manager.storage_full();
    }

    /// Removes a saved layout that no longer describes the device.
    async fn remove_layout<S: RecordStore>(&mut self, slot: usize, store: &mut S) {
        if let Some(d) = self
            .manager
            .devices
            .get(slot)
            .and_then(Option::as_ref)
            .filter(|_| self.manager.storage_ready)
        {
            crate::layouts::remove(store, d.id).await;
        }
    }

    /// The stack has saved the pairing attempt's bond. The saved device with its identity is
    /// found in the secondary loop before the bond is saved and the link admitted.
    fn bonded<B: Bluetooth>(&mut self, link: LinkId, identity: Peer, radio: &mut B, now: u64) {
        // Stale callbacks cannot delete a newer connection's native bond.
        // Backends finish rejected-pair cleanup before Disconnected.
        if self
            .manager
            .connection(link)
            .is_none_or(|c| c.device.is_some())
        {
            return;
        }
        let current = self.pair.as_ref().is_some_and(|p| p.link == Some(link));
        if !current {
            self.manager
                .close(link, Some(Error::AuthenticationRejected), radio);
            return;
        }
        let active = self.serial.active();
        let link_peer = self.manager.connection(link).unwrap().peer;
        let pair = self.pair.as_mut().unwrap();
        if pair.bonding.is_some() {
            return;
        }
        if pair.cancelling.is_some() || now >= pair.deadline || !active {
            return self.bond_failed(identity, Error::AuthenticationRejected, radio);
        }
        // A Classic bond keeps the address it paired with, and no bond changes transport.
        if identity.transport != link_peer.transport
            || (identity.transport == Transport::Classic && identity != link_peer)
        {
            return self.bond_failed(identity, Error::AuthenticationFailed, radio);
        }
        pair.bonding = Some(Bonding {
            identity,
            scan: Records::new(2),
            direct: false,
        });
    }

    /// Ends the attempt whose bond could not be saved.
    fn bond_failed<B: Bluetooth>(&mut self, identity: Peer, error: Error, radio: &mut B) {
        if let Some(pair) = &mut self.pair
            && pair.expected.is_none()
        {
            // Adopt may already have succeeded before the record write
            // failed. Explicit cleanup waits for disconnection.
            pair.cleanup = Some(identity);
        }
        self.stop_pairing(error, radio);
    }

    /// One step of matching the attempt's new bond with a saved device: looks up a resident device
    /// or the saved device the attempt started with, or reads one saved device record, and saves
    /// the bond once the device with its identity is found or every record has been read.
    async fn match_bond<S: RecordStore, B: Bluetooth>(
        &mut self,
        store: &mut S,
        radio: &mut B,
        now: u64,
    ) {
        let Some(pair) = &self.pair else { return };
        let (Some(link), Some(bonding)) = (pair.link, &pair.bonding) else {
            return;
        };
        let identity = bonding.identity;
        if !bonding.direct {
            let known = self
                .manager
                .peer(identity)
                .and_then(|slot| self.manager.devices[slot].as_ref())
                .map(|d| d.id)
                .or(pair.saved.filter(|_| pair.expected == Some(identity)));
            self.pair.as_mut().unwrap().bonding.as_mut().unwrap().direct = true;
            if let Some(id) = known {
                match (Policies { store }).load(id).await {
                    Ok(policy) if policy.peer == identity => {
                        return self
                            .finish_bond(link, identity, Some(policy), store, radio, now)
                            .await;
                    }
                    Ok(_)
                    | Err(crate::storage::Error::Missing | crate::storage::Error::Corrupt) => {}
                    Err(_) => return self.bond_failed(identity, Error::StorageFailed, radio),
                }
                return;
            }
        }
        let scan = &mut self.pair.as_mut().unwrap().bonding.as_mut().unwrap().scan;
        let saved = match scan.next(store).await {
            Err(error) => return self.bond_failed(identity, error, radio),
            Ok(Visit::Page) => return,
            Ok(Visit::Record(id)) => match (Policies { store }).load(id).await {
                Ok(policy) if policy.peer == identity => Some(policy),
                Ok(_) | Err(crate::storage::Error::Missing | crate::storage::Error::Corrupt) => {
                    return;
                }
                Err(_) => return self.bond_failed(identity, Error::StorageFailed, radio),
            },
            Ok(Visit::Done) => None,
        };
        self.finish_bond(link, identity, saved, store, radio, now)
            .await
    }

    /// Saves the attempt's bond for `saved`, the saved device with its identity, or for a new
    /// device, and admits the link.
    async fn finish_bond<S: RecordStore, B: Bluetooth>(
        &mut self,
        link: LinkId,
        identity: Peer,
        saved: Option<Policy>,
        store: &mut S,
        radio: &mut B,
        now: u64,
    ) {
        let name = self.pair.as_ref().unwrap().name.clone();
        let result = self
            .manager
            .bonded(link, identity, saved, name.as_bytes(), store, radio)
            .await;
        match result {
            Ok((id, slot)) => {
                if let Some(c) = self.manager.connection_mut(link) {
                    c.deadline = now.saturating_add(CONNECT_TIMEOUT_MS);
                }
                let pair = self.pair.take().unwrap();
                for c in &mut self.candidates {
                    if c.id == pair.candidate {
                        c.peer = identity;
                    }
                }
                // A device that is disabled, or has no room in the stack, connects once enabled.
                if slot.is_none() {
                    self.close(link, None, radio);
                }
                self.mark(id, DEVICE | SETTINGS | WARNINGS);
                self.adapter_dirty = true;
                self.pairing_ended(
                    &pair,
                    p::pairing::Step::Done(p::PairingDone { device: id as u32 }),
                );
            }
            Err(error) => self.bond_failed(identity, error, radio),
        }
    }

    /// Advances the pairing attempt: opens its link once other setup links have closed and the
    /// stack's bonds have been synced, and finishes a cancelled attempt once its link is gone.
    /// Returns whether it used storage.
    async fn advance_pair<S: RecordStore, B: Bluetooth>(
        &mut self,
        store: &mut S,
        radio: &mut B,
        now: u64,
    ) -> bool {
        let Some(pair) = &self.pair else { return false };
        if pair.link.is_some() {
            let prompt_expired = pair
                .prompt
                .as_ref()
                .is_some_and(|p| !p.answered && now >= p.deadline);
            if (pair.cancelling.is_none() && now >= pair.deadline) || prompt_expired {
                self.stop_pairing(Error::Timeout, radio);
                return false;
            }
            // A link the priority loop is closing reports its own error once it is gone.
            let open = pair
                .link
                .and_then(|link| self.manager.connection(link))
                .is_some_and(|c| !c.closing);
            if pair.cancelling.is_none() && pair.bonding.is_some() && open {
                self.match_bond(store, radio, now).await;
                return true;
            }
            return false;
        }
        if let Some(error) = pair.cancelling {
            let cleanup = self.pair.as_mut().unwrap().cleanup.take();
            let result = if let Some(peer) = cleanup {
                radio.forget(peer).await
            } else {
                Ok(())
            };
            let restored = self.manager.finish_pair(store).await;
            let code = result.err().or(restored.err()).unwrap_or(error);
            let pair = self.pair.take().unwrap();
            self.pairing_ended(
                &pair,
                p::pairing::Step::Failed(wire::error_code(code) as i32),
            );
            return true;
        }
        if now >= pair.deadline {
            self.stop_pairing(Error::Timeout, radio);
            return false;
        }
        if !self.links_settled() {
            return false;
        }
        if !self.manager.radio_ready || !self.manager.storage_ready {
            self.stop_pairing(Error::RadioUnavailable, radio);
            return false;
        }
        // Background work syncs the stack's bonds first, one record per step.
        if !pair.synced {
            return false;
        }
        let (address, expected, deadline) = (pair.address, pair.expected, pair.deadline);
        // Free space is checked with every deferred save on flash.
        if self.save_for_admission(store, now).await.is_err() {
            self.stop_pairing(Error::RadioUnavailable, radio);
            return true;
        }
        let result = match self
            .manager
            .prepare_pair(address, expected, store, radio)
            .await
        {
            Ok(()) => self.manager.pair(address, deadline, radio),
            Err(error) => Err(error),
        };
        match result {
            Ok(id) => self.pair.as_mut().unwrap().link = Some(id),
            Err(error) => {
                let error = self.manager.finish_pair(store).await.err().unwrap_or(error);
                let pair = self.pair.take().unwrap();
                self.pairing_ended(
                    &pair,
                    p::pairing::Step::Failed(wire::error_code(error) as i32),
                );
            }
        }
        true
    }

    /// Whether the stack's bonds should be synced: they may differ from the resident entries and
    /// no pairing attempt is under way, or an attempt waits for them before opening its link,
    /// once no other link is being set up or closing.
    fn sync_wanted(&self) -> bool {
        match &self.pair {
            None => self.manager.bonds_pending,
            Some(pair) => {
                pair.link.is_none()
                    && pair.cancelling.is_none()
                    && !pair.synced
                    && self.links_settled()
            }
        }
    }

    /// Whether no link is being set up or closing.
    fn links_settled(&self) -> bool {
        !self
            .manager
            .connections
            .iter()
            .flatten()
            .any(|c| c.closing || c.runtime.is_none())
    }

    /// Whether syncing the stack's bonds can go ahead: not while a pairing attempt's link holds
    /// its provisional bond.
    fn sync_allowed(&self) -> bool {
        self.pair.as_ref().is_none_or(|p| p.link.is_none())
    }

    fn update_scan<B: Bluetooth>(&mut self, radio: &mut B, reconnecting: bool, now: u64) {
        if !self.manager.radio_ready {
            return;
        }
        let mut wanted = self.scan.as_ref().map(|s| (s.token, s.classic, s.ble));
        // Exclusive backends alternate one-second windows while saved peers are
        // eligible. The logical scan stays open, retaining its token and results.
        if reconnecting && !radio.capabilities().ble_scan_and_connect && now % 2000 >= 1000 {
            wanted = wanted.map(|(token, classic, _)| (token, classic, false));
            if wanted.is_some_and(|(_, classic, ble)| !classic && !ble) {
                wanted = None;
            }
        }
        if wanted != self.radio_scan {
            let (token, classic, ble) = wanted.unwrap_or((self.scan_seq, false, false));
            if radio.scan(token, classic, ble).is_ok() {
                self.radio_scan = wanted;
            }
        }
    }

    /// Finishes one unpair whose link has now closed, not while a connection waits for its first
    /// input. Returns whether it used storage.
    async fn finish_unpairs<S: RecordStore, B: Bluetooth>(
        &mut self,
        store: &mut S,
        radio: &mut B,
        now: u64,
    ) -> bool {
        if self.manager.starting(now) {
            return false;
        }
        let mut i = 0;
        while i < self.unpairing.len() {
            let id = self.unpairing[i];
            let Some(slot) = self.manager.find(id) else {
                self.unpairing.swap_remove(i);
                continue;
            };
            if self.manager.link_for(slot).is_some() {
                i += 1;
                continue;
            }
            self.unpairing.swap_remove(i);
            let peer = self.manager.devices[slot].as_ref().unwrap().peer;
            if let Err(code) = self.remove(id, peer, store, radio).await
                && let Some(d) = self.manager.devices[slot].as_mut()
            {
                d.deleting = false;
                d.error = Some(code);
                self.mark(id, DEVICE);
            }
            return true;
        }
        false
    }

    /// One step of background work: continues the setup, pairing, unpair or storage work in
    /// progress, or starts the next. All but the pairing the user is waiting on wait while a
    /// connection waits for its first input. Returns whether it did anything.
    async fn maintain<S: RecordStore, B: Bluetooth>(
        &mut self,
        store: &mut S,
        radio: &mut B,
        now: u64,
    ) -> bool {
        self.unsaved.start_pass();
        // A pairing's bond is matched before other work, so the device connects promptly.
        let worked = self.advance_pair(store, radio, now).await
            || self.continue_work(store, radio, now).await
            || self.setup(store, radio, now).await
            || self.finish_unpairs(store, radio, now).await
            || self.background(store, radio, now).await;
        if !worked {
            self.unsaved.end_pass();
        }
        worked
    }

    /// Background storage work: reading connected devices' policies, saving their layouts,
    /// cleaning up lost device records and profiles, filling the stack, syncing its bonds,
    /// repairing profile roles and retrying profile loads. One step at a time, none while a
    /// connection waits for its first input. Failed work is tried again after a backoff, and work
    /// waiting for its backoff does not hold up the rest. Returns whether it did anything.
    async fn background<S: RecordStore, B: Bluetooth>(
        &mut self,
        store: &mut S,
        radio: &mut B,
        now: u64,
    ) -> bool {
        if !self.manager.storage_ready || self.manager.starting(now) {
            return false;
        }
        match self.manager.hydrate(store, now).await {
            Ok(Some((slot, roles))) => {
                self.mark_slot(slot, DEVICE | SETTINGS);
                if let Some(id) = self.manager.devices[slot].as_ref().map(|d| d.id) {
                    self.job(id).apply = true;
                    match roles {
                        // Without memory to list it, the record is updated at a later
                        // connection.
                        Some((roles, true)) => {
                            let _ = self.unsaved.mark(Record::DeviceRoles(id, roles), now);
                        }
                        // The record holds the roles reported now, whatever an earlier
                        // connection left waiting.
                        Some((roles, false)) => self.unsaved.saved(&Record::DeviceRoles(id, roles)),
                        None => {}
                    }
                }
                return true;
            }
            Ok(None) => {}
            Err((id, Error::NotFound)) => {
                // The connected device's record was lost.
                let _ = self.manager.lose(id, store, radio).await;
                return true;
            }
            Err(_) => return true,
        }
        if self.layout_pending() && self.write_allowed(now) {
            self.save_layouts(true, store).await;
            return true;
        }
        if let Some(&id) = self.manager.lost_devices.first()
            && self.lost_retry.due(now)
            && self.write_allowed(now)
        {
            if (Policies { store }).remove(id).await.is_ok() {
                self.manager.lost_devices.remove(0);
                self.lost_retry.succeeded();
            } else {
                // Kept, behind any others, until a cleanup succeeds.
                self.manager.lost_devices.rotate_left(1);
                self.lost_retry.failed(now);
            }
            return true;
        }
        // Work that takes several steps starts once the previous one has finished.
        if self.work.is_none() {
            if let Some(&id) = self.manager.lost_profiles.first()
                && self.lost_retry.due(now)
                && self.write_allowed(now)
            {
                self.manager.lost_profiles.remove(0);
                self.work = Some(Work::LostProfile(id, None));
                return self.continue_work(store, radio, now).await;
            }
            // A pairing attempt waiting for the sync does not wait out the backoff.
            let pairing = self.pair.is_some() && self.sync_wanted();
            if (self.manager.vacated || (self.sync_wanted() && self.sync_allowed()))
                && (self.bonds_retry.due(now) || pairing)
            {
                if self.manager.vacated {
                    let changed = self.manager.retire();
                    if self.manager.has_room() {
                        self.work = Some(Work::Fill {
                            scan: Records::new(2),
                            changed,
                        });
                    } else {
                        self.filled(Ok(changed), radio, now);
                    }
                } else {
                    self.work = Some(Work::Bonds(BondSync::Inventory));
                }
                return self.continue_work(store, radio, now).await;
            }
        }
        // Free space is counted for the status once writes have stopped. Admission checks count
        // it themselves when it is out of date.
        if let Ok(generation) = store.generation().await
            && self.counted != Some(generation)
        {
            self.refresh_available(store).await;
            return true;
        }
        // Released memory lets each device that did not fit try again, in turn, in a pass over
        // the devices. A release during a pass is left to the next one.
        if self.retry_from == 0 {
            self.retry_released = core::mem::take(&mut self.profiles_released)
                || self.manager.profiles.used() < self.reported_memory;
        }
        match self
            .manager
            .retry_profiles(store, self.retry_released, self.retry_from, now)
            .await
        {
            Some((slot, changed)) => {
                self.retry_from = slot + 1;
                if changed {
                    self.mark_slot(slot, DEVICE);
                }
                true
            }
            None => {
                self.retry_from = 0;
                false
            }
        }
    }

    /// One step of the background work in progress, unless it is paused. Returns whether it did
    /// anything.
    async fn continue_work<S: RecordStore, B: Bluetooth>(
        &mut self,
        store: &mut S,
        radio: &mut B,
        now: u64,
    ) -> bool {
        if !self.manager.storage_ready || self.manager.starting(now) {
            return false;
        }
        let Some(work) = self.work.take() else {
            return false;
        };
        match work {
            Work::Fill { mut scan, changed } => match scan.next(store).await {
                Err(error) => self.filled(Err(error), radio, now),
                Ok(Visit::Done) => self.filled(Ok(changed), radio, now),
                Ok(Visit::Page) => self.work = Some(Work::Fill { scan, changed }),
                Ok(Visit::Record(id)) => match self.manager.fill_record(id, store).await {
                    Ok(added) => {
                        self.work = Some(Work::Fill {
                            scan,
                            changed: changed || added,
                        })
                    }
                    Err(error) => self.filled(Err(error), radio, now),
                },
            },
            Work::Bonds(sync) => {
                if !self.sync_allowed() {
                    self.work = Some(Work::Bonds(sync));
                    return false;
                }
                self.sync_bonds(sync, store, radio, now).await;
            }
            Work::LostProfile(id, scan) => {
                if !self.write_allowed(now) {
                    self.work = Some(Work::LostProfile(id, scan));
                    return false;
                }
                match self.clean_lost_profile(id, scan, store, radio, now).await {
                    Ok(Some(scan)) => self.work = Some(Work::LostProfile(id, Some(scan))),
                    Ok(None) => {
                        // A connection that found it lost meanwhile queued it again.
                        self.manager.lost_profiles.retain(|p| *p != id);
                        self.lost_retry.succeeded();
                    }
                    Err(_) => {
                        // Kept, behind any others, until a cleanup succeeds.
                        if !self.manager.lost_profiles.contains(&id) {
                            self.manager.lost_profiles.push(id);
                        }
                        self.lost_retry.failed(now);
                    }
                }
            }
        }
        true
    }

    /// Ends a fill: syncs the stack's bonds next when the resident set changed.
    fn filled<B: Bluetooth>(&mut self, result: Result<bool, Error>, radio: &mut B, now: u64) {
        match result {
            Ok(changed) => {
                self.manager.bonds_pending |= changed;
                if self.sync_wanted() && self.sync_allowed() {
                    self.work = Some(Work::Bonds(BondSync::Inventory));
                } else {
                    self.bonds_retry.succeeded();
                }
            }
            Err(error) => {
                // A failure leaves the work to the next fill. An attempt waiting for the sync
                // that follows fails with it.
                if self.pair.is_some() && self.sync_wanted() {
                    self.stop_pairing(error, radio);
                }
                self.manager.vacated = true;
                self.bonds_retry.failed(now);
            }
        }
    }

    /// One step of syncing the stack's bonds with the resident entries: reads the stack's bonds,
    /// removes one that no resident entry or link uses, or loads one entry's saved bond. Each
    /// step checks the entries as they are then, so changes between steps are followed.
    async fn sync_bonds<S: RecordStore, B: Bluetooth>(
        &mut self,
        sync: BondSync,
        store: &mut S,
        radio: &mut B,
        now: u64,
    ) {
        let result = match sync {
            BondSync::Inventory => {
                // A change during the sync, such as a link closing after its bond was kept, asks
                // for another one.
                self.manager.bonds_pending = false;
                radio
                    .bonds()
                    .await
                    .map(|peers| Some(BondSync::Forget(peers)))
            }
            BondSync::Forget(mut peers) => match peers.pop() {
                Some(peer) => self
                    .manager
                    .forget_stale(peer, radio)
                    .await
                    .map(|()| Some(BondSync::Forget(peers))),
                None => Ok(Some(BondSync::Import(0))),
            },
            BondSync::Import(slot) => match self.manager.next_resident(slot) {
                Some(slot) => self
                    .manager
                    .import(slot, store, radio)
                    .await
                    .map(|()| Some(BondSync::Import(slot + 1))),
                None => Ok(None),
            },
        };
        match result {
            Ok(Some(next)) => self.work = Some(Work::Bonds(next)),
            Ok(None) => {
                self.bonds_retry.succeeded();
                // A link still closing kept its bond; the attempt waits for another sync.
                let synced = self.links_settled() && !self.manager.bonds_pending;
                if let Some(pair) = &mut self.pair {
                    pair.synced |= synced;
                }
            }
            Err(error) => {
                self.manager.bonds_pending = true;
                self.bonds_retry.failed(now);
                // An attempt waiting for the sync fails with it.
                if self.pair.is_some() && self.sync_wanted() {
                    self.stop_pairing(error, radio);
                }
            }
        }
    }

    /// Releases an editor's profile once the editor has gone quiet or its interface changed, and
    /// saves its edits.
    async fn release_editor<S: RecordStore>(&mut self, store: &mut S, now: u64) {
        let Some(editor) = &self.editor else { return };
        let saved = interfaces::preference(
            &self.manager.preference.configuration_interfaces,
            editor.interface,
        );
        if now.saturating_sub(editor.last) >= EDITOR_IDLE_MS
            || !saved.enabled
            || saved.profile != Some(editor.profile)
        {
            self.drop_editor();
            // Edits there is no memory to hand over are written from the editor first. An editor
            // whose edits are still not saved is released at a later try.
            if self.editor.is_some()
                && (self.manager.storage_ready || self.manager.unsaved_unready)
                && self.save_editor(store, now, false).await
            {
                self.drop_editor();
            }
            if self.editor.is_none() {
                self.save_all(store, now).await;
            }
        }
    }

    /// Marks devices whose settings, information or warnings changed since the last check.
    fn collect_changes(&mut self) {
        for slot in 0..self.manager.devices.len() {
            let Some(d) = self.manager.devices[slot].as_mut() else {
                continue;
            };
            let id = d.id;
            let Some(live) = d.live.as_mut() else {
                continue;
            };
            let mut bits = 0;
            let mut keys = if core::mem::take(&mut live.catalog.catalog_changed) {
                ALL_KEYS
            } else {
                0
            };
            keys |= live
                .catalog
                .take_changed()
                .fold(0, |keys, r| keys | key_bit(r.metadata.key));
            if keys != 0 {
                // Read-only records are information on the device record.
                bits |= DEVICE;
            }
            if !live.catalog.info.changes().is_empty() {
                bits |= DEVICE;
            }
            if core::mem::take(&mut live.warnings_changed) {
                bits |= WARNINGS;
            }
            if keys != 0 {
                self.touch(id, keys);
            }
            if bits != 0 {
                self.mark(id, bits);
            }
        }
        while let Some(id) = self.manager.removed.pop() {
            self.forget_device(id);
            self.removed.push_back(id);
            self.adapter_dirty = true;
        }
        while let Some(id) = self.manager.changed.pop() {
            self.mark(id, DEVICE);
        }
        let ready = self.manager.radio_ready && self.manager.storage_ready;
        if self.reported_ready != ready {
            self.reported_ready = ready;
            self.adapter_dirty = true;
        }
        let memory = self.manager.profiles.used();
        // A release is remembered until the background step that retries loading runs.
        self.profiles_released |= memory < self.reported_memory;
        if self.reported_memory != memory {
            self.reported_memory = memory;
            self.adapter_dirty |= self.manager.profiles_supported();
        }
        self.release_reported();
    }

    /// Writes at most one pending event, when the serial output is free. Returns whether it
    /// wrote one.
    async fn flush<S: RecordStore, B: Bluetooth>(
        &mut self,
        store: &mut S,
        radio: &mut B,
        now: u64,
    ) -> bool {
        if !self.serial.idle() {
            return false;
        }
        let event = if core::mem::take(&mut self.adapter_dirty) {
            Some(Ev::Adapter(self.status(radio)))
        } else if let Some(id) = self.profile_removed.pop_front() {
            Some(Ev::ProfileRemoved(p::ProfileRemoved { id: id as u32 }))
        } else if let Some(event) = self.profile_event(store, radio, now).await {
            Some(event)
        } else if let Some(id) = self.removed.pop_front() {
            Some(Ev::DeviceRemoved(p::DeviceRemoved { id: id as u32 }))
        } else if core::mem::take(&mut self.pairing_dirty)
            && let Some(pairing) = self.pairing.clone()
        {
            Some(Ev::Pairing(pairing))
        } else {
            let event = if self.scan_turn {
                match self.scan_event() {
                    Some(event) => Some(event),
                    None => self.device_event(store, radio, now).await,
                }
            } else {
                match self.device_event(store, radio, now).await {
                    Some(event) => Some(event),
                    None => self.scan_event(),
                }
            };
            if event.is_some() {
                self.scan_turn = !self.scan_turn;
            }
            event
        };
        match event {
            Some(event) => {
                self.serial.event(event);
                true
            }
            None => false,
        }
    }

    /// Whether event `read` may read its records: it is not backing off after a failed read.
    fn event_due(&self, read: EventRead, now: u64) -> bool {
        self.event_retries
            .iter()
            .find(|(r, _)| *r == read)
            .is_none_or(|(_, retry)| retry.due(now))
    }

    /// Records the outcome of event `read`. Returns whether the event is settled: its read
    /// succeeded, or its record is gone and the record's own removal event follows. Any other
    /// failure keeps the event pending and grows its backoff, without holding up other events.
    /// Only the event's own outcome ends its backoff.
    fn event_settled(&mut self, read: EventRead, error: Option<&p::Error>, now: u64) -> bool {
        let index = self.event_retries.iter().position(|(r, _)| *r == read);
        match (error, index) {
            (Some(error), index) if error.code != p::ErrorCode::NotFound as i32 => {
                let index = index.unwrap_or_else(|| {
                    self.event_retries.push((read, Backoff::default()));
                    self.event_retries.len() - 1
                });
                self.event_retries[index].1.failed(now);
                false
            }
            (_, Some(index)) => {
                self.event_retries.swap_remove(index);
                true
            }
            (_, None) => true,
        }
    }

    /// Whether device `id`'s connection holds its policy, so its events read no record.
    fn policy_loaded(&self, id: u64) -> bool {
        self.manager
            .find(id)
            .and_then(|s| self.manager.devices[s].as_ref())
            .and_then(|d| d.live.as_ref())
            .is_some_and(|l| l.policy.is_some())
    }

    /// The next profile record or rules event, starting from the entry after the last one sent.
    /// A read that fails keeps its event pending, and that event waits for its own backoff while
    /// other events go ahead. Rules a connection or configuration
    /// interface has loaded need no read.
    async fn profile_event<S: RecordStore, B: Bluetooth>(
        &mut self,
        store: &mut S,
        radio: &mut B,
        now: u64,
    ) -> Option<Ev> {
        let mut waiting = 0;
        while waiting < self.profile_dirty.len() {
            let index = self.next_profile % self.profile_dirty.len();
            let (id, bits) = self.profile_dirty[index];
            let (profile, rules) = (EventRead::Profile(id), EventRead::Rules(id));
            let event = if bits & PROFILE != 0 && self.event_due(profile, now) {
                let result = self.profile_record(id as u32, store, radio).await;
                if self.event_settled(profile, result.as_ref().err(), now) {
                    self.profile_dirty[index].1 &= !PROFILE;
                }
                result.ok().map(Ev::Profile)
            } else if bits & RULES != 0 && (self.loaded(id).is_some() || self.event_due(rules, now))
            {
                let result = self.rules_event(id, store).await;
                if self.event_settled(rules, result.as_ref().err(), now)
                    && !self.touched_rules.iter().any(|(p, _)| *p == id)
                {
                    self.profile_dirty[index].1 &= !RULES;
                }
                result.ok().flatten()
            } else if bits == 0 {
                self.profile_dirty.remove(index);
                continue;
            } else {
                // Only events whose reads back off are left here.
                waiting += 1;
                self.next_profile = index + 1;
                continue;
            };
            if self.profile_dirty.get(index).is_some_and(|(_, b)| *b == 0) {
                self.profile_dirty.remove(index);
                self.next_profile = index;
            } else {
                self.next_profile = index + 1;
            }
            if event.is_some() {
                return event;
            }
        }
        None
    }

    /// The changes to the rules of profile `id` whose inputs were touched: the current rule for
    /// each that has one, and the input of each that does not. `None` when there is nothing to
    /// send. An event carries at most a page of rules; touched inputs beyond it stay touched for
    /// the next rules event of the profile, as do all of them when the rules cannot be read.
    async fn rules_event<S: RecordStore>(
        &mut self,
        id: u64,
        store: &mut S,
    ) -> Result<Option<Ev>, p::Error> {
        if !self.touched_rules.iter().any(|(p, _)| *p == id) {
            return Ok(None);
        }
        let saved;
        let map;
        let rules = match self.loaded(id) {
            Some(loaded) => {
                map = loaded;
                map.borrow()
            }
            None => {
                saved = core::cell::RefCell::new(self.rules(id, store).await?);
                saved.borrow()
            }
        };
        let Some(index) = self.touched_rules.iter().position(|(p, _)| *p == id) else {
            return Ok(None);
        };
        let (_, mut inputs) = self.touched_rules.swap_remove(index);
        // An event carries at most a page of rules; the rest follow in the next ones.
        if inputs.len() > RULES_PAGE {
            let rest = inputs.split_off(RULES_PAGE);
            self.touched_rules.push((id, rest));
        }
        let mut changed = Vec::new();
        let mut removed = Vec::new();
        for input in inputs {
            match rules.get(input) {
                Some(rule) => changed.push(wire_rule(rule)),
                None => removed.push(wire_usage(input)),
            }
        }
        Ok(
            (!changed.is_empty() || !removed.is_empty()).then_some(Ev::ProfileRulesChanged(
                p::ProfileRulesChanged {
                    profile: id as u32,
                    changed,
                    removed,
                },
            )),
        )
    }

    /// The changes to device `id`'s settings since the client last saw them: each touched
    /// setting it has now, and each setting the client may hold that it no longer has. For a
    /// device that is not tracked, the client may hold any touched setting. Touched settings
    /// stay touched when the settings cannot be read, so the next settings event of the device
    /// carries them.
    async fn settings_event<S: RecordStore, B: Bluetooth>(
        &mut self,
        id: u64,
        store: &mut S,
        radio: &mut B,
    ) -> Result<Option<Ev>, p::Error> {
        let settings = self.device_settings(id as u32, store, radio).await?;
        let touched = match self.touched.iter().position(|(d, _)| *d == id) {
            Some(index) => self.touched.swap_remove(index).1,
            None => ALL_KEYS,
        };
        let present = settings
            .iter()
            .filter_map(|s| wire::parse_setting_key(&s.key))
            .fold(0, |keys, key| keys | key_bit(key));
        let changed: Vec<p::Setting> = settings
            .into_iter()
            .filter(|s| wire::parse_setting_key(&s.key).is_some_and(|k| key_bit(k) & touched != 0))
            .collect();
        let sent = changed
            .iter()
            .filter_map(|s| wire::parse_setting_key(&s.key))
            .fold(0, |keys, key| keys | key_bit(key));
        let (gone, held) = match self.reported.iter().find(|r| r.id == id) {
            Some(reported) => (reported.keys & !present, reported.keys),
            None => (touched & !present, ALL_KEYS),
        };
        if let Some(reported) = self.reported(id) {
            reported.keys = (held & present) | sent;
        }
        let removed: Vec<p::SettingRef> = SettingKey::ALL
            .into_iter()
            .filter(|k| key_bit(*k) & gone != 0)
            .map(|k| p::SettingRef {
                integration: p::IntegrationKind::Hidpp as i32,
                key: wire::setting_key(k),
            })
            .collect();
        Ok(
            (!changed.is_empty() || !removed.is_empty()).then_some(Ev::SettingsChanged(
                p::SettingsChanged {
                    device: id as u32,
                    changed,
                    removed,
                },
            )),
        )
    }

    /// The changes to device `id`'s warnings since the client last saw them. Warnings exist only
    /// while a device is connected, and a device stays tracked until the events that follow its
    /// disconnection have been written, so the client holds none of an untracked device's.
    fn warnings_event(&mut self, id: u64) -> Option<Ev> {
        let current = self.warnings_of(self.manager.find(id));
        let held = match self.reported(id) {
            Some(reported) => core::mem::replace(&mut reported.warnings, current.clone()),
            None => Vec::new(),
        };
        let added: Vec<DeviceWarning> = current
            .iter()
            .filter(|w| !held.contains(w))
            .copied()
            .collect();
        let removed: Vec<DeviceWarning> = wire::sorted_warnings(&held)
            .into_iter()
            .filter(|w| !current.contains(w))
            .collect();
        (!added.is_empty() || !removed.is_empty()).then_some(Ev::WarningsChanged(
            p::WarningsChanged {
                device: id as u32,
                added: added.iter().map(wire::warning).collect(),
                removed: removed.iter().map(wire::warning).collect(),
            },
        ))
    }

    /// The next device, settings or warnings event, starting from the entry after the last one
    /// sent. Device and settings events read the device's saved policy until its connection has
    /// read it, so for a connection that is starting they wait until it has forwarded input or
    /// waited for it; other devices' events go ahead meanwhile. A read that fails keeps its event
    /// pending, and that event waits for its own backoff while other events go ahead. Events of a connection that holds its policy read nothing.
    async fn device_event<S: RecordStore, B: Bluetooth>(
        &mut self,
        store: &mut S,
        radio: &mut B,
        now: u64,
    ) -> Option<Ev> {
        let mut waiting = 0;
        while waiting < self.dirty.len() {
            let index = self.next_dirty % self.dirty.len();
            let (id, bits) = self.dirty[index];
            let (device, settings) = (EventRead::Device(id), EventRead::Settings(id));
            let ready = !self.manager.waiting_for_input(id, now);
            let loaded = self.policy_loaded(id);
            let event = if bits & DEVICE != 0 && ready && (loaded || self.event_due(device, now)) {
                let result = self.policy_of(id as u32, store, radio).await;
                if self.event_settled(device, result.as_ref().err(), now) {
                    self.dirty[index].1 &= !DEVICE;
                }
                result
                    .ok()
                    .map(|(policy, slot)| Ev::Device(wire::device(&self.manager, &policy, slot)))
            } else if bits & SETTINGS != 0 && ready && (loaded || self.event_due(settings, now)) {
                let result = self.settings_event(id, store, radio).await;
                if self.event_settled(settings, result.as_ref().err(), now) {
                    self.dirty[index].1 &= !SETTINGS;
                }
                result.ok().flatten()
            } else if bits & WARNINGS != 0 {
                self.dirty[index].1 &= !WARNINGS;
                self.warnings_event(id)
            } else if bits == 0 {
                self.dirty.remove(index);
                continue;
            } else {
                // Only events that wait for the connection's first input or for a read's backoff
                // are left here.
                waiting += 1;
                self.next_dirty = index + 1;
                continue;
            };
            if self.dirty.get(index).is_some_and(|(_, b)| *b == 0) {
                self.dirty.remove(index);
                self.next_dirty = index;
            } else {
                self.next_dirty = index + 1;
            }
            if event.is_some() {
                return event;
            }
        }
        None
    }

    fn scan_event(&mut self) -> Option<Ev> {
        if let Some(c) = self.candidates.iter_mut().find(|c| c.dirty) {
            c.dirty = false;
            return Some(Ev::ScanFound(p::Candidate {
                id: c.id,
                transport: wire::transport(c.peer.transport) as i32,
                name: c.name.clone().into(),
                kinds: wire::kinds(c.kind),
                rssi: c.rssi,
            }));
        }
        self.scan_done.take().map(Ev::ScanDone)
    }

    /// Runs the priority loop's work once, then the secondary loop's work until it has nothing
    /// more to do, for an owner that runs the application from one loop.
    pub async fn poll<S: RecordStore, B: Bluetooth>(
        &mut self,
        store: &mut S,
        radio: &mut B,
        leds: u8,
        now: u64,
    ) {
        self.operate(store, radio, leds, now).await;
        while self.work(store, radio, now).await {}
    }

    /// The secondary loop's application work after serial input: releases an idle editor, starts
    /// settings jobs, then writes one event when the serial output is free, or else takes one
    /// step of background work. Returns whether it wrote an event or took a step. Once the
    /// bootloader has been requested, it enters it when output and input have been sent.
    pub async fn work<S: RecordStore, B: Bluetooth>(
        &mut self,
        store: &mut S,
        radio: &mut B,
        now: u64,
    ) -> bool {
        self.release_editor(store, now).await;
        if let Some(deadline) = self.reboot_at {
            if ((self.serial.queued() == 0 && self.manager.forward.pending() == 0)
                || now >= deadline)
                && self.build.bootloader.is_some()
            {
                self.save_all(store, now).await;
                if let Some(boot) = &self.build.bootloader {
                    (boot.enter)();
                }
            }
            return false;
        }
        if self.save(store, now).await {
            return true;
        }
        self.start_jobs(now);
        self.collect_changes();
        // Events and background work take turns, so a stream of events cannot hold up pairing
        // deadlines or storage work.
        if core::mem::take(&mut self.maintenance_turn) && self.maintain(store, radio, now).await {
            return true;
        }
        if self.serial.active() && self.flush(store, radio, now).await {
            self.maintenance_turn = true;
            return true;
        }
        self.maintain(store, radio, now).await
    }

    /// The priority loop's work after radio events: link timers and output to devices,
    /// reconnecting saved devices and the scan.
    pub async fn operate<S: RecordStore, B: Bluetooth>(
        &mut self,
        store: &mut S,
        radio: &mut B,
        leds: u8,
        now: u64,
    ) {
        if self.reboot_at.is_some() {
            return;
        }
        if self.scan.as_ref().is_some_and(|s| now >= s.deadline) {
            self.stop_scan(radio);
        }
        for index in 0..devices::ACTIVE_CONNECTIONS {
            let id = self.manager.connections[index].as_ref().map(|c| c.id);
            match self.manager.poll_link(index, leds, now, radio) {
                Ok(Some(slot)) => self.mark_slot(slot, DEVICE),
                Err(e) => {
                    if let Some(id) = id {
                        self.close(id, Some(e), radio);
                    }
                }
                _ => {}
            }
        }
        if self.pair.is_none() && self.manager.storage_ready && self.manager.radio_ready {
            for slot in 0..self.manager.devices.len() {
                if self.manager.connections.iter().flatten().count()
                    >= devices::ACTIVE_CONNECTIONS - 1
                    || self
                        .manager
                        .connections
                        .iter()
                        .flatten()
                        .any(|c| c.runtime.is_none() || c.closing)
                {
                    break;
                }
                let Some(d) = self.manager.devices[slot]
                    .as_ref()
                    .filter(|d| d.peer.transport == Transport::Classic && d.page_due(now))
                else {
                    continue;
                };
                let id = d.id;
                let layout = crate::layouts::load(store, id, Transport::Classic).await;
                match self.manager.connect(
                    slot,
                    false,
                    now.saturating_add(CONNECT_TIMEOUT_MS),
                    layout.as_ref(),
                    radio,
                ) {
                    Ok(_) => self.mark(id, DEVICE),
                    // A radio busy with other link setup takes the page shortly, without
                    // reading the saved layout on every poll meanwhile.
                    Err(Error::Busy | Error::Capacity) => self.manager.devices[slot]
                        .as_mut()
                        .unwrap()
                        .defer_page(now.saturating_add(PAGE_BUSY_MS)),
                    Err(e) => {
                        self.manager.devices[slot].as_mut().unwrap().connection(
                            ConnectionState::Disconnected,
                            Some(e),
                            now,
                        );
                        self.mark(id, DEVICE);
                    }
                }
            }
        }
        let available = self.pair.is_none()
            && self.manager.storage_ready
            && self.manager.radio_ready
            && self.manager.ble_admission();
        let peers: Vec<_> = self
            .manager
            .devices
            .iter()
            .filter_map(|device| {
                let d = device.as_ref()?;
                (available && d.peer.transport == Transport::Ble && d.admit_due(now))
                    .then_some(d.peer)
            })
            .collect();
        if self.manager.radio_ready
            && let Err(error) = radio.reconnect(&peers)
            && error != Error::Busy
        {
            self.radio_failed(error, now);
        }
        self.update_scan(radio, !peers.is_empty(), now);
    }

    fn dropped(&mut self, link: LinkId, error: Option<Error>, now: u64) {
        if let Some(slot) = self.manager.disconnected(link, error, now) {
            if let Some(slot) = slot {
                self.mark_slot(slot, DEVICE | SETTINGS | WARNINGS);
            }
            if let Some(pair) = &mut self.pair
                && pair.link == Some(link)
            {
                pair.link = None;
                pair.prompt = None;
                if pair.cancelling.is_none() {
                    pair.cancelling = Some(error.unwrap_or(Error::ConnectionFailed));
                }
            }
        }
    }
}
