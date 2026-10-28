//! An adapter's page: Details, Settings, Profiles and Diagnostics. Settings and Profiles stage
//! their changes until Save sends them together in one request; renaming, connecting and
//! disconnecting act at once.
use super::{
    Action, App, Dialog, Job, Kind, Spot,
    fleet::Fleet,
    words,
    world::{AdapterView, Storage},
};
use crate::{
    commands,
    controller::{AdapterUpdate, Command, Outcome},
    error::Error,
    model,
    profiles::{self, InterfaceUpdate},
};
use cordial_protocol::{self as p, ConfigurationInterface, Platform};

/// Whether the adapter takes a restart: connected, with a storage target this version supports and
/// no migration in hand.
pub fn restartable(a: &AdapterView) -> bool {
    a.connected()
        && matches!(
            a.storage,
            Storage::Normal | Storage::Required | Storage::Blocked(_)
        )
}

pub const PLATFORMS: [Platform; 3] = [Platform::Linux, Platform::Windows, Platform::Mac];

/// The platform of the computer running the application.
pub fn host_platform() -> Platform {
    Platform::Linux
}

/// The staged values in `draft` that differ from what the adapter reports.
pub fn adapter_pending(draft: &AdapterUpdate, status: &p::Status) -> AdapterUpdate {
    let interfaces = if profiles::available(status) {
        draft
            .interfaces
            .iter()
            .filter_map(|u| {
                let saved = profiles::interface(status, u.interface)?;
                let update = InterfaceUpdate {
                    interface: u.interface,
                    enabled: u.enabled.filter(|on| *on != saved.enabled),
                    profile: u.profile.filter(|id| *id != saved.profile),
                };
                (update.enabled.is_some() || update.profile.is_some()).then_some(update)
            })
            .collect()
    } else {
        Vec::new()
    };
    AdapterUpdate {
        platform: draft.platform.filter(|p| *p != status.platform()),
        transports: draft
            .transports
            .iter()
            .copied()
            .filter(|(t, on)| model::transport_enabled(status, *t).is_some_and(|e| e != *on))
            .collect(),
        interfaces,
    }
}

impl<F: Fleet> App<F> {
    /// The adapter's staged changes that differ from its status.
    pub fn adapter_changes(&self, a: &AdapterView) -> AdapterUpdate {
        match (self.adapter_drafts.get(&a.id), a.view()) {
            (Some(draft), Some(_)) if self.immediate.is_some() => draft.clone(),
            (Some(draft), Some(status)) => adapter_pending(draft, status),
            _ => AdapterUpdate::default(),
        }
    }
    pub fn adapter_saving(&self, id: &str) -> bool {
        self.running(id, |k| matches!(k, Kind::AdapterSave(_)))
    }
    /// Staged values lock while the adapter isn't connected and ready, or while saving.
    pub fn adapter_locked(&self, a: &AdapterView) -> bool {
        a.live().is_none() || self.adapter_saving(&a.id)
    }
    /// Stages a change to the adapter's settings and clears the last save's failure.
    pub(crate) fn stage_adapter(&mut self, id: &str, change: impl FnOnce(&mut AdapterUpdate)) {
        let Some(status) = self.adapter(id).and_then(|a| a.view().cloned()) else {
            return;
        };
        if self.immediate.is_some() {
            let mut update = AdapterUpdate::default();
            change(&mut update);
            self.stage_immediate_adapter(id, update);
            return;
        }
        let mut draft = self.adapter_drafts.remove(id).unwrap_or_default();
        change(&mut draft);
        let draft = adapter_pending(&draft, &status);
        if !draft.is_empty() {
            self.adapter_drafts.insert(id.to_owned(), draft);
        }
        self.notes.remove(&Spot::AdapterSave(id.to_owned()));
    }
    pub(crate) fn stage_interface(
        &mut self,
        id: &str,
        i: ConfigurationInterface,
        enabled: Option<bool>,
        profile: Option<u32>,
    ) {
        self.stage_adapter(id, |d| {
            match d.interfaces.iter_mut().find(|u| u.interface == i) {
                Some(u) => {
                    u.enabled = enabled.or(u.enabled);
                    u.profile = profile.or(u.profile);
                }
                None => d.interfaces.push(InterfaceUpdate {
                    interface: i,
                    enabled,
                    profile,
                }),
            }
        });
    }
    /// Saves the adapter's staged changes.
    pub(crate) fn save_adapter(&mut self, id: &str) {
        let Some(a) = self.adapter(id).cloned() else {
            return;
        };
        let changes = self.adapter_changes(&a);
        if changes.is_empty() || self.adapter_locked(&a) || self.immediate_busy(id) {
            return;
        }
        let Some(status) = a.live() else { return };
        if let Some(e) = profiles::interface_refusal(status, &changes.interfaces) {
            self.notes
                .insert(Spot::AdapterSave(id.to_owned()), words::failure(&e));
            return;
        }
        self.notes.remove(&Spot::AdapterSave(id.to_owned()));
        self.run(
            id,
            Kind::AdapterSave(changes.clone()),
            Command::AdapterSave(changes),
        );
    }
    pub(crate) fn adapter_result(&mut self, job: &Job, result: &Result<Outcome, Error>) {
        let id = job.adapter.clone();
        match (&job.kind, result) {
            (Kind::AdapterSave(sent), Ok(_)) => {
                if let Some(draft) = self.adapter_drafts.get_mut(&id) {
                    super::immediate::subtract_adapter(draft, sent);
                    if draft.is_empty() {
                        self.adapter_drafts.remove(&id);
                    }
                }
            }
            (Kind::AdapterSave(_), Err(e)) => {
                self.notes.insert(Spot::AdapterSave(id), words::failure(e));
            }
            (Kind::Rename, Ok(_)) => {
                if matches!(&self.dialog, Some(Dialog::Rename(a)) if *a == id) {
                    self.dialog = None;
                }
            }
            (Kind::Rename, Err(e)) => {
                if matches!(&self.dialog, Some(Dialog::Rename(a)) if *a == id) {
                    self.notes.insert(Spot::Dialog, words::failure(e));
                }
            }
            _ => {}
        }
    }
    /// Handles an adapter page action; false when `action` isn't one.
    pub(crate) fn adapter_action(&mut self, id: &str, action: &Action) -> bool {
        let Some(a) = self.adapter(id).cloned() else {
            return false;
        };
        let locked = !self.adapter_editable(&a);
        match action {
            Action::Platform(p) if !locked => {
                let p = *p;
                self.stage_adapter(id, |d| d.platform = Some(p));
            }
            Action::SwitchPlatform if !locked => {
                let p = host_platform();
                self.stage_adapter(id, |d| d.platform = Some(p));
            }
            Action::Transport(t, on) if !locked => {
                let (t, on) = (*t, *on);
                self.stage_adapter(id, |d| {
                    d.transports.retain(|(x, _)| *x != t);
                    d.transports.push((t, on));
                });
            }
            Action::InterfaceEnabled(i, on) if !locked => {
                self.stage_interface(id, *i, Some(*on), None);
            }
            // None clears the profile only while the interface is off.
            Action::InterfaceProfile(i, profile) if !locked => {
                let off = self.staged_interface(&a, *i).is_some_and(|s| !s.enabled);
                if *profile != 0 || off {
                    self.stage_interface(id, *i, None, Some(*profile));
                }
            }
            Action::InterfacePick(i) if !locked => {
                let staged = self.staged_interface(&a, *i);
                self.dialog = Some(Dialog::Pick {
                    adapter: id.to_owned(),
                    purpose: super::PickFor::Interface(*i),
                    chosen: staged.map_or(0, |s| s.profile),
                });
                self.notes.remove(&Spot::Dialog);
                self.open_picker(id);
            }
            Action::AdapterSave => self.save_adapter(id),
            Action::AdapterDiscard if !self.adapter_saving(id) => {
                self.adapter_drafts.remove(id);
                self.notes.remove(&Spot::AdapterSave(id.to_owned()));
            }
            Action::Restart
                if restartable(&a) && !self.running(id, |k| matches!(k, Kind::Restart)) =>
            {
                self.run(id, Kind::Restart, Command::Restart);
            }
            Action::Bootloader if a.status.as_ref().is_some_and(model::development) => {
                self.dialog = Some(Dialog::Bootloader(id.to_owned()));
            }
            Action::DevicesRetry
                if a.connected() && !self.running(id, |k| matches!(k, Kind::Quiet)) =>
            {
                self.run(id, Kind::Quiet, Command::Devices);
            }
            _ => return self.profiles_action(id, action),
        }
        true
    }
    pub(crate) fn open_rename(&mut self, id: &str) {
        let Some(a) = self.adapter(id) else { return };
        let name = a.live().map(|s| words::clean(&s.name)).unwrap_or_default();
        self.form.limit = 64;
        self.form.set_value(&name);
        self.notes.remove(&Spot::Dialog);
        self.dialog = Some(Dialog::Rename(id.to_owned()));
    }
    pub fn renaming(&self, id: &str) -> bool {
        self.running(id, |k| matches!(k, Kind::Rename))
    }
    /// Sends the Rename dialog's name, or the default name with `reset`.
    pub(crate) fn submit_rename(&mut self, id: &str, reset: bool) {
        if self.renaming(id) || self.adapter(id).and_then(AdapterView::live).is_none() {
            return;
        }
        let name = if reset {
            None
        } else {
            match commands::adapter_name(&self.form.value()) {
                Some(name) => Some(name.to_owned()),
                None => {
                    self.notes
                        .insert(Spot::Dialog, words::ADAPTER_NAME_INVALID.into());
                    return;
                }
            }
        };
        self.notes.remove(&Spot::Dialog);
        self.run(id, Kind::Rename, Command::Name(name));
    }
    /// An interface's preferences as staged.
    pub fn staged_interface(
        &self,
        a: &AdapterView,
        i: ConfigurationInterface,
    ) -> Option<p::ConfigurationInterfaceSupport> {
        let view = a.view()?;
        let draft = self
            .adapter_drafts
            .get(&a.id)
            .map(|d| d.interfaces.clone())
            .unwrap_or_default();
        profiles::configured(view, &draft)
            .into_iter()
            .find(|s| s.interface == i as i32)
    }
}
