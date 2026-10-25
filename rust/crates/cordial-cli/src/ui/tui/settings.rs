//! A device's Settings tab: its gamepad output, its settings grouped by category, each with the
//! control its type calls for and a marker for its saved state, and the readings that belong beside
//! them. Every control stages a change; Save sends every staged setting of the device in one
//! request, then a staged gamepad output in another.
use super::{
    Action, Job, Kind, Menu, Model, Spot, Submission, Tab,
    fleet::Fleet,
    layout::{self, Choice, Layout, Tone, accent, dim, err, ok, span, styled, warn},
    render::{PageView, button_if},
    words,
    world::DeviceView,
};
use crate::{
    controller::{Command, Outcome, Target},
    error::Error,
    model,
    settings::{self, Field, Leaf, Setting, Type, Value},
    ui::{catalog, text},
};
use cordial_protocol::{self as p, ErrorCode, OutputMode, SettingState};
use ratatui::{style::Style, text::Line};
use std::collections::BTreeMap;

/// The automatic free spin threshold that turns automatic switching off.
const FREE_SPIN_OFF: i64 = 255;

/// A staged change to one setting.
#[derive(Clone, Debug, PartialEq)]
pub enum Draft {
    /// A value to save. `policy` marks Save Current Value and Save Device Value, which are
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
enum Change {
    Set(Value),
    Forget,
}

/// The value the device keeps when nothing is staged: the saved value, else the reading.
fn base(s: &Setting) -> Option<Value> {
    s.saved.or(s.value)
}

/// Whether the setting is the automatic free spin threshold, shown as a switch with its threshold
/// beside it.
fn is_free_spin(s: &Setting) -> bool {
    s.field.leaf == Leaf::FreeSpinThreshold
        && settings::kind(s) == Type::Integer
        && settings::choices(s).is_empty()
}

/// An integer setting's range for stepping and checking.
#[derive(Clone, Copy)]
struct Bounds {
    min: Option<i64>,
    max: Option<i64>,
    step: i64,
    base: i64,
}

fn bounds(s: &Setting) -> Bounds {
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
fn step_value(b: Bounds, from: Option<&Value>, delta: i64) -> Value {
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
fn parse_typed(s: &Setting, typed: &str) -> Option<Value> {
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
fn change_of(s: &Setting, draft: Option<&Draft>) -> Result<Option<Change>, ()> {
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
fn range_text(s: &Setting) -> String {
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
        Some(Value::Integer(n)) => n.to_string(),
        Some(Value::Choice(n)) => catalog::choice_words(leaf, *n),
    }
}

/// The unit shown beside a value; none when the value reads "Never".
fn display_unit(leaf: Leaf, v: Option<&Value>) -> &'static str {
    match v {
        Some(Value::Integer(0)) if leaf == Leaf::AutoOff => "",
        _ => catalog::unit(leaf),
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
struct Marker {
    text: &'static str,
    look: Style,
    items: Vec<(&'static str, Option<Action>)>,
}

impl<F: Fleet> Model<F> {
    fn drafts_of(&self, d: &DeviceView) -> Option<&BTreeMap<Field, Draft>> {
        self.setting_drafts.get(&(d.adapter.clone(), d.d.id))
    }

    fn draft_of(&self, d: &DeviceView, field: &Field) -> Option<&Draft> {
        self.drafts_of(d)?.get(field)
    }

    fn settings_of(&self, d: &DeviceView) -> Vec<Setting> {
        self.state_of(&d.adapter)
            .map(|st| {
                catalog::presented(&st.settings_of(d.d.id))
                    .into_iter()
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }

    fn setting(&self, d: &DeviceView, field: &Field) -> Option<Setting> {
        self.settings_of(d).into_iter().find(|s| s.field == *field)
    }

    /// The device's settings are current: it is connected and its Logitech Features are up.
    fn settings_current(d: &DeviceView) -> bool {
        d.connected() && model::hidpp_up(&d.d) == model::Up::Active
    }

    fn refreshing(&self, d: &DeviceView) -> bool {
        let id = d.d.id;
        self.running(
            &d.adapter,
            |k| matches!(k, Kind::Device(device, Spot::SettingsNote(..)) if *device == id),
        )
    }

    /// Whether the settings form takes no changes now.
    fn form_busy(&self, d: &DeviceView) -> bool {
        !self.adapter_ready(d)
            || self.device_saving(d, Tab::Settings)
            || self.refreshing(d)
            || self.settings_busy(d)
    }

    /// The gamepad output modes the Settings tab offers the device; none when it has no Gamepad
    /// Output to choose or no choice differs from the saved one.
    pub(super) fn gamepad_choices(&self, d: &DeviceView) -> Vec<OutputMode> {
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
    pub(super) fn adapter_ready(&self, d: &DeviceView) -> bool {
        self.adapter(&d.adapter)
            .is_some_and(|a| a.connected() && a.ready)
    }

    /// Whether the device can be asked for its current settings.
    fn can_refresh(&self, d: &DeviceView) -> bool {
        !self.form_busy(d) && d.connected()
    }

    fn submitting(&self, d: &DeviceView) -> bool {
        self.submissions
            .get(&(d.adapter.clone(), d.d.id))
            .is_some_and(|s| s.running)
    }

    /// The value a setting's control shows.
    fn shown_value(&self, d: &DeviceView, s: &Setting) -> Option<Value> {
        match self.draft_of(d, &s.field) {
            Some(Draft::Set { value, .. }) => Some(*value),
            Some(Draft::Typed(t)) => parse_typed(s, t).or_else(|| base(s)),
            Some(Draft::Forget) => s.value,
            None => base(s),
        }
    }

    fn set_draft(&mut self, d: &DeviceView, s: &Setting, draft: Option<Draft>) {
        let key = (d.adapter.clone(), d.d.id);
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

    /// Stages a value; a value the device keeps already drops the draft.
    fn edit(&mut self, d: &DeviceView, s: &Setting, value: Value) {
        let keep_policy = matches!(
            self.draft_of(d, &s.field),
            Some(Draft::Set { policy: true, .. })
        ) && s.saved.is_none();
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

    /// The staged changes and whether any is invalid.
    fn setting_changes(&self, d: &DeviceView) -> (Vec<(Field, Change)>, bool) {
        let mut out = Vec::new();
        let mut invalid = false;
        let Some(drafts) = self.drafts_of(d) else {
            return (out, false);
        };
        for s in self.settings_of(d) {
            match change_of(&s, drafts.get(&s.field)) {
                Ok(Some(c)) => out.push((s.field, c)),
                Ok(None) => {}
                Err(()) => invalid = true,
            }
        }
        (out, invalid)
    }

    fn settings_dirty(&self, d: &DeviceView) -> bool {
        let (changes, invalid) = self.setting_changes(d);
        invalid || !changes.is_empty()
    }

    /// Handles a settings action; false when `action` isn't one.
    pub(super) fn settings_action(&mut self, adapter: &str, id: u32, action: &Action) -> bool {
        let Some(d) = self.device(adapter, id).cloned() else {
            return false;
        };
        let busy = self.form_busy(&d);
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
                let (x, y) = self
                    .hits
                    .iter()
                    .find(|h| h.action == *action)
                    .map_or((0, 0), |h| (h.x, h.y + 1));
                let items = settings::choices(&s)
                    .into_iter()
                    .map(|v| {
                        let label = value_text(s.field.leaf, Some(&v));
                        (label, Some(Action::SettingChoice(*k, v)))
                    })
                    .collect();
                self.menu = Some(Menu { x, y, items });
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
                let (x, y) = self
                    .hits
                    .iter()
                    .find(|h| h.action == *action)
                    .map_or((0, 0), |h| (h.x, h.y + 1));
                let items = marker
                    .items
                    .into_iter()
                    .map(|(label, action)| (label.to_owned(), action))
                    .collect();
                self.menu = Some(Menu { x, y, items });
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
                        matches!(s.status, Some(Err(_))) && self.draft_of(&d, &s.field).is_none()
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
                self.send_settings(&d, sent, false);
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
    pub(super) fn settings_savable(&self, d: &DeviceView) -> bool {
        let (changes, invalid) = self.setting_changes(d);
        let gamepad = self.device_changes(d).gamepad;
        !self.form_busy(d)
            && !invalid
            && (!changes.is_empty() || gamepad.is_some())
            && !(gamepad.is_some() && self.device_locked(d))
    }

    /// Saves the device's staged settings, then its staged gamepad output.
    pub(super) fn save_settings(&mut self, adapter: &str, id: u32) {
        let Some(d) = self.device(adapter, id).cloned() else {
            return;
        };
        if !self.settings_savable(&d) {
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
            self.send_settings(&d, sent, gamepad.is_some());
        }
    }

    /// Sends staged settings, and once they save the staged gamepad output when `gamepad`. A
    /// gamepad output save can reconnect USB, so nothing else is in flight when it is sent.
    fn send_settings(&mut self, d: &DeviceView, sent: Vec<(Field, Draft)>, gamepad: bool) {
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
            match change_of(s, Some(draft)) {
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

    pub(super) fn settings_result(&mut self, job: &Job, result: &Result<Outcome, Error>) {
        let Kind::SettingsSave {
            device,
            sent,
            gamepad,
        } = &job.kind
        else {
            return;
        };
        let key = (job.adapter.clone(), *device);
        match result {
            Ok(_) => {
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
                if *gamepad && let Some(d) = self.device(&key.0, key.1).cloned() {
                    self.save_device(&d, Tab::Settings);
                }
            }
            Err(e) => {
                let unknown = e
                    .dongle
                    .as_ref()
                    .is_some_and(|w| w.code() == ErrorCode::StorageFailed && w.outcome_unknown)
                    || words::failure(e) == words::CLOSED;
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
        }
    }

    /// Ends typing a number: the typed text becomes the row's draft.
    pub(super) fn commit_editing(&mut self) {
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
    pub(super) fn revert_editing(&mut self) {
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

    /// Steps the highlighted setting's value, or moves its choice, by `delta`; false when the
    /// highlight isn't on a setting.
    pub(super) fn adjust_focused(&mut self, delta: i64) -> bool {
        let Some(focus) = self.focus.clone() else {
            return false;
        };
        let super::Page::Device(adapter, id) = self.shown() else {
            return false;
        };
        let key = match &focus {
            Action::SettingStep(k, _) | Action::SettingEdit(k) => *k,
            Action::SettingChoice(k, _) | Action::SettingSelect(k) | Action::SettingBool(k, _) => {
                *k
            }
            _ => return false,
        };
        let Some(d) = self.device(&adapter, id).cloned() else {
            return false;
        };
        let Some(s) = self.setting(&d, &key) else {
            return false;
        };
        if self.form_busy(&d) {
            return true;
        }
        let choices = settings::choices(&s);
        let shown = self.shown_value(&d, &s);
        if settings::kind(&s) == Type::Bool {
            self.edit(&d, &s, Value::Bool(delta > 0));
        } else if !choices.is_empty() {
            let i = shown
                .as_ref()
                .and_then(|v| choices.iter().position(|c| c == v));
            let n = choices.len() as i64;
            let next = match i {
                Some(i) => (i as i64 + delta).clamp(0, n - 1),
                None if delta > 0 => 0,
                None => n - 1,
            };
            self.edit(&d, &s, choices[next as usize]);
            self.focus = match &focus {
                Action::SettingChoice(..) => {
                    Some(Action::SettingChoice(key, choices[next as usize]))
                }
                other => Some(other.clone()),
            };
        } else if settings::kind(&s) == Type::Integer {
            let v = step_value(bounds(&s), shown.as_ref(), delta);
            self.edit(&d, &s, v);
        }
        true
    }

    fn marker(&self, d: &DeviceView, s: &Setting, busy: bool) -> Marker {
        let key = s.field;
        let fresh = Self::settings_current(d) && s.value.is_some();
        let can_keep = fresh && !busy && s.value.is_some_and(|v| settings::accepts(s, &v));
        let forget = (
            "Forget Saved Value",
            (!busy).then_some(Action::ForgetSetting(key)),
        );
        let staged = !matches!(change_of(s, self.draft_of(d, &key)), Ok(None));
        if staged {
            return Marker {
                text: "✎ Changed",
                look: accent(),
                items: vec![("Undo Change", (!busy).then_some(Action::Undo(key)))],
            };
        }
        if s.saved.is_none() {
            return Marker {
                text: "○ Not Saved",
                look: dim(),
                items: vec![("Save Current Value", can_keep.then_some(Action::Keep(key)))],
            };
        }
        match s.status {
            Some(Ok(SettingState::ChangedOnDevice)) => Marker {
                text: "◆ Changed on Device",
                look: warn(),
                items: vec![
                    ("Save Device Value", can_keep.then_some(Action::Keep(key))),
                    forget,
                ],
            },
            Some(Err(_)) => Marker {
                text: "▲ Failed",
                look: warn(),
                items: vec![forget],
            },
            Some(Ok(SettingState::Unsupported)) => Marker {
                text: "▲ Can't Apply Now",
                look: warn(),
                items: vec![forget],
            },
            _ => Marker {
                text: "● Saved",
                look: ok(),
                items: vec![forget],
            },
        }
    }

    /// The control of one setting.
    fn control(&self, d: &DeviceView, s: &Setting, busy: bool, w: usize) -> Layout {
        let mut c = Layout::new(w);
        let key = s.field;
        let leaf = key.leaf;
        let fresh = Self::settings_current(d) && s.value.is_some();
        let stale = if fresh { Style::new() } else { dim() };
        let shown = self.shown_value(d, s);
        let choices = settings::choices(s);
        let kind = settings::kind(s);
        if kind == Type::Choice && choices.is_empty() {
            let unit = display_unit(leaf, s.value.as_ref());
            let mut text = value_text(leaf, s.value.as_ref());
            if !unit.is_empty() {
                text = format!("{text} {unit}");
            }
            c.label(&text, stale);
            return c;
        }
        if kind == Type::Bool {
            let on = match &shown {
                Some(Value::Bool(b)) => Some(*b),
                _ => None,
            };
            let next = on.is_none_or(|on| !on);
            return layout::switch(on, Action::SettingBool(key, next), !busy);
        }
        if is_free_spin(s) {
            let on = !matches!(shown, Some(Value::Integer(FREE_SPIN_OFF)));
            return layout::switch(Some(on), Action::FreeSpin(key, !on), !busy);
        }
        if !choices.is_empty() {
            let labels: Vec<String> = choices.iter().map(|v| value_text(leaf, Some(v))).collect();
            let short = choices.len() <= 3
                && labels.iter().map(|l| l.chars().count()).sum::<usize>() <= 24
                && shown.as_ref().is_some_and(|v| choices.contains(v));
            if short {
                for (v, label) in choices.iter().zip(labels) {
                    let chosen = shown.as_ref() == Some(v);
                    let text = if chosen {
                        format!("[● {label}]")
                    } else {
                        format!("[○ {label}]")
                    };
                    let look = match () {
                        _ if busy => dim(),
                        _ if chosen => Tone::Chosen.style(),
                        _ => Tone::Normal.style(),
                    };
                    let x = layout::line_width(c.lines.last().unwrap_or(&Line::default()));
                    let x = if x > 0 { x + 1 } else { 0 };
                    if c.lines.is_empty() {
                        c.row();
                    }
                    let y = c.lines.len() - 1;
                    if x > 0 {
                        c.lines[y].spans.push(span(" ", Style::new()));
                    }
                    let tw = text::width(&text);
                    c.lines[y].spans.push(span(text, look));
                    if !busy {
                        c.hits.push(layout::Hit {
                            x,
                            y,
                            w: tw,
                            action: Action::SettingChoice(key, *v),
                        });
                    }
                }
                c.width = layout::line_width(&c.lines[0]);
                return c;
            }
            let text = format!("{} ▾", value_text(leaf, shown.as_ref()));
            if busy {
                c.disabled(&text);
            } else {
                c.button(&text, Action::SettingSelect(key), Tone::Normal);
            }
            return c;
        }
        self.stepper(&mut c, d, s, busy);
        c
    }

    /// An integer's stepper: down, the value (typed when clicked), up and the unit.
    fn stepper(&self, c: &mut Layout, d: &DeviceView, s: &Setting, busy: bool) {
        let key = s.field;
        let shown = self.shown_value(d, s);
        button_if(c, "-", Action::SettingStep(key, -1), Tone::Normal, !busy);
        if self.editing == Some(key) {
            let mut form = self.form.clone();
            let line = super::keys::edit_line(&mut form, true, 8);
            let y = c.lines.len() - 1;
            c.lines[y].spans.push(span(" ", Style::new()));
            c.lines[y].spans.extend(line.spans);
        } else {
            let text = match self.draft_of(d, &key) {
                Some(Draft::Typed(t)) => t.clone(),
                _ => value_text(key.leaf, shown.as_ref()),
            };
            button_if(c, &text, Action::SettingEdit(key), Tone::Normal, !busy);
        }
        button_if(c, "+", Action::SettingStep(key, 1), Tone::Normal, !busy);
        let unit = display_unit(key.leaf, shown.as_ref());
        if !unit.is_empty() {
            c.label(unit, dim());
        }
        c.width = c.lines.iter().map(layout::line_width).max().unwrap_or(0);
    }

    /// The Settings tab.
    pub(super) fn device_settings(&mut self, d: &DeviceView, v: &mut PageView) {
        let settings = self.settings_of(d);
        let readings = readings(&d.d);
        let figures = words::wheel_figures(d.d.information.as_ref());
        let busy = self.form_busy(d);
        let key = (d.adapter.clone(), d.d.id);
        let submission = self.submissions.get(&key).cloned();
        let mut categories: Vec<&'static str> = Vec::new();
        for leaf in settings
            .iter()
            .map(|s| s.field.leaf)
            .chain(readings.iter().map(|(f, _)| f.leaf))
        {
            let c = catalog::category(leaf);
            if !categories.contains(&c) {
                categories.push(c);
            }
        }
        if !figures.is_empty() && !categories.contains(&"Wheel") {
            categories.push("Wheel");
        }
        let w = v.body.width;
        let gamepad = self.gamepad_choices(d);
        let gamepad_change = self.device_changes(d).gamepad;
        let gamepad_saving = self.device_saving(d, Tab::Settings);
        let b = &mut v.body;
        if !gamepad.is_empty() {
            let shown = gamepad_change.or_else(|| model::gamepad_mode(&d.d));
            let options = gamepad
                .into_iter()
                .map(|mode| Choice {
                    label: text::output_mode_name(mode).into(),
                    action: Action::GamepadOutput(mode),
                    chosen: Some(mode) == shown,
                })
                .collect();
            b.section("Gamepad");
            let mut group = Layout::new(w.saturating_sub(4));
            group.choice_if("", 0, options, !busy);
            let mark = match () {
                _ if gamepad_saving => Some(format!("{} Sending", super::spinner())),
                _ if gamepad_change.is_some() => Some("✎ Changed".to_owned()),
                _ => None,
            };
            if let Some(mark) = mark {
                let mut m = Layout::new(group.width);
                m.lines.push(Line::from(span(format!(" {mark}"), accent())));
                group.append(m);
            }
            b.labelled(Line::from(span("Gamepad Output", Style::new())), group);
        }
        for category in categories {
            b.section(category);
            for s in settings
                .iter()
                .filter(|s| catalog::category(s.field.leaf) == category)
            {
                let label = if is_free_spin(s) {
                    "Automatic Free Spin".to_owned()
                } else {
                    catalog::label(&s.field)
                };
                let label = Line::from(span(label, Style::new()));
                let mut group = self.control(d, s, busy, w.saturating_sub(4));
                if group.lines.is_empty() {
                    group.row();
                }
                let marker = self.marker(d, s, busy);
                let saving = submission
                    .as_ref()
                    .is_some_and(|sub| sub.running && sub.fields.contains(&s.field));
                let (mark_text, mark_look) = if saving {
                    (format!("{} Sending", super::spinner()), accent())
                } else {
                    (marker.text.to_owned(), marker.look)
                };
                group.width = w.saturating_sub(4);
                let mut mark = Layout::new(group.width);
                mark.lines
                    .push(Line::from(span(format!(" {mark_text}"), mark_look)));
                if !saving {
                    mark.hits.push(layout::Hit {
                        x: 1,
                        y: 0,
                        w: text::width(&mark_text),
                        action: Action::Marker(s.field),
                    });
                }
                group.append(mark);
                b.labelled(label, group);
                if is_free_spin(s)
                    && !matches!(self.shown_value(d, s), Some(Value::Integer(FREE_SPIN_OFF)))
                {
                    let mut c = Layout::new(w);
                    self.stepper(&mut c, d, s, busy);
                    let label = format!("  {}", catalog::label(&s.field));
                    b.labelled(styled(label, Style::new()), c);
                }
                let mut notes: Vec<(String, Style)> = Vec::new();
                if let Some(sub) = &submission
                    && !sub.running
                    && sub.fields.contains(&s.field)
                    && let Some(e) = &sub.error
                {
                    let text = if sub.unknown {
                        words::reason(e).to_owned()
                    } else {
                        format!("Couldn't Save: {}", words::reason(e))
                    };
                    notes.push((text, err()));
                }
                if change_of(s, self.draft_of(d, &s.field)).is_err() {
                    notes.push((range_text(s), err()));
                }
                let fresh = Self::settings_current(d) && s.value.is_some();
                if s.saved.is_some() && s.status == Some(Ok(SettingState::ChangedOnDevice)) && fresh
                {
                    notes.push((
                        format!("Device: {}", value_text(s.field.leaf, s.value.as_ref())),
                        dim(),
                    ));
                }
                if let Some(Err(code)) = s.status {
                    notes.push((words::reason(&words::code_text(code)).to_owned(), dim()));
                }
                if !notes.is_empty() {
                    let mut line = Line::from(span("  ", Style::new()));
                    for (i, (text, look)) in notes.into_iter().enumerate() {
                        if i > 0 {
                            line.spans.push(span(" · ", dim()));
                        }
                        line.spans.push(span(text, look));
                    }
                    b.line(line);
                }
            }
            let look = if d.connected() { Style::new() } else { dim() };
            for (field, value) in readings
                .iter()
                .filter(|(f, _)| catalog::category(f.leaf) == category)
            {
                let mut text = value_text(field.leaf, Some(value));
                let unit = display_unit(field.leaf, Some(value));
                if !unit.is_empty() {
                    text = format!("{text} {unit}");
                }
                b.labelled(styled(catalog::label(field), Style::new()), {
                    let mut r = Layout::new(w);
                    r.label(&text, look);
                    r.width = text::width(&text);
                    r
                });
            }
            if category == "Wheel" {
                for (label, value) in &figures {
                    b.fact(label, value, look);
                }
            }
        }

        // The bar.
        let bar = &mut v.bar;
        let saving = submission.as_ref().is_some_and(|s| s.running) || gamepad_saving;
        let starting = d.connected() && model::hidpp_up(&d.d) == model::Up::Starting;
        if saving || self.refreshing(d) || starting {
            bar.label(&format!("{}", super::spinner()), accent());
        }
        let note = self
            .notes
            .get(&Spot::SettingsNote(key.0.clone(), key.1))
            .cloned()
            .or_else(|| {
                submission
                    .as_ref()
                    .filter(|s| !s.running && s.error.is_some() && !s.unknown)
                    .map(|s| format!("Couldn't Save {}", s.fields.len()))
            })
            .or_else(|| match model::hidpp_up(&d.d) {
                model::Up::Error(code) => Some(words::code_text(code)),
                _ => None,
            });
        if let Some(note) = note {
            bar.para(&note, err());
        }
        if self.settings_errors.contains_key(&key) {
            bar.para(words::SETTINGS_READ_FAILED, dim());
            let reloading = self.running(
                &d.adapter,
                |k| matches!(k, Kind::SettingsList(x) if *x == d.d.id),
            );
            button_if(
                bar,
                "Retry",
                Action::SettingsReload,
                Tone::Normal,
                !reloading,
            );
        }
        let mut right = Layout::new(bar.width);
        button_if(
            &mut right,
            "Refresh",
            Action::SettingsRefresh,
            Tone::Normal,
            self.can_refresh(d),
        );
        let failed: Vec<&Setting> = settings
            .iter()
            .filter(|s| matches!(s.status, Some(Err(_))) && s.saved.is_some())
            .collect();
        if !failed.is_empty() {
            let any_free = failed.iter().any(|s| self.draft_of(d, &s.field).is_none());
            button_if(
                &mut right,
                "Retry",
                Action::SettingsReapply,
                Tone::Normal,
                !busy && any_free,
            );
        }
        let dirty = self.settings_dirty(d) || gamepad_change.is_some();
        let (changes, invalid) = self.setting_changes(d);
        button_if(
            &mut right,
            "Discard",
            Action::SettingsDiscard,
            Tone::Normal,
            !self.submitting(d) && !gamepad_saving && dirty,
        );
        button_if(
            &mut right,
            "Save",
            Action::SettingsSave,
            Tone::Primary,
            !busy
                && (!changes.is_empty() || gamepad_change.is_some())
                && !invalid
                && !(gamepad_change.is_some() && self.device_locked(d)),
        );
        bar.align_right(right);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steps_stay_in_range_at_the_limits() {
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
