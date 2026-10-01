//! The Device settings page replaces the device list and details with a saved
//! device's settings and an editor for one of them. The controls and the
//! editor's staging actions only stage changes; Save sends every staged
//! change of the device at once, and Discard drops them. Opening and
//! navigating the page read only the adapter's own records.
//!
//! The adapter stores a Save in one write and replies, then applies the
//! saved values to the device; each setting's apply outcome arrives later in
//! its status. With Logitech Features off, values are stored and not applied.
use super::{
    Action, Area, Job, Model, connected,
    layout::{
        self, Choice, Layout, Styled, Tone, dim, err, inherit, ok, pad_str, span, styled, warn,
    },
    pending_for,
    view::{device_status, hidpp_status, spinner},
};
use crate::{
    controller::{Command, Outcome, State},
    error::Error,
    model::{self, Type, Up},
    ui::{
        Backend, catalog,
        command::steps,
        text::{self, display, display_name, on_off},
    },
};
use cordial_protocol::{self as p, ErrorCode, SettingState, keys, value::Value};
use ratatui::{style::Style, text::Line};
use std::collections::{HashMap, HashSet};

/// The key column of the editor.
const EDITOR_KEY: usize = 13;

/// SmartShift's threshold that turns it off.
const SMARTSHIFT_OFF: i64 = 255;

/// A staged change to one setting.
#[derive(Clone, Debug, PartialEq)]
pub enum Change {
    /// A value to save. `policy` marks Save Current Value and Save Device
    /// Value, which are changes even when they match the value shown.
    Set {
        value: Value,
        policy: bool,
    },
    Forget,
}

#[derive(Default)]
pub struct Page {
    /// The saved device whose page is open, or empty.
    pub device: String,
    pub key: Option<String>,
    pub collapsed: HashSet<&'static str>,
    /// Staged changes by device and key, kept until saved, discarded or the
    /// session ends.
    pub drafts: HashMap<String, HashMap<String, Change>>,
    pub list_scroll: usize,
    pub editor_scroll: usize,
    pub reveal: bool,
    /// Devices whose settings list is being read.
    pub loading: HashSet<String>,
    /// Why the open page's last read failed.
    pub load_err: Option<Error>,
    /// Why the last Save failed.
    pub job_note: String,
    pub job_look: Style,
    /// Last reported apply state by device and key, for the activity log.
    pub setting_states: HashMap<String, Option<Result<SettingState, ErrorCode>>>,
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

/// Why the page can't save, refresh or stage changes now, or "".
pub(super) fn settings_busy(st: &State, d: &p::Device, saving: bool) -> &'static str {
    if !st.available {
        return "Lost the Adapter Connection";
    }
    if saving {
        return "Saving Settings…";
    }
    for (command, text) in [
        ("setting set", "Saving Settings…"),
        ("device refresh", "Reading Settings…"),
        ("device set hidpp", "Saving Logitech Features…"),
    ] {
        if pending_for(st, command, &d.id) {
            return text;
        }
    }
    match model::hidpp_up(d) {
        Up::Starting => "Setting Up Logitech Features…",
        _ => "",
    }
}

fn fresh_words(d: &p::Device, s: &p::Setting) -> &'static str {
    match () {
        _ if model::current(s).is_none() && !connected(d) => "Disconnected",
        _ if model::current(s).is_none() => "Not Read",
        _ if catalog::fresh(d, s) => "Read from Device",
        _ => "Last Known",
    }
}

/// A value as the TUI labels it.
pub(super) fn human_value(key: &str, v: Option<&Value>) -> String {
    let Some(v) = v else {
        return "Unavailable".into();
    };
    let unit = catalog::unit(key);
    match v {
        Value::Bool(b) => if *b { "On" } else { "Off" }.into(),
        Value::Integer(n) if key == keys::WHEEL_THRESHOLD && *n == SMARTSHIFT_OFF => "Off".into(),
        Value::Integer(n) if unit.is_empty() => n.to_string(),
        Value::Integer(n) => format!("{n} {unit}"),
        Value::Text(t) if model::key(key).is_some_and(|(k, _)| k.kind == keys::Kind::Enum) => {
            display(&catalog::choice_words(key, t))
        }
        Value::Text(t) => display(t),
        Value::Color(c) => format!("#{c:06x}"),
    }
}

/// An integer setting's range, such as "5-300, Steps of 5".
fn range_words(s: &p::Setting) -> Option<String> {
    let (min, max, step) = model::range(s)?;
    let range = format!("{min}-{max}");
    Some(if step > 1 {
        format!("{range}, Steps of {step}")
    } else {
        range
    })
}

/// A saved setting's state. With Logitech Features off a saved value is
/// only stored.
fn status_words(d: &p::Device, s: &p::Setting) -> (&'static str, Style) {
    match model::applied(s) {
        None => ("Not Saved", layout::plain()),
        Some(Ok(SettingState::Pending)) if !model::hidpp_enabled(d) => ("Saved", ok()),
        Some(Ok(SettingState::Pending)) => ("Pending", warn()),
        Some(Ok(SettingState::Applied)) => ("Applied", ok()),
        Some(Ok(SettingState::ChangedOnDevice)) => ("Changed on Device", warn()),
        Some(Ok(SettingState::Unsupported)) => ("Can't Apply Now", dim()),
        Some(Err(_)) => ("Failed", err()),
    }
}

/// The value the device keeps when nothing is staged: the saved value, else
/// the reading.
fn base(s: &p::Setting) -> Option<Value> {
    model::saved(s).or_else(|| model::current(s))
}

/// A staged change that would change something; None when it matches what
/// is saved, or forgets a value that isn't saved.
fn effective(s: &p::Setting, draft: Option<&Change>) -> Option<Change> {
    match draft? {
        Change::Forget if model::saved(s).is_some() => Some(Change::Forget),
        Change::Forget => None,
        Change::Set { value, .. } if !model::accepts(s, value) => None,
        Change::Set { value, policy } if !policy && Some(value) == base(s).as_ref() => None,
        change => Some(change.clone()),
    }
}

/// An integer setting's range for stepping: its limits, else no bounds.
#[derive(Clone, Copy)]
struct Bounds {
    min: Option<i64>,
    max: Option<i64>,
    step: i64,
    base: i64,
}

fn bounds(s: &p::Setting) -> Bounds {
    match model::range(s) {
        Some((min, max, step)) => Bounds {
            min: Some(min),
            max: Some(max),
            step,
            base: min,
        },
        None => Bounds {
            min: None,
            max: None,
            step: 1,
            base: 0,
        },
    }
}

/// SmartShift's threshold range while it is On: its limits without the value
/// the device takes as Off.
fn smartshift_bounds(s: &p::Setting) -> Bounds {
    let mut b = bounds(s);
    b.max = Some(b.max.unwrap_or(SMARTSHIFT_OFF - 1).min(SMARTSHIFT_OFF - 1));
    b
}

/// Moves an integer by delta within its range, on its steps; an off-step
/// reading moves to the next step first.
fn step_value(b: Bounds, from: Option<&Value>, delta: i64) -> Value {
    let fine = b.step.max(1);
    let mut n = match from {
        Some(Value::Integer(n)) => {
            let off = (n - b.base).rem_euclid(fine);
            match () {
                _ if off == 0 => n + delta,
                _ if delta > 0 => n - off + delta,
                _ => n - off + delta + fine,
            }
        }
        _ if delta < 0 && b.max.is_some() => b.max.unwrap(),
        _ => b.base,
    };
    if let Some(min) = b.min {
        n = n.max(min);
    }
    if let Some(max) = b.max {
        n = n.min(max);
    }
    Value::Integer(n)
}

/// A button, or the same label disabled in its place.
fn button_if(b: &mut Layout, label: &str, action: Action, tone: Tone, enabled: bool) {
    if enabled {
        b.button(label, action, tone);
    } else {
        b.disabled(label);
    }
}

impl<B: Backend> Model<B> {
    /// Whether a saved device's settings page is shown.
    pub(super) fn settings_open(&self, st: &State) -> bool {
        !self.page.device.is_empty()
            && self.session.is_some()
            && Self::find(st, &self.page.device).0.is_some()
    }

    pub(super) fn open_settings(&mut self, id: String) {
        self.close_files();
        self.page.device = id;
        self.page.key = None;
        self.page.list_scroll = 0;
        self.page.editor_scroll = 0;
        self.page.load_err = None;
        self.page.job_note.clear();
        self.focus = None;
        let id = self.page.device.clone();
        self.load_settings(&id);
    }

    /// Leaves the settings page. Staged changes are kept.
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
        let prefix = format!("{id}/");
        self.page
            .setting_states
            .retain(|k, _| !k.starts_with(&prefix));
    }

    /// Whether a Save of the device's settings is running.
    pub(super) fn saving(&self, id: &str) -> bool {
        self.jobs.values().any(|j| {
            j.save.is_some()
                && matches!(&j.command, Command::SettingsSave { device, .. } if device == id)
        })
    }

    /// Leaves a page whose device was removed.
    pub(super) fn sync_settings(&mut self) {
        let Some(st) = self.state() else {
            return;
        };
        if !self.page.device.is_empty() && Self::find(&st, &self.page.device).0.is_none() {
            self.close_settings();
        }
    }

    /// Reads a device's settings list, unless a read is under way. Events
    /// keep it current afterwards.
    fn load_settings(&mut self, id: &str) {
        if !self.page.loading.insert(id.to_owned()) {
            return;
        }
        let mut job = Job::new(Command::Settings(id.to_owned()));
        job.load = true;
        self.execute_job(job);
    }

    fn draft(&self, id: &str, key: &str) -> Option<&Change> {
        self.page.drafts.get(id)?.get(key)
    }

    /// The value the controls show: the staged value, else the saved value,
    /// else a legal current reading. A reading that cannot be set, such as a
    /// temporary mode, is never offered as chosen.
    fn edit_value(&self, s: &p::Setting) -> Option<Value> {
        match self.draft(&self.page.device, &s.key) {
            Some(Change::Set { value, .. }) => return Some(value.clone()),
            Some(Change::Forget) => {}
            None => {
                if let Some(saved) = model::saved(s) {
                    return Some(saved);
                }
            }
        }
        model::current(s).filter(|v| model::accepts(s, v))
    }

    /// Stages a value from a control. Choosing what the device keeps anyway
    /// drops the draft, except that a staged save of an unsaved value stays
    /// one; nothing is sent until Save.
    fn set_draft(&mut self, s: &p::Setting, v: Value) {
        if !model::accepts(s, &v) {
            return;
        }
        let drafts = self
            .page
            .drafts
            .entry(self.page.device.clone())
            .or_default();
        let policy = model::saved(s).is_none()
            && matches!(drafts.get(&s.key), Some(Change::Set { policy: true, .. }));
        if !policy && Some(&v) == base(s).as_ref() {
            drafts.remove(&s.key);
        } else {
            drafts.insert(s.key.clone(), Change::Set { value: v, policy });
        }
    }

    /// The staged changes that would change something, in display order.
    fn changes(&self, id: &str, settings: &[p::Setting]) -> Vec<(String, Change)> {
        catalog::presented(settings)
            .into_iter()
            .filter_map(|s| Some((s.key.clone(), effective(s, self.draft(id, &s.key))?)))
            .collect()
    }

    /// The visible setting rows, in display order.
    fn visible_keys(&self, settings: &[p::Setting]) -> Vec<String> {
        let known = catalog::presented(settings);
        let mut keys = Vec::new();
        for category in catalog::categories(settings) {
            if self.page.collapsed.contains(category) {
                continue;
            }
            keys.extend(
                known
                    .iter()
                    .filter(|s| catalog::category(&s.key) == category)
                    .map(|s| s.key.clone()),
            );
        }
        keys
    }

    /// A setting row's tag: a staged change, else its saved state.
    fn row_tag(&self, d: &p::Device, s: &p::Setting) -> (&'static str, Style) {
        let staged = effective(s, self.draft(&d.id, &s.key)).is_some();
        match model::applied(s) {
            _ if staged && self.saving(&d.id) => ("◌ Sending", warn()),
            _ if staged => ("✎ Changed", layout::accent()),
            None => ("○ Not Saved", dim()),
            Some(Err(_)) => ("✕ Failed", err()),
            Some(Ok(SettingState::Unsupported)) => ("○ Can't Apply Now", dim()),
            Some(Ok(SettingState::ChangedOnDevice)) if catalog::fresh(d, s) => {
                ("◆ Changed on Device", warn())
            }
            Some(Ok(SettingState::Pending)) if model::hidpp_enabled(d) && connected(d) => {
                ("◌ Pending", warn())
            }
            _ => ("● Saved", ok()),
        }
    }

    pub(super) fn settings_pane(&mut self, st: &State, w: usize, h: usize) -> Layout {
        let d = Self::find(st, &self.page.device).0.unwrap().clone();
        let id = d.id.clone();
        let settings = st.settings_of(&id).to_vec();
        let loaded = st.settings.contains_key(&id);
        let mut b = Layout::new(w.saturating_sub(4));
        let mut pinned = Layout::new(w.saturating_sub(4));
        let (text, look) = device_status(&d);
        b.line(Line::from(vec![
            span(text, look),
            span(
                format!(" · Logitech Features {}", on_off(model::hidpp_enabled(&d))),
                dim(),
            ),
        ]));
        if let Some((t, l)) = hidpp_status(&d) {
            b.para(&t, l);
        }
        if let Some(e) = &self.page.load_err {
            b.para(
                &format!("✕ Couldn't Load Settings: {}", text::error_words(e)),
                err(),
            );
            b.button("Retry", Action::SettingsReload, Tone::Normal);
        } else if !loaded {
            b.line(styled(format!("{} Loading Settings…", spinner()), warn()));
        }
        let known = catalog::presented(&settings);
        if loaded && self.page.load_err.is_none() && known.is_empty() {
            b.para("No Settings", dim());
        }
        let busy = settings_busy(st, &d, self.saving(&id));
        let inner = b.width;
        let tag_w = if inner >= 64 { 20 } else { 14 };
        let value_w = (inner.saturating_sub(2 + tag_w) * 2 / 5).max(6);
        let label_w = inner.saturating_sub(2 + tag_w + value_w).max(4);
        let mut selected_line = None;
        for category in catalog::categories(&settings) {
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
                .filter(|s| catalog::category(&s.key) == category)
            {
                let (row_base, look, marker) = if Some(&s.key) == self.page.key.as_ref() {
                    selected_line = Some(b.lines.len());
                    (
                        layout::selected(),
                        inherit(layout::bold(), layout::selected()),
                        "▌ ",
                    )
                } else {
                    (layout::plain(), layout::plain(), "  ")
                };
                // The form value: what is staged, else saved, else read.
                let shown = match self.draft(&id, &s.key) {
                    Some(Change::Set { value, .. }) => Some(value.clone()),
                    _ => model::saved(s).or_else(|| model::current(s)),
                };
                let value = human_value(&s.key, shown.as_ref());
                let value_look =
                    if !busy.is_empty() || !catalog::fresh(&d, s) && model::saved(s).is_none() {
                        dim()
                    } else {
                        layout::plain()
                    };
                let (tag, tag_look) = self.row_tag(&d, s);
                let label = layout::truncate_str(&display(&catalog::label(&s.key)), label_w - 1);
                let mut row: Styled = Line::from(vec![
                    span(marker, inherit(layout::accent(), row_base)),
                    span(pad_str(&label, label_w), look),
                    span(
                        pad_str(&layout::truncate_str(&value, value_w - 1), value_w),
                        inherit(value_look, row_base),
                    ),
                    span(
                        layout::truncate_str(tag, tag_w),
                        inherit(tag_look, row_base),
                    ),
                ]);
                let fill = inner.saturating_sub(layout::line_width(&row));
                row.spans.push(span(" ".repeat(fill), row_base));
                b.control(row, Action::Setting(s.key.clone()));
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
        if !busy.is_empty() {
            pinned.para(&format!("{} {busy}", spinner()), warn());
        } else if !self.page.job_note.is_empty() {
            pinned.para(&self.page.job_note.clone(), self.page.job_look);
        }
        pinned.row();
        let idle = busy.is_empty();
        let can_save = idle && !self.changes(&id, &settings).is_empty();
        button_if(
            &mut pinned,
            "Save",
            Action::SaveAll,
            Tone::Primary,
            can_save,
        );
        let staged = self.page.drafts.get(&id).is_some_and(|d| !d.is_empty());
        button_if(
            &mut pinned,
            "Discard",
            Action::Discard,
            Tone::Normal,
            idle && staged,
        );
        let can = idle && connected(&d) && loaded;
        button_if(
            &mut pinned,
            "Refresh",
            Action::SettingsRefresh,
            Tone::Normal,
            can,
        );
        pinned.button_right("‹ Back", Action::SettingsBack, Tone::Normal);
        let title = format!("Settings · {}", display_name(Some(&d.name)));
        self.frame(&title, b, pinned, Some(Area::SetList), false, w, h)
    }

    pub(super) fn editor_pane(&mut self, st: &State, w: usize, h: usize) -> Layout {
        let (title, b, actions) = self.editor(st, w);
        self.frame(&title, b, actions, Some(Area::Editor), false, w, h)
    }

    /// The selected setting's card, value controls and staging actions.
    pub(super) fn editor(&self, st: &State, w: usize) -> (String, Layout, Layout) {
        let mut b = Layout::new(w.saturating_sub(4));
        let mut actions = Layout::new(w.saturating_sub(4));
        let d = Self::find(st, &self.page.device).0.unwrap();
        let id = &d.id;
        let Some(s) = self
            .page
            .key
            .as_ref()
            .and_then(|k| st.settings_of(id).iter().find(|s| s.key == *k))
        else {
            b.para("No Setting Selected", dim());
            return ("Setting".into(), b, actions);
        };
        let field = |b: &mut Layout, key: &str, value: &str, st: Style| {
            b.field_at(key, EDITOR_KEY, value, st);
        };
        let is_fresh = catalog::fresh(d, s);
        let look = if is_fresh { layout::plain() } else { dim() };
        let current = model::current(s);
        field(
            &mut b,
            "Current",
            &format!(
                "{} · {}",
                human_value(&s.key, current.as_ref()),
                fresh_words(d, s)
            ),
            look,
        );
        let title = display(&catalog::label(&s.key));
        match model::saved(s) {
            Some(saved) => field(
                &mut b,
                "Saved",
                &human_value(&s.key, Some(&saved)),
                layout::plain(),
            ),
            None => field(&mut b, "Saved", "Not Saved", dim()),
        }
        if let Some(range) = range_words(s) {
            field(&mut b, "Range", &range, layout::plain());
        }
        if model::saved(s).is_some() {
            let (text, look) = status_words(d, s);
            field(&mut b, "Status", text, look);
        }
        if let Some(Err(code)) = model::applied(s) {
            field(&mut b, "Error", &text::hidpp_words(code), err());
        }
        b.row();
        // Controls stay in place while unavailable, dim and without targets.
        let busy = settings_busy(st, d, self.saving(id));
        let locked = !busy.is_empty();
        let (first_line, first_hit) = (b.lines.len(), b.hits.len());
        let value = self.edit_value(s);
        let choice = |label: String, action: Action, chosen: bool| Choice {
            label,
            action,
            chosen,
        };
        let choices = model::choices(s);
        match model::kind(s) {
            Some(Type::Bool) => {
                let on = match value {
                    Some(Value::Bool(b)) => Some(b),
                    _ => None,
                };
                let options = layout::on_off(
                    on,
                    Action::Draft(s.key.clone(), Value::Bool(true)),
                    Action::Draft(s.key.clone(), Value::Bool(false)),
                );
                b.choice("Value", EDITOR_KEY, options);
            }
            _ if !choices.is_empty() => {
                let options = choices
                    .iter()
                    .map(|v| {
                        choice(
                            human_value(&s.key, Some(v)),
                            Action::Draft(s.key.clone(), v.clone()),
                            value.as_ref() == Some(v),
                        )
                    })
                    .collect();
                b.choice("Value", EDITOR_KEY, options);
            }
            Some(Type::Integer) if s.key == keys::WHEEL_THRESHOLD => {
                let n = match value {
                    Some(Value::Integer(n)) => Some(n),
                    _ => None,
                };
                let on = n.is_some_and(|n| n != SMARTSHIFT_OFF);
                let options = layout::on_off(
                    n.map(|_| on),
                    Action::Switch(s.key.clone(), smartshift_on(s, value.as_ref())),
                    Action::Switch(s.key.clone(), Value::Integer(SMARTSHIFT_OFF)),
                );
                b.choice("Value", EDITOR_KEY, options);
                if on {
                    stepper(&mut b, "Threshold", s, smartshift_bounds(s), value.as_ref());
                }
            }
            Some(Type::Integer) => stepper(&mut b, "Value", s, bounds(s), value.as_ref()),
            _ => field(
                &mut b,
                "Value",
                &human_value(&s.key, current.as_ref()),
                look,
            ),
        }
        if locked {
            b.hits.truncate(first_hit);
            for line in &mut b.lines[first_line..] {
                *line = styled(layout::strip(line), dim());
            }
        }
        // The staging actions: each stages a change, and nothing is sent until Save.
        let staging = busy.is_empty();
        actions.row();
        let staged = effective(s, self.draft(id, &s.key)).is_some();
        let settable = is_fresh && current.as_ref().is_some_and(|v| model::accepts(s, v));
        if staged {
            button_if(
                &mut actions,
                "Undo Change",
                Action::Undo(s.key.clone()),
                Tone::Normal,
                staging,
            );
        } else if model::saved(s).is_none() {
            button_if(
                &mut actions,
                "Save Current Value",
                Action::Keep(s.key.clone()),
                Tone::Normal,
                staging && settable,
            );
        } else {
            if model::applied(s) == Some(Ok(SettingState::ChangedOnDevice)) {
                button_if(
                    &mut actions,
                    "Save Device Value",
                    Action::Keep(s.key.clone()),
                    Tone::Normal,
                    staging && settable,
                );
            }
            button_if(
                &mut actions,
                "Forget Saved Value",
                Action::Forget(s.key.clone()),
                Tone::Normal,
                staging,
            );
        }
        (title, b, actions)
    }

    /// Handles the page's controls. Every mouse and key path passes here, so
    /// the drawn guards hold for all of them.
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
        let id = d.id.clone();
        let settings = st.settings_of(&id).to_vec();
        let setting = |key: &str| settings.iter().find(|s| s.key == key).cloned();
        let busy = !settings_busy(&st, &d, self.saving(&id)).is_empty();
        match action {
            Action::SettingsBack => self.close_settings(),
            Action::SettingsReload => {
                self.page.load_err = None;
                self.load_settings(&id);
            }
            Action::SettingsRefresh => {
                if busy || !connected(&d) {
                    return;
                }
                self.page.job_note.clear();
                self.execute(Command::Refresh(id));
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
            // Nothing changes while the device's settings work runs.
            Action::Draft(..)
            | Action::Switch(..)
            | Action::Step(..)
            | Action::Keep(_)
            | Action::Undo(_)
            | Action::Forget(_)
            | Action::Discard
            | Action::SaveAll
                if busy => {}
            Action::Draft(key, v) | Action::Switch(key, v) => {
                if let Some(s) = setting(&key) {
                    self.set_draft(&s, v);
                }
            }
            Action::Step(key, delta) => {
                if let Some(s) = setting(&key) {
                    let from = self.edit_value(&s);
                    let range = if key == keys::WHEEL_THRESHOLD {
                        smartshift_bounds(&s)
                    } else {
                        bounds(&s)
                    };
                    self.set_draft(&s, step_value(range, from.as_ref(), delta));
                }
            }
            Action::Keep(key) => {
                if let Some(s) = setting(&key)
                    && catalog::fresh(&d, &s)
                    && let Some(v) = model::current(&s).filter(|v| model::accepts(&s, v))
                {
                    let change = Change::Set {
                        value: v,
                        policy: true,
                    };
                    self.page.drafts.entry(id).or_default().insert(key, change);
                }
            }
            Action::Forget(key) => {
                if setting(&key).is_some_and(|s| model::saved(&s).is_some()) {
                    self.page
                        .drafts
                        .entry(id)
                        .or_default()
                        .insert(key, Change::Forget);
                }
            }
            Action::Undo(key) => {
                if let Some(drafts) = self.page.drafts.get_mut(&id) {
                    drafts.remove(&key);
                }
            }
            Action::Discard => {
                if let Some(drafts) = self.page.drafts.get_mut(&id) {
                    drafts.clear();
                }
            }
            Action::SaveAll => {
                let changes = self.changes(&id, &settings);
                if changes.is_empty() {
                    return;
                }
                let mut set = Vec::new();
                let mut forget = Vec::new();
                for (key, change) in &changes {
                    match change {
                        Change::Set { value, .. } => set.push((key.clone(), value.clone())),
                        Change::Forget => forget.push(key.clone()),
                    }
                }
                self.page.job_note.clear();
                let mut job = Job::new(Command::SettingsSave {
                    device: id,
                    set,
                    forget,
                });
                job.save = Some(changes.into_iter().map(|(k, _)| k).collect());
                self.execute_job(job);
            }
            _ => {}
        }
    }

    /// Records a settings command's outcome on the page: a stored Save drops
    /// the drafts it sent, unless they were edited since.
    pub(super) fn settings_result(&mut self, job: &Job, result: &Result<Outcome, Error>) {
        let Command::SettingsSave {
            device,
            set,
            forget,
            ..
        } = &job.command
        else {
            return;
        };
        match result {
            Ok(_) => {
                if let Some(drafts) = self.page.drafts.get_mut(device) {
                    for (key, value) in set {
                        if matches!(drafts.get(key), Some(Change::Set { value: v, .. }) if v == value)
                        {
                            drafts.remove(key);
                        }
                    }
                    for key in forget {
                        if drafts.get(key) == Some(&Change::Forget) {
                            drafts.remove(key);
                        }
                    }
                }
                if *device == self.page.device {
                    self.page.job_note.clear();
                }
            }
            Err(e) => {
                let words = text::error_words(e);
                let name = self.label(device);
                self.note(
                    super::activity::Kind::Bad,
                    format!("Couldn't save the settings of {name}: {words}"),
                );
                if *device == self.page.device {
                    self.page.job_note = format!("✕ Couldn't Save: {words}");
                    self.page.job_look = err();
                }
            }
        }
    }

    /// Selects the next or previous visible setting row.
    pub(super) fn move_setting(&mut self, delta: isize) {
        let Some(st) = self.state() else {
            return;
        };
        let keys = self.visible_keys(st.settings_of(&self.page.device));
        if keys.is_empty() {
            return;
        }
        let i = match self
            .page
            .key
            .as_ref()
            .and_then(|k| keys.iter().position(|x| x == k))
        {
            None if delta < 0 => keys.len() - 1,
            None => 0,
            Some(i) => i.saturating_add_signed(delta).min(keys.len() - 1),
        };
        self.page.key = Some(keys[i].clone());
        self.page.editor_scroll = 0;
        self.page.reveal = true;
        self.focus = None;
    }

    /// Left and Right edit the selected setting: the previous or next choice,
    /// or a fine step. Space switches between On and Off.
    pub(super) fn edit_selected(&mut self, delta: i64, toggle: bool) {
        let Some(st) = self.state() else {
            return;
        };
        let Some(s) = self
            .page
            .key
            .as_ref()
            .and_then(|k| {
                st.settings_of(&self.page.device)
                    .iter()
                    .find(|s| s.key == *k)
            })
            .cloned()
        else {
            return;
        };
        let value = self.edit_value(&s);
        let choices = model::choices(&s);
        let key = s.key.clone();
        let action = match model::kind(&s) {
            Some(Type::Bool) => {
                let next = match (value, toggle) {
                    (Some(Value::Bool(b)), true) => !b,
                    (_, true) => true,
                    // On comes first: Left chooses On and Right Off.
                    (_, false) => delta < 0,
                };
                Action::Draft(key, Value::Bool(next))
            }
            _ if key == keys::WHEEL_THRESHOLD && toggle => {
                let on = matches!(value, Some(Value::Integer(n)) if n != SMARTSHIFT_OFF);
                let next = if on {
                    Value::Integer(SMARTSHIFT_OFF)
                } else {
                    smartshift_on(&s, value.as_ref())
                };
                Action::Switch(key, next)
            }
            _ if toggle => return,
            _ if !choices.is_empty() => {
                let n = choices.len() as i64;
                let at = choices.iter().position(|c| Some(c) == value.as_ref());
                let i = match at {
                    Some(i) => (i as i64 + delta).clamp(0, n - 1),
                    None if delta < 0 => n - 1,
                    None => 0,
                };
                Action::Draft(key, choices[i as usize].clone())
            }
            Some(Type::Integer) => {
                if key == keys::WHEEL_THRESHOLD
                    && !matches!(value, Some(Value::Integer(n)) if n != SMARTSHIFT_OFF)
                {
                    return;
                }
                let (fine, _) = steps(&s);
                Action::Step(key, delta.signum() * fine)
            }
            _ => return,
        };
        self.action(action);
    }

    /// Backspace drops the selected setting's staged change.
    pub(super) fn undo_selected(&mut self) {
        if let Some(key) = self.page.key.clone() {
            self.action(Action::Undo(key));
        }
    }
}

/// The threshold SmartShift turns On with: the one shown, else the saved or
/// read one, else the highest.
fn smartshift_on(s: &p::Setting, value: Option<&Value>) -> Value {
    [value.cloned(), model::saved(s), model::current(s)]
        .into_iter()
        .flatten()
        .find(|v| matches!(v, Value::Integer(k) if (1..SMARTSHIFT_OFF).contains(k)))
        .unwrap_or(Value::Integer(SMARTSHIFT_OFF - 1))
}

/// Fine and coarse increments around an integer value, and the range ends.
fn stepper(b: &mut Layout, key: &str, s: &p::Setting, bounds: Bounds, value: Option<&Value>) {
    let (fine, mut coarse) = steps(s);
    if let (Some(min), Some(max)) = (bounds.min, bounds.max)
        && (max - min) / fine <= 20
    {
        coarse = 0;
    }
    let n = match value {
        Some(Value::Integer(n)) => Some(*n),
        _ => None,
    };
    let at_min = n.is_some_and(|n| bounds.min.is_some_and(|m| n <= m));
    let at_max = n.is_some_and(|n| bounds.max.is_some_and(|m| n >= m));
    b.line(styled(pad_str(key, EDITOR_KEY - 1), dim())); // Buttons add a space.
    let control = |b: &mut Layout, label: String, delta: i64, enabled: bool| {
        button_if(
            b,
            &label,
            Action::Step(s.key.clone(), delta),
            Tone::Normal,
            enabled,
        );
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
    let unit = catalog::unit(&s.key);
    if !unit.is_empty() {
        b.label(unit, dim());
    }
    if let (Some(min), Some(max)) = (bounds.min, bounds.max) {
        b.line(Line::from(pad_str("", EDITOR_KEY - 1)));
        let draft = |n| Action::Draft(s.key.clone(), Value::Integer(n));
        button_if(b, &format!("Min {min}"), draft(min), Tone::Normal, !at_min);
        button_if(b, &format!("Max {max}"), draft(max), Tone::Normal, !at_max);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::catalog::tests::{boolean, integer};

    #[test]
    fn steps_snap_to_the_range() {
        let s = integer(keys::BACKLIGHT_DELAY_POWERED, 5, 300, 5);
        assert_eq!(steps(&s), (5, 50));
        let b = bounds(&s);
        assert_eq!(
            step_value(b, Some(&Value::Integer(12)), 5),
            Value::Integer(15)
        );
        assert_eq!(
            step_value(b, Some(&Value::Integer(12)), -5),
            Value::Integer(10)
        );
        assert_eq!(
            step_value(b, Some(&Value::Integer(10)), -50),
            Value::Integer(5)
        );
        assert_eq!(step_value(b, None, -5), Value::Integer(300));
        assert_eq!(step_value(b, None, 5), Value::Integer(5));
    }

    #[test]
    fn staged_changes_compare_with_what_the_device_keeps() {
        let mut s = boolean(keys::WHEEL_INVERT);
        let set_type = |s: &mut p::Setting, value: Option<bool>, saved: Option<bool>| {
            s.r#type = Some(p::setting::Type::Bool(p::BoolSetting { value, saved }));
        };
        set_type(&mut s, Some(false), None);
        let set = |v: bool, policy| Change::Set {
            value: Value::Bool(v),
            policy,
        };
        // Unsaved: choosing the reading is no change, but saving it is.
        assert_eq!(effective(&s, Some(&set(false, false))), None);
        assert_eq!(
            effective(&s, Some(&set(false, true))),
            Some(set(false, true))
        );
        assert_eq!(effective(&s, Some(&Change::Forget)), None);
        // Saved: the saved value is what counts, and forgetting is a change.
        set_type(&mut s, Some(false), Some(true));
        s.status = Some(p::setting::Status::State(SettingState::Applied as i32));
        assert_eq!(effective(&s, Some(&set(true, false))), None);
        assert_eq!(
            effective(&s, Some(&set(false, false))),
            Some(set(false, false))
        );
        assert_eq!(effective(&s, Some(&Change::Forget)), Some(Change::Forget));
        // A value the setting doesn't take is never sent.
        let wrong = Change::Set {
            value: Value::Integer(1),
            policy: true,
        };
        assert_eq!(effective(&s, Some(&wrong)), None);
    }
}
