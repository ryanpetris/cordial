//! Staged changes to the adapter's settings and to saved devices' preferences. Choosing a value
//! stages it; Save sends every staged change of the adapter in one request, and Discard drops
//! them. A device's details and its layers each have their own Save and Discard, which send or
//! drop only their own changes. Changes stay staged across views, dialogs and selections.
use super::{
    Action, Dialog, Job, Model, PickFor, can_set_platform,
    layout::{Layout, Tone},
    profiles::conflicting,
    settings::{self, button_if},
};
use crate::{
    controller::{AdapterUpdate, Command, DeviceUpdate, Outcome, State},
    error::Error,
    model,
    profiles::{self, InterfaceUpdate},
    ui::{Backend, text},
};
use cordial_protocol::{self as p, ConfigurationInterface};

/// The staged values in `draft` that differ from what the adapter reports.
pub(super) fn adapter_pending(draft: &AdapterUpdate, status: &p::Status) -> AdapterUpdate {
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

/// The staged values in `draft` that differ from the device's saved ones.
pub(super) fn device_pending(draft: &DeviceUpdate, d: &p::Device) -> DeviceUpdate {
    DeviceUpdate {
        enabled: draft.enabled.filter(|v| *v != d.enabled),
        trusted: draft.trusted.filter(|v| *v != d.trusted),
        blocked: draft.blocked.filter(|v| *v != d.blocked),
        hidpp: draft.hidpp.filter(|v| *v != model::hidpp_enabled(d)),
        layers: draft
            .layers
            .clone()
            .filter(|l| l.as_slice() != profiles::layers(d)),
    }
}

fn device_key(st: &State, id: u32) -> String {
    format!("{}/{id}", st.status.id)
}

impl<B: Backend> Model<B> {
    /// The adapter's staged changes that differ from its saved settings.
    pub(super) fn adapter_draft(&self, st: &State) -> AdapterUpdate {
        self.adapter_drafts
            .get(&st.status.id)
            .map(|d| adapter_pending(d, &st.status))
            .unwrap_or_default()
    }

    /// A device's staged changes that differ from its saved preferences.
    pub(super) fn device_draft(&self, st: &State, d: &p::Device) -> DeviceUpdate {
        self.device_drafts
            .get(&device_key(st, d.id))
            .map(|x| device_pending(x, d))
            .unwrap_or_default()
    }

    /// The device's preferences with staged values in place of saved ones.
    pub(super) fn device_values(&self, st: &State, d: &p::Device) -> DeviceUpdate {
        let draft = self.device_draft(st, d);
        DeviceUpdate {
            enabled: Some(draft.enabled.unwrap_or(d.enabled)),
            trusted: Some(draft.trusted.unwrap_or(d.trusted)),
            blocked: Some(draft.blocked.unwrap_or(d.blocked)),
            hidpp: Some(draft.hidpp.unwrap_or(model::hidpp_enabled(d))),
            layers: Some(draft.layers.unwrap_or_else(|| profiles::layers(d).to_vec())),
        }
    }

    /// Why the shown adapter's last save failed, until its staged changes change.
    pub(super) fn adapter_error(&self, st: &State) -> Option<&str> {
        self.adapter_err
            .as_ref()
            .filter(|(id, _)| *id == st.status.id)
            .map(|(_, e)| e.as_str())
    }

    /// Why the last save of a device's details failed.
    pub(super) fn details_error(&self, st: &State, d: &p::Device) -> Option<&str> {
        let key = device_key(st, d.id);
        self.details_err
            .as_ref()
            .filter(|(k, _)| *k == key)
            .map(|(_, e)| e.as_str())
    }

    /// Why the last save of a device's layers failed.
    pub(super) fn layers_error(&self, st: &State, d: &p::Device) -> Option<&str> {
        let key = device_key(st, d.id);
        self.layers_err
            .as_ref()
            .filter(|(k, _)| *k == key)
            .map(|(_, e)| e.as_str())
    }

    /// The adapter's Save and Discard buttons, for every staged adapter change.
    pub(super) fn adapter_buttons(&self, st: &State, pinned: &mut Layout) {
        let draft = self.adapter_draft(st);
        let saving = self.adapter_saving();
        let complete = profiles::interface_refusal(&st.status, &draft.interfaces).is_none();
        let can_save = can_set_platform(st) && !saving && !draft.is_empty() && complete;
        button_if(pinned, "Save", Action::AdapterSave, Tone::Primary, can_save);
        let can_discard = !saving && !draft.is_empty();
        button_if(
            pinned,
            "Discard",
            Action::AdapterDiscard,
            Tone::Normal,
            can_discard,
        );
    }

    /// Whether an adapter save is running, or its reply was cut short and its outcome is still
    /// to be settled.
    pub(super) fn adapter_saving(&self) -> bool {
        self.unsettled.is_some() || self.running(|c| matches!(c, Command::AdapterSave(_)))
    }

    pub(super) fn device_saving(&self, id: u32) -> bool {
        self.running(|c| matches!(c, Command::DeviceSave(d, _) if *d == id))
    }

    fn stage_adapter(&mut self, st: &State, change: impl FnOnce(&mut AdapterUpdate)) {
        let key = st.status.id.clone();
        let mut draft = self.adapter_drafts.remove(&key).unwrap_or_default();
        change(&mut draft);
        let draft = adapter_pending(&draft, &st.status);
        if !draft.is_empty() {
            self.adapter_drafts.insert(key, draft);
        }
        self.adapter_err = None;
    }

    /// Stages a configuration interface's switch or profile. Turning an interface on also turns
    /// off the interfaces it conflicts with.
    fn stage_interface(
        &mut self,
        st: &State,
        i: ConfigurationInterface,
        enabled: Option<bool>,
        profile: Option<u32>,
    ) {
        let configured = profiles::configured(&st.status, &self.adapter_draft(st).interfaces);
        let others = if enabled == Some(true) {
            conflicting(&configured, i)
        } else {
            Vec::new()
        };
        self.stage_adapter(st, |d| {
            let mut set = |interface, enabled: Option<bool>, profile: Option<u32>| match d
                .interfaces
                .iter_mut()
                .find(|u| u.interface == interface)
            {
                Some(u) => {
                    u.enabled = enabled.or(u.enabled);
                    u.profile = profile.or(u.profile);
                }
                None => d.interfaces.push(InterfaceUpdate {
                    interface,
                    enabled,
                    profile,
                }),
            };
            for other in others {
                set(other, Some(false), None);
            }
            set(i, enabled, profile);
        });
    }

    /// Stages a change to a device's details, or to its layers when `layers`, and clears the
    /// last failure of that part's save.
    fn stage_device(
        &mut self,
        st: &State,
        d: &p::Device,
        layers: bool,
        change: impl FnOnce(&mut DeviceUpdate),
    ) {
        let key = device_key(st, d.id);
        let mut draft = self.device_drafts.remove(&key).unwrap_or_default();
        change(&mut draft);
        let draft = device_pending(&draft, d);
        if !draft.is_empty() {
            self.device_drafts.insert(key.clone(), draft);
        }
        self.clear_device_err(&key, layers);
    }

    fn clear_device_err(&mut self, key: &str, layers: bool) {
        let err = if layers {
            &mut self.layers_err
        } else {
            &mut self.details_err
        };
        if err.as_ref().is_some_and(|(k, _)| k == key) {
            *err = None;
        }
    }

    /// Handles staging, saving and discarding; false when `action` isn't one of those.
    pub(super) fn staging_action(&mut self, action: &Action) -> bool {
        let interface_pick = matches!(
            (action, &self.dialog),
            (
                Action::PickProfile(_),
                Some(Dialog::ProfilePick(PickFor::Interface(_)))
            )
        );
        let adapter_action = interface_pick
            || matches!(
                action,
                Action::Platform(_)
                    | Action::Transport(..)
                    | Action::InterfaceEnabled(..)
                    | Action::AdapterSave
                    | Action::AdapterDiscard
            );
        let Some(st) = self.state() else {
            return adapter_action || self.device_staging(action, None);
        };
        if !adapter_action {
            return self.device_staging(action, Some(&st));
        }
        // Adapter changes wait while a save runs or the adapter can't take them.
        let adapter_idle = can_set_platform(&st) && !self.adapter_saving();
        let config = profiles::available(&st.status);
        match action {
            Action::Platform(p) if adapter_idle => {
                self.stage_adapter(&st, |d| d.platform = Some(*p));
            }
            Action::Transport(t, on) if adapter_idle && model::supports(&st.status, *t) => {
                self.stage_adapter(&st, |d| {
                    d.transports.retain(|(r, _)| r != t);
                    d.transports.push((*t, *on));
                });
            }
            Action::InterfaceEnabled(i, on) if adapter_idle && config => {
                self.stage_interface(&st, *i, Some(*on), None);
            }
            Action::PickProfile(id) => {
                if let Some(Dialog::ProfilePick(PickFor::Interface(i))) = self.dialog
                    && adapter_idle
                    && config
                {
                    self.stage_interface(&st, i, None, Some(*id));
                    self.dialog = None;
                }
            }
            Action::AdapterSave if adapter_idle => self.save_adapter(&st),
            Action::AdapterDiscard if !self.adapter_saving() => {
                self.adapter_drafts.remove(&st.status.id);
                self.adapter_err = None;
            }
            _ => {}
        }
        true
    }

    fn device_staging(&mut self, action: &Action, st: Option<&State>) -> bool {
        let staging = matches!(
            action,
            Action::Enable
                | Action::Disable
                | Action::Trust
                | Action::Untrust
                | Action::Block
                | Action::Unblock
                | Action::Hidpp(_)
                | Action::PickProfile(_)
                | Action::LayerUp(_)
                | Action::LayerDown(_)
                | Action::LayerRemove(_)
                | Action::DeviceSave
                | Action::DeviceDiscard
                | Action::LayersSave
                | Action::LayersDiscard
        );
        let Some(st) = st.filter(|_| staging) else {
            return staging;
        };
        let (Some(d), _) = Self::find(st, self.selected) else {
            return true;
        };
        let d = d.clone();
        if self.device_saving(d.id) {
            return true;
        }
        let layers = self.device_values(st, &d).layers.unwrap_or_default();
        let config = profiles::available(&st.status);
        let reorder = |m: &mut Self, change: &dyn Fn(&mut Vec<u32>)| {
            let mut next = layers.clone();
            change(&mut next);
            m.stage_device(st, &d, true, |x| x.layers = Some(next));
        };
        match action {
            Action::Enable | Action::Disable => {
                let on = *action == Action::Enable;
                self.stage_device(st, &d, false, |x| x.enabled = Some(on));
            }
            Action::Trust | Action::Untrust => {
                let on = *action == Action::Trust;
                self.stage_device(st, &d, false, |x| x.trusted = Some(on));
            }
            Action::Block | Action::Unblock => {
                let on = *action == Action::Block;
                self.stage_device(st, &d, false, |x| x.blocked = Some(on));
            }
            // Logitech Features wait while the device's settings work runs.
            Action::Hidpp(on) if settings::settings_busy(st, &d, self.saving(d.id)).is_empty() => {
                self.stage_device(st, &d, false, |x| x.hidpp = Some(*on));
            }
            // A choice from the chooser adds the profile last and returns to the device.
            Action::PickProfile(id)
                if config
                    && *id != 0
                    && self.dialog == Some(Dialog::ProfilePick(PickFor::Layer)) =>
            {
                let max = profiles::max_layers(&st.status).unwrap_or(0) as usize;
                if layers.len() < max {
                    reorder(self, &|l: &mut Vec<u32>| l.push(*id));
                }
                self.dialog = None;
            }
            Action::LayerUp(i) if config && *i > 0 && *i < layers.len() => {
                reorder(self, &|l: &mut Vec<u32>| l.swap(*i - 1, *i));
            }
            Action::LayerDown(i) if config && *i + 1 < layers.len() => {
                reorder(self, &|l: &mut Vec<u32>| l.swap(*i, *i + 1));
            }
            Action::LayerRemove(i) if config && *i < layers.len() => {
                reorder(self, &|l: &mut Vec<u32>| {
                    l.remove(*i);
                });
            }
            Action::DeviceSave | Action::LayersSave => {
                let draft = self.device_draft(st, &d);
                let layers = *action == Action::LayersSave;
                let sent = if layers {
                    DeviceUpdate {
                        layers: draft.layers,
                        ..DeviceUpdate::default()
                    }
                } else {
                    DeviceUpdate {
                        layers: None,
                        ..draft
                    }
                };
                if !sent.is_empty() {
                    self.clear_device_err(&device_key(st, d.id), layers);
                    self.execute(Command::DeviceSave(d.id, sent));
                }
            }
            Action::DeviceDiscard | Action::LayersDiscard => {
                let layers = *action == Action::LayersDiscard;
                self.stage_device(st, &d, layers, |x| {
                    if layers {
                        x.layers = None;
                    } else {
                        *x = DeviceUpdate {
                            layers: x.layers.take(),
                            ..DeviceUpdate::default()
                        };
                    }
                });
            }
            _ => {}
        }
        true
    }

    /// Saves the adapter's staged changes, asking first when the save reconnects USB.
    fn save_adapter(&mut self, st: &State) {
        let draft = self.adapter_draft(st);
        if draft.is_empty() {
            return;
        }
        if let Some(e) = profiles::interface_refusal(&st.status, &draft.interfaces) {
            self.adapter_err = Some((st.status.id.clone(), text::sentence(&text::error_words(&e))));
            return;
        }
        if profiles::reconnects(&st.status, &draft.interfaces) {
            self.dialog = Some(Dialog::SaveAdapter);
            return;
        }
        self.adapter_err = None;
        self.execute(Command::AdapterSave(draft));
    }

    /// Answers the save confirmation; false when the dialog isn't one.
    pub(super) fn staging_confirm(&mut self, dialog: &Dialog) -> bool {
        if *dialog != Dialog::SaveAdapter {
            return false;
        }
        self.dialog = None;
        if let Some(st) = self
            .state()
            .filter(|st| can_set_platform(st) && !self.adapter_saving())
        {
            let draft = self.adapter_draft(&st);
            if !draft.is_empty() {
                self.adapter_err = None;
                self.execute(Command::AdapterSave(draft));
            }
        }
        true
    }

    /// Records a save's outcome: a stored save drops the staged values it sent, unless they were
    /// changed since, and a failure keeps them.
    pub(super) fn staging_result(&mut self, job: &Job, result: &Result<Outcome, Error>) {
        let Some(st) = self.state() else { return };
        match (&job.command, result) {
            (Command::AdapterSave(sent), Ok(_)) => {
                if let Some(draft) = self.adapter_drafts.get_mut(&st.status.id) {
                    if draft.platform == sent.platform {
                        draft.platform = None;
                    }
                    draft.transports.retain(|t| !sent.transports.contains(t));
                    draft.interfaces.retain(|u| !sent.interfaces.contains(u));
                    if draft.is_empty() {
                        self.adapter_drafts.remove(&st.status.id);
                    }
                }
            }
            (Command::AdapterSave(_), Err(e)) => {
                let words = text::capitalized(&text::error_words(e));
                self.adapter_err = Some((st.status.id.clone(), words));
            }
            (Command::DeviceSave(id, sent), Ok(_)) => {
                let key = device_key(&st, *id);
                if let Some(draft) = self.device_drafts.get_mut(&key) {
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
                    if draft.layers == sent.layers {
                        draft.layers = None;
                    }
                    if draft.is_empty() {
                        self.device_drafts.remove(&key);
                    }
                }
            }
            (Command::DeviceSave(id, sent), Err(e)) => {
                let failed = Some((
                    device_key(&st, *id),
                    text::capitalized(&text::error_words(e)),
                ));
                if sent.layers.is_some() {
                    self.layers_err = failed;
                } else {
                    self.details_err = failed;
                }
            }
            _ => {}
        }
    }
}
