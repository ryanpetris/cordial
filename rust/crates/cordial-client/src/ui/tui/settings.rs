//! The Device settings page replaces the device list and details with a saved
//! device's settings and an editor for one of them. Controls only edit a local
//! draft: Save sends one `hidpp.setting.set`, Default one `hidpp.setting.forget`, and
//! Refresh and Apply their device commands. Opening and navigating the page
//! read only the adapter's own records. While the device is disconnected, the
//! saved records stay visible and only Default works; drafts are kept but
//! cannot be edited or saved. With HID++ off, a connected device is still
//! read and Save stores a value on the dongle without applying it; Apply
//! waits until HID++ is on.
use super::{
    Action, Area, Job, Model, connected, device_id,
    layout::{
        self, Choice, Layout, Styled, Tone, dim, err, inherit, ok, pad_str, span, styled, warn,
    },
    pending_for,
    view::{device_status, settings_status, spinner},
};
use crate::{
    client::Error,
    controller::{Command, DeviceSettings, Failure, Outcome, SettingInput, State},
    ui::{
        Backend, catalog,
        command::steps,
        text::{self, display, display_name, on_off, yes_no},
    },
};
use cordial_protocol::{
    errors::ErrorCode,
    identifiers::NormalizationState,
    messages::Device,
    settings::{
        ObservationSource, Setting, SettingKey, SettingScope, SettingState, SettingType,
        SettingValue,
    },
};
use ratatui::{style::Style, text::Line};
use std::collections::{HashMap, HashSet};

/// The key column of the editor.
const EDITOR_KEY: usize = 17;

#[derive(Default)]
pub struct Page {
    /// The saved device whose page is open, or empty.
    pub device: String,
    pub key: Option<SettingKey>,
    pub collapsed: HashSet<&'static str>,
    /// Unsaved values by device and key, kept until saved, cancelled or the
    /// session ends.
    pub drafts: HashMap<String, HashMap<SettingKey, SettingValue>>,
    /// Failed saves, and Saves and Defaults in flight, by device and key.
    pub save_err: HashMap<(String, SettingKey), String>,
    pub sending: HashMap<(String, SettingKey), usize>,
    pub list_scroll: usize,
    pub editor_scroll: usize,
    pub reveal: bool,
    /// A settings snapshot is being read, why the last failed, and the
    /// state it was read for.
    pub loading: bool,
    pub load_err: Option<Error>,
    pub load_mark: String,
    /// The last Refresh or Apply outcome.
    pub job_note: String,
    pub job_look: Style,
    /// Last reported state by device and key, for the activity log.
    pub setting_states: HashMap<String, SettingState>,
}

impl Page {
    /// Drops what belonged to the previous adapter; categories stay collapsed.
    pub fn reset(&mut self) {
        let collapsed = std::mem::take(&mut self.collapsed);
        *self = Self {
            collapsed,
            ..Self::default()
        };
    }
}

pub(super) fn cache(st: &State, id: &str) -> DeviceSettings {
    st.settings.get(&device_id(id)).cloned().unwrap_or_default()
}

/// Why Save, Default, Refresh and Apply are unavailable now, or "".
pub(super) fn settings_busy(st: &State, d: &Device) -> &'static str {
    if !st.available {
        return "Lost the adapter connection.";
    }
    for (command, text) in [
        ("hidpp.setting.set", "Saving a setting…"),
        ("hidpp.setting.forget", "Forgetting a saved value…"),
        ("hidpp.setting.refresh", "Reading current values…"),
        ("hidpp.setting.apply", "Applying saved values…"),
        ("device.hidpp.set", "Saving the HID++ preference…"),
    ] {
        if pending_for(st, command, &d.device_id.0).is_some() {
            return text;
        }
    }
    match d.settings_state {
        cordial_protocol::identifiers::SettingsState::Discovering => {
            return "The adapter is reading this device's settings.";
        }
        cordial_protocol::identifiers::SettingsState::Applying => {
            return "The adapter is applying saved settings to this device.";
        }
        _ => {}
    }
    match d.normalization_state {
        NormalizationState::Probing
        | NormalizationState::Configuring
        | NormalizationState::Resetting => "The adapter is setting up HID++ on this device.",
        _ => "",
    }
}

/// Why the settings cannot be read or edited now; only Default works meanwhile.
fn live_reason(d: &Device) -> &'static str {
    match (connected(d), d.hidpp_enabled) {
        (true, _) => "",
        (false, true) => {
            "Connect the device to read or change its settings. Saved values are applied when it connects."
        }
        (false, false) => "Connect the device to read or change its settings.",
    }
}

/// Why saved values cannot be applied now.
fn apply_reason(d: &Device) -> &'static str {
    match live_reason(d) {
        "" if !d.hidpp_enabled => "HID++ is off, so saved values aren't applied.",
        reason => reason,
    }
}

/// Whether an observation is a current reading of the device. Readings are
/// not current while it is disconnected, HID++ is being reset, or the cache is
/// being updated. HID++ off does not stop reading.
fn fresh(d: &Device, c: &DeviceSettings, s: &Setting) -> bool {
    s.fresh
        && s.observed != SettingValue::Null
        && c.current
        && connected(d)
        && d.normalization_state != NormalizationState::Resetting
}

/// A current reading that differs from the saved value.
fn differs(d: &Device, c: &DeviceSettings, s: &Setting) -> bool {
    s.managed && fresh(d, c, s) && s.observed != s.desired
}

fn fresh_words(d: &Device, c: &DeviceSettings, s: &Setting) -> String {
    let reason = if !connected(d) {
        "device disconnected"
    } else if d.normalization_state == NormalizationState::Resetting {
        "HID++ being reset"
    } else if !c.current {
        "updating"
    } else {
        ""
    };
    let null = s.observed == SettingValue::Null;
    match () {
        _ if null && !reason.is_empty() => reason.into(),
        _ if null => "not read".into(),
        _ if fresh(d, c, s) && s.observation_source == Some(ObservationSource::Event) => {
            "reported by device".into()
        }
        _ if fresh(d, c, s) => "read from device".into(),
        _ if !reason.is_empty() => format!("stale: {reason}"),
        _ => "stale: not read again yet".into(),
    }
}

/// A value as the TUI labels it.
pub(super) fn human_value(s: &Setting, v: &SettingValue) -> String {
    match v {
        SettingValue::Null => "Unavailable".into(),
        SettingValue::Bool(b) => if *b { "On" } else { "Off" }.into(),
        _ => {
            if let Some(text) = catalog::readout_text(s, v) {
                return display(&text);
            }
            match v {
                SettingValue::Integer(255) if s.key == SettingKey::WheelThreshold => {
                    "255 (automatic switching off)".into()
                }
                SettingValue::Integer(n) => n.to_string(),
                SettingValue::Text(t) if s.kind == SettingType::Enum => {
                    display(&catalog::choice_words(s.key, t))
                }
                SettingValue::Text(t) => display(t),
                _ => String::new(),
            }
        }
    }
}

fn scope_words(s: &Setting) -> &'static str {
    match s.scope {
        SettingScope::CurrentHost => "This computer's host slot only",
        SettingScope::Device => "Whole device, for every paired computer",
    }
}

/// A managed setting's apply state. With HID++ off the saved value waits.
fn status_words(d: &Device, s: &Setting) -> (&'static str, Style) {
    use SettingState::*;
    let off = !d.hidpp_enabled;
    match s.state {
        Pending | Applying if off => ("○ Saved, not applied: HID++ is off", dim()),
        ChangedOnDevice if off => ("≠ Device differs; not applied while HID++ is off", warn()),
        Uncertain if off => ("? Uncertain; applied again once HID++ is on", warn()),
        Pending => ("◌ Pending: applied when the device is next ready", warn()),
        Applying => ("◌ Applying…", warn()),
        Applied => ("● Applied", ok()),
        ChangedOnDevice => ("≠ Changed on device", warn()),
        Unsupported => ("○ The device doesn't support it now", dim()),
        Error => ("✕ Failed", err()),
        Uncertain => ("? Uncertain; Apply rereads it first", warn()),
        Unmanaged => ("Default", layout::plain()),
    }
}

/// A setting row's tag.
fn row_tag(d: &Device, c: &DeviceSettings, s: &Setting, draft: bool) -> (&'static str, Style) {
    use SettingState::*;
    match () {
        _ if draft => ("✎ Unsaved", warn()),
        _ if !s.writable => ("", layout::plain()),
        _ if !s.managed && s.error.is_some() => ("✕ Read failed", err()),
        _ if !s.managed => ("Default", dim()),
        _ if s.state == Error => ("✕ Failed", err()),
        _ if s.state == Uncertain => ("? Uncertain", warn()),
        _ if s.state == Unsupported => ("○ Unsupported", dim()),
        _ if !d.hidpp_enabled && matches!(s.state, Pending | Applying) => ("○ Not applied", dim()),
        _ if differs(d, c, s) || s.state == ChangedOnDevice && fresh(d, c, s) => {
            ("≠ Differs", warn())
        }
        _ if s.state == Applying => ("◌ Applying", warn()),
        _ if s.state == Pending => ("◌ Pending", warn()),
        _ => ("● Saved", ok()),
    }
}

/// Moves an integer by delta within its range, on its steps; an off-step
/// reading moves to the next step first.
fn step_value(s: &Setting, from: &SettingValue, delta: i64) -> SettingValue {
    let (fine, _) = steps(s);
    let base = catalog::base(s);
    let mut n = match from {
        SettingValue::Integer(n) => {
            let off = (n - base).rem_euclid(fine);
            match () {
                _ if off == 0 => n + delta,
                _ if delta > 0 => n - off + delta,
                _ => n - off + delta + fine,
            }
        }
        _ if delta < 0 && s.max.is_some() => s.max.unwrap(),
        _ => base,
    };
    if let Some(min) = s.min {
        n = n.max(min);
    }
    if let Some(max) = s.max {
        n = n.min(max);
    }
    SettingValue::Integer(n)
}

fn disabled_row(label: &str, w: usize) -> Layout {
    let mut r = Layout::new(w);
    r.disabled(label);
    r
}

impl<B: Backend> Model<B> {
    /// Whether a saved device's settings page is shown.
    pub(super) fn settings_open(&self, st: &State) -> bool {
        !self.page.device.is_empty()
            && self.session.is_some()
            && Self::find(st, &self.page.device).0.is_some()
    }

    pub(super) fn open_settings(&mut self, id: String) {
        self.page.device = id;
        self.page.key = None;
        self.page.list_scroll = 0;
        self.page.editor_scroll = 0;
        self.page.load_err = None;
        self.page.load_mark.clear();
        self.page.job_note.clear();
        self.focus = None;
    }

    pub(super) fn close_settings(&mut self) {
        self.page.device.clear();
        self.page.key = None;
        self.page.load_err = None;
        self.page.job_note.clear();
        self.focus = None;
    }

    /// Discards everything the page kept for a device's bond.
    pub(super) fn forget_device(&mut self, id: &str) {
        self.page.drafts.remove(id);
        self.page.save_err.retain(|(d, _), _| d != id);
        self.page.sending.retain(|(d, _), _| d != id);
        let prefix = format!("{id}/");
        self.page
            .setting_states
            .retain(|k, _| !k.starts_with(&prefix));
    }

    /// Keeps an open page's cache current: loads the snapshot when there is
    /// none or it was invalidated, such as by missed notifications or a
    /// changed catalog. A failed or still outdated load is retried only once
    /// the device or the adapter's revision changes.
    pub(super) fn sync_settings(&mut self) {
        let Some(st) = self.state() else {
            return;
        };
        // Drafts belong to a bond's known settings: once the session drops
        // them, as for a removed or newly paired device, so does the page.
        let dropped: Vec<String> = self
            .page
            .drafts
            .keys()
            .filter(|id| !cache(&st, id).loaded)
            .cloned()
            .collect();
        for id in dropped {
            self.forget_device(&id);
        }
        if self.page.device.is_empty() || self.page.loading {
            return;
        }
        let Some(d) = Self::find(&st, &self.page.device).0.cloned() else {
            if st.current {
                self.close_settings(); // The device was removed.
            }
            return;
        };
        // Loading is automatic, so only an adapter offering it is asked.
        if !st.available
            || cache(&st, &d.device_id.0).current
            || !self.offers(&st, &Action::SettingsReload)
        {
            return;
        }
        let mark = [
            text::wire(&d.state),
            on_off(d.hidpp_enabled).into(),
            text::wire(&d.normalization_state),
            text::wire(&d.settings_state),
            d.settings_revision.to_string(),
            st.revision.to_string(),
            st.current.to_string(),
        ]
        .join("/");
        if mark == self.page.load_mark {
            return;
        }
        self.page.loading = true;
        self.page.load_mark = mark;
        let mut job = Job::new(Command::Settings(d.device_id.0.clone()));
        job.load = true;
        self.execute_job(job);
    }

    fn draft(&self, id: &str, key: SettingKey) -> Option<&SettingValue> {
        self.page.drafts.get(id)?.get(&key)
    }

    /// The value the controls show: the draft, else the saved value, else a
    /// legal current reading. An observed value that cannot be set, such as a
    /// temporary mode, is never offered.
    fn edit_value(&self, s: &Setting) -> SettingValue {
        if let Some(v) = self.draft(&self.page.device, s.key) {
            return v.clone();
        }
        if s.managed {
            return s.desired.clone();
        }
        if s.accepts(&s.observed) {
            return s.observed.clone();
        }
        SettingValue::Null
    }

    /// Edits the unsaved value. Choosing the saved value again leaves no
    /// draft, except while a Save or Default is in flight and that saved value
    /// is about to change; nothing is sent until Save.
    fn set_draft(&mut self, s: &Setting, v: SettingValue) {
        if !s.accepts(&v) {
            return;
        }
        let id = self.page.device.clone();
        let sending = self
            .page
            .sending
            .get(&(id.clone(), s.key))
            .copied()
            .unwrap_or(0);
        let drafts = self.page.drafts.entry(id.clone()).or_default();
        if s.managed && v == s.desired && sending == 0 {
            drafts.remove(&s.key);
        } else {
            drafts.insert(s.key, v);
        }
        self.page.save_err.remove(&(id, s.key));
    }

    /// The visible setting rows, in display order.
    fn visible_keys(&self, c: &DeviceSettings) -> Vec<SettingKey> {
        let known = catalog::presented(&c.settings);
        let mut keys = Vec::new();
        for category in catalog::categories(&c.settings) {
            if self.page.collapsed.contains(category) {
                continue;
            }
            keys.extend(
                known
                    .iter()
                    .filter(|s| catalog::category(s.key) == category)
                    .map(|s| s.key),
            );
        }
        keys
    }

    pub(super) fn settings_pane(&mut self, st: &State, w: usize, h: usize) -> Layout {
        let d = Self::find(st, &self.page.device).0.unwrap().clone();
        let c = cache(st, &d.device_id.0);
        let mut b = Layout::new(w.saturating_sub(4));
        let mut pinned = Layout::new(w.saturating_sub(4));
        let (text, look) = device_status(&d);
        b.line(Line::from(vec![
            span(text, look),
            span(format!(" · HID++ {}", on_off(d.hidpp_enabled)), dim()),
        ]));
        if let Some((t, l)) = settings_status(&d) {
            b.para(&t, l);
        }
        if let Some(e) = &self.page.load_err {
            b.para(
                &format!("✕ Couldn't load settings: {}", text::error_words(e)),
                err(),
            );
            if e.wire
                .as_ref()
                .is_some_and(|w| w.code == ErrorCode::SettingsUnavailable)
            {
                b.para(
                    "They appear once the device connects and the adapter reads them.",
                    dim(),
                );
            }
            b.button("Retry", Action::SettingsReload, Tone::Normal);
        } else if !c.loaded {
            b.line(styled(format!("{} Loading settings…", spinner()), warn()));
        } else if !c.current {
            b.line(styled(format!("{} Updating…", spinner()), warn()));
        }
        let known = catalog::presented(&c.settings);
        if c.loaded && c.current && self.page.load_err.is_none() && known.is_empty() {
            b.para("The device reported no settings.", dim());
        }
        if connected(&d) && !d.hidpp_enabled && c.loaded {
            b.para(
                "○ HID++ is off: current values are still read from the device, but saved values aren't applied. Save only stores a value on the dongle.",
                warn(),
            );
        }
        let inner = b.width;
        let tag_w = 14;
        let value_w = (inner.saturating_sub(2 + tag_w) * 2 / 5).max(6);
        let label_w = inner.saturating_sub(2 + tag_w + value_w).max(4);
        let mut selected_line = None;
        for category in catalog::categories(&c.settings) {
            b.row();
            let collapsed = self.page.collapsed.contains(category);
            let arrow = if collapsed { "▸ " } else { "▾ " };
            b.control(
                styled(format!("{arrow}{}", display(category)), layout::title()),
                Action::Category(category),
            );
            if collapsed {
                continue;
            }
            for s in known
                .iter()
                .filter(|s| catalog::category(s.key) == category)
            {
                let (base, look, marker) = if Some(s.key) == self.page.key {
                    selected_line = Some(b.lines.len());
                    (
                        layout::selected(),
                        inherit(layout::bold(), layout::selected()),
                        "▌ ",
                    )
                } else {
                    (layout::plain(), layout::plain(), "  ")
                };
                let has_draft = self.draft(&d.device_id.0, s.key).is_some();
                let value = human_value(s, &s.observed);
                let value_look = if fresh(&d, &c, s) {
                    layout::plain()
                } else {
                    dim()
                };
                let (tag, tag_look) = row_tag(&d, &c, s, has_draft);
                let label = layout::truncate_str(&display(catalog::label(s.key)), label_w - 1);
                let mut row: Styled = Line::from(vec![
                    span(marker, inherit(layout::accent(), base)),
                    span(pad_str(&label, label_w), look),
                    span(
                        pad_str(&layout::truncate_str(&value, value_w - 1), value_w),
                        inherit(value_look, base),
                    ),
                    span(layout::truncate_str(tag, tag_w), inherit(tag_look, base)),
                ]);
                let fill = inner.saturating_sub(layout::line_width(&row));
                row.spans.push(span(" ".repeat(fill), base));
                b.control(row, Action::Setting(s.key));
            }
        }
        if self.page.reveal
            && let Some(line) = selected_line
        {
            self.page.reveal = false;
            self.page.list_scroll = self
                .page
                .list_scroll
                .max(line.saturating_sub(h.saturating_sub(3)))
                .min(line);
        }
        let busy = settings_busy(st, &d);
        let (live, apply) = (live_reason(&d), apply_reason(&d));
        if !busy.is_empty() {
            pinned.para(
                &format!(
                    "{} {busy} Save, Default, Refresh and Apply wait until it finishes.",
                    spinner()
                ),
                warn(),
            );
        } else if !live.is_empty() {
            pinned.para(&format!("{live} Only Default works meanwhile."), dim());
        } else {
            if !self.page.job_note.is_empty() {
                pinned.para(&self.page.job_note.clone(), self.page.job_look);
            }
            if !apply.is_empty() {
                pinned.para(
                    &format!("{apply} Refresh reads the device; Save stores a value without applying it. Turn HID++ on to apply saved values."),
                    dim(),
                );
            }
        }
        pinned.row();
        // What the adapter doesn't offer is hidden; what it offers but can't
        // do now is shown disabled.
        let buttons = [
            (Action::SettingsRefresh, "Refresh", live.is_empty()),
            (
                Action::SettingsApply,
                "Apply saved values",
                apply.is_empty(),
            ),
        ];
        for (action, label, possible) in buttons {
            if !self.offers(st, &action) {
                continue;
            }
            if busy.is_empty() && possible && c.loaded {
                pinned.button(label, action, Tone::Normal);
            } else {
                pinned.disabled(label);
            }
        }
        if !d.hidpp_enabled && busy.is_empty() && self.offers(st, &Action::SettingsHidppOn) {
            pinned.button("Turn HID++ on", Action::SettingsHidppOn, Tone::Normal);
        }
        pinned.button_right("‹ Back", Action::SettingsBack, Tone::Normal);
        let title = format!("Settings · {}", display_name(d.name.as_deref()));
        self.frame(&title, b, pinned, Some(Area::SetList), false, w, h)
    }

    pub(super) fn editor_pane(&mut self, st: &State, w: usize, h: usize) -> Layout {
        let (title, b, actions) = self.editor(st, w);
        self.frame(&title, b, actions, Some(Area::Editor), false, w, h)
    }

    /// The selected setting's card, value controls and actions.
    pub(super) fn editor(&self, st: &State, w: usize) -> (String, Layout, Layout) {
        let mut b = Layout::new(w.saturating_sub(4));
        let mut actions = Layout::new(w.saturating_sub(4));
        let d = Self::find(st, &self.page.device).0.unwrap();
        let c = cache(st, &d.device_id.0);
        let Some(s) = self
            .page
            .key
            .and_then(|k| c.settings.iter().find(|s| s.key == k))
        else {
            b.para(
                "Select a setting to see its current value and change it.",
                dim(),
            );
            return ("Setting".into(), b, actions);
        };
        let info = catalog::info_for(s.key).0;
        let field = |b: &mut Layout, key: &str, value: &str, st: Style| {
            b.field_at(key, EDITOR_KEY, value, st);
        };
        let is_fresh = fresh(d, &c, s);
        let look = if is_fresh { layout::plain() } else { dim() };
        field(
            &mut b,
            "Current Value:",
            &format!(
                "{} · {}",
                human_value(s, &s.observed),
                fresh_words(d, &c, s)
            ),
            look,
        );
        if let Some(facts) = catalog::readout(s, &s.observed) {
            for f in facts.iter().filter(|f| !f.name.is_empty()) {
                b.field_at(
                    "",
                    EDITOR_KEY,
                    &display(&format!("{}: {}", f.name, f.value)),
                    look,
                );
            }
        }
        if !info.note.is_empty() {
            b.field_at("", EDITOR_KEY, info.note, dim());
        }
        let title = display(info.label);
        if !s.writable {
            if let Some(code) = s.error {
                field(&mut b, "Read Failed:", &text::hidpp_words(code), err());
            }
            b.row();
            b.para("Read-only information from the device.", dim());
            return (title, b, actions);
        }
        field(
            &mut b,
            "Saved on Dongle:",
            yes_no(s.managed),
            layout::plain(),
        );
        if s.managed && (!is_fresh || s.observed != s.desired) {
            field(
                &mut b,
                "Saved Value:",
                &human_value(s, &s.desired),
                layout::plain(),
            );
        }
        if differs(d, &c, s) {
            b.para("≠ Current value differs from saved", warn());
        }
        if s.managed {
            let (text, look) = status_words(d, s);
            field(&mut b, "Status:", text, look);
        }
        if let Some(code) = s.error {
            let key = if s.managed { "Error:" } else { "Read Failed:" };
            field(&mut b, key, &text::hidpp_words(code), err());
        }
        field(&mut b, "Applies To:", scope_words(s), dim());
        if s.key == SettingKey::WheelThreshold
            && let Some(mode) = c.settings.iter().find(|m| m.key == SettingKey::WheelMode)
        {
            let current = if mode.managed {
                &mode.desired
            } else {
                &mode.observed
            };
            if *current == SettingValue::Text("freespin".into()) {
                b.para("No effect while the wheel is in free-spin mode.", warn());
            }
        }
        b.row();
        // Offline, a draft is kept but cannot be edited. An adapter that
        // can't save values offers no value controls at all.
        let blocked = live_reason(d);
        let editable = self.offers(st, &Action::Save(s.key));
        let (first_line, first_hit) = (b.lines.len(), b.hits.len());
        let value = self.edit_value(s);
        if s.kind == SettingType::Bool {
            let on = match value {
                SettingValue::Bool(b) => Some(b),
                _ => None,
            };
            b.choice(
                "New Value:",
                EDITOR_KEY,
                vec![
                    Choice {
                        label: "On".into(),
                        action: Action::Draft(s.key, SettingValue::Bool(true)),
                        chosen: on == Some(true),
                    },
                    Choice {
                        label: "Off".into(),
                        action: Action::Draft(s.key, SettingValue::Bool(false)),
                        chosen: on == Some(false),
                    },
                ],
            );
        } else if !s.choices.is_empty() {
            let options = s
                .choices
                .iter()
                .map(|choice| Choice {
                    label: human_value(s, choice),
                    action: Action::Draft(s.key, choice.clone()),
                    chosen: value == *choice,
                })
                .collect();
            b.choice("New Value:", EDITOR_KEY, options);
        } else if s.kind == SettingType::Integer {
            if s.key == SettingKey::WheelThreshold {
                let n = match value {
                    SettingValue::Integer(n) => Some(n),
                    _ => None,
                };
                let on = n.is_some_and(|n| n != 255);
                let start = if on {
                    value.clone()
                } else {
                    [&s.desired, &s.observed]
                        .into_iter()
                        .find(|v| matches!(v, SettingValue::Integer(k) if (1..=254).contains(k)))
                        .cloned()
                        .unwrap_or(SettingValue::Integer(254))
                };
                b.choice(
                    "Auto Switching:",
                    EDITOR_KEY,
                    vec![
                        Choice {
                            label: "On".into(),
                            action: Action::Switch(s.key, start),
                            chosen: on,
                        },
                        Choice {
                            label: "Off".into(),
                            action: Action::Switch(s.key, SettingValue::Integer(255)),
                            chosen: n.is_some() && !on,
                        },
                    ],
                );
            }
            stepper(&mut b, s, &value);
        } else {
            b.hang(
                styled(pad_str("New Value:", EDITOR_KEY), dim()),
                "Use hidpp setting set in the shell to change text.",
                dim(),
            );
        }
        if !editable {
            b.hits.truncate(first_hit);
            b.lines.truncate(first_line);
        } else if !blocked.is_empty() {
            b.hits.truncate(first_hit);
            for line in &mut b.lines[first_line..] {
                *line = styled(layout::strip(line), dim());
            }
        }
        let draft = self.draft(&d.device_id.0, s.key);
        if let Some(draft) = draft {
            b.row();
            b.hang(
                styled("✎ Unsaved: ", warn()),
                &human_value(s, draft),
                warn(),
            );
            if blocked.is_empty() {
                b.para(
                    if d.hidpp_enabled {
                        "Save stores it on the dongle and applies it. It is reapplied on reconnect with HID++ on, HID++ enable and adapter platform changes."
                    } else {
                        "HID++ is off: Save only stores it on the dongle. The device keeps its current value until HID++ is turned on, which applies it."
                    },
                    dim(),
                );
            }
        }
        if let Some(text) = self.page.save_err.get(&(d.device_id.0.clone(), s.key)) {
            b.para(&format!("✕ {text}"), err());
        }
        let busy = settings_busy(st, d);
        let save = if d.hidpp_enabled { "Save" } else { "Save only" };
        if !busy.is_empty() {
            actions.para(&format!("{} {busy}", spinner()), warn());
        } else if editable && !blocked.is_empty() {
            actions.para(
                "Connect the device to change it. Default still works.",
                dim(),
            );
        } else if editable && blocked.is_empty() && !d.hidpp_enabled {
            actions.para(
                "HID++ is off: Save only stores the value on the dongle without applying it.",
                dim(),
            );
        }
        actions.row();
        if editable {
            if draft.is_some() && busy.is_empty() && blocked.is_empty() {
                actions.button(save, Action::Save(s.key), Tone::Primary);
            } else {
                actions.disabled(save);
            }
            if draft.is_some() {
                actions.button("Cancel", Action::CancelDraft(s.key), Tone::Normal);
            } else {
                actions.disabled("Cancel");
            }
        }
        if s.managed && self.offers(st, &Action::Default(s.key)) {
            if busy.is_empty() {
                actions.button_right("Default", Action::Default(s.key), Tone::Normal);
            } else {
                let w = actions.width;
                actions.align_right(disabled_row("Default", w));
            }
            actions.para(
                "Default: Forget saved value; leave device unchanged.",
                dim(),
            );
        }
        (title, b, actions)
    }

    /// Handles the page's controls.
    pub(super) fn settings_action(&mut self, action: Action) {
        let Some(st) = self.state() else {
            return;
        };
        if self.page.device.is_empty() {
            return;
        }
        let Some(d) = Self::find(&st, &self.page.device).0.cloned() else {
            return;
        };
        let c = cache(&st, &d.device_id.0);
        let setting = |key: &SettingKey| c.settings.iter().find(|s| s.key == *key).cloned();
        let busy = !settings_busy(&st, &d).is_empty();
        let offline = !live_reason(&d).is_empty();
        let id = d.device_id.0.clone();
        match action {
            Action::SettingsBack => self.close_settings(),
            Action::SettingsReload => {
                self.page.load_err = None;
                self.page.load_mark.clear();
            }
            Action::SettingsRefresh | Action::SettingsApply => {
                let apply = action == Action::SettingsApply;
                if busy || offline || apply && !apply_reason(&d).is_empty() {
                    return;
                }
                self.page.job_note.clear();
                self.execute(if apply {
                    Command::SettingsApply(id)
                } else {
                    Command::SettingsRefresh(id)
                });
            }
            Action::SettingsHidppOn => {
                if !busy && !d.hidpp_enabled {
                    self.execute(Command::Hidpp(id, true));
                }
            }
            Action::Category(category) => {
                if !self.page.collapsed.remove(category) {
                    self.page.collapsed.insert(category);
                }
            }
            Action::Setting(key) if setting(&key).is_some() => {
                self.page.key = Some(key);
                self.page.editor_scroll = 0;
            }
            // Only Default works while the device can't be read or changed.
            Action::Draft(..) | Action::Switch(..) | Action::Step(..) | Action::Save(_)
                if offline => {}
            Action::Draft(key, v) | Action::Switch(key, v) => {
                if let Some(s) = setting(&key) {
                    self.set_draft(&s, v);
                }
            }
            Action::Step(key, delta) => {
                if let Some(s) = setting(&key) {
                    let v = step_value(&s, &self.edit_value(&s), delta);
                    self.set_draft(&s, v);
                }
            }
            Action::CancelDraft(key) => {
                if let Some(drafts) = self.page.drafts.get_mut(&id) {
                    drafts.remove(&key);
                }
                self.page.save_err.remove(&(id, key));
            }
            Action::Save(key) => {
                let Some(v) = self.draft(&id, key).cloned() else {
                    return;
                };
                if busy || setting(&key).is_none() {
                    return;
                }
                self.send(
                    v.clone(),
                    Command::SettingSet(id, key, SettingInput::Value(v)),
                );
            }
            Action::Default(key) => {
                if busy || !setting(&key).is_some_and(|s| s.managed) {
                    return;
                }
                let v = self.draft(&id, key).cloned().unwrap_or_default();
                self.send(v, Command::SettingForget(id, key));
            }
            _ => {}
        }
    }

    /// Runs a Save or Default of a draft, Null for none. It is in flight until
    /// its own result, which carries the draft it sent.
    fn send(&mut self, draft: SettingValue, command: Command) {
        let (Command::SettingSet(id, key, _) | Command::SettingForget(id, key)) = &command else {
            return;
        };
        let k = (id.clone(), *key);
        *self.page.sending.entry(k.clone()).or_default() += 1;
        self.page.save_err.remove(&k);
        let mut job = Job::new(command);
        job.sent = Some(draft);
        self.execute_job(job);
    }

    /// Records a settings command's outcome on the page. The result of a page
    /// Save or Default discards the draft it sent, but not one edited since;
    /// a failure keeps the draft and says why.
    pub(super) fn settings_result(&mut self, job: &Job, result: &Result<Outcome, Failure>) {
        match &job.command {
            Command::SettingSet(id, key, _) | Command::SettingForget(id, key) => {
                let Some(sent) = &job.sent else {
                    return;
                };
                let k = (id.clone(), *key);
                match self.page.sending.get_mut(&k) {
                    Some(n) if *n > 1 => *n -= 1,
                    _ => {
                        self.page.sending.remove(&k);
                    }
                }
                if let Err(f) = result {
                    self.page.save_err.insert(k, text::error_words(&f.error));
                    return;
                }
                if self.draft(id, *key).is_some_and(|v| v != sent) {
                    return; // Edited while in flight, so newer than the draft sent.
                }
                if let Some(drafts) = self.page.drafts.get_mut(id) {
                    drafts.remove(key);
                }
                self.page.save_err.remove(&k);
            }
            Command::SettingsRefresh(_) | Command::SettingsApply(_) => {
                let outcome = match result {
                    Ok(o) => Some(o),
                    Err(f) => f.partial.as_deref(),
                };
                let summary = match outcome {
                    Some(Outcome::Job {
                        counts: Some(c), ..
                    }) => catalog::job_summary(&job.command, c),
                    _ => String::new(),
                };
                let refresh = matches!(job.command, Command::SettingsRefresh(_));
                (self.page.job_note, self.page.job_look) = match result {
                    Err(f) => {
                        let mut note = format!("✕ {}", text::error_words(&f.error));
                        if !summary.is_empty() {
                            note = format!("{note} ({summary})");
                        }
                        (note, err())
                    }
                    Ok(_) if refresh => (format!("● Read current values: {summary}"), ok()),
                    Ok(_) => (format!("● Applied saved values: {summary}"), ok()),
                };
            }
            _ => {}
        }
    }

    /// Selects the next or previous visible setting row.
    pub(super) fn move_setting(&mut self, delta: isize) {
        let Some(st) = self.state() else {
            return;
        };
        let keys = self.visible_keys(&cache(&st, &self.page.device));
        if keys.is_empty() {
            return;
        }
        let i = match self
            .page
            .key
            .and_then(|k| keys.iter().position(|x| *x == k))
        {
            None if delta < 0 => keys.len() - 1,
            None => 0,
            Some(i) => i.saturating_add_signed(delta).min(keys.len() - 1),
        };
        self.page.key = Some(keys[i]);
        self.page.editor_scroll = 0;
        self.page.reveal = true;
        self.focus = None;
    }
}

/// Fine and coarse increments around an integer value, and the range ends.
fn stepper(b: &mut Layout, s: &Setting, value: &SettingValue) {
    let (fine, coarse) = steps(s);
    let n = match value {
        SettingValue::Integer(n) => Some(*n),
        _ => None,
    };
    let at_min = n.is_some_and(|n| s.min.is_some_and(|m| n <= m));
    let at_max = n.is_some_and(|n| s.max.is_some_and(|m| n >= m));
    b.line(styled(pad_str("New Value:", EDITOR_KEY - 1), dim())); // Buttons add a space.
    let control = |b: &mut Layout, label: String, delta: i64, enabled: bool| {
        if enabled {
            b.button(&label, Action::Step(s.key, delta), Tone::Normal);
        } else {
            b.disabled(&label);
        }
    };
    if coarse > 0 {
        control(b, format!("−{coarse}"), -coarse, !at_min);
    }
    control(b, format!("−{fine}"), -fine, !at_min);
    b.label(
        &n.map_or_else(|| "—".into(), |n| n.to_string()),
        layout::bold(),
    );
    control(b, format!("+{fine}"), fine, !at_max);
    if coarse > 0 {
        control(b, format!("+{coarse}"), coarse, !at_max);
    }
    if let (Some(min), Some(max)) = (s.min, s.max) {
        b.line(Line::from(pad_str("", EDITOR_KEY - 1)));
        if at_min {
            b.disabled(&format!("Min {min}"));
        } else {
            b.button(
                &format!("Min {min}"),
                Action::Draft(s.key, SettingValue::Integer(min)),
                Tone::Normal,
            );
        }
        if at_max {
            b.disabled(&format!("Max {max}"));
        } else {
            b.button(
                &format!("Max {max}"),
                Action::Draft(s.key, SettingValue::Integer(max)),
                Tone::Normal,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::catalog::tests::setting;

    #[test]
    fn steps_snap_to_the_range() {
        let mut s = setting(SettingKey::BacklightDelayPowered);
        s.writable = true;
        s.min = Some(5);
        s.max = Some(300);
        s.step = Some(5);
        assert_eq!(steps(&s), (5, 50));
        assert_eq!(
            step_value(&s, &SettingValue::Integer(12), 5),
            SettingValue::Integer(15)
        );
        assert_eq!(
            step_value(&s, &SettingValue::Integer(12), -5),
            SettingValue::Integer(10)
        );
        assert_eq!(
            step_value(&s, &SettingValue::Integer(10), -50),
            SettingValue::Integer(5)
        );
        assert_eq!(
            step_value(&s, &SettingValue::Null, -5),
            SettingValue::Integer(300)
        );
        assert_eq!(
            step_value(&s, &SettingValue::Null, 5),
            SettingValue::Integer(5)
        );
    }
}
