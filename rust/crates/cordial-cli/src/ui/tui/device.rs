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
use cordial_protocol::{self as p, DeviceState, ErrorCode, InactiveReason, keys};
use ratatui::{style::Style, text::Line};

/// The staged values in `draft` that differ from the device's saved ones.
pub fn device_pending(draft: &DeviceUpdate, d: &p::Device) -> DeviceUpdate {
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

/// Information keys the Information card shows, in order.
const DETAIL_KEYS: [&str; 6] = [
    keys::DEVICE_MANUFACTURER,
    keys::DEVICE_MODEL,
    keys::DEVICE_SERIAL,
    keys::FIRMWARE_VERSION,
    keys::HARDWARE_REVISION,
    keys::SOFTWARE_REVISION,
];

/// Information keys the Identifiers card shows, in order.
const IDENTIFIER_KEYS: [&str; 5] = [
    keys::VENDOR_REGISTRY,
    keys::VENDOR_ID,
    keys::PRODUCT_ID,
    keys::PRODUCT_VERSION,
    keys::BOOTLOADER_VERSION,
];

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
            layers: Some(c.layers.unwrap_or_else(|| profiles::layers(&d.d).to_vec())),
        }
    }

    pub(super) fn device_saving(&self, d: &DeviceView, layers: bool) -> bool {
        let id = d.d.id;
        self.running(&d.adapter, |k| {
            matches!(k, Kind::DeviceSave { device, layers: l, .. } if *device == id && *l == layers)
        })
    }

    fn device_busy(&self, d: &DeviceView) -> bool {
        let id = d.d.id;
        self.running(&d.adapter, |k| match k {
            Kind::Device(device, _) | Kind::Forget(device) => *device == id,
            _ => false,
        })
    }

    fn device_locked(&self, d: &DeviceView) -> bool {
        !self.adapter_ready(d) || self.device_saving(d, false) || self.device_saving(d, true)
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

    fn stage_device(
        &mut self,
        d: &DeviceView,
        layers: bool,
        change: impl FnOnce(&mut DeviceUpdate),
    ) {
        let key = Self::device_key(d);
        let mut draft = self.device_drafts.remove(&key).unwrap_or_default();
        change(&mut draft);
        let draft = device_pending(&draft, &d.d);
        if !draft.is_empty() {
            self.device_drafts.insert(key.clone(), draft);
        }
        let spot = if layers {
            Spot::LayersSave(key.0, key.1)
        } else {
            Spot::DetailsSave(key.0, key.1)
        };
        self.notes.remove(&spot);
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
                    Toggle::Enabled => self.stage_device(&d, false, |u| u.enabled = Some(on)),
                    Toggle::Trusted => self.stage_device(&d, false, |u| u.trusted = Some(on)),
                    Toggle::Blocked => self.stage_device(&d, false, |u| u.blocked = Some(on)),
                    Toggle::Hidpp if !self.settings_busy(&d) => {
                        self.stage_device(&d, false, |u| u.hidpp = Some(on))
                    }
                    Toggle::Hidpp => {}
                }
            }
            Action::Connect => {
                let spot = Spot::DeviceBar(key.0.clone(), key.1);
                if !self.adapter_ready(&d) {
                    self.notes
                        .insert(spot, words::code_text(ErrorCode::NotReady));
                } else if model::inactive(&d.d) == Some(InactiveReason::TransportDisabled) {
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
            Action::DetailsSave => self.save_device(&d, false),
            Action::LayersSave => self.save_device(&d, true),
            Action::DetailsDiscard if !self.device_saving(&d, false) => {
                if let Some(draft) = self.device_drafts.get_mut(&key) {
                    *draft = DeviceUpdate {
                        layers: draft.layers.take(),
                        ..DeviceUpdate::default()
                    };
                    if draft.is_empty() {
                        self.device_drafts.remove(&key);
                    }
                }
                self.notes.remove(&Spot::DetailsSave(key.0, key.1));
            }
            Action::LayersDiscard if !self.device_saving(&d, true) => {
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
                    self.stage_device(&d, true, |u| u.layers = Some(layers));
                }
            }
            Action::LayerDown(i) if !locked => {
                let i = *i;
                let mut layers = self.device_values(&d).layers.unwrap_or_default();
                if i + 1 < layers.len() {
                    layers.swap(i, i + 1);
                    self.stage_device(&d, true, |u| u.layers = Some(layers));
                }
            }
            Action::LayerRemove(i) if !locked => {
                let i = *i;
                let mut layers = self.device_values(&d).layers.unwrap_or_default();
                if i < layers.len() {
                    layers.remove(i);
                    self.stage_device(&d, true, |u| u.layers = Some(layers));
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
                let read_failed =
                    self.settings_errors.contains_key(&key) || self.warnings_failed.contains(&key);
                if read_failed || !d.connected() {
                    self.notes.remove(&spot);
                    self.list_settings(adapter, id);
                    self.run(adapter, Kind::Device(id, spot), Command::Warnings(target));
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
        self.stage_device(&d, true, |u| u.layers = Some(layers));
    }

    /// Saves a device's staged details, or its layers when `layers`.
    fn save_device(&mut self, d: &DeviceView, layers: bool) {
        let changes = self.device_changes(d);
        let sent = if layers {
            DeviceUpdate {
                layers: changes.layers,
                ..DeviceUpdate::default()
            }
        } else {
            DeviceUpdate {
                layers: None,
                ..changes
            }
        };
        if sent.is_empty() || self.device_locked(d) {
            return;
        }
        let (a, id) = Self::device_key(d);
        let spot = if layers {
            Spot::LayersSave(a.clone(), id)
        } else {
            Spot::DetailsSave(a.clone(), id)
        };
        let refusal = self.state_of(&a).and_then(|st| {
            let enabled = sent.enabled.unwrap_or(d.d.enabled);
            let blocked = sent.blocked.unwrap_or(d.d.blocked);
            if commands::capacity_refused(st, &d.d, enabled, blocked) {
                return Some(words::error_text(&p::Error {
                    code: ErrorCode::NoCapacity as i32,
                    reason: cordial_protocol::CapacityReason::Enabled as i32,
                    ..Default::default()
                }));
            }
            if sent.hidpp.is_some() && self.settings_busy(d) {
                return Some(words::code_text(ErrorCode::Busy));
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
                layers,
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
                    if draft.layers == sent.layers {
                        draft.layers = None;
                    }
                    if draft.is_empty() {
                        self.device_drafts.remove(&key);
                    }
                }
            }
            (Kind::DeviceSave { device, layers, .. }, Err(e)) => {
                let spot = if *layers {
                    Spot::LayersSave(adapter, *device)
                } else {
                    Spot::DetailsSave(adapter, *device)
                };
                self.notes.insert(spot, words::failure(e));
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
            (Kind::Device(device, _), Ok(_)) if matches!(job.command, Command::Warnings(_)) => {
                self.warnings_failed.remove(&(adapter, *device));
            }
            (Kind::Device(device, spot), Err(e)) => {
                if matches!(job.command, Command::Warnings(_)) {
                    self.warnings_failed.insert((adapter.clone(), *device));
                }
                let text = match self.state_of(&adapter).map(|st| st.status.clone()) {
                    Some(status) => match &job.command {
                        Command::Connect(_) => {
                            let t = self
                                .device(&adapter, job_device(job))
                                .map(|d| d.d.transport());
                            match t {
                                Some(t) => words::transport_failure(e, &status, t),
                                None => words::failure(e),
                            }
                        }
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
        let status = words::device_status(&d.d);
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
        if has_settings(st.as_ref(), &d.d) {
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
        if let Some(reason) = model::inactive(&d.d)
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
        for key in DETAIL_KEYS {
            if let Some(value) = model::info(&d.d.info, key) {
                b.fact(words::info_label(key), &words::info_value(key, value), look);
            }
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
        let saving = self.device_saving(d, false);
        let busy_now = self.device_busy(d);
        let connect_running = self.running(
            &d.adapter,
            |k| matches!(k, Kind::Device(device, Spot::DeviceBar(..)) if *device == d.d.id),
        ) && !d.connected();
        let connecting = d.d.state() == DeviceState::Connecting || connect_running;
        if busy_now || saving || connecting {
            bar.label(&format!("{}", super::spinner()), layout::accent());
        }
        let dirty = !DeviceUpdate {
            layers: None,
            ..changes
        }
        .is_empty();
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
            let can_connect =
                model::inactive(&d.d).is_none() && d.d.state() == DeviceState::Disconnected;
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
        let saving = self.device_saving(d, true);
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
        let st = self.state_of(&a);
        let warnings = st.map(|st| st.warnings_of(id).to_vec()).unwrap_or_default();
        let look = if d.connected() {
            layout::plain()
        } else {
            dim()
        };
        let b = &mut v.body;
        let warnings_failed = self.warnings_failed.contains(&(a.clone(), id));
        if warnings_failed || !warnings.is_empty() {
            b.section("Device Warnings");
            if warnings_failed {
                b.para(words::WARNINGS_READ_FAILED, layout::plain());
            }
            for w in &warnings {
                b.fact(
                    text::warning_label(w.code()),
                    text::warning_text(w.code()),
                    look,
                );
                b.fact("", &text::warning_context(w), dim());
            }
        }
        if let Some(code) = model::last_error(&d.d) {
            b.section("Connection");
            b.fact("Last Error", &words::code_text(code), layout::plain());
        }
        if d.connected()
            && let Some(code) = model::profile_error(&d.d)
        {
            b.section("Profiles");
            b.fact("Status", "Not Loaded", err());
            b.fact("", &words::profile_error_text(code), dim());
        }
        if let Some(e) = self.settings_errors.get(&(a.clone(), id)) {
            b.section("Device Settings");
            b.para(
                &format!("{} {e}", words::SETTINGS_READ_FAILED),
                layout::plain(),
            );
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
        if d.connected()
            && let Some(security) = &d.d.security
        {
            b.section("Security");
            for (label, value) in words::security_facts(security) {
                b.fact(label, &value, layout::plain());
            }
        }
        b.section("Identifiers");
        for key in IDENTIFIER_KEYS {
            if let Some(value) = model::info(&d.d.info, key) {
                b.fact(words::info_label(key), &words::info_value(key, value), look);
            }
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
        let retry = self.settings_errors.contains_key(&(a.clone(), id))
            || self.warnings_failed.contains(&(a, id))
            || self.notes.contains_key(&spot);
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

/// The device a device command acts on.
fn job_device(job: &Job) -> u32 {
    match &job.kind {
        Kind::Device(id, _) | Kind::Forget(id) => *id,
        Kind::DeviceSave { device, .. } | Kind::SettingsSave { device, .. } => *device,
        _ => 0,
    }
}
