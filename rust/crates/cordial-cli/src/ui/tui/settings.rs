//! A device's Settings tab: its settings grouped by category, each with the control its type
//! calls for and a marker for its saved state, and the readings that belong beside them. Every
//! control stages a change; Save sends every staged change of the device in one request.
use super::{
    Action, Job, Kind, Menu, Model, Spot, Submission,
    fleet::Fleet,
    layout::{self, Layout, Tone, accent, dim, err, ok, span, styled, warn},
    render::{PageView, button_if},
    words,
    world::DeviceView,
};
use crate::{
    controller::{Command, Outcome, Target},
    error::Error,
    model::{self, Type},
    ui::{catalog, text},
};
use cordial_protocol::{self as p, ErrorCode, SettingState, keys, value::Value};
use ratatui::{style::Style, text::Line};
use std::collections::BTreeMap;

/// SmartShift's threshold that turns it off.
const SMARTSHIFT_OFF: i64 = 255;

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
fn base(s: &p::Setting) -> Option<Value> {
    model::saved(s).or_else(|| model::current(s))
}

fn is_smartshift(s: &p::Setting) -> bool {
    s.key == keys::WHEEL_THRESHOLD
        && model::kind(s) == Some(Type::Integer)
        && model::choices(s).is_empty()
}

/// An integer setting's range for stepping and checking.
#[derive(Clone, Copy)]
struct Bounds {
    min: Option<i64>,
    max: Option<i64>,
    step: i64,
    base: i64,
}

fn bounds(s: &p::Setting) -> Bounds {
    let mut b = match model::range(s) {
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
    if is_smartshift(s) {
        b.min = Some(b.min.unwrap_or(1).max(1));
        b.max = Some(b.max.unwrap_or(SMARTSHIFT_OFF - 1).min(SMARTSHIFT_OFF - 1));
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
fn parse_typed(s: &p::Setting, typed: &str) -> Option<Value> {
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
fn change_of(s: &p::Setting, draft: Option<&Draft>) -> Result<Option<Change>, ()> {
    match draft {
        None => Ok(None),
        Some(Draft::Forget) => Ok(model::saved(s).is_some().then_some(Change::Forget)),
        Some(Draft::Typed(t)) => {
            let v = parse_typed(s, t).ok_or(())?;
            Ok((Some(&v) != base(s).as_ref()).then_some(Change::Set(v)))
        }
        Some(Draft::Set { value, policy }) => {
            if !model::accepts(s, value) {
                return Err(());
            }
            Ok((*policy || Some(value) != base(s).as_ref()).then(|| Change::Set(value.clone())))
        }
    }
}

/// The range a typed number must be in, as words.
fn range_text(s: &p::Setting) -> String {
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
pub fn value_text(key: &str, v: Option<&Value>) -> String {
    match v {
        None => "Unknown".into(),
        Some(Value::Bool(b)) => if *b { "On" } else { "Off" }.into(),
        Some(Value::Integer(0)) if key == keys::POWER_AUTO_OFF => "Never".into(),
        Some(Value::Integer(n)) => n.to_string(),
        Some(Value::Text(t)) => text::display(&catalog::choice_words(key, t)),
        Some(Value::Color(c)) => format!("#{c:06X}"),
    }
}

/// The unit shown beside a value; none when the value reads "Never".
fn display_unit(key: &str, v: Option<&Value>) -> &'static str {
    match v {
        Some(Value::Integer(0)) if key == keys::POWER_AUTO_OFF => "",
        _ => catalog::unit(key),
    }
}

/// The device's information entries that read like settings and aren't settings themselves.
pub fn readings(d: &p::Device) -> Vec<&p::Info> {
    const FIGURES: [&str; 3] = [
        keys::WHEEL_RESOLUTION_MULTIPLIER,
        keys::WHEEL_RATCHETS_PER_ROTATION,
        keys::WHEEL_DIAMETER,
    ];
    catalog::presented_info(&d.info)
        .into_iter()
        .filter(|i| !catalog::category(&i.key).is_empty() && !FIGURES.contains(&i.key.as_str()))
        .collect()
}

/// The marker of a setting's saved state, and the menu it opens.
struct Marker {
    text: &'static str,
    look: Style,
    items: Vec<(&'static str, Option<Action>)>,
}

impl<F: Fleet> Model<F> {
    fn drafts_of(&self, d: &DeviceView) -> Option<&BTreeMap<String, Draft>> {
        self.setting_drafts.get(&(d.adapter.clone(), d.d.id))
    }

    fn draft_of(&self, d: &DeviceView, key: &str) -> Option<&Draft> {
        self.drafts_of(d)?.get(key)
    }

    fn settings_of(&self, d: &DeviceView) -> Vec<p::Setting> {
        self.state_of(&d.adapter)
            .map(|st| {
                catalog::presented(st.settings_of(d.d.id))
                    .into_iter()
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }

    fn setting(&self, d: &DeviceView, key: &str) -> Option<p::Setting> {
        self.settings_of(d).into_iter().find(|s| s.key == key)
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
        !self.adapter_ready(d) || self.refreshing(d) || self.settings_busy(d)
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
    fn shown_value(&self, d: &DeviceView, s: &p::Setting) -> Option<Value> {
        match self.draft_of(d, &s.key) {
            Some(Draft::Set { value, .. }) => Some(value.clone()),
            Some(Draft::Typed(t)) => parse_typed(s, t).or_else(|| base(s)),
            Some(Draft::Forget) => model::current(s),
            None => base(s),
        }
    }

    fn set_draft(&mut self, d: &DeviceView, s: &p::Setting, draft: Option<Draft>) {
        let key = (d.adapter.clone(), d.d.id);
        let drafts = self.setting_drafts.entry(key.clone()).or_default();
        match draft {
            Some(draft) => {
                drafts.insert(s.key.clone(), draft);
            }
            None => {
                drafts.remove(&s.key);
            }
        }
        if drafts.is_empty() {
            self.setting_drafts.remove(&key);
        }
        self.notes.remove(&Spot::SettingsNote(key.0, key.1));
    }

    /// Stages a value; a value the device keeps already drops the draft.
    fn edit(&mut self, d: &DeviceView, s: &p::Setting, value: Value) {
        let keep_policy = matches!(
            self.draft_of(d, &s.key),
            Some(Draft::Set { policy: true, .. })
        ) && model::saved(s).is_none();
        let draft = if Some(&value) == base(s).as_ref() && !keep_policy {
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
    fn setting_changes(&self, d: &DeviceView) -> (Vec<(String, Change)>, bool) {
        let mut out = Vec::new();
        let mut invalid = false;
        let Some(drafts) = self.drafts_of(d) else {
            return (out, false);
        };
        for s in self.settings_of(d) {
            match change_of(&s, drafts.get(&s.key)) {
                Ok(Some(c)) => out.push((s.key.clone(), c)),
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
            Action::SettingBool(k, v) if !busy => {
                if let Some(s) = self.setting(&d, k) {
                    self.edit(&d, &s, Value::Bool(*v));
                }
            }
            Action::SettingChoice(k, v) if !busy => {
                self.menu = None;
                if let Some(s) = self.setting(&d, k) {
                    self.edit(&d, &s, v.clone());
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
                let items = model::choices(&s)
                    .into_iter()
                    .map(|v| {
                        let label = value_text(&s.key, Some(&v));
                        (label, Some(Action::SettingChoice(k.clone(), v)))
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
                    self.editing = Some(k.clone());
                }
            }
            Action::SmartShift(k, on) if !busy => {
                if let Some(s) = self.setting(&d, k) {
                    let v = if *on {
                        let keep = [model::saved(&s), model::current(&s)]
                            .into_iter()
                            .flatten()
                            .find(|v| matches!(v, Value::Integer(n) if (1..SMARTSHIFT_OFF).contains(n)));
                        keep.unwrap_or(Value::Integer(SMARTSHIFT_OFF - 1))
                    } else {
                        Value::Integer(SMARTSHIFT_OFF)
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
                    && let Some(v) = model::current(&s)
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
                let sent: Vec<(String, Draft)> = self
                    .settings_of(&d)
                    .into_iter()
                    .filter(|s| {
                        matches!(model::applied(s), Some(Err(_)))
                            && self.draft_of(&d, &s.key).is_none()
                    })
                    .filter_map(|s| {
                        let v = model::saved(&s)?;
                        Some((
                            s.key.clone(),
                            Draft::Set {
                                value: v,
                                policy: true,
                            },
                        ))
                    })
                    .collect();
                self.send_settings(&d, sent);
            }
            Action::SettingsDiscard if !self.submitting(&d) => {
                self.setting_drafts.remove(&key);
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

    /// Saves the device's staged settings.
    pub(super) fn save_settings(&mut self, adapter: &str, id: u32) {
        let Some(d) = self.device(adapter, id).cloned() else {
            return;
        };
        let (changes, invalid) = self.setting_changes(&d);
        if self.form_busy(&d) || invalid || changes.is_empty() {
            return;
        }
        let sent: Vec<(String, Draft)> = changes
            .into_iter()
            .filter_map(|(k, _)| Some((k.clone(), self.draft_of(&d, &k)?.clone())))
            .collect();
        self.send_settings(&d, sent);
    }

    fn send_settings(&mut self, d: &DeviceView, sent: Vec<(String, Draft)>) {
        if sent.is_empty() {
            return;
        }
        let settings = self.settings_of(d);
        let mut set = Vec::new();
        let mut forget = Vec::new();
        for (k, draft) in &sent {
            let Some(s) = settings.iter().find(|s| s.key == *k) else {
                continue;
            };
            match change_of(s, Some(draft)) {
                Ok(Some(Change::Set(v))) => set.push((k.clone(), v)),
                Ok(Some(Change::Forget)) => forget.push(k.clone()),
                _ => {}
            }
        }
        let key = (d.adapter.clone(), d.d.id);
        self.submissions.insert(
            key.clone(),
            Submission {
                running: true,
                keys: sent.iter().map(|(k, _)| k.clone()).collect(),
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
            },
            Command::SettingsSave {
                device: key.1,
                set,
                forget,
            },
        );
    }

    pub(super) fn settings_result(&mut self, job: &Job, result: &Result<Outcome, Error>) {
        let Kind::SettingsSave { device, sent } = &job.kind else {
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
            Action::SettingStep(k, _) | Action::SettingEdit(k) => k.clone(),
            Action::SettingChoice(k, _) | Action::SettingSelect(k) | Action::SettingBool(k, _) => {
                k.clone()
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
        let choices = model::choices(&s);
        let shown = self.shown_value(&d, &s);
        if model::kind(&s) == Some(Type::Bool) {
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
            self.edit(&d, &s, choices[next as usize].clone());
            self.focus = match &focus {
                Action::SettingChoice(..) => {
                    Some(Action::SettingChoice(key, choices[next as usize].clone()))
                }
                other => Some(other.clone()),
            };
        } else if model::kind(&s) == Some(Type::Integer) {
            let v = step_value(bounds(&s), shown.as_ref(), delta);
            self.edit(&d, &s, v);
        }
        true
    }

    fn marker(&self, d: &DeviceView, s: &p::Setting, busy: bool) -> Marker {
        let key = &s.key;
        let fresh = Self::settings_current(d) && model::current(s).is_some();
        let can_keep = fresh && !busy && model::current(s).is_some_and(|v| model::accepts(s, &v));
        let forget = (
            "Forget Saved Value",
            (!busy).then(|| Action::ForgetSetting(key.clone())),
        );
        let staged = !matches!(change_of(s, self.draft_of(d, key)), Ok(None));
        if staged {
            return Marker {
                text: "✎ Changed",
                look: accent(),
                items: vec![("Undo Change", (!busy).then(|| Action::Undo(key.clone())))],
            };
        }
        if model::saved(s).is_none() {
            return Marker {
                text: "○ Not Saved",
                look: dim(),
                items: vec![(
                    "Save Current Value",
                    can_keep.then(|| Action::Keep(key.clone())),
                )],
            };
        }
        match model::applied(s) {
            Some(Ok(SettingState::ChangedOnDevice)) => Marker {
                text: "◆ Changed on Device",
                look: warn(),
                items: vec![
                    (
                        "Save Device Value",
                        can_keep.then(|| Action::Keep(key.clone())),
                    ),
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
    fn control(&self, d: &DeviceView, s: &p::Setting, busy: bool, w: usize) -> Layout {
        let mut c = Layout::new(w);
        let key = &s.key;
        let fresh = Self::settings_current(d) && model::current(s).is_some();
        let stale = if fresh { Style::new() } else { dim() };
        let shown = self.shown_value(d, s);
        let choices = model::choices(s);
        let kind = model::kind(s);
        let text_only = matches!(kind, Some(Type::Text | Type::Color))
            || (kind == Some(Type::Enum) && choices.is_empty());
        if text_only {
            let v = model::current(s);
            let unit = display_unit(key, v.as_ref());
            let mut text = value_text(key, v.as_ref());
            if !unit.is_empty() {
                text = format!("{text} {unit}");
            }
            c.label(&text, stale);
            return c;
        }
        if kind == Some(Type::Bool) {
            let on = match &shown {
                Some(Value::Bool(b)) => Some(*b),
                _ => None,
            };
            let next = on.is_none_or(|on| !on);
            return layout::switch(on, Action::SettingBool(key.clone(), next), !busy);
        }
        if is_smartshift(s) {
            let on = !matches!(shown, Some(Value::Integer(SMARTSHIFT_OFF)));
            return layout::switch(Some(on), Action::SmartShift(key.clone(), !on), !busy);
        }
        if !choices.is_empty() {
            let labels: Vec<String> = choices.iter().map(|v| value_text(key, Some(v))).collect();
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
                            action: Action::SettingChoice(key.clone(), v.clone()),
                        });
                    }
                }
                c.width = layout::line_width(&c.lines[0]);
                return c;
            }
            let text = format!("{} ▾", value_text(key, shown.as_ref()));
            if busy {
                c.disabled(&text);
            } else {
                c.button(&text, Action::SettingSelect(key.clone()), Tone::Normal);
            }
            return c;
        }
        self.stepper(&mut c, d, s, busy);
        c
    }

    /// An integer's stepper: down, the value (typed when clicked), up and the unit.
    fn stepper(&self, c: &mut Layout, d: &DeviceView, s: &p::Setting, busy: bool) {
        let key = &s.key;
        let shown = self.shown_value(d, s);
        button_if(
            c,
            "-",
            Action::SettingStep(key.clone(), -1),
            Tone::Normal,
            !busy,
        );
        if self.editing.as_deref() == Some(key.as_str()) {
            let mut form = self.form.clone();
            let line = super::keys::edit_line(&mut form, true, 8);
            let y = c.lines.len() - 1;
            c.lines[y].spans.push(span(" ", Style::new()));
            c.lines[y].spans.extend(line.spans);
        } else {
            let text = match self.draft_of(d, key) {
                Some(Draft::Typed(t)) => t.clone(),
                _ => value_text(key, shown.as_ref()),
            };
            button_if(
                c,
                &text,
                Action::SettingEdit(key.clone()),
                Tone::Normal,
                !busy,
            );
        }
        button_if(
            c,
            "+",
            Action::SettingStep(key.clone(), 1),
            Tone::Normal,
            !busy,
        );
        let unit = display_unit(key, shown.as_ref());
        if !unit.is_empty() {
            c.label(unit, dim());
        }
        c.width = c.lines.iter().map(layout::line_width).max().unwrap_or(0);
    }

    /// The Settings tab.
    pub(super) fn device_settings(&mut self, d: &DeviceView, v: &mut PageView) {
        let settings = self.settings_of(d);
        let readings: Vec<p::Info> = readings(&d.d).into_iter().cloned().collect();
        let figures = words::wheel_figures(&d.d.info);
        let busy = self.form_busy(d);
        let key = (d.adapter.clone(), d.d.id);
        let submission = self.submissions.get(&key).cloned();
        let mut categories: Vec<&'static str> = Vec::new();
        for k in settings
            .iter()
            .map(|s| s.key.as_str())
            .chain(readings.iter().map(|i| i.key.as_str()))
        {
            let c = catalog::category(k);
            if !categories.contains(&c) {
                categories.push(c);
            }
        }
        if !figures.is_empty() && !categories.contains(&"Wheel") {
            categories.push("Wheel");
        }
        let w = v.body.width;
        let b = &mut v.body;
        for category in categories {
            b.section(category);
            for s in settings
                .iter()
                .filter(|s| catalog::category(&s.key) == category)
            {
                let label = Line::from(span(catalog::label(&s.key), Style::new()));
                let mut group = self.control(d, s, busy, w.saturating_sub(4));
                if group.lines.is_empty() {
                    group.row();
                }
                let marker = self.marker(d, s, busy);
                let saving = submission
                    .as_ref()
                    .is_some_and(|sub| sub.running && sub.keys.contains(&s.key));
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
                        action: Action::Marker(s.key.clone()),
                    });
                }
                group.append(mark);
                b.labelled(label, group);
                if is_smartshift(s)
                    && !matches!(self.shown_value(d, s), Some(Value::Integer(SMARTSHIFT_OFF)))
                {
                    let mut c = Layout::new(w);
                    self.stepper(&mut c, d, s, busy);
                    b.labelled(styled("  SmartShift Threshold", Style::new()), c);
                }
                let mut notes: Vec<(String, Style)> = Vec::new();
                if let Some(sub) = &submission
                    && !sub.running
                    && sub.keys.contains(&s.key)
                    && let Some(e) = &sub.error
                {
                    let text = if sub.unknown {
                        words::reason(e).to_owned()
                    } else {
                        format!("Couldn't Save: {}", words::reason(e))
                    };
                    notes.push((text, err()));
                }
                if change_of(s, self.draft_of(d, &s.key)).is_err() {
                    notes.push((range_text(s), err()));
                }
                let fresh = Self::settings_current(d) && model::current(s).is_some();
                if model::saved(s).is_some()
                    && model::applied(s) == Some(Ok(SettingState::ChangedOnDevice))
                    && fresh
                {
                    let reading = model::current(s);
                    notes.push((
                        format!("Device: {}", value_text(&s.key, reading.as_ref())),
                        dim(),
                    ));
                }
                if let Some(Err(code)) = model::applied(s) {
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
            for i in readings
                .iter()
                .filter(|i| catalog::category(&i.key) == category)
            {
                let value = i.value.as_ref().and_then(|v| v.value.clone());
                let mut text = match &value {
                    Some(Value::Text(t)) => text::display(&catalog::choice_words(&i.key, t)),
                    v => value_text(&i.key, v.as_ref()),
                };
                let unit = display_unit(&i.key, value.as_ref());
                if !unit.is_empty() {
                    text = format!("{text} {unit}");
                }
                b.labelled(styled(catalog::label(&i.key), Style::new()), {
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
        let saving = submission.as_ref().is_some_and(|s| s.running);
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
                    .map(|s| format!("Couldn't Save {}", s.keys.len()))
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
        let failed: Vec<p::Setting> = settings
            .iter()
            .filter(|s| matches!(model::applied(s), Some(Err(_))) && model::saved(s).is_some())
            .cloned()
            .collect();
        if !failed.is_empty() {
            let any_free = failed.iter().any(|s| self.draft_of(d, &s.key).is_none());
            button_if(
                &mut right,
                "Retry",
                Action::SettingsReapply,
                Tone::Normal,
                !busy && any_free,
            );
        }
        let dirty = self.settings_dirty(d);
        let (changes, invalid) = self.setting_changes(d);
        button_if(
            &mut right,
            "Discard",
            Action::SettingsDiscard,
            Tone::Normal,
            !self.submitting(d) && dirty,
        );
        button_if(
            &mut right,
            "Save",
            Action::SettingsSave,
            Tone::Primary,
            !busy && !changes.is_empty() && !invalid,
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
