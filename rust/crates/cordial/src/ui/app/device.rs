//! A saved device's page: Details, Settings, Profiles, and Diagnostics. The Details switches and
//! the Profiles layers each stage their changes until their own Save; Connect, Disconnect, and
//! Forget act at once.
use super::{
    Action, App, Dialog, Job, Kind, PickFor, Spot, Tab, Toggle, fleet::Fleet, words,
    world::DeviceView,
};
use crate::{
    commands,
    controller::{Command, DeviceUpdate, Outcome, Target},
    error::Error,
    model, profiles,
    ui::text,
};
use cordial_protocol::{self as p, ErrorCode, InactiveReason, OutputMode};

/// The staged values in `draft` that differ from the device's saved ones.
pub fn device_pending(draft: &DeviceUpdate, d: &p::Device) -> DeviceUpdate {
    DeviceUpdate {
        enabled: draft.enabled.filter(|v| *v != d.enabled),
        trusted: draft.trusted.filter(|v| *v != d.trusted),
        blocked: draft.blocked.filter(|v| *v != d.blocked),
        hidpp: draft.hidpp.filter(|v| *v != model::hidpp_enabled(d)),
        gamepad: draft.gamepad.filter(|m| Some(*m) != model::gamepad_mode(d)),
        layers: draft
            .layers
            .clone()
            .filter(|l| l.as_slice() != profiles::layers(d)),
    }
}

/// Identity facts the Information card shows, in order.
pub(crate) const DETAILS: [&str; 6] = [
    "Manufacturer",
    "Model",
    "Serial Number",
    "Firmware",
    "Hardware",
    "Software",
];

/// A failed save as sentences naming the adapter and what it couldn't save, then the reason
/// when the reason doesn't already say so.
pub fn save_failed(what: &str, reason: &str) -> String {
    if reason.starts_with("The adapter couldn't save") {
        reason.to_owned()
    } else {
        format!("The adapter couldn't save {what}. {reason}")
    }
}

/// The settings a save sent, counted: "1 setting" or "{n} settings".
pub fn settings_count(n: usize) -> String {
    match n {
        1 => "1 setting".to_owned(),
        n => format!("{n} settings"),
    }
}

/// The device's identity facts with these labels, in the labels' order.
pub(crate) fn facts(d: &p::Device, labels: &[&str]) -> Vec<(&'static str, String)> {
    let all = text::identity(d.information.as_ref());
    labels
        .iter()
        .filter_map(|l| all.iter().find(|(label, _)| label == l).cloned())
        .collect()
}

impl<F: Fleet> App<F> {
    pub(crate) fn device_key(d: &DeviceView) -> (String, u32) {
        (d.adapter.clone(), d.d.id)
    }
    /// The device's staged changes that differ from its record.
    pub fn device_changes(&self, d: &DeviceView) -> DeviceUpdate {
        self.device_drafts
            .get(&Self::device_key(d))
            .map(|draft| {
                if self.immediate.is_some() {
                    draft.clone()
                } else {
                    device_pending(draft, &d.d)
                }
            })
            .unwrap_or_default()
    }
    /// The device's preferences as staged.
    pub fn device_values(&self, d: &DeviceView) -> DeviceUpdate {
        let c = self.device_changes(d);
        DeviceUpdate {
            enabled: Some(c.enabled.unwrap_or(d.d.enabled)),
            trusted: Some(c.trusted.unwrap_or(d.d.trusted)),
            blocked: Some(c.blocked.unwrap_or(d.d.blocked)),
            hidpp: Some(c.hidpp.unwrap_or_else(|| model::hidpp_enabled(&d.d))),
            gamepad: c.gamepad.or_else(|| model::gamepad_mode(&d.d)),
            layers: Some(c.layers.unwrap_or_else(|| profiles::layers(&d.d).to_vec())),
        }
    }
    /// Whether the device's changes from `tab` are being saved.
    pub fn device_saving(&self, d: &DeviceView, tab: Tab) -> bool {
        let id = d.d.id;
        self.running(
            &d.adapter,
            |k| matches!(k, Kind::DeviceSave { device, tab: t, .. } if *device == id && *t == tab),
        )
    }
    pub fn device_busy(&self, d: &DeviceView) -> bool {
        let id = d.d.id;
        self.running(&d.adapter, |k| match k {
            Kind::Device(device, _) | Kind::Forget(device) => *device == id,
            _ => false,
        })
    }
    /// Whether the device's preferences take no changes now: a save is running, including a
    /// settings save that saves the staged gamepad output once it finishes.
    pub fn device_locked(&self, d: &DeviceView) -> bool {
        let id = d.d.id;
        !self.adapter_ready(d)
            || self.running(&d.adapter, |k| match k {
                Kind::DeviceSave { device, .. } => *device == id,
                Kind::SettingsSave {
                    device, gamepad, ..
                } => *device == id && *gamepad,
                _ => false,
            })
    }
    /// Whether the device's Logitech Features or settings are busy.
    pub fn settings_busy(&self, d: &DeviceView) -> bool {
        let key = Self::device_key(d);
        let saving = self.submissions.get(&key).is_some_and(|s| s.running);
        let hidpp = self.running(&d.adapter, |k| {
            matches!(k, Kind::DeviceSave { device, sent, .. } if *device == d.d.id && sent.hidpp.is_some())
        });
        saving || hidpp
    }
    /// Stages a change made on `tab`.
    pub(crate) fn stage_device(
        &mut self,
        d: &DeviceView,
        tab: Tab,
        change: impl FnOnce(&mut DeviceUpdate),
    ) {
        if self.immediate.is_some() {
            let mut update = DeviceUpdate::default();
            change(&mut update);
            self.stage_immediate_device(d, tab, update);
            return;
        }
        let key = Self::device_key(d);
        let mut draft = self.device_drafts.remove(&key).unwrap_or_default();
        change(&mut draft);
        let draft = device_pending(&draft, &d.d);
        if !draft.is_empty() {
            self.device_drafts.insert(key.clone(), draft);
        }
        self.notes.remove(&save_spot(tab, key.0, key.1));
    }
    /// Runs a device command whose failure shows at `spot`.
    pub(crate) fn device_command(&mut self, d: &DeviceView, command: Command, spot: Spot) {
        self.notes.remove(&spot);
        self.run(&d.adapter, Kind::Device(d.d.id, spot), command);
    }
    /// Handles a device page action; false when `action` isn't one.
    pub(crate) fn device_action(&mut self, adapter: &str, id: u32, action: &Action) -> bool {
        let Some(d) = self.device(adapter, id).cloned() else {
            return false;
        };
        let target = Target::Id(id);
        let key = (adapter.to_owned(), id);
        let locked = !self.device_editable(&d);
        match action {
            Action::DeviceToggle(toggle, on) if !locked => {
                let on = *on;
                match toggle {
                    Toggle::Enabled => {
                        self.stage_device(&d, Tab::Details, |u| u.enabled = Some(on))
                    }
                    Toggle::Trusted => {
                        self.stage_device(&d, Tab::Details, |u| u.trusted = Some(on))
                    }
                    Toggle::Blocked => {
                        self.stage_device(&d, Tab::Details, |u| u.blocked = Some(on))
                    }
                    Toggle::Hidpp if self.immediate.is_some() || !self.settings_busy(&d) => {
                        self.stage_device(&d, Tab::Details, |u| u.hidpp = Some(on))
                    }
                    Toggle::Hidpp => {}
                }
            }
            Action::Connect => {
                let spot = Spot::DeviceBar(key.0.clone(), key.1);
                if !self.adapter_ready(&d) {
                    self.notes
                        .insert(spot, words::code_text(ErrorCode::NotReady));
                } else if d.read_error.is_none()
                    && model::inactive(&d.d) == Some(InactiveReason::TransportDisabled)
                {
                    self.notes
                        .insert(spot, words::transport_disabled_text(d.d.transport()));
                } else {
                    self.device_command(&d, Command::Connect(target), spot);
                }
            }
            // The adapter answers these only once it is ready.
            Action::Disconnect if self.adapter_ready(&d) => {
                let spot = Spot::DeviceBar(key.0.clone(), key.1);
                self.device_command(&d, Command::Disconnect(target), spot);
            }
            Action::Forget if self.adapter_ready(&d) => {
                self.notes.remove(&Spot::Dialog);
                self.dialog = Some(Dialog::Forget(key.0, key.1));
            }
            Action::DetailsSave => self.save_device(&d, Tab::Details),
            Action::LayersSave => self.save_device(&d, Tab::Profiles),
            Action::DetailsDiscard if !self.device_saving(&d, Tab::Details) => {
                if let Some(draft) = self.device_drafts.get_mut(&key) {
                    *draft = DeviceUpdate {
                        layers: draft.layers.take(),
                        gamepad: draft.gamepad,
                        ..DeviceUpdate::default()
                    };
                    if draft.is_empty() {
                        self.device_drafts.remove(&key);
                    }
                }
                self.notes.remove(&Spot::DetailsSave(key.0, key.1));
            }
            Action::LayersDiscard if !self.device_saving(&d, Tab::Profiles) => {
                if let Some(draft) = self.device_drafts.get_mut(&key) {
                    draft.layers = None;
                    if draft.is_empty() {
                        self.device_drafts.remove(&key);
                    }
                }
                self.notes.remove(&Spot::LayersSave(key.0, key.1));
            }
            Action::LayerUp(i) if !locked => {
                let i = *i;
                let mut layers = self.device_values(&d).layers.unwrap_or_default();
                if i > 0 && i < layers.len() {
                    layers.swap(i, i - 1);
                    self.stage_device(&d, Tab::Profiles, |u| u.layers = Some(layers));
                }
            }
            Action::LayerDown(i) if !locked => {
                let i = *i;
                let mut layers = self.device_values(&d).layers.unwrap_or_default();
                if i + 1 < layers.len() {
                    layers.swap(i, i + 1);
                    self.stage_device(&d, Tab::Profiles, |u| u.layers = Some(layers));
                }
            }
            Action::LayerMove {
                adapter: a,
                device,
                layers: seen,
                from,
                to,
            } if !locked && a == adapter && *device == id => {
                let mut layers = self.device_values(&d).layers.unwrap_or_default();
                if layers == *seen && *from < layers.len() && *to < layers.len() && from != to {
                    let moved = layers.remove(*from);
                    layers.insert(*to, moved);
                    self.stage_device(&d, Tab::Profiles, |u| u.layers = Some(layers));
                }
            }
            Action::LayerRemove(i) if !locked => {
                let i = *i;
                let mut layers = self.device_values(&d).layers.unwrap_or_default();
                if i < layers.len() {
                    layers.remove(i);
                    self.stage_device(&d, Tab::Profiles, |u| u.layers = Some(layers));
                }
            }
            Action::LayerAdd if !locked => {
                self.dialog = Some(Dialog::Pick {
                    adapter: adapter.to_owned(),
                    purpose: PickFor::Layer(id),
                    chosen: 0,
                });
                self.notes.remove(&Spot::Dialog);
                self.open_picker(adapter);
            }
            Action::DiagnosticsRefresh if !self.device_busy(&d) && self.adapter_ready(&d) => {
                let spot = Spot::Diagnostics(key.0.clone(), key.1);
                let read_failed = self.settings_errors.contains_key(&key) || d.read_error.is_some();
                if read_failed || !d.connected() {
                    self.notes.remove(&spot);
                    self.list_settings(adapter, id);
                    self.run(adapter, Kind::Device(id, spot), Command::Get(target));
                } else {
                    self.device_command(&d, Command::Refresh(target), spot);
                }
            }
            _ => return self.settings_action(adapter, id, action),
        }
        true
    }
    /// Adds a profile chosen in the picker to the end of a device's layers.
    pub(crate) fn add_layer(&mut self, adapter: &str, id: u32, profile: u32) {
        let Some(d) = self.device(adapter, id).cloned() else {
            return;
        };
        let mut layers = self.device_values(&d).layers.unwrap_or_default();
        layers.push(profile);
        self.stage_device(&d, Tab::Profiles, |u| u.layers = Some(layers));
    }
    /// Saves the device's changes staged on `tab`.
    pub(crate) fn save_device(&mut self, d: &DeviceView, tab: Tab) {
        let sent = tab_changes(&self.device_changes(d), tab);
        if sent.is_empty() || self.device_locked(d) || self.immediate_busy(&d.adapter) {
            return;
        }
        let (a, id) = Self::device_key(d);
        let spot = save_spot(tab, a.clone(), id);
        let refusal = self.state_of(&a).and_then(|st| {
            let enabled = sent.enabled.unwrap_or(d.d.enabled);
            let blocked = sent.blocked.unwrap_or(d.d.blocked);
            // A record that is only last known leaves capacity to the adapter.
            if d.read_error.is_none() && commands::capacity_refused(st, &d.d, enabled, blocked) {
                return Some(words::error_text(
                    &p::Error {
                        code: ErrorCode::NoCapacity as i32,
                        reason: p::ErrorReason::EnabledDevices as i32,
                        ..Default::default()
                    },
                    words::Scope::Save,
                ));
            }
            if sent.hidpp.is_some() && self.settings_busy(d) {
                return Some(words::code_text(ErrorCode::Busy));
            }
            if let Some(mode) = sent.gamepad
                && mode != OutputMode::Disabled
                && !model::gamepad_modes(&st.status).contains(&mode)
            {
                return Some(words::code_text(ErrorCode::Unsupported));
            }
            if let Some(l) = &sent.layers {
                let max = profiles::max_layers(&st.status).unwrap_or(0) as usize;
                if !profiles::available(&st.status) {
                    return Some(words::code_text(ErrorCode::Unsupported));
                }
                if l.len() > max || l.contains(&0) {
                    return Some(words::code_text(ErrorCode::BadArgs));
                }
            }
            None
        });
        if let Some(why) = refusal {
            self.notes.insert(spot, why);
            return;
        }
        self.notes.remove(&spot);
        self.run(
            &a,
            Kind::DeviceSave {
                device: id,
                sent: sent.clone(),
                tab,
            },
            Command::DeviceSave(id, sent),
        );
    }
    pub(crate) fn device_result(&mut self, job: &Job, result: &Result<Outcome, Error>) {
        let adapter = job.adapter.clone();
        match (&job.kind, result) {
            (Kind::DeviceSave { device, sent, tab }, result)
                if result.is_ok() || result.as_ref().is_err_and(|e| e.complete()) =>
            {
                if let Err(e) = result {
                    self.notes
                        .insert(save_spot(*tab, adapter.clone(), *device), words::failure(e));
                }
                let key = (adapter, *device);
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
                    if draft.gamepad == sent.gamepad {
                        draft.gamepad = None;
                    }
                    if draft.layers == sent.layers {
                        draft.layers = None;
                    }
                    if draft.is_empty() {
                        self.device_drafts.remove(&key);
                    }
                }
            }
            (Kind::DeviceSave { device, sent, tab }, Err(e)) => {
                let spot = save_spot(*tab, adapter.clone(), *device);
                // A failure that an ended session reports late no longer applies once the
                // device's listed record shows the change saved.
                let ended = self.session_of(&adapter).map(|s| s.slot) != Some(job.slot);
                let saved = self
                    .devices
                    .iter()
                    .find(|d| d.adapter == adapter && d.d.id == *device)
                    .is_some_and(|d| device_pending(sent, &d.d).is_empty());
                if !(ended && saved) {
                    self.notes.insert(spot, words::failure(e));
                }
            }
            (Kind::Forget(device), Ok(_)) => {
                if self.dialog == Some(Dialog::Forget(adapter.clone(), *device)) {
                    self.dialog = None;
                }
            }
            (Kind::Forget(device), Err(e)) => {
                if self.dialog == Some(Dialog::Forget(adapter, *device)) {
                    self.notes.insert(Spot::Dialog, words::failure(e));
                }
            }
            (Kind::Device(_, spot), Err(e)) => {
                let text = match self.state_of(&adapter).map(|st| st.status.clone()) {
                    Some(status) => match &job.command {
                        Command::Connect(_) => {
                            let t = self
                                .device(&adapter, job_device(job))
                                .map(|d| d.d.transport());
                            match t {
                                Some(t) => {
                                    words::transport_failure(e, &status, t, words::Scope::Save)
                                }
                                None => words::failure(e),
                            }
                        }
                        Command::Get(_) => words::read_failure(e),
                        _ => words::failure(e),
                    },
                    None => words::failure(e),
                };
                self.notes.insert(spot.clone(), text);
            }
            _ => {}
        }
    }
    pub(crate) fn forget_confirm(&mut self, adapter: &str, id: u32) {
        let busy = self.running(adapter, |k| matches!(k, Kind::Forget(d) if *d == id));
        let ready = self
            .device(adapter, id)
            .is_some_and(|d| self.adapter_ready(d));
        if busy || !ready {
            return;
        }
        self.notes.remove(&Spot::Dialog);
        if !self.run(adapter, Kind::Forget(id), Command::Unpair(Target::Id(id))) {
            self.notes.insert(Spot::Dialog, words::GONE.into());
        }
    }
}
pub fn tab_changes(changes: &DeviceUpdate, tab: Tab) -> DeviceUpdate {
    match tab {
        Tab::Profiles => DeviceUpdate {
            layers: changes.layers.clone(),
            ..DeviceUpdate::default()
        },
        Tab::Settings => DeviceUpdate {
            gamepad: changes.gamepad,
            ..DeviceUpdate::default()
        },
        Tab::Details => DeviceUpdate {
            layers: None,
            gamepad: None,
            ..changes.clone()
        },
        Tab::Diagnostics => DeviceUpdate::default(),
    }
}

/// Where a failed save of `tab`'s changes shows.
pub(crate) fn save_spot(tab: Tab, adapter: String, id: u32) -> Spot {
    match tab {
        Tab::Profiles => Spot::LayersSave(adapter, id),
        Tab::Settings => Spot::SettingsNote(adapter, id),
        _ => Spot::DetailsSave(adapter, id),
    }
}

/// The device a device command acts on.
pub(crate) fn job_device(job: &Job) -> u32 {
    match &job.kind {
        Kind::Device(id, _) | Kind::Forget(id) => *id,
        Kind::DeviceSave { device, .. } | Kind::SettingsSave { device, .. } => *device,
        _ => 0,
    }
}
