//! A saved device's page: Details, Settings, Profiles and Diagnostics. The Details switches and
//! the Profiles layers each stage their changes until their own Save; Connect, Disconnect and
//! Forget act at once.
use super::{
    Action, Dialog, Job, Kind, Model, PickFor, Spot, Tab, Toggle,
    fleet::Fleet,
    layout::{self, Layout, Tone, bold, dim, err, info_style, warn},
    render::{DialogView, PageView, button_if, pill, staged_label},
    words,
    world::{DeviceView, has_settings},
};
use crate::{
    commands,
    controller::{Command, DeviceUpdate, Outcome, Target},
    error::Error,
    model, profiles,
    ui::text,
};
use cordial_protocol::{self as p, DeviceState, ErrorCode, InactiveReason, OutputMode};
use ratatui::{style::Style, text::Line};

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
const DETAILS: [&str; 6] = [
    "Manufacturer",
    "Model",
    "Serial Number",
    "Firmware",
    "Hardware",
    "Software",
];

/// The device's identity facts with these labels, in the labels' order.
fn facts(d: &p::Device, labels: &[&str]) -> Vec<(&'static str, String)> {
    let all = text::identity(d.information.as_ref());
    labels
        .iter()
        .filter_map(|l| all.iter().find(|(label, _)| label == l).cloned())
        .collect()
}

impl<F: Fleet> Model<F> {
    fn device_key(d: &DeviceView) -> (String, u32) {
        (d.adapter.clone(), d.d.id)
    }

    /// The device's staged changes that differ from its record.
    pub(super) fn device_changes(&self, d: &DeviceView) -> DeviceUpdate {
        self.device_drafts
            .get(&Self::device_key(d))
            .map(|draft| device_pending(draft, &d.d))
            .unwrap_or_default()
    }

    /// The device's preferences as staged.
    fn device_values(&self, d: &DeviceView) -> DeviceUpdate {
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
    pub(super) fn device_saving(&self, d: &DeviceView, tab: Tab) -> bool {
        let id = d.d.id;
        self.running(
            &d.adapter,
            |k| matches!(k, Kind::DeviceSave { device, tab: t, .. } if *device == id && *t == tab),
        )
    }

    fn device_busy(&self, d: &DeviceView) -> bool {
        let id = d.d.id;
        self.running(&d.adapter, |k| match k {
            Kind::Device(device, _) | Kind::Forget(device) => *device == id,
            _ => false,
        })
    }

    /// Whether the device's preferences take no changes now: a save is running, including a
    /// settings save that saves the staged gamepad output once it finishes.
    pub(super) fn device_locked(&self, d: &DeviceView) -> bool {
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
    pub(super) fn settings_busy(&self, d: &DeviceView) -> bool {
        let key = Self::device_key(d);
        let saving = self.submissions.get(&key).is_some_and(|s| s.running);
        let hidpp = self.running(&d.adapter, |k| {
            matches!(k, Kind::DeviceSave { device, sent, .. } if *device == d.d.id && sent.hidpp.is_some())
        });
        saving || hidpp
    }

    /// Stages a change made on `tab`.
    pub(super) fn stage_device(
        &mut self,
        d: &DeviceView,
        tab: Tab,
        change: impl FnOnce(&mut DeviceUpdate),
    ) {
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
    fn device_command(&mut self, d: &DeviceView, command: Command, spot: Spot) {
        self.notes.remove(&spot);
        self.run(&d.adapter, Kind::Device(d.d.id, spot), command);
    }

    /// Handles a device page action; false when `action` isn't one.
    pub(super) fn device_action(&mut self, adapter: &str, id: u32, action: &Action) -> bool {
        let Some(d) = self.device(adapter, id).cloned() else {
            return false;
        };
        let target = Target::Id(id);
        let key = (adapter.to_owned(), id);
        let locked = self.device_locked(&d);
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
                    Toggle::Hidpp if !self.settings_busy(&d) => {
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
                self.dialog_scroll = 0;
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
    pub(super) fn add_layer(&mut self, adapter: &str, id: u32, profile: u32) {
        let Some(d) = self.device(adapter, id).cloned() else {
            return;
        };
        let mut layers = self.device_values(&d).layers.unwrap_or_default();
        layers.push(profile);
        self.stage_device(&d, Tab::Profiles, |u| u.layers = Some(layers));
    }

    /// Saves the device's changes staged on `tab`.
    pub(super) fn save_device(&mut self, d: &DeviceView, tab: Tab) {
        let sent = tab_changes(&self.device_changes(d), tab);
        if sent.is_empty() || self.device_locked(d) {
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

    pub(super) fn device_result(&mut self, job: &Job, result: &Result<Outcome, Error>) {
        let adapter = job.adapter.clone();
        match (&job.kind, result) {
            (Kind::DeviceSave { device, sent, .. }, Ok(_)) => {
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

    pub(super) fn forget_confirm(&mut self, adapter: &str, id: u32) {
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

    pub(super) fn forget_dialog(&mut self, adapter: &str, id: u32, w: usize) -> DialogView {
        let name = self
            .device(adapter, id)
            .map_or_else(|| "this device".to_owned(), |d| d.name.clone());
        let mut body = Layout::new(w);
        body.para(words::FORGET_TEXT, layout::plain());
        if let Some(e) = self.notes.get(&Spot::Dialog) {
            body.para(e, err());
        }
        let busy_now = self.running(adapter, |k| matches!(k, Kind::Forget(d) if *d == id));
        let mut buttons = Layout::new(w);
        let mut right = Layout::new(w);
        button_if(
            &mut right,
            "Cancel",
            Action::Cancel,
            Tone::Normal,
            !busy_now,
        );
        button_if(
            &mut right,
            "Forget",
            Action::Confirm,
            Tone::Danger,
            !busy_now
                && self
                    .device(adapter, id)
                    .is_some_and(|d| self.adapter_ready(d)),
        );
        buttons.align_right(right);
        DialogView {
            title: format!("Forget “{name}”?"),
            body,
            buttons,
        }
    }

    /// The device's page.
    pub(super) fn device_page(&mut self, adapter: &str, id: u32, w: usize) -> PageView {
        let Some(d) = self.device(adapter, id).cloned() else {
            return PageView::new(Line::default(), w);
        };
        let status = d.status();
        let tone = match () {
            _ if d.connected() => layout::ok(),
            _ if d.d.blocked => warn(),
            _ => dim(),
        };
        let name_look = if d.connected() { bold() } else { dim() };
        let mut title = Line::from(layout::span(d.name.clone(), name_look));
        title.spans.push(layout::span("  ", Style::new()));
        title.spans.extend(pill(status, tone).spans);
        if let Some(b) = &d.battery
            && let Some(text) = b.text()
        {
            let look = match () {
                _ if b.low() => err(),
                _ if b.stale() => dim(),
                _ => Style::new(),
            };
            title
                .spans
                .push(layout::span(format!("  Battery {text}"), look));
        }
        let mut v = PageView::new(title, w);
        let st = self.state_of(adapter).cloned();
        v.tabs = vec![Tab::Details];
        if has_settings(st.as_ref(), &d.d) || !self.gamepad_choices(&d).is_empty() {
            v.tabs.push(Tab::Settings);
        }
        let profile_support = self.adapter(adapter).is_some_and(|a| a.profile_support());
        if profile_support && d.d.profiles.is_some() {
            v.tabs.push(Tab::Profiles);
        }
        v.tabs.push(Tab::Diagnostics);
        v.tab = if v.tabs.contains(&self.tab) {
            self.tab
        } else {
            Tab::Details
        };
        if let Some(reason) = model::inactive(&d.d).filter(|_| d.read_error.is_none())
            && reason != InactiveReason::Disabled
        {
            v.banners.push((words::inactive_text(&d.d), info_style()));
        }
        match v.tab {
            Tab::Details => self.device_details(&d, &mut v),
            Tab::Settings => self.device_settings(&d, &mut v),
            Tab::Profiles => self.device_layers(&d, &mut v),
            Tab::Diagnostics => self.device_diagnostics(&d, &mut v),
        }
        v
    }

    fn device_details(&self, d: &DeviceView, v: &mut PageView) {
        let changes = self.device_changes(d);
        let values = self.device_values(d);
        let locked = self.device_locked(d);
        let b = &mut v.body;
        b.section("Connection");
        let enabled = values.enabled.unwrap_or(d.d.enabled);
        let blocked = values.blocked.unwrap_or(d.d.blocked);
        // Turning the device on or unblocking it isn't offered while the adapter would refuse
        // it for want of a place for its transport.
        let refused = |enabled: bool, blocked: bool| {
            self.state_of(&d.adapter)
                .is_some_and(|st| commands::capacity_refused(st, &d.d, enabled, blocked))
        };
        let no_enable = !enabled && refused(true, blocked);
        let no_unblock = blocked && refused(enabled, false);
        let rows = [
            (
                "Use This Device",
                Toggle::Enabled,
                enabled,
                changes.enabled.is_some(),
                !locked && !no_enable,
            ),
            (
                "Automatic Connections",
                Toggle::Trusted,
                values.trusted.unwrap_or(d.d.trusted),
                changes.trusted.is_some(),
                !locked,
            ),
            (
                "Logitech Features",
                Toggle::Hidpp,
                values.hidpp.unwrap_or(false),
                changes.hidpp.is_some(),
                !locked && !self.settings_busy(d),
            ),
            (
                "Block Connections",
                Toggle::Blocked,
                blocked,
                changes.blocked.is_some(),
                !locked && !no_unblock,
            ),
        ];
        for (label, toggle, on, staged, enabled) in rows {
            b.labelled(
                staged_label(label, staged),
                layout::switch(Some(on), Action::DeviceToggle(toggle, !on), enabled),
            );
        }
        b.section("Information");
        if !model::known_kinds(&d.d.kinds).is_empty() {
            b.fact("Device Type", words::kind_text(d.kind), layout::plain());
        }
        let look = if d.connected() {
            layout::plain()
        } else {
            dim()
        };
        for (label, value) in facts(&d.d, &DETAILS) {
            b.fact(label, &text::display(&value), look);
        }
        let adapter = self
            .adapter(&d.adapter)
            .map(|a| a.name.clone())
            .unwrap_or_default();
        b.fact("Adapter", &adapter, layout::plain());
        if let Some(t) = model::transport(d.d.transport) {
            b.fact("Bluetooth", words::transport_name(t), layout::plain());
        }
        let roles: Vec<&str> = model::roles(&d.d)
            .into_iter()
            .filter_map(words::role_text)
            .collect();
        if !roles.is_empty() {
            b.fact("Input", &roles.join(", "), layout::plain());
        }

        let (a, id) = Self::device_key(d);
        let bar = &mut v.bar;
        let ready = self.adapter_ready(d);
        button_if(bar, "Forget Device", Action::Forget, Tone::Danger, ready);
        let note = self
            .notes
            .get(&Spot::DetailsSave(a.clone(), id))
            .or_else(|| self.notes.get(&Spot::DeviceBar(a.clone(), id)));
        if let Some(e) = note {
            bar.para(e, err());
        }
        let saving = self.device_saving(d, Tab::Details);
        let busy_now = self.device_busy(d);
        let connect_running = self.running(
            &d.adapter,
            |k| matches!(k, Kind::Device(device, Spot::DeviceBar(..)) if *device == d.d.id),
        ) && !d.connected();
        let connecting = d.d.state() == DeviceState::Connecting || connect_running;
        if busy_now || saving || connecting {
            bar.label(&format!("{}", super::spinner()), layout::accent());
        }
        let dirty = !tab_changes(&changes, Tab::Details).is_empty();
        let mut right = Layout::new(bar.width);
        if d.connected() || connecting {
            button_if(
                &mut right,
                "Disconnect",
                Action::Disconnect,
                Tone::Normal,
                !busy_now && ready,
            );
        } else {
            // A record that is only last known leaves Connect to the adapter.
            let can_connect = d.read_error.is_some()
                || (model::inactive(&d.d).is_none() && d.d.state() == DeviceState::Disconnected);
            let tone = if can_connect && !dirty {
                Tone::Primary
            } else {
                Tone::Normal
            };
            button_if(
                &mut right,
                "Connect",
                Action::Connect,
                tone,
                !busy_now && can_connect,
            );
        }
        button_if(
            &mut right,
            "Discard",
            Action::DetailsDiscard,
            Tone::Normal,
            !saving && dirty,
        );
        button_if(
            &mut right,
            "Save",
            Action::DetailsSave,
            Tone::Primary,
            !locked && dirty,
        );
        bar.align_right(right);
    }

    fn device_layers(&self, d: &DeviceView, v: &mut PageView) {
        let changes = self.device_changes(d);
        let layers = self.device_values(d).layers.unwrap_or_default();
        let locked = self.device_locked(d);
        let b = &mut v.body;
        b.row();
        b.line(staged_label("Profiles", changes.layers.is_some()).style(bold()));
        let n = layers.len();
        for (i, id) in layers.iter().enumerate() {
            let mut label = Line::from(layout::span(
                self.profile_text(&d.adapter, *id),
                Style::new(),
            ));
            let marks = self.role_marks(&d.adapter, *id);
            if !marks.is_empty() {
                label.spans.push(layout::span(format!("  {marks}"), dim()));
            }
            let mut controls = Layout::new(b.width);
            button_if(
                &mut controls,
                "↑",
                Action::LayerUp(i),
                Tone::Normal,
                !locked && i > 0,
            );
            button_if(
                &mut controls,
                "↓",
                Action::LayerDown(i),
                Tone::Normal,
                !locked && i + 1 < n,
            );
            button_if(
                &mut controls,
                "×",
                Action::LayerRemove(i),
                Tone::Normal,
                !locked,
            );
            b.labelled(label, controls);
        }
        let max = self
            .adapter(&d.adapter)
            .and_then(|a| a.status.as_ref())
            .and_then(profiles::max_layers)
            .unwrap_or(0) as usize;
        let mut add = Layout::new(b.width);
        button_if(
            &mut add,
            "+ Add Profile",
            Action::LayerAdd,
            Tone::Normal,
            !locked && n < max,
        );
        b.add(add);

        let (a, id) = Self::device_key(d);
        let bar = &mut v.bar;
        if let Some(e) = self.notes.get(&Spot::LayersSave(a, id)) {
            bar.para(e, err());
        }
        let saving = self.device_saving(d, Tab::Profiles);
        if saving {
            bar.label(&format!("{}", super::spinner()), layout::accent());
        }
        let dirty = changes.layers.is_some();
        let mut right = Layout::new(bar.width);
        button_if(
            &mut right,
            "Discard",
            Action::LayersDiscard,
            Tone::Normal,
            !saving && dirty,
        );
        button_if(
            &mut right,
            "Save",
            Action::LayersSave,
            Tone::Primary,
            !locked && dirty,
        );
        bar.align_right(right);
    }

    /// A profile's roles as short marks.
    pub(super) fn role_marks(&self, adapter: &str, id: u32) -> String {
        self.profile_roles(adapter, id)
            .into_iter()
            .filter_map(words::role_text)
            .collect::<Vec<_>>()
            .join(", ")
    }

    fn device_diagnostics(&self, d: &DeviceView, v: &mut PageView) {
        let (a, id) = Self::device_key(d);
        let look = if d.connected() {
            layout::plain()
        } else {
            dim()
        };
        let b = &mut v.body;
        if let Some(e) = model::last_error(&d.d) {
            b.section("Connection");
            b.fact(
                "Last Error",
                &words::error_text(e, words::Scope::Other),
                layout::plain(),
            );
        }
        if d.connected()
            && let Some(e) = model::profile_error(&d.d)
        {
            b.section("Profiles");
            b.fact("Status", "Not Loaded", err());
            b.fact("", &words::profile_error_text(e), dim());
        }
        if let Some(e) = &d.read_error {
            b.section("Saved Data");
            let text = match words::error_text(e, words::Scope::Read) {
                read if read == words::READ_FAILED => {
                    format!("{} Try again.", words::DEVICE_READ_FAILED)
                }
                read => format!("{} {read}", words::DEVICE_READ_FAILED),
            };
            b.para(&text, layout::plain());
        }
        if let Some(e) = self.settings_errors.get(&(a.clone(), id)) {
            b.section("Device Settings");
            let text = if e == words::READ_FAILED {
                format!("{} Try again.", words::SETTINGS_READ_FAILED)
            } else {
                format!("{} {e}", words::SETTINGS_READ_FAILED)
            };
            b.para(&text, layout::plain());
        }
        if let Some(i) = model::hidpp(&d.d) {
            b.section("Logitech Features");
            b.fact(
                "HID++ Protocol",
                &words::version_text(&d.d),
                layout::plain(),
            );
            b.fact("Status", &words::integration_text(i), layout::plain());
        }
        if let Some(o) = model::gamepad_output(&d.d).filter(|_| model::gamepad_saved(&d.d)) {
            b.section("Gamepad Output");
            b.fact("Status", &text::output_state_words(o), layout::plain());
            if let Some(slot) = o.slot {
                b.fact("Slot", &text::slot_number(slot), layout::plain());
            }
        }
        if d.connected()
            && let Some(security) = &d.d.security
        {
            b.section("Security");
            for (label, value) in words::security_facts(security) {
                b.fact(label, &value, layout::plain());
            }
        }
        b.section("Identifiers");
        for (label, value) in facts(&d.d, &text::IDENTIFIERS) {
            b.fact(label, &text::display(&value), look);
        }
        b.fact("Device ID", &id.to_string(), layout::plain());

        let bar = &mut v.bar;
        let spot = Spot::Diagnostics(a.clone(), id);
        if let Some(e) = self.notes.get(&spot) {
            bar.para(e, err());
        }
        let busy_now = self.device_busy(d);
        if busy_now {
            bar.label(&format!("{}", super::spinner()), layout::accent());
        }
        let retry = self.settings_errors.contains_key(&(a, id))
            || self.notes.contains_key(&spot)
            || d.read_error.is_some();
        let label = if retry { "Retry" } else { "Refresh" };
        let mut right = Layout::new(bar.width);
        button_if(
            &mut right,
            label,
            Action::DiagnosticsRefresh,
            Tone::Normal,
            !busy_now && self.adapter_ready(d),
        );
        bar.align_right(right);
    }
}

/// The changes in `changes` that `tab` stages and saves: Profiles the layers, Settings the gamepad
/// output, and Details the rest.
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
fn save_spot(tab: Tab, adapter: String, id: u32) -> Spot {
    match tab {
        Tab::Profiles => Spot::LayersSave(adapter, id),
        Tab::Settings => Spot::SettingsNote(adapter, id),
        _ => Spot::DetailsSave(adapter, id),
    }
}

/// The device a device command acts on.
fn job_device(job: &Job) -> u32 {
    match &job.kind {
        Kind::Device(id, _) | Kind::Forget(id) => *id,
        Kind::DeviceSave { device, .. } | Kind::SettingsSave { device, .. } => *device,
        _ => 0,
    }
}
