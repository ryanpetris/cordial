//! A saved device's page: Details, Settings, Profiles, and Diagnostics. The Details switches and
//! the Profiles layers each stage their changes until their own Save; Connect, Disconnect, and
//! Forget act at once.
use super::Kind;
use super::{
    Action, Model, Spot, Tab, Toggle,
    fleet::Fleet,
    layout::{self, Layout, Tone, bold, dim, err, info_style, warn},
    render::{DialogView, PageView, button_if, pill, staged_label},
    words,
    world::{DeviceView, has_settings},
};
use crate::{commands, model, profiles, ui::text};
use cordial_protocol::{DeviceState, InactiveReason};
use ratatui::{style::Style, text::Line};

use crate::ui::app::device::*;

impl<F: Fleet> Model<F> {
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

        let (a, id) = crate::ui::app::App::<F>::device_key(d);
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

        let (a, id) = crate::ui::app::App::<F>::device_key(d);
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
        let (a, id) = crate::ui::app::App::<F>::device_key(d);
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
            if let Some(v) = text::hidpp_protocol_text(&d.d) {
                b.fact("HID++ Protocol", &v, layout::plain());
            }
            b.fact("Status", &words::integration_text(i), layout::plain());
        }
        if let Some(o) = model::gamepad_output(&d.d).filter(|_| model::gamepad_saved(&d.d)) {
            b.section("Gamepad Output");
            b.fact("Status", &text::output_state_words(o), layout::plain());
            if let Some(slot) = o.slot {
                b.fact("Slot", &text::slot_number(slot), layout::plain());
            }
        }
        let security =
            d.d.security
                .as_ref()
                .filter(|_| d.connected())
                .map(words::security_facts)
                .unwrap_or_default();
        if !security.is_empty() {
            b.section("Security");
            for (label, value) in security {
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
