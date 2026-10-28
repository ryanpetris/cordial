//! A device's Settings tab: its gamepad output, its settings grouped by category, each with the
//! control its type calls for and a marker for its saved state, and the readings that belong beside
//! them. Every control stages a change; Save sends every staged setting of the device in one
//! request, then a staged gamepad output in another.
use super::{
    Action, App, Job, Kind, Menu, Spot, Submission, Tab, fleet::Fleet, words, world::DeviceView,
};
use crate::{
    controller::{Command, Outcome, Target},
    error::Error,
    model,
    settings::{self, Field, Leaf, Setting, Type, Value},
    ui::catalog,
};
use cordial_protocol::{self as p, ErrorOutcome, OutputMode, SettingState};
use std::collections::BTreeMap;

use super::Tone;
/// The automatic free spin threshold that turns automatic switching off.
pub const FREE_SPIN_OFF: i64 = 255;

/// A staged change to one setting.
#[derive(Clone, Debug, PartialEq)]
pub enum Draft {
    /// A value to save. `policy` marks Persist Current Value and Save Device Value, which are
    /// changes even when they match the value shown.
    Set {
        value: Value,
        policy: bool,
    },
    /// A typed number, parsed when saving.
    Typed(String),
    Forget,
}

/// What a staged change sends, once checked.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Change {
    Set(Value),
    Forget,
}

/// The value the device keeps when nothing is staged: the saved value, else the reading.
pub(crate) fn base(s: &Setting) -> Option<Value> {
    s.saved.or(s.value)
}

/// Whether the setting is the automatic free spin threshold, shown as a switch with its threshold
/// beside it.
pub fn is_free_spin(s: &Setting) -> bool {
    s.field.leaf == Leaf::FreeSpinThreshold
        && settings::kind(s) == Type::Integer
        && settings::choices(s).is_empty()
}

/// An integer setting's range for stepping and checking.
#[derive(Clone, Copy)]
pub(crate) struct Bounds {
    min: Option<i64>,
    max: Option<i64>,
    step: i64,
    base: i64,
}

pub(crate) fn bounds(s: &Setting) -> Bounds {
    let mut b = match settings::range(s) {
        Some((min, max, step)) => Bounds {
            min: Some(min),
            max: Some(max),
            step: step.max(1),
            base: min,
        },
        None => Bounds {
            min: None,
            max: None,
            step: 1,
            base: 0,
        },
    };
    if is_free_spin(s) {
        b.min = Some(b.min.unwrap_or(1).max(1));
        b.max = Some(b.max.unwrap_or(FREE_SPIN_OFF - 1).min(FREE_SPIN_OFF - 1));
    }
    b
}

/// Moves an integer by one step in `delta`'s direction within its range, on its steps. The
/// arithmetic is wide, so a range at the limits of i64 can't overflow.
pub(crate) fn step_value(b: Bounds, from: Option<&Value>, delta: i64) -> Value {
    let fine = i128::from(b.step.max(1));
    let base = i128::from(b.base);
    let start = match from {
        Some(Value::Integer(n)) => Some(i128::from(*n)),
        _ => None,
    };
    let mut n: i128 = match from {
        Some(Value::Integer(n)) => {
            let n = i128::from(*n);
            let off = (n - base).rem_euclid(fine);
            match () {
                _ if off == 0 => n + i128::from(delta.signum()) * fine,
                _ if delta > 0 => n - off + fine,
                _ => n - off,
            }
        }
        _ if delta < 0 && b.max.is_some() => i128::from(b.max.unwrap_or(0)),
        _ => i128::from(b.min.unwrap_or(b.base)),
    };
    if let Some(max) = b.max {
        // The highest value on the steps, which the maximum itself may not be; a reading above
        // it is never lowered by stepping up.
        let max = i128::from(max);
        let top = max - (max - base).rem_euclid(fine);
        let top = start
            .filter(|v| *v > top && delta > 0)
            .map_or(top, |v| v.min(max));
        n = n.min(top);
    }
    if let Some(min) = b.min {
        n = n.max(i128::from(min));
    }
    Value::Integer(n.clamp(i128::from(i64::MIN), i128::from(i64::MAX)) as i64)
}

/// A typed number as the setting would save it, or None when it isn't acceptable.
pub(crate) fn parse_typed(s: &Setting, typed: &str) -> Option<Value> {
    let n: i64 = typed.trim().parse().ok()?;
    let b = bounds(s);
    if b.min.is_some_and(|min| n < min) || b.max.is_some_and(|max| n > max) {
        return None;
    }
    if (i128::from(n) - i128::from(b.base)).rem_euclid(i128::from(b.step)) != 0 {
        return None;
    }
    Some(Value::Integer(n))
}

/// The change a draft makes; Err for a draft that can't be saved, None for no change.
pub(crate) fn change_of(s: &Setting, draft: Option<&Draft>) -> Result<Option<Change>, ()> {
    match draft {
        None => Ok(None),
        Some(Draft::Forget) => Ok(s.saved.is_some().then_some(Change::Forget)),
        Some(Draft::Typed(t)) => {
            let v = parse_typed(s, t).ok_or(())?;
            Ok((Some(v) != base(s)).then_some(Change::Set(v)))
        }
        Some(Draft::Set { value, policy }) => {
            if !settings::accepts(s, value) {
                return Err(());
            }
            Ok((*policy || Some(*value) != base(s)).then_some(Change::Set(*value)))
        }
    }
}

/// The range a typed number must be in, as words.
pub fn range_text(s: &Setting) -> String {
    let b = bounds(s);
    let mut text = match (b.min, b.max) {
        (Some(min), Some(max)) => format!("{min}-{max}"),
        (Some(min), None) => format!("At Least {min}"),
        (None, Some(max)) => format!("At Most {max}"),
        (None, None) => "Whole Numbers".into(),
    };
    if b.step > 1 {
        text.push_str(&format!(", Steps of {}", b.step));
    }
    text
}

/// A value in words.
pub fn value_text(leaf: Leaf, v: Option<&Value>) -> String {
    match v {
        None => "Unknown".into(),
        Some(Value::Bool(b)) => if *b { "On" } else { "Off" }.into(),
        Some(Value::Integer(0)) if leaf == Leaf::AutoOff => "Never".into(),
        Some(Value::Integer(FREE_SPIN_OFF)) if leaf == Leaf::FreeSpinThreshold => "Off".into(),
        Some(Value::Integer(n)) => n.to_string(),
        Some(Value::Choice(n)) => catalog::choice_words(leaf, *n),
    }
}

/// The unit shown beside a value; none while it is unknown or when it reads "Never".
pub fn display_unit(leaf: Leaf, v: Option<&Value>) -> &'static str {
    match v {
        None => "",
        Some(Value::Integer(0)) if leaf == Leaf::AutoOff => "",
        _ => catalog::unit(leaf),
    }
}

/// A value in words with its unit.
pub fn unit_text(leaf: Leaf, v: Option<&Value>) -> String {
    let text = value_text(leaf, v);
    match display_unit(leaf, v) {
        "" => text,
        unit => format!("{text} {unit}"),
    }
}

/// The device's readings of behavior it doesn't accept writes for, other than the wheel's
/// figures.
pub fn readings(d: &p::Device) -> Vec<(Field, Value)> {
    const FIGURES: [Leaf; 3] = [
        Leaf::ResolutionMultiplier,
        Leaf::RatchetsPerRotation,
        Leaf::Diameter,
    ];
    d.information
        .as_ref()
        .map(settings::readings)
        .unwrap_or_default()
        .into_iter()
        .filter(|(f, _)| !FIGURES.contains(&f.leaf))
        .collect()
}

/// The marker of a setting's saved state, and the menu it opens.
pub struct Marker {
    pub text: &'static str,
    pub tone: Tone,
    pub items: Vec<(&'static str, Option<Action>)>,
}

impl<F: Fleet> App<F> {
    pub fn drafts_of(&self, d: &DeviceView) -> Option<&BTreeMap<Field, Draft>> {
        self.setting_drafts.get(&(d.adapter.clone(), d.d.id))
    }
    pub fn draft_of(&self, d: &DeviceView, field: &Field) -> Option<&Draft> {
        self.drafts_of(d)?.get(field)
    }
    pub fn settings_of(&self, d: &DeviceView) -> Vec<Setting> {
        self.state_of(&d.adapter)
            .map(|st| {
                catalog::presented(&st.settings_of(d.d.id))
                    .into_iter()
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }
    pub fn setting(&self, d: &DeviceView, field: &Field) -> Option<Setting> {
        self.settings_of(d).into_iter().find(|s| s.field == *field)
    }
    /// The device's settings are current: it is connected and its Logitech Features are up.
    pub fn settings_current(d: &DeviceView) -> bool {
        d.connected() && model::hidpp_up(&d.d) == model::Up::Active
    }
    pub fn refreshing(&self, d: &DeviceView) -> bool {
        let id = d.d.id;
        self.running(
            &d.adapter,
            |k| matches!(k, Kind::Device(device, Spot::SettingsNote(..)) if *device == id),
        )
    }
    /// Whether the settings form takes no changes now.
    pub fn form_busy(&self, d: &DeviceView) -> bool {
        !self.adapter_ready(d)
            || self.device_saving(d, Tab::Settings)
            || self.refreshing(d)
            || self.settings_busy(d)
    }
    /// The gamepad output modes the Settings tab offers the device; none when it has no Gamepad
    /// Output to choose or no choice differs from the saved one.
    pub fn gamepad_choices(&self, d: &DeviceView) -> Vec<OutputMode> {
        // The adapter's last known status, so the control stays while the adapter reconnects.
        let choices = self
            .adapter(&d.adapter)
            .and_then(|a| a.status.as_ref())
            .filter(|_| model::gamepad(&d.d))
            .map(|status| model::gamepad_choices(status, &d.d))
            .unwrap_or_default();
        let saved = model::gamepad_mode(&d.d);
        if choices.iter().any(|m| Some(*m) != saved) {
            choices
        } else {
            Vec::new()
        }
    }
    /// Whether the device's adapter is connected and ready, so it can save settings.
    pub fn adapter_ready(&self, d: &DeviceView) -> bool {
        d.available
            && self
                .adapter(&d.adapter)
                .is_some_and(|a| a.connected() && a.ready)
    }
    /// Whether the device can be asked for its current settings.
    /// A record that is only last known leaves that to the adapter.
    pub fn can_refresh(&self, d: &DeviceView) -> bool {
        !self.form_busy(d) && (d.connected() || d.read_error.is_some())
    }
    pub fn submitting(&self, d: &DeviceView) -> bool {
        self.submissions
            .get(&(d.adapter.clone(), d.d.id))
            .is_some_and(|s| s.running)
    }
    /// The value a setting's control shows.
    pub fn shown_value(&self, d: &DeviceView, s: &Setting) -> Option<Value> {
        match self.draft_of(d, &s.field) {
            Some(Draft::Set { value, .. }) => Some(*value),
            Some(Draft::Typed(t)) => parse_typed(s, t).or_else(|| base(s)),
            Some(Draft::Forget) => s.value,
            None => base(s),
        }
    }
    pub(crate) fn set_draft(&mut self, d: &DeviceView, s: &Setting, draft: Option<Draft>) {
        let key = (d.adapter.clone(), d.d.id);
        self.edit_immediate_setting(&key, s.field);
        let drafts = self.setting_drafts.entry(key.clone()).or_default();
        match draft {
            Some(draft) => {
                drafts.insert(s.field, draft);
            }
            None => {
                drafts.remove(&s.field);
            }
        }
        if drafts.is_empty() {
            self.setting_drafts.remove(&key);
        }
        self.notes.remove(&Spot::SettingsNote(key.0, key.1));
    }
    /// Stages a value. Immediate editing retains an explicit choice even when an older
    /// readback matches it; a running write may still change that value.
    pub(crate) fn edit(&mut self, d: &DeviceView, s: &Setting, value: Value) {
        let keep_policy = self.immediate.is_some()
            || (matches!(
                self.draft_of(d, &s.field),
                Some(Draft::Set { policy: true, .. })
            ) && s.saved.is_none());
        let draft = if Some(value) == base(s) && !keep_policy {
            None
        } else {
            Some(Draft::Set {
                value,
                policy: keep_policy,
            })
        };
        self.set_draft(d, s, draft);
    }
    fn setting_change(
        &self,
        setting: &Setting,
        draft: Option<&Draft>,
    ) -> Result<Option<Change>, ()> {
        if self.immediate.is_some() && matches!(draft, Some(Draft::Forget)) {
            Ok(Some(Change::Forget))
        } else {
            change_of(setting, draft)
        }
    }
    /// The staged changes and whether any is invalid.
    pub(crate) fn setting_changes(&self, d: &DeviceView) -> (Vec<(Field, Change)>, bool) {
        let mut out = Vec::new();
        let mut invalid = false;
        let Some(drafts) = self.drafts_of(d) else {
            return (out, false);
        };
        for s in self.settings_of(d) {
            match self.setting_change(&s, drafts.get(&s.field)) {
                Ok(Some(c)) => out.push((s.field, c)),
                Ok(None) => {}
                Err(()) => invalid = true,
            }
        }
        (out, invalid)
    }
    pub fn settings_dirty(&self, d: &DeviceView) -> bool {
        let (changes, invalid) = self.setting_changes(d);
        invalid || !changes.is_empty()
    }
    /// Handles a settings action; false when `action` isn't one.
    pub(crate) fn settings_action(&mut self, adapter: &str, id: u32, action: &Action) -> bool {
        let Some(d) = self.device(adapter, id).cloned() else {
            return false;
        };
        let busy = !self.settings_editable(&d);
        let key = (adapter.to_owned(), id);
        match action {
            Action::GamepadOutput(mode) if !busy => {
                let mode = *mode;
                self.stage_device(&d, Tab::Settings, |u| u.gamepad = Some(mode));
            }
            Action::SettingBool(k, v) if !busy => {
                if let Some(s) = self.setting(&d, k) {
                    self.edit(&d, &s, Value::Bool(*v));
                }
            }
            Action::SettingChoice(k, v) if !busy => {
                self.menu = None;
                if let Some(s) = self.setting(&d, k) {
                    self.edit(&d, &s, *v);
                }
            }
            Action::SettingSelect(k) if !busy => {
                let Some(s) = self.setting(&d, k) else {
                    return true;
                };
                let items = settings::choices(&s)
                    .into_iter()
                    .map(|v| {
                        let label = unit_text(s.field.leaf, Some(&v));
                        (label, Some(Action::SettingChoice(*k, v)))
                    })
                    .collect();
                self.menu = Some(Menu { items });
            }
            Action::SettingStep(k, delta) if !busy => {
                if let Some(s) = self.setting(&d, k) {
                    self.commit_editing();
                    let from = self.shown_value(&d, &s);
                    let v = step_value(bounds(&s), from.as_ref(), *delta);
                    self.edit(&d, &s, v);
                }
            }
            Action::SettingEdit(k) if !busy => {
                if let Some(s) = self.setting(&d, k) {
                    let shown = self.shown_value(&d, &s);
                    let text = match self.draft_of(&d, k) {
                        Some(Draft::Typed(t)) => t.clone(),
                        _ => match shown {
                            Some(Value::Integer(n)) => n.to_string(),
                            _ => String::new(),
                        },
                    };
                    self.form.limit = 12;
                    self.form.set_value(&text);
                    self.editing = Some(*k);
                }
            }
            Action::FreeSpin(k, on) if !busy => {
                if let Some(s) = self.setting(&d, k) {
                    let v = if *on {
                        let keep = [s.saved, s.value].into_iter().flatten().find(
                            |v| matches!(v, Value::Integer(n) if (1..FREE_SPIN_OFF).contains(n)),
                        );
                        keep.unwrap_or(Value::Integer(FREE_SPIN_OFF - 1))
                    } else {
                        Value::Integer(FREE_SPIN_OFF)
                    };
                    self.edit(&d, &s, v);
                }
            }
            Action::Marker(k) => {
                let Some(s) = self.setting(&d, k) else {
                    return true;
                };
                let marker = self.marker(&d, &s, busy);
                let items = marker
                    .items
                    .into_iter()
                    .map(|(label, action)| (label.to_owned(), action))
                    .collect();
                self.menu = Some(Menu { items });
            }
            Action::Undo(k) => {
                self.menu = None;
                if let Some(s) = self.setting(&d, k) {
                    self.set_draft(&d, &s, None);
                }
            }
            Action::Keep(k) if !busy => {
                self.menu = None;
                if let Some(s) = self.setting(&d, k)
                    && let Some(v) = s.value
                {
                    self.set_draft(
                        &d,
                        &s,
                        Some(Draft::Set {
                            value: v,
                            policy: true,
                        }),
                    );
                }
            }
            Action::ForgetSetting(k) if !busy => {
                self.menu = None;
                if let Some(s) = self.setting(&d, k) {
                    self.set_draft(&d, &s, Some(Draft::Forget));
                }
            }
            Action::SettingsRefresh if self.can_refresh(&d) => {
                let spot = Spot::SettingsNote(key.0.clone(), key.1);
                self.notes.remove(&spot);
                self.run(
                    adapter,
                    Kind::Device(id, spot),
                    Command::Refresh(Target::Id(id)),
                );
            }
            Action::SettingsReload => {
                if !self.running(adapter, |k| matches!(k, Kind::SettingsList(x) if *x == id)) {
                    self.list_settings(adapter, id);
                }
            }
            Action::SettingsReapply if !busy => {
                let sent: Vec<(Field, Draft)> = self
                    .settings_of(&d)
                    .into_iter()
                    .filter(|s| {
                        matches!(&s.status, Some(Err(e)) if !model::unsupported(e))
                            && self.draft_of(&d, &s.field).is_none()
                    })
                    .filter_map(|s| {
                        let v = s.saved?;
                        Some((
                            s.field,
                            Draft::Set {
                                value: v,
                                policy: true,
                            },
                        ))
                    })
                    .collect();
                if self.immediate.is_some() {
                    for (field, draft) in sent {
                        if let Some(setting) = self.setting(&d, &field) {
                            self.set_draft(&d, &setting, Some(draft));
                        }
                    }
                } else {
                    self.send_settings(&d, sent, false);
                }
            }
            Action::SettingsDiscard
                if !self.submitting(&d) && !self.device_saving(&d, Tab::Settings) =>
            {
                self.setting_drafts.remove(&key);
                if let Some(draft) = self.device_drafts.get_mut(&key) {
                    draft.gamepad = None;
                    if draft.is_empty() {
                        self.device_drafts.remove(&key);
                    }
                }
                self.editing = None;
                self.notes.remove(&Spot::SettingsNote(key.0, key.1));
            }
            Action::SettingsSave => {
                self.commit_editing();
                self.save_settings(adapter, id);
            }
            _ => return false,
        }
        true
    }
    /// Whether the device's staged settings or gamepad output can be saved now.
    pub fn settings_savable(&self, d: &DeviceView) -> bool {
        let (changes, invalid) = self.setting_changes(d);
        let gamepad = self.device_changes(d).gamepad;
        !self.form_busy(d)
            && !invalid
            && (!changes.is_empty() || gamepad.is_some())
            && !(gamepad.is_some() && self.device_locked(d))
    }
    /// Saves the device's staged settings, then its staged gamepad output.
    pub(crate) fn save_settings(&mut self, adapter: &str, id: u32) {
        let Some(d) = self.device(adapter, id).cloned() else {
            return;
        };
        if !self.settings_savable(&d) || self.immediate_busy(adapter) {
            return;
        }
        let gamepad = self.device_changes(&d).gamepad;
        let (changes, _) = self.setting_changes(&d);
        let sent: Vec<(Field, Draft)> = changes
            .into_iter()
            .filter_map(|(k, _)| Some((k, self.draft_of(&d, &k)?.clone())))
            .collect();
        if sent.is_empty() {
            self.save_device(&d, Tab::Settings);
        } else {
            self.send_settings(&d, sent, self.immediate.is_none() && gamepad.is_some());
        }
    }
    /// Sends staged settings, and once they save the staged gamepad output when `gamepad`. A
    /// gamepad output save can reconnect USB, so nothing else is in flight when it is sent.
    pub(crate) fn send_settings(
        &mut self,
        d: &DeviceView,
        sent: Vec<(Field, Draft)>,
        gamepad: bool,
    ) {
        if sent.is_empty() {
            return;
        }
        let settings = self.settings_of(d);
        let mut set = Vec::new();
        let mut forget = Vec::new();
        for (k, draft) in &sent {
            let Some(s) = settings.iter().find(|s| s.field == *k) else {
                continue;
            };
            match self.setting_change(s, Some(draft)) {
                Ok(Some(Change::Set(v))) => set.push((*k, v)),
                Ok(Some(Change::Forget)) => forget.push(*k),
                _ => {}
            }
        }
        let key = (d.adapter.clone(), d.d.id);
        self.submissions.insert(
            key.clone(),
            Submission {
                running: true,
                fields: sent.iter().map(|(k, _)| *k).collect(),
                error: None,
                unknown: false,
            },
        );
        self.notes.remove(&Spot::SettingsNote(key.0.clone(), key.1));
        self.run(
            &key.0,
            Kind::SettingsSave {
                device: key.1,
                sent,
                gamepad,
            },
            Command::SettingsSave {
                device: key.1,
                set,
                forget,
            },
        );
    }
    pub(crate) fn settings_result(&mut self, job: &Job, result: &Result<Outcome, Error>) {
        let Kind::SettingsSave {
            device,
            sent,
            gamepad,
        } = &job.kind
        else {
            return;
        };
        let key = (job.adapter.clone(), *device);
        // An outcome this app doesn't know reads as none.
        let outcome = match result {
            Err(e) => e
                .dongle
                .as_ref()
                .map(|w| ErrorOutcome::try_from(w.outcome).ok()),
            Ok(_) => None,
        };
        match result.as_ref().err() {
            Some(e) if outcome != Some(Some(ErrorOutcome::Complete)) => {
                // Only an outcome confirms what was saved. The changes are saved together, so a
                // partial save leaves each one's outcome unknown, as does an adapter failure
                // without an outcome. A local failure saved nothing unless it came after sending.
                let unknown = match outcome {
                    Some(o) => o != Some(ErrorOutcome::NoEffect),
                    None => e.unconfirmed(),
                };
                let text = match words::failure(e) {
                    t if t.is_empty() => words::SETTINGS_SAVE_FAILED.to_owned(),
                    t => t,
                };
                if let Some(s) = self.submissions.get_mut(&key) {
                    s.running = false;
                    s.error = Some(text);
                    s.unknown = unknown;
                }
            }
            // A failure after a complete save shows with the form rather than with each change.
            failed => {
                if let Some(e) = failed {
                    self.notes
                        .insert(Spot::SettingsNote(key.0.clone(), key.1), words::failure(e));
                }
                self.submissions.remove(&key);
                if let Some(drafts) = self.setting_drafts.get_mut(&key) {
                    for (k, draft) in sent {
                        // A draft changed since it was sent stays staged.
                        if drafts.get(k) == Some(draft) {
                            drafts.remove(k);
                        }
                    }
                    if drafts.is_empty() {
                        self.setting_drafts.remove(&key);
                    }
                }
                if *gamepad
                    && self.immediate.is_none()
                    && let Some(d) = self.device(&key.0, key.1).cloned()
                {
                    self.save_device(&d, Tab::Settings);
                }
            }
        }
    }
    /// Ends typing a number: the typed text becomes the row's draft.
    pub(crate) fn commit_editing(&mut self) {
        let Some(k) = self.editing.take() else {
            return;
        };
        let super::Page::Device(adapter, id) = self.shown() else {
            return;
        };
        let Some(d) = self.device(&adapter, id).cloned() else {
            return;
        };
        let Some(s) = self.setting(&d, &k) else {
            return;
        };
        let typed = self.form.value();
        match parse_typed(&s, &typed) {
            Some(v) => self.edit(&d, &s, v),
            None => self.set_draft(&d, &s, Some(Draft::Typed(typed))),
        }
    }
    /// Ends typing a number and drops the row's draft, as Esc does.
    pub(crate) fn revert_editing(&mut self) {
        let Some(k) = self.editing.take() else {
            return;
        };
        if let super::Page::Device(adapter, id) = self.shown()
            && let Some(d) = self.device(&adapter, id).cloned()
            && let Some(s) = self.setting(&d, &k)
        {
            self.set_draft(&d, &s, None);
        }
    }
    pub fn marker(&self, d: &DeviceView, s: &Setting, busy: bool) -> Marker {
        let key = s.field;
        let fresh = Self::settings_current(d) && s.value.is_some();
        let can_keep = fresh && !busy && s.value.is_some_and(|v| settings::accepts(s, &v));
        let forget = (
            "Forget Saved Value",
            (!busy).then_some(Action::ForgetSetting(key)),
        );
        let staged = !matches!(change_of(s, self.draft_of(d, &key)), Ok(None));
        if staged && self.immediate.is_none() {
            return Marker {
                text: "✎ Changed",
                tone: Tone::Accent,
                items: vec![("Undo Change", (!busy).then_some(Action::Undo(key)))],
            };
        }
        if s.saved.is_none() {
            return Marker {
                text: "○ Not Saved",
                tone: Tone::Dim,
                items: vec![(
                    "Persist Current Value",
                    can_keep.then_some(Action::Keep(key)),
                )],
            };
        }
        match &s.status {
            Some(Ok(SettingState::ChangedOnDevice)) => Marker {
                text: "◆ Changed on Device",
                tone: Tone::Warning,
                items: vec![
                    ("Save Device Value", can_keep.then_some(Action::Keep(key))),
                    forget,
                ],
            },
            Some(Err(e)) if model::unsupported(e) => Marker {
                text: "▲ Can't Apply Now",
                tone: Tone::Warning,
                items: vec![forget],
            },
            Some(Err(_)) => Marker {
                text: "▲ Failed",
                tone: Tone::Warning,
                items: vec![forget],
            },
            _ => Marker {
                text: "● Saved",
                tone: Tone::Success,
                items: vec![forget],
            },
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_show_their_units() {
        let int = |n| Some(Value::Integer(n));
        assert_eq!(unit_text(Leaf::Dpi, int(800).as_ref()), "800 DPI");
        assert_eq!(unit_text(Leaf::AutoOff, int(30).as_ref()), "30 s");
        assert_eq!(unit_text(Leaf::AutoOff, int(0).as_ref()), "Never");
        assert_eq!(unit_text(Leaf::AutoOff, None), "Unknown");
        assert_eq!(
            unit_text(Leaf::WheelMode, Some(&Value::Choice(1))),
            "Free Spin"
        );
        assert_eq!(
            unit_text(Leaf::FreeSpinThreshold, int(FREE_SPIN_OFF).as_ref()),
            "Off"
        );
        assert_eq!(unit_text(Leaf::FreeSpinThreshold, int(30).as_ref()), "30");
        assert_eq!(
            unit_text(Leaf::AutoOff, int(FREE_SPIN_OFF).as_ref()),
            "255 s",
            "only the free spin threshold reads its off value as Off"
        );
    }

    #[test]
    pub(crate) fn steps_stay_in_range_at_the_limits() {
        let b = Bounds {
            min: Some(i64::MIN),
            max: Some(i64::MAX),
            step: i64::MAX,
            base: i64::MIN,
        };
        assert_eq!(
            step_value(b, Some(&Value::Integer(i64::MAX)), 1),
            Value::Integer(i64::MAX),
            "a reading above the highest step isn't lowered"
        );
        assert_eq!(
            step_value(b, Some(&Value::Integer(i64::MIN)), -1),
            Value::Integer(i64::MIN)
        );
        let b = Bounds {
            min: Some(200),
            max: Some(4000),
            step: 50,
            base: 200,
        };
        assert_eq!(
            step_value(b, Some(&Value::Integer(1025)), 1),
            Value::Integer(1050)
        );
        assert_eq!(
            step_value(b, Some(&Value::Integer(1025)), -1),
            Value::Integer(1000)
        );
        assert_eq!(
            step_value(b, Some(&Value::Integer(4000)), 1),
            Value::Integer(4000)
        );
        let b = Bounds {
            max: Some(4010),
            ..b
        };
        assert_eq!(
            step_value(b, Some(&Value::Integer(3950)), 1),
            Value::Integer(4000),
            "the highest value on the steps"
        );
    }
}
