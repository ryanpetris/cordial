//! Native immediate application uses the shared drafts as desired values. Running jobs own
//! immutable requests; uncertain requests remain visible but are excluded from later writes.
use super::{
    AdapterView, App, DeviceView, Fleet, Job, Kind, Slot, Spot, Tab,
    device::{save_spot, tab_changes},
    settings::Draft,
    words,
};
use crate::{
    controller::{AdapterUpdate, DeviceUpdate, Outcome},
    error::Error,
    settings::Field,
};
use cordial_protocol::ErrorOutcome;
use std::collections::{BTreeMap, HashMap};

type DeviceKey = (String, u32);

#[derive(Clone, Default)]
struct Intents {
    adapters: HashMap<String, AdapterUpdate>,
    devices: HashMap<DeviceKey, DeviceUpdate>,
    settings: HashMap<DeviceKey, BTreeMap<Field, Draft>>,
}

struct WriteFailure {
    spot: Spot,
    intents: Intents,
    text: String,
}

#[derive(Default)]
pub(crate) struct Immediate {
    uncertain: Intents,
    uncertain_notes: HashMap<Spot, String>,
    /// The session whose failed writes need a fresh connection's readback.
    sessions: HashMap<String, Slot>,
    /// Unresolved fields retain their failure even if their page is no longer shown.
    errors: Vec<WriteFailure>,
}

impl Immediate {
    fn uncertain_at(&self, spot: &Spot) -> bool {
        match spot {
            Spot::AdapterSave(id) => self
                .uncertain
                .adapters
                .get(id)
                .is_some_and(|d| !d.is_empty()),
            Spot::DetailsSave(id, device)
            | Spot::LayersSave(id, device)
            | Spot::SettingsNote(id, device) => {
                let key = (id.clone(), *device);
                let tab = match spot {
                    Spot::DetailsSave(..) => Tab::Details,
                    Spot::LayersSave(..) => Tab::Profiles,
                    _ => Tab::Settings,
                };
                self.uncertain
                    .devices
                    .get(&key)
                    .is_some_and(|d| !tab_changes(d, tab).is_empty())
                    || tab == Tab::Settings
                        && self
                            .uncertain
                            .settings
                            .get(&key)
                            .is_some_and(|d| !d.is_empty())
            }
            _ => false,
        }
    }
}

pub(crate) fn is_write(kind: &Kind) -> bool {
    matches!(
        kind,
        Kind::AdapterSave(_) | Kind::DeviceSave { .. } | Kind::SettingsSave { .. }
    )
}

pub(crate) fn unknown(error: &Error) -> bool {
    match error
        .dongle
        .as_ref()
        .map(|e| ErrorOutcome::try_from(e.outcome))
    {
        Some(Ok(ErrorOutcome::NoEffect | ErrorOutcome::Complete)) => false,
        Some(_) => true,
        None => error.unconfirmed(),
    }
}

pub(crate) fn subtract_adapter(draft: &mut AdapterUpdate, sent: &AdapterUpdate) {
    if draft.platform == sent.platform {
        draft.platform = None;
    }
    draft.transports.retain(|t| !sent.transports.contains(t));
    for update in &mut draft.interfaces {
        if let Some(old) = sent
            .interfaces
            .iter()
            .find(|s| s.interface == update.interface)
        {
            if update.enabled == old.enabled {
                update.enabled = None;
            }
            if update.profile == old.profile {
                update.profile = None;
            }
        }
    }
    draft
        .interfaces
        .retain(|u| u.enabled.is_some() || u.profile.is_some());
}

fn subtract_device(draft: &mut DeviceUpdate, sent: &DeviceUpdate) {
    if draft.enabled == sent.enabled {
        draft.enabled = None;
    }
    if draft.trusted == sent.trusted {
        draft.trusted = None;
    }
    if draft.blocked == sent.blocked {
        draft.blocked = None;
    }
    if draft.hidpp == sent.hidpp {
        draft.hidpp = None;
    }
    if draft.gamepad == sent.gamepad {
        draft.gamepad = None;
    }
    if draft.layers == sent.layers {
        draft.layers = None;
    }
}

impl Intents {
    fn subtract(&mut self, sent: &Self) {
        for (id, old) in &sent.adapters {
            if let Some(draft) = self.adapters.get_mut(id) {
                subtract_adapter(draft, old);
            }
        }
        for (key, old) in &sent.devices {
            if let Some(draft) = self.devices.get_mut(key) {
                subtract_device(draft, old);
            }
        }
        for (key, old) in &sent.settings {
            if let Some(drafts) = self.settings.get_mut(key) {
                drafts.retain(|field, draft| old.get(field) != Some(draft));
            }
        }
        self.adapters.retain(|_, d| !d.is_empty());
        self.devices.retain(|_, d| !d.is_empty());
        self.settings.retain(|_, d| !d.is_empty());
    }

    fn forget(&mut self, touched: &Self) {
        for (id, sent) in &touched.adapters {
            if let Some(d) = self.adapters.get_mut(id) {
                if sent.platform.is_some() {
                    d.platform = None;
                }
                d.transports
                    .retain(|(t, _)| !sent.transports.iter().any(|(s, _)| s == t));
                for u in &mut d.interfaces {
                    if let Some(s) = sent.interfaces.iter().find(|s| s.interface == u.interface) {
                        if s.enabled.is_some() {
                            u.enabled = None;
                        }
                        if s.profile.is_some() {
                            u.profile = None;
                        }
                    }
                }
                d.interfaces
                    .retain(|u| u.enabled.is_some() || u.profile.is_some());
            }
        }
        for (key, sent) in &touched.devices {
            if let Some(d) = self.devices.get_mut(key) {
                if sent.enabled.is_some() {
                    d.enabled = None;
                }
                if sent.trusted.is_some() {
                    d.trusted = None;
                }
                if sent.blocked.is_some() {
                    d.blocked = None;
                }
                if sent.hidpp.is_some() {
                    d.hidpp = None;
                }
                if sent.gamepad.is_some() {
                    d.gamepad = None;
                }
                if sent.layers.is_some() {
                    d.layers = None;
                }
            }
        }
        for (key, sent) in &touched.settings {
            if let Some(d) = self.settings.get_mut(key) {
                d.retain(|k, _| !sent.contains_key(k));
            }
        }
        self.subtract(&Self::default());
    }

    fn empty(&self) -> bool {
        self.adapters.is_empty() && self.devices.is_empty() && self.settings.is_empty()
    }

    fn of_job(job: &Job) -> Self {
        let mut out = Self::default();
        match &job.kind {
            Kind::AdapterSave(sent) => {
                out.adapters.insert(job.adapter.clone(), sent.clone());
            }
            Kind::DeviceSave { device, sent, .. } => {
                out.devices
                    .insert((job.adapter.clone(), *device), sent.clone());
            }
            Kind::SettingsSave { device, sent, .. } => {
                out.settings.insert(
                    (job.adapter.clone(), *device),
                    sent.iter().cloned().collect(),
                );
            }
            _ => {}
        }
        out
    }

    fn merge(&mut self, other: Self) {
        for (id, next) in other.adapters {
            let d = self.adapters.entry(id).or_default();
            d.platform = next.platform.or(d.platform);
            for t in next.transports {
                d.transports.retain(|old| old.0 != t.0);
                d.transports.push(t);
            }
            for u in next.interfaces {
                if let Some(old) = d
                    .interfaces
                    .iter_mut()
                    .find(|old| old.interface == u.interface)
                {
                    old.enabled = u.enabled.or(old.enabled);
                    old.profile = u.profile.or(old.profile);
                } else {
                    d.interfaces.push(u);
                }
            }
        }
        for (key, next) in other.devices {
            let d = self.devices.entry(key).or_default();
            d.enabled = next.enabled.or(d.enabled);
            d.trusted = next.trusted.or(d.trusted);
            d.blocked = next.blocked.or(d.blocked);
            d.hidpp = next.hidpp.or(d.hidpp);
            d.gamepad = next.gamepad.or(d.gamepad);
            d.layers = next.layers.or(d.layers.take());
        }
        for (key, next) in other.settings {
            self.settings.entry(key).or_default().extend(next);
        }
    }
}

impl<F: Fleet> App<F> {
    /// Enables optimistic staging and serialized immediate writes for the native desktop.
    pub fn enable_immediate(&mut self) {
        self.immediate = Some(Immediate::default());
    }

    /// Whether an adapter control accepts a new desired value.
    pub fn adapter_editable(&self, a: &AdapterView) -> bool {
        if self.immediate.is_some() {
            a.live().is_some()
        } else {
            !self.adapter_locked(a)
        }
    }

    /// Whether a device preference accepts a new desired value.
    pub fn device_editable(&self, d: &DeviceView) -> bool {
        if self.immediate.is_some() {
            self.adapter_ready(d)
        } else {
            !self.device_locked(d)
        }
    }

    /// Whether a device setting accepts a new desired value.
    pub fn settings_editable(&self, d: &DeviceView) -> bool {
        if self.immediate.is_some() {
            self.adapter_ready(d) && !self.refreshing(d)
        } else {
            !self.form_busy(d)
        }
    }

    pub(crate) fn immediate_busy(&self, adapter: &str) -> bool {
        self.immediate.is_some() && self.running(adapter, is_write)
    }

    fn intents(&self) -> Intents {
        Intents {
            adapters: self.adapter_drafts.clone(),
            devices: self.device_drafts.clone(),
            settings: self.setting_drafts.clone(),
        }
    }

    fn put_intents(&mut self, intents: Intents) {
        self.adapter_drafts = intents.adapters;
        self.device_drafts = intents.devices;
        self.setting_drafts = intents.settings;
    }

    fn pending_intents(&self) -> Intents {
        let mut pending = self.intents();
        if let Some(immediate) = &self.immediate {
            pending.subtract(&immediate.uncertain);
        }
        pending
    }

    /// Accepted settings still waiting to finish, excluding failed and uncertain requests.
    pub fn writes_pending(&self) -> bool {
        self.jobs.values().any(|j| is_write(&j.kind)) || !self.pending_intents().empty()
    }

    /// The latest unresolved settings failure, including one whose session or page has disappeared.
    pub fn write_error(&self) -> Option<&str> {
        Some(self.immediate.as_ref()?.errors.last()?.text.as_str())
    }

    /// Acknowledges the failure shown by Quit without retrying its request.
    pub fn acknowledge_write_error(&mut self) {
        if let Some(immediate) = &mut self.immediate {
            immediate.errors.clear();
        }
    }

    pub(crate) fn immediate_note(&self, spot: &Spot) -> Option<&str> {
        let immediate = self.immediate.as_ref()?;
        if immediate.uncertain_at(spot)
            && let Some(text) = immediate.uncertain_notes.get(spot)
        {
            return Some(text.as_str());
        }
        immediate
            .errors
            .iter()
            .rev()
            .find(|e| &e.spot == spot)
            .map(|e| e.text.as_str())
    }

    fn resolve_write_errors(&mut self, touched: &Intents) {
        let Some(immediate) = &mut self.immediate else {
            return;
        };
        immediate.errors.retain_mut(|e| {
            e.intents.forget(touched);
            if !e.intents.empty() {
                return true;
            }
            if self.notes.get(&e.spot) == Some(&e.text) {
                self.notes.remove(&e.spot);
            }
            false
        });
    }

    fn record_write_error(&mut self, spot: Spot, intents: Intents, text: String) {
        // Each field retains only its latest failure, so repeated retries cannot grow history.
        self.resolve_write_errors(&intents);
        self.immediate.as_mut().unwrap().errors.push(WriteFailure {
            spot,
            intents,
            text,
        });
    }

    fn prune_uncertain_notes(&mut self) {
        let Some(immediate) = &mut self.immediate else {
            return;
        };
        let resolved: Vec<_> = immediate
            .uncertain_notes
            .keys()
            .filter(|spot| !immediate.uncertain_at(spot))
            .cloned()
            .collect();
        for spot in resolved {
            if let Some(text) = immediate.uncertain_notes.remove(&spot)
                && self.notes.get(&spot) == Some(&text)
            {
                self.notes.remove(&spot);
            }
        }
    }

    pub(crate) fn stage_immediate_adapter(&mut self, id: &str, update: AdapterUpdate) {
        let mut next = Intents::default();
        next.adapters.insert(id.to_owned(), update);
        self.immediate.as_mut().unwrap().uncertain.forget(&next);
        let mut drafts = self.intents();
        drafts.merge(next);
        self.put_intents(drafts);
        self.notes.remove(&Spot::AdapterSave(id.to_owned()));
        self.prune_uncertain_notes();
    }

    pub(crate) fn stage_immediate_device(
        &mut self,
        d: &DeviceView,
        tab: Tab,
        update: DeviceUpdate,
    ) {
        let mut next = Intents::default();
        next.devices.insert((d.adapter.clone(), d.d.id), update);
        self.immediate.as_mut().unwrap().uncertain.forget(&next);
        let mut drafts = self.intents();
        drafts.merge(next);
        self.put_intents(drafts);
        self.notes
            .remove(&save_spot(tab, d.adapter.clone(), d.d.id));
        self.prune_uncertain_notes();
    }

    pub(crate) fn edit_immediate_setting(&mut self, key: &DeviceKey, field: Field) {
        if let Some(immediate) = &mut self.immediate
            && let Some(drafts) = immediate.uncertain.settings.get_mut(key)
        {
            drafts.remove(&field);
        }
        self.prune_uncertain_notes();
    }

    /// Starts at most one configuration write per adapter. Later edits coalesce in the drafts.
    pub fn apply_pending(&mut self) {
        if self.immediate.is_none() || self.quitting {
            return;
        }
        let original = self.intents();
        self.put_intents(self.pending_intents());
        let mut rejected = Intents::default();
        let adapters: Vec<_> = self.adapters.iter().map(|a| a.id.clone()).collect();
        for id in adapters {
            if self.immediate_busy(&id) {
                continue;
            }
            let mut attempts = Vec::new();
            if let Some(sent) = self.adapter_drafts.get(&id).filter(|d| !d.is_empty()) {
                attempts.push((
                    Kind::AdapterSave(sent.clone()),
                    Spot::AdapterSave(id.clone()),
                ));
            }
            for d in self.devices.iter().filter(|d| d.adapter == id) {
                let changes = self.device_changes(d);
                for tab in [Tab::Details, Tab::Profiles] {
                    let sent = tab_changes(&changes, tab);
                    if !sent.is_empty() {
                        attempts.push((
                            Kind::DeviceSave {
                                device: d.d.id,
                                sent,
                                tab,
                            },
                            save_spot(tab, id.clone(), d.d.id),
                        ));
                    }
                }
                if let Some(drafts) = self.setting_drafts.get(&(id.clone(), d.d.id)) {
                    attempts.push((
                        Kind::SettingsSave {
                            device: d.d.id,
                            sent: drafts.iter().map(|(k, v)| (*k, v.clone())).collect(),
                            gamepad: false,
                        },
                        Spot::SettingsNote(id.clone(), d.d.id),
                    ));
                }
                let sent = tab_changes(&changes, Tab::Settings);
                if !sent.is_empty() {
                    attempts.push((
                        Kind::DeviceSave {
                            device: d.d.id,
                            sent,
                            tab: Tab::Settings,
                        },
                        Spot::SettingsNote(id.clone(), d.d.id),
                    ));
                }
            }
            for (kind, spot) in attempts {
                if self.immediate_busy(&id) {
                    break;
                }
                let ready = self.adapter(&id).is_some_and(|a| a.live().is_some());
                if ready {
                    match &kind {
                        Kind::AdapterSave(_) => self.save_adapter(&id),
                        Kind::DeviceSave { device, tab, .. } => {
                            if let Some(d) = self.device(&id, *device).cloned() {
                                self.save_device(&d, *tab);
                            }
                        }
                        Kind::SettingsSave { device, .. } => self.save_settings(&id, *device),
                        _ => {}
                    }
                }
                if self.immediate_busy(&id) {
                    break;
                }
                // A read can temporarily own the settings form. Its completion retries staging.
                if let Kind::SettingsSave { device, .. } = &kind
                    && self
                        .device(&id, *device)
                        .is_some_and(|d| self.refreshing(d))
                {
                    continue;
                }
                let text = if self.held.contains_key(&id)
                    || self
                        .state_of(&id)
                        .is_some_and(|st| st.session_ending.is_some())
                {
                    words::ADAPTER_GONE.into()
                } else {
                    self.notes
                        .get(&spot)
                        .cloned()
                        .unwrap_or_else(|| words::code_text(cordial_protocol::ErrorCode::NotReady))
                };
                let job = Job {
                    slot: 0,
                    adapter: id.clone(),
                    command: crate::controller::Command::Status,
                    kind,
                };
                let sent = Intents::of_job(&job);
                self.record_write_error(spot.clone(), sent.clone(), text.clone());
                self.notes.insert(spot, text);
                rejected.merge(sent);
            }
        }
        self.put_intents(original);
        let mut remaining = self.intents();
        remaining.subtract(&rejected);
        self.put_intents(remaining);
    }

    pub(crate) fn immediate_result(&mut self, job: &Job, result: &Result<Outcome, Error>) {
        if !is_write(&job.kind) {
            return;
        }
        let sent = Intents::of_job(job);
        let Err(error) = result else {
            self.resolve_write_errors(&sent);
            self.immediate.as_mut().unwrap().uncertain.forget(&sent);
            self.prune_uncertain_notes();
            return;
        };
        let uncertain = unknown(error);
        let text = if uncertain && error.dongle.is_none() {
            format!(
                "The adapter couldn't confirm whether the change was saved. {}",
                words::failure(error)
            )
        } else {
            words::failure(error)
        };
        self.record_write_error(write_spot(job), sent.clone(), text.clone());
        let immediate = self.immediate.as_mut().unwrap();
        if uncertain {
            immediate.uncertain.merge(sent);
            immediate
                .uncertain_notes
                .insert(write_spot(job), text.clone());
            immediate.sessions.insert(job.adapter.clone(), job.slot);
        } else {
            let mut remaining = self.intents();
            remaining.subtract(&sent);
            self.put_intents(remaining);
        }
        self.notes.insert(write_spot(job), text.clone());
        if let Kind::SettingsSave { device, .. } = &job.kind
            && let Some(submission) = self.submissions.get_mut(&(job.adapter.clone(), *device))
        {
            submission.error = Some(text);
            submission.unknown = uncertain;
        }
    }

    pub(crate) fn immediate_disconnect(&mut self, id: &str, slot: Slot) {
        if self.immediate.is_none() {
            return;
        }
        let jobs: Vec<_> = self
            .jobs
            .iter()
            .filter(|((s, _), j)| *s == slot && is_write(&j.kind))
            .map(|(key, j)| (*key, j.clone()))
            .collect();
        let mut pending = self.pending_intents();
        pending.adapters.retain(|a, _| a == id);
        pending.devices.retain(|(a, _), _| a == id);
        pending.settings.retain(|(a, _), _| a == id);
        if jobs.is_empty() && pending.empty() {
            return;
        }
        for (adapter, update) in &pending.adapters {
            let mut intents = Intents::default();
            intents.adapters.insert(adapter.clone(), update.clone());
            self.record_write_error(
                Spot::AdapterSave(adapter.clone()),
                intents,
                words::ADAPTER_GONE.into(),
            );
        }
        for ((adapter, device), update) in &pending.devices {
            for tab in [Tab::Details, Tab::Profiles, Tab::Settings] {
                let sent = tab_changes(update, tab);
                if !sent.is_empty() {
                    let mut intents = Intents::default();
                    intents.devices.insert((adapter.clone(), *device), sent);
                    self.record_write_error(
                        save_spot(tab, adapter.clone(), *device),
                        intents,
                        words::ADAPTER_GONE.into(),
                    );
                }
            }
        }
        for ((adapter, device), drafts) in &pending.settings {
            let mut intents = Intents::default();
            intents
                .settings
                .insert((adapter.clone(), *device), drafts.clone());
            self.record_write_error(
                Spot::SettingsNote(adapter.clone(), *device),
                intents,
                words::ADAPTER_GONE.into(),
            );
        }
        for (key, job) in jobs {
            self.jobs.remove(&key);
            let error = Error::new("the connection to the adapter closed");
            self.immediate_result(&job, &Err(error));
            if let Kind::SettingsSave { device, .. } = job.kind {
                let text = self.write_error().unwrap().to_owned();
                if let Some(submission) = self.submissions.get_mut(&(id.to_owned(), device)) {
                    submission.running = false;
                    submission.unknown = true;
                    submission.error = Some(text);
                }
            }
        }
        // No accepted value is replayed after a disconnected session, including follow-up edits.
        let immediate = self.immediate.as_mut().unwrap();
        immediate.uncertain.merge(pending);
        immediate.sessions.insert(id.to_owned(), slot);
    }

    pub(crate) fn immediate_settings_readback(&mut self, id: &str, device: u32) {
        let Some(immediate) = &mut self.immediate else {
            return;
        };
        let key = (id.to_owned(), device);
        let mut resolved = Intents::default();
        if let Some(sent) = immediate.uncertain.settings.remove(&key) {
            resolved.settings.insert(key.clone(), sent);
        }
        if let Some(submission) = self.submissions.get_mut(&key)
            && !submission.running
            && submission.unknown
        {
            submission.error = None;
            submission.unknown = false;
        }
        self.resolve_write_errors(&resolved);
        let mut remaining = self.intents();
        remaining.subtract(&resolved);
        self.put_intents(remaining);
        self.prune_uncertain_notes();
    }

    pub(crate) fn immediate_readback(&mut self, id: &str, device: Option<u32>, slot: Slot) {
        let Some(immediate) = &mut self.immediate else {
            return;
        };
        if immediate.sessions.get(id).is_none_or(|old| *old == slot) {
            return;
        }
        if let Some(device) = device {
            return self.immediate_settings_readback(id, device);
        }
        let mut resolved = Intents::default();
        if let Some(sent) = immediate.uncertain.adapters.remove(id) {
            resolved.adapters.insert(id.to_owned(), sent);
        }
        let keys: Vec<_> = immediate
            .uncertain
            .devices
            .keys()
            .filter(|(a, _)| a == id)
            .cloned()
            .collect();
        for key in keys {
            if let Some(sent) = immediate.uncertain.devices.remove(&key) {
                resolved.devices.insert(key, sent);
            }
        }
        self.resolve_write_errors(&resolved);
        let mut remaining = self.intents();
        remaining.subtract(&resolved);
        self.put_intents(remaining);
        self.prune_uncertain_notes();
    }
}

fn write_spot(job: &Job) -> Spot {
    match &job.kind {
        Kind::AdapterSave(_) => Spot::AdapterSave(job.adapter.clone()),
        Kind::DeviceSave { device, tab, .. } => save_spot(*tab, job.adapter.clone(), *device),
        Kind::SettingsSave { device, .. } => Spot::SettingsNote(job.adapter.clone(), *device),
        _ => unreachable!(),
    }
}
