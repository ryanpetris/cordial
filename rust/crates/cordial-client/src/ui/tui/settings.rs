//! The Device settings page replaces the device list and details with a saved
//! device's settings and an editor for one of them. The controls and the
//! editor's staging actions only stage changes; Save sends every staged change
//! of the device, one at a time, and Discard drops them. Opening and
//! navigating the page read only the adapter's own records. While the device
//! is disconnected, the saved records stay visible, values can't change and
//! only forgetting a saved value can be staged and saved. With Logitech
//! Features off, a connected device is still read and Save stores values on
//! the dongle without applying them.
//!
//! `hidpp.setting.set` is acknowledged once the value is stored; the device is
//! written afterwards by the adapter's device job. The Save queue therefore
//! sends the next change only after the device's settings work is idle and
//! the cached catalog shows the sent value settled, so each change's outcome
//! is its own and no two setters compete.
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
    controller::{Command, DeviceSettings, Failure, Outcome, SessionId, SettingInput, State},
    ui::{
        Backend, catalog,
        command::steps,
        text::{self, display, display_name, on_off},
    },
};
use cordial_protocol::{
    errors::ErrorCode,
    identifiers::{NormalizationState, SettingsState},
    messages::Device,
    settings::{
        ObservationSource, Setting, SettingKey, SettingScope, SettingState, SettingType,
        SettingValue,
    },
};
use ratatui::{style::Style, text::Line};
use std::{
    collections::{HashMap, HashSet},
    time::{Duration, Instant},
};

/// The key column of the editor.
const EDITOR_KEY: usize = 13;

/// How long a saved value may take to reach the device: the adapter's job
/// deadline, then time for its outcome to settle and the catalog to reload.
/// It also bounds waiting for a reply and resending after busy refusals.
const APPLY_WAIT: Duration = Duration::from_secs(90 + 15);

/// The least time between busy refusals and sending the same change again.
const BUSY_RESEND: Duration = Duration::from_secs(1);

/// A staged change to one setting.
#[derive(Clone, Debug, PartialEq)]
pub enum Change {
    /// A value to save. `policy` marks Save Current Value and Save Device
    /// Value, which are changes even when they match the value shown.
    Set {
        value: SettingValue,
        policy: bool,
    },
    Forget,
}

/// Where one change of a Save stands.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Status {
    Pending,
    Saving,
    /// Stored on the dongle; Logitech Features are off, so nothing is applied.
    Saved,
    Applied,
    NotSaved,
    /// Stored, but the device didn't take it.
    NotApplied,
    NotSent,
}

/// One change of a Save.
#[derive(Clone, Debug)]
pub struct Item {
    pub key: SettingKey,
    /// What is sent: a value, or forgetting the saved one.
    pub change: Change,
    pub status: Status,
    pub error: Option<String>,
    /// The staged change this item sends, dropped once it is stored unless
    /// edited since.
    draft: Option<Change>,
    /// A value that didn't apply in an earlier Save, kept for Retry; it is
    /// not part of this Save and is never sent by it.
    kept: bool,
}

/// How far the item being sent has got. `busy` is when the adapter first
/// refused the item as busy, which bounds how long it is sent again.
#[derive(Clone, Copy, Debug)]
enum Step {
    /// Ready to send.
    Next { busy: Option<Instant> },
    /// The request is in flight since `sent`.
    Sending {
        sent: Instant,
        busy: Option<Instant>,
    },
    /// The adapter refused it as busy, last at `at`; it is sent again once
    /// the device's settings work is idle.
    Busy { since: Instant, at: Instant },
    /// Stored; waiting for the device job and the catalog.
    Applying(Instant),
}

/// A device's latest Save, bound to the adapter session it was sent on.
#[derive(Clone, Debug)]
pub struct Submission {
    session: SessionId,
    pub items: Vec<Item>,
    pub running: bool,
    step: Step,
}

impl Submission {
    fn current(&self) -> Option<usize> {
        self.items
            .iter()
            .position(|i| matches!(i.status, Status::Pending | Status::Saving))
    }
    /// Items with this outcome, leaving out failures since resolved.
    fn count(&self, status: Status, d: &Device, c: &DeviceSettings) -> usize {
        self.items
            .iter()
            .filter(|i| i.status == status && !resolved(d, c, i))
            .count()
    }
    fn item(&self, key: SettingKey) -> Option<&Item> {
        self.items.iter().find(|i| i.key == key)
    }
    /// The item's outcome to show, unless it is a failure since resolved.
    fn shown(&self, key: SettingKey, d: &Device, c: &DeviceSettings) -> Option<&Item> {
        self.item(key).filter(|i| !resolved(d, c, i))
    }
}

#[derive(Default)]
pub struct Page {
    /// The saved device whose page is open, or empty.
    pub device: String,
    pub key: Option<SettingKey>,
    pub collapsed: HashSet<&'static str>,
    /// Staged changes by device and key, kept until saved, discarded or the
    /// session ends.
    pub drafts: HashMap<String, HashMap<SettingKey, Change>>,
    /// The latest Save of each device.
    pub saves: HashMap<String, Submission>,
    pub list_scroll: usize,
    pub editor_scroll: usize,
    pub reveal: bool,
    /// Devices whose settings snapshot is being read, and the state each was
    /// last read for.
    pub loading: HashSet<String>,
    pub load_marks: HashMap<String, String>,
    /// Why the open page's last read failed.
    pub load_err: Option<Error>,
    /// Why the last Refresh failed.
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

#[cfg(test)]
impl Page {
    /// Moves the start of a Save's wait for the device back by `by`, as if
    /// that much time had passed.
    pub fn age_apply(&mut self, id: &str, by: Duration) {
        if let Some(save) = self.saves.get_mut(id)
            && let Step::Applying(since) = &mut save.step
        {
            *since -= by;
        }
    }
}

pub(super) fn cache(st: &State, id: &str) -> DeviceSettings {
    st.settings.get(&device_id(id)).cloned().unwrap_or_default()
}

/// Why the page can't save, refresh or stage changes now, or "".
pub(super) fn settings_busy(st: &State, d: &Device, saving: bool) -> &'static str {
    if !st.available {
        return "Lost the Adapter Connection";
    }
    if saving {
        return "Saving Settings…";
    }
    for (command, text) in [
        ("hidpp.setting.set", "Saving Settings…"),
        ("hidpp.setting.forget", "Saving Settings…"),
        ("hidpp.setting.refresh", "Reading Settings…"),
        ("hidpp.setting.apply", "Applying Settings…"),
        ("device.hidpp.set", "Saving Logitech Features…"),
    ] {
        if pending_for(st, command, &d.device_id.0).is_some() {
            return text;
        }
    }
    match d.settings_state {
        SettingsState::Discovering => return "Reading Settings…",
        SettingsState::Applying => return "Applying Settings…",
        _ => {}
    }
    match d.normalization_state {
        NormalizationState::Probing
        | NormalizationState::Configuring
        | NormalizationState::Resetting => "Setting Up Logitech Features…",
        _ => "",
    }
}

/// Whether the device's settings work is idle: no setting request of this
/// session and no adapter job reading or applying settings.
fn device_idle(st: &State, d: &Device) -> bool {
    ![
        "hidpp.setting.set",
        "hidpp.setting.forget",
        "hidpp.setting.refresh",
        "hidpp.setting.apply",
    ]
    .iter()
    .any(|c| pending_for(st, c, &d.device_id.0).is_some())
        && !matches!(
            d.settings_state,
            SettingsState::Discovering | SettingsState::Applying
        )
}

/// Whether an observation is a current reading of the device. Readings are
/// not current while it is disconnected, Logitech Features are being reset,
/// or the cache is being updated. Logitech Features off does not stop reading.
fn fresh(d: &Device, c: &DeviceSettings, s: &Setting) -> bool {
    s.fresh
        && s.observed != SettingValue::Null
        && c.current
        && connected(d)
        && d.normalization_state != NormalizationState::Resetting
}

fn fresh_words(d: &Device, c: &DeviceSettings, s: &Setting) -> &'static str {
    let reason = if !connected(d) {
        "Disconnected"
    } else if d.normalization_state == NormalizationState::Resetting {
        "Resetting"
    } else if !c.current {
        "Updating"
    } else {
        ""
    };
    match () {
        _ if s.observed == SettingValue::Null && !reason.is_empty() => reason,
        _ if s.observed == SettingValue::Null => "Not Read",
        _ if fresh(d, c, s) && s.observation_source == Some(ObservationSource::Event) => {
            "Reported by Device"
        }
        _ if fresh(d, c, s) => "Read from Device",
        _ => "Last Known",
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
            let unit = catalog::info_for(s.key).0.unit;
            match v {
                SettingValue::Integer(255) if s.key == SettingKey::WheelThreshold => "Off".into(),
                SettingValue::Integer(n) if unit.is_empty() => n.to_string(),
                SettingValue::Integer(n) => format!("{n} {unit}"),
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
        SettingScope::CurrentHost => "This Computer",
        SettingScope::Device => "Whole Device",
    }
}

/// An integer setting's range, such as "5-300, Steps of 5".
fn range_words(s: &Setting) -> Option<String> {
    if s.kind != SettingType::Integer || !s.choices.is_empty() {
        return None;
    }
    let range = match (s.min, s.max) {
        (Some(min), Some(max)) => format!("{min}-{max}"),
        (Some(min), None) => format!("At Least {min}"),
        (None, Some(max)) => format!("At Most {max}"),
        (None, None) => return None,
    };
    Some(match s.step.filter(|n| *n > 1) {
        Some(step) => format!("{range}, Steps of {step}"),
        None => range,
    })
}

/// A saved setting's state. With Logitech Features off a saved value is
/// only stored.
fn status_words(d: &Device, s: &Setting) -> (&'static str, Style) {
    use SettingState::*;
    match s.state {
        Pending | Applying if !d.hidpp_enabled => ("Saved", ok()),
        Pending => ("Pending", warn()),
        Applying => ("Applying…", warn()),
        Applied => ("Applied", ok()),
        ChangedOnDevice => ("Changed on Device", warn()),
        Unsupported => ("Can't Apply Now", dim()),
        Error => ("Failed", err()),
        Uncertain => ("Unconfirmed", warn()),
        Unmanaged => ("Not Saved", layout::plain()),
    }
}

/// The value the device keeps when nothing is staged: the saved value, else
/// the reading.
fn base(s: &Setting) -> &SettingValue {
    if s.managed { &s.desired } else { &s.observed }
}

/// A staged change that would change something; None when it matches what
/// is saved, or forgets a value that isn't saved.
fn effective(s: &Setting, draft: Option<&Change>) -> Option<Change> {
    match draft? {
        Change::Forget if s.managed => Some(Change::Forget),
        Change::Forget => None,
        Change::Set { value, .. } if !s.writable || !s.accepts(value) => None,
        Change::Set { value, policy } if !policy && value == base(s) => None,
        change => Some(change.clone()),
    }
}

/// Whether the current catalog already shows a failed change took effect,
/// as after a reconnect or Refresh, so its failure is no longer news.
fn resolved(d: &Device, c: &DeviceSettings, item: &Item) -> bool {
    if !matches!(item.status, Status::NotSaved | Status::NotApplied) {
        return false;
    }
    let Some(s) = c.settings.iter().find(|s| s.key == item.key) else {
        return false;
    };
    match &item.change {
        Change::Forget => !s.managed,
        Change::Set { value, .. } => {
            s.managed && s.desired == *value && s.state == SettingState::Applied && fresh(d, c, s)
        }
    }
}

/// A Save outcome as the page shows it.
fn outcome_words(item: &Item) -> Option<(String, Style)> {
    let reason = item
        .error
        .as_ref()
        .map(|e| format!(": {e}"))
        .unwrap_or_default();
    Some(match item.status {
        Status::NotSaved => (format!("✕ Couldn't Save{reason}"), err()),
        Status::NotApplied => (format!("✕ Didn't Apply{reason}"), err()),
        Status::NotSent => (format!("○ Not Sent{reason}"), dim()),
        _ => return None,
    })
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
        self.page.device = id;
        self.page.key = None;
        self.page.list_scroll = 0;
        self.page.editor_scroll = 0;
        self.page.load_err = None;
        self.page.load_marks.remove(&self.page.device);
        self.page.job_note.clear();
        self.focus = None;
    }

    /// Leaves the settings page. Staged changes are kept, and a running Save
    /// goes on.
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
        self.page.saves.remove(id);
        let prefix = format!("{id}/");
        self.page
            .setting_states
            .retain(|k, _| !k.starts_with(&prefix));
    }

    pub(super) fn saving(&self, id: &str) -> bool {
        self.page.saves.get(id).is_some_and(|s| s.running)
    }

    /// Keeps the settings cache current for the open page and for every
    /// device with a running Save: loads the snapshot when there is none or
    /// it was invalidated, such as by missed notifications or grouped
    /// changes. A failed or still outdated load is retried only once the
    /// device or the adapter's revision changes.
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
            .filter(|id| !cache(&st, id).loaded && !self.saving(id))
            .cloned()
            .collect();
        for id in dropped {
            self.forget_device(&id);
        }
        if !self.page.device.is_empty()
            && Self::find(&st, &self.page.device).0.is_none()
            && st.current
        {
            self.close_settings(); // The device was removed.
        }
        let mut ids: Vec<String> = self
            .page
            .saves
            .iter()
            .filter(|(_, s)| s.running)
            .map(|(id, _)| id.clone())
            .collect();
        if !self.page.device.is_empty() && !ids.contains(&self.page.device) {
            ids.push(self.page.device.clone());
        }
        // Loading is automatic, so only an adapter offering it is asked.
        if !st.available || !self.offers(&st, &Action::SettingsReload) {
            return;
        }
        for id in ids {
            let Some(d) = Self::find(&st, &id).0.cloned() else {
                continue;
            };
            if self.page.loading.contains(&id) || cache(&st, &id).current {
                continue;
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
            if self.page.load_marks.get(&id) == Some(&mark) {
                continue;
            }
            self.page.load_marks.insert(id.clone(), mark);
            self.load_settings(&id);
        }
    }

    /// Reads a device's settings snapshot, unless a read is under way.
    fn load_settings(&mut self, id: &str) {
        if !self.page.loading.insert(id.to_owned()) {
            return;
        }
        let mut job = Job::new(Command::Settings(id.to_owned()));
        job.load = true;
        self.execute_job(job);
    }

    fn draft(&self, id: &str, key: SettingKey) -> Option<&Change> {
        self.page.drafts.get(id)?.get(&key)
    }

    /// The value the controls show: the staged value, else the saved value,
    /// else a legal current reading. An observed value that cannot be set,
    /// such as a temporary mode, is never offered as chosen.
    fn edit_value(&self, s: &Setting) -> SettingValue {
        match self.draft(&self.page.device, s.key) {
            Some(Change::Set { value, .. }) => return value.clone(),
            Some(Change::Forget) => {}
            None if s.managed => return s.desired.clone(),
            None => {}
        }
        if s.accepts(&s.observed) {
            s.observed.clone()
        } else {
            SettingValue::Null
        }
    }

    /// Stages a value from a control. Choosing what the device keeps anyway
    /// drops the draft, except that a staged save of an unsaved value stays
    /// one; nothing is sent until Save.
    fn set_draft(&mut self, s: &Setting, v: SettingValue) {
        if !s.accepts(&v) {
            return;
        }
        let drafts = self
            .page
            .drafts
            .entry(self.page.device.clone())
            .or_default();
        let policy =
            !s.managed && matches!(drafts.get(&s.key), Some(Change::Set { policy: true, .. }));
        if !policy && v == *base(s) {
            drafts.remove(&s.key);
        } else {
            drafts.insert(s.key, Change::Set { value: v, policy });
        }
    }

    /// The staged changes that would change something, in display order.
    fn changes(&self, id: &str, c: &DeviceSettings) -> Vec<(SettingKey, Change)> {
        catalog::presented(&c.settings)
            .into_iter()
            .filter_map(|s| Some((s.key, effective(s, self.draft(id, s.key))?)))
            .collect()
    }

    /// What Save sends: the staged changes, or while the device is
    /// disconnected only those forgetting a saved value; other drafts stay.
    fn to_save(&self, d: &Device, c: &DeviceSettings) -> Vec<(SettingKey, Change)> {
        let mut changes = self.changes(&d.device_id.0, c);
        if !connected(d) {
            changes.retain(|(_, change)| *change == Change::Forget);
        }
        changes
    }

    /// The not-applied values of the device's latest Save that can be sent
    /// again: each still the saved value, one the setting takes, and not
    /// changed by a staged draft. Other drafts stay staged.
    fn retryable(&self, st: &State, d: &Device) -> Vec<(SettingKey, Change)> {
        let Some(save) = self.page.saves.get(&d.device_id.0).filter(|s| !s.running) else {
            return Vec::new();
        };
        let c = cache(st, &d.device_id.0);
        save.items
            .iter()
            .filter(|i| i.status == Status::NotApplied && !resolved(d, &c, i))
            .filter_map(|i| {
                let Change::Set { value, .. } = &i.change else {
                    return None;
                };
                let s = c.settings.iter().find(|s| s.key == i.key)?;
                (s.writable
                    && s.managed
                    && s.state != SettingState::Unsupported
                    && s.desired == *value
                    && s.accepts(value)
                    && effective(s, self.draft(&d.device_id.0, i.key)).is_none())
                .then(|| (i.key, i.change.clone()))
            })
            .collect()
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

    /// A setting row's tag: its Save progress or outcome, a staged change,
    /// else its saved state.
    fn row_tag(&self, d: &Device, c: &DeviceSettings, s: &Setting) -> (&'static str, Style) {
        use SettingState::*;
        let save = self.page.saves.get(&d.device_id.0);
        let item = save.and_then(|save| save.shown(s.key, d, c));
        let staged = effective(s, self.draft(&d.device_id.0, s.key)).is_some();
        match () {
            _ if item.is_some_and(|i| i.status == Status::Saving) => ("◌ Sending", warn()),
            _ if item.is_some_and(|i| i.status == Status::Pending) => ("◌ Queued", warn()),
            _ if staged => ("✎ Changed", layout::accent()),
            _ if item.is_some_and(|i| i.status == Status::NotSaved) => ("✕ Couldn't Save", err()),
            _ if item.is_some_and(|i| i.status == Status::NotApplied) => ("✕ Didn't Apply", err()),
            _ if !s.writable => ("", layout::plain()),
            _ if !s.managed && s.error.is_some() => ("✕ Read Failed", err()),
            _ if !s.managed => ("○ Not Saved", dim()),
            _ if s.state == Error => ("✕ Failed", err()),
            _ if s.state == Uncertain => ("? Unconfirmed", warn()),
            _ if s.state == Unsupported => ("○ Can't Apply Now", dim()),
            _ if s.state == ChangedOnDevice && fresh(d, c, s) => ("◆ Changed on Device", warn()),
            _ => ("● Saved", ok()),
        }
    }

    pub(super) fn settings_pane(&mut self, st: &State, w: usize, h: usize) -> Layout {
        let d = Self::find(st, &self.page.device).0.unwrap().clone();
        let id = d.device_id.0.clone();
        let c = cache(st, &id);
        let mut b = Layout::new(w.saturating_sub(4));
        let mut pinned = Layout::new(w.saturating_sub(4));
        let (text, look) = device_status(&d);
        b.line(Line::from(vec![
            span(text, look),
            span(
                format!(" · Logitech Features {}", on_off(d.hidpp_enabled)),
                dim(),
            ),
        ]));
        if let Some((t, l)) = settings_status(&d) {
            b.para(&t, l);
        }
        if let Some(e) = &self.page.load_err {
            b.para(
                &format!("✕ Couldn't Load Settings: {}", text::error_words(e)),
                err(),
            );
            b.button("Retry", Action::SettingsReload, Tone::Normal);
        } else if !c.loaded {
            b.line(styled(format!("{} Loading Settings…", spinner()), warn()));
        } else if !c.current {
            b.line(styled(format!("{} Updating…", spinner()), warn()));
        }
        let known = catalog::presented(&c.settings);
        if c.loaded && c.current && self.page.load_err.is_none() && known.is_empty() {
            b.para("No Settings", dim());
        }
        let busy = settings_busy(st, &d, self.saving(&id));
        let inner = b.width;
        let tag_w = if inner >= 64 { 20 } else { 14 };
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
                let (row_base, look, marker) = if Some(s.key) == self.page.key {
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
                let shown = match self.draft(&id, s.key) {
                    Some(Change::Set { value, .. }) => value.clone(),
                    None if s.managed => s.desired.clone(),
                    _ => s.observed.clone(),
                };
                let value = human_value(s, &shown);
                let value_look = if !busy.is_empty() || !fresh(&d, &c, s) && !s.managed {
                    dim()
                } else {
                    layout::plain()
                };
                let (tag, tag_look) = self.row_tag(&d, &c, s);
                let label = layout::truncate_str(&display(catalog::label(s.key)), label_w - 1);
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
        let save = self.page.saves.get(&id);
        if !busy.is_empty() {
            let progress = save.filter(|s| s.running).map_or(String::new(), |s| {
                let sent = s.items.iter().filter(|i| !i.kept).count();
                let at = s.current().unwrap_or(sent - 1);
                format!(" {} of {sent}", at + 1)
            });
            pinned.para(&format!("{} {busy}{progress}", spinner()), warn());
        } else if let Some(save) = save {
            let counts: Vec<String> = [
                ("Couldn't Save", save.count(Status::NotSaved, &d, &c)),
                ("Didn't Apply", save.count(Status::NotApplied, &d, &c)),
                ("Not Sent", save.count(Status::NotSent, &d, &c)),
            ]
            .iter()
            .filter(|(_, n)| *n > 0)
            .map(|(t, n)| format!("{t} {n}"))
            .collect();
            if !counts.is_empty() {
                pinned.para(&format!("✕ {}", counts.join(" · ")), err());
            }
        }
        if busy.is_empty() && !self.page.job_note.is_empty() {
            pinned.para(&self.page.job_note.clone(), self.page.job_look);
        }
        pinned.row();
        let idle = busy.is_empty();
        // Offline, only forgetting a saved value reaches the adapter.
        let can_save = idle && !self.to_save(&d, &c).is_empty();
        if self.offers(st, &Action::SaveAll) {
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
        }
        if self.offers(st, &Action::SettingsRefresh) {
            let can = idle && connected(&d) && c.loaded;
            button_if(
                &mut pinned,
                "Refresh",
                Action::SettingsRefresh,
                Tone::Normal,
                can,
            );
        }
        if save.is_some_and(|s| !s.running && s.count(Status::NotApplied, &d, &c) > 0) {
            let can =
                idle && connected(&d) && d.hidpp_enabled && !self.retryable(st, &d).is_empty();
            button_if(&mut pinned, "Retry", Action::RetrySave, Tone::Normal, can);
        }
        pinned.button_right("‹ Back", Action::SettingsBack, Tone::Normal);
        let title = format!("Settings · {}", display_name(d.name.as_deref()));
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
        let id = &d.device_id.0;
        let c = cache(st, id);
        let Some(s) = self
            .page
            .key
            .and_then(|k| c.settings.iter().find(|s| s.key == k))
        else {
            b.para("No Setting Selected", dim());
            return ("Setting".into(), b, actions);
        };
        let field = |b: &mut Layout, key: &str, value: &str, st: Style| {
            b.field_at(key, EDITOR_KEY, value, st);
        };
        let is_fresh = fresh(d, &c, s);
        let look = if is_fresh { layout::plain() } else { dim() };
        field(
            &mut b,
            "Current",
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
        let title = display(catalog::label(s.key));
        if !s.writable {
            if let Some(code) = s.error {
                field(&mut b, "Read Failed", &text::hidpp_words(code), err());
            }
            return (title, b, actions);
        }
        if s.managed {
            field(
                &mut b,
                "Saved",
                &human_value(s, &s.desired),
                layout::plain(),
            );
        } else {
            field(&mut b, "Saved", "Not Saved", dim());
        }
        field(&mut b, "Applies To", scope_words(s), layout::plain());
        if let Some(range) = range_words(s) {
            field(&mut b, "Range", &range, layout::plain());
        }
        if s.managed {
            let (text, look) = status_words(d, s);
            field(&mut b, "Status", text, look);
        }
        if let Some(code) = s.error {
            let key = if s.managed { "Error" } else { "Read Failed" };
            field(&mut b, key, &text::hidpp_words(code), err());
        }
        b.row();
        // Controls stay in place while unavailable, dim and without targets.
        // An adapter that can't save values offers no value controls at all.
        let busy = settings_busy(st, d, self.saving(id));
        let locked = !busy.is_empty() || !connected(d);
        let editable = self.offers(st, &Action::Draft(s.key, SettingValue::Null));
        let (first_line, first_hit) = (b.lines.len(), b.hits.len());
        let value = self.edit_value(s);
        let choice = |label: String, action: Action, chosen: bool| Choice {
            label,
            action,
            chosen,
        };
        if s.kind == SettingType::Bool {
            let on = match value {
                SettingValue::Bool(b) => Some(b),
                _ => None,
            };
            let turn = Action::Draft(s.key, SettingValue::Bool(on != Some(true)));
            b.toggle("Value", EDITOR_KEY, on, turn);
        } else if !s.choices.is_empty() {
            let options = s
                .choices
                .iter()
                .map(|v| {
                    choice(
                        human_value(s, v),
                        Action::Draft(s.key, v.clone()),
                        value == *v,
                    )
                })
                .collect();
            b.choice("Value", EDITOR_KEY, options);
        } else if s.kind == SettingType::Integer {
            if s.key == SettingKey::WheelThreshold {
                let n = match value {
                    SettingValue::Integer(n) => Some(n),
                    _ => None,
                };
                let on = n.is_some_and(|n| n != 255);
                let turn = if on {
                    SettingValue::Integer(255)
                } else {
                    smartshift_on(s, &value)
                };
                b.toggle(
                    "Value",
                    EDITOR_KEY,
                    n.map(|_| on),
                    Action::Switch(s.key, turn),
                );
                if on {
                    stepper(&mut b, "Threshold", &smartshift_range(s), &value);
                }
            } else {
                stepper(&mut b, "Value", s, &value);
            }
        } else {
            field(&mut b, "Value", &human_value(s, &s.observed), look);
        }
        if !editable {
            b.hits.truncate(first_hit);
            b.lines.truncate(first_line);
        } else if locked {
            b.hits.truncate(first_hit);
            for line in &mut b.lines[first_line..] {
                *line = styled(layout::strip(line), dim());
            }
        }
        if let Some(item) = self
            .page
            .saves
            .get(id)
            .and_then(|save| save.shown(s.key, d, &c))
            && let Some((text, look)) = outcome_words(item)
        {
            b.row();
            b.para(&text, look);
        }
        // The staging actions: each stages a change, and nothing is sent until Save.
        let staging = busy.is_empty();
        actions.row();
        let staged = effective(s, self.draft(id, s.key)).is_some();
        let settable = is_fresh && s.accepts(&s.observed);
        if staged {
            button_if(
                &mut actions,
                "Undo Change",
                Action::Undo(s.key),
                Tone::Normal,
                staging,
            );
        } else if !s.managed {
            if self.offers(st, &Action::Keep(s.key)) {
                let can = staging && settable;
                button_if(
                    &mut actions,
                    "Save Current Value",
                    Action::Keep(s.key),
                    Tone::Normal,
                    can,
                );
            }
        } else {
            if s.state == SettingState::ChangedOnDevice && self.offers(st, &Action::Keep(s.key)) {
                let can = staging && settable;
                button_if(
                    &mut actions,
                    "Save Device Value",
                    Action::Keep(s.key),
                    Tone::Normal,
                    can,
                );
            }
            if self.offers(st, &Action::Forget(s.key)) {
                button_if(
                    &mut actions,
                    "Forget Saved Value",
                    Action::Forget(s.key),
                    Tone::Normal,
                    staging,
                );
            }
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
        let id = d.device_id.0.clone();
        let c = cache(&st, &id);
        let setting = |key: &SettingKey| c.settings.iter().find(|s| s.key == *key).cloned();
        let busy = !settings_busy(&st, &d, self.saving(&id)).is_empty();
        let offline = !connected(&d);
        match action {
            Action::SettingsBack => self.close_settings(),
            Action::SettingsReload => {
                self.page.load_err = None;
                self.page.load_marks.remove(&id);
            }
            Action::SettingsRefresh => {
                if busy || offline {
                    return;
                }
                self.page.job_note.clear();
                self.execute(Command::SettingsRefresh(id));
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
            // Values change only while the device is connected and nothing runs.
            Action::Draft(..) | Action::Switch(..) | Action::Step(..) | Action::Keep(_)
                if offline || busy => {}
            Action::Undo(_)
            | Action::Forget(_)
            | Action::Discard
            | Action::SaveAll
            | Action::RetrySave
                if busy => {}
            Action::Draft(key, v) | Action::Switch(key, v) => {
                if let Some(s) = setting(&key) {
                    self.set_draft(&s, v);
                }
            }
            Action::Step(key, delta) => {
                if let Some(s) = setting(&key) {
                    let from = self.edit_value(&s);
                    let range = if key == SettingKey::WheelThreshold {
                        smartshift_range(&s)
                    } else {
                        s.clone()
                    };
                    self.set_draft(&s, step_value(&range, &from, delta));
                }
            }
            Action::Keep(key) => {
                if let Some(s) = setting(&key)
                    && fresh(&d, &c, &s)
                    && s.accepts(&s.observed)
                {
                    let change = Change::Set {
                        value: s.observed.clone(),
                        policy: true,
                    };
                    self.page.drafts.entry(id).or_default().insert(key, change);
                }
            }
            Action::Forget(key) => {
                if setting(&key).is_some_and(|s| s.managed) {
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
                let changes = self.to_save(&d, &c);
                if changes.is_empty() {
                    return;
                }
                self.submit(&id, changes, true);
            }
            Action::RetrySave => {
                let retry = self.retryable(&st, &d);
                if offline || !d.hidpp_enabled || retry.is_empty() {
                    return;
                }
                self.submit(&id, retry, false);
            }
            _ => {}
        }
    }

    /// Starts a Save of `changes`, replacing the device's previous outcome
    /// except for values that didn't apply to other settings, which stay for
    /// Retry. With `staged`, each item remembers the draft it sends.
    fn submit(&mut self, id: &str, changes: Vec<(SettingKey, Change)>, staged: bool) {
        let Some(session) = self.session else {
            return;
        };
        let kept: Vec<Item> = match (self.page.saves.get(id), self.state()) {
            (Some(save), Some(st)) if save.session == session => {
                let c = cache(&st, id);
                let d = Self::find(&st, id).0;
                save.items
                    .iter()
                    .filter(|i| i.status == Status::NotApplied)
                    .filter(|i| !changes.iter().any(|(key, _)| *key == i.key))
                    .filter(|i| d.is_none_or(|d| !resolved(d, &c, i)))
                    .map(|i| Item {
                        draft: None,
                        kept: true,
                        ..i.clone()
                    })
                    .collect()
            }
            _ => Vec::new(),
        };
        let mut items: Vec<Item> = changes
            .into_iter()
            .map(|(key, change)| Item {
                key,
                draft: staged.then(|| self.draft(id, key).cloned()).flatten(),
                change: match change {
                    Change::Set { value, .. } => Change::Set {
                        value,
                        policy: false,
                    },
                    Change::Forget => Change::Forget,
                },
                status: Status::Pending,
                error: None,
                kept: false,
            })
            .collect();
        items.extend(kept);
        self.page.saves.insert(
            id.to_owned(),
            Submission {
                session,
                items,
                running: true,
                step: Step::Next { busy: None },
            },
        );
        self.page.job_note.clear();
        self.sync_saves();
    }

    /// Advances every running Save as far as the adapter's state allows.
    pub(super) fn sync_saves(&mut self) {
        let ids: Vec<String> = self
            .page
            .saves
            .iter()
            .filter(|(_, s)| s.running)
            .map(|(id, _)| id.clone())
            .collect();
        for id in ids {
            self.advance(&id);
        }
    }

    /// Settles the current item of a device's Save, sends the next one, or
    /// ends the Save.
    fn advance(&mut self, id: &str) {
        loop {
            let Some(save) = self.page.saves.get(id).filter(|s| s.running) else {
                return;
            };
            let st = self
                .state()
                .filter(|st| Some(save.session) == self.session && st.available);
            let Some(st) = st else {
                return self.stop(id, "Lost the adapter connection");
            };
            let Some(i) = save.current() else {
                return self.finish_save(id);
            };
            let Some(d) = Self::find(&st, id).0.cloned() else {
                return self.stop(id, "The device was removed");
            };
            let item = save.items[i].clone();
            match save.step {
                Step::Sending { sent, .. } => {
                    // The outcome of a request without a reply is unknown.
                    if sent.elapsed() > APPLY_WAIT {
                        self.settle(id, i, Status::NotSaved, Some("No reply".into()));
                        return self.stop(id, "");
                    }
                    return;
                }
                Step::Busy { since, at } => {
                    if since.elapsed() > APPLY_WAIT {
                        let why = Some("The device stayed busy".into());
                        self.settle(id, i, Status::NotSaved, why);
                        return self.stop(id, "");
                    }
                    if !device_idle(&st, &d) || at.elapsed() < BUSY_RESEND {
                        return;
                    }
                    self.set_step(id, Step::Next { busy: Some(since) });
                }
                Step::Applying(since) => {
                    if !connected(&d) {
                        self.settle(id, i, Status::NotApplied, Some("Disconnected".into()));
                        return self.stop(id, "The device disconnected");
                    }
                    let c = cache(&st, id);
                    let row = c.settings.iter().find(|s| s.key == item.key);
                    let Change::Set { value, .. } = &item.change else {
                        self.settle(id, i, Status::Saved, None);
                        continue;
                    };
                    // The job has ended and the catalog shows its outcome for
                    // the value sent, not an earlier row state.
                    let settled = row.filter(|s| {
                        c.current
                            && s.managed
                            && s.desired == *value
                            && !matches!(s.state, SettingState::Pending | SettingState::Applying)
                    });
                    match settled {
                        Some(s) if device_idle(&st, &d) => {
                            let (status, error) = if s.state == SettingState::Applied {
                                (Status::Applied, None)
                            } else {
                                let why = match s.error {
                                    Some(code) => text::hidpp_words(code),
                                    None => status_words(&d, s).0.into(),
                                };
                                (Status::NotApplied, Some(why))
                            };
                            self.settle(id, i, status, error);
                        }
                        _ if since.elapsed() > APPLY_WAIT => {
                            self.settle(id, i, Status::NotApplied, Some("Timed out".into()));
                            // The outcome event may have been missed: read the
                            // settings again, so a value that did apply shows.
                            if self.offers(&st, &Action::SettingsReload) {
                                self.load_settings(id);
                            }
                            return self.stop(id, "");
                        }
                        _ => return,
                    }
                }
                Step::Next { busy } => {
                    if let Some(why) = self.hold(&st, &d, id, &item) {
                        self.settle(id, i, Status::NotSent, Some(why.into()));
                        continue;
                    }
                    if item.change != Change::Forget && !connected(&d) {
                        return self.stop(id, "The device disconnected");
                    }
                    let command = match &item.change {
                        Change::Set { value, .. } => Command::SettingSet(
                            id.to_owned(),
                            item.key,
                            SettingInput::Value(value.clone()),
                        ),
                        Change::Forget => Command::SettingForget(id.to_owned(), item.key),
                    };
                    if let Some(save) = self.page.saves.get_mut(id) {
                        save.items[i].status = Status::Saving;
                        save.step = Step::Sending {
                            sent: Instant::now(),
                            busy,
                        };
                    }
                    let mut job = Job::new(command);
                    job.save = true;
                    self.execute_job(job);
                    return;
                }
            }
        }
    }

    /// Why an item must stay unsent: the Manual Backlight Level applies only
    /// in permanent manual mode, so a level saved with a Backlight Mode
    /// change waits for a new mode to be stored, and with Logitech Features
    /// on, for the device to report permanent manual, whether or not this
    /// Save changes the mode. A device without a Backlight Mode setting has
    /// nothing to wait for, and with Logitech Features off a level is only
    /// stored. Forgetting the mode changes nothing on the device, so only the
    /// reported mode counts then.
    fn hold(&self, st: &State, d: &Device, id: &str, item: &Item) -> Option<&'static str> {
        if item.key != SettingKey::BacklightLevel {
            return None;
        }
        let save = self.page.saves.get(id)?;
        if let Some(mode) = save.item(SettingKey::BacklightMode).filter(|i| !i.kept)
            && matches!(mode.change, Change::Set { .. })
        {
            match mode.status {
                Status::Applied => {}
                Status::Saved if !d.hidpp_enabled => {}
                // Stored, but the device didn't take it.
                Status::NotApplied => return Some("Backlight Mode isn't permanent manual"),
                _ => return Some("Backlight Mode wasn't saved"),
            }
        }
        if !d.hidpp_enabled {
            return None;
        }
        let c = cache(st, id);
        let mode = c
            .settings
            .iter()
            .find(|s| s.key == SettingKey::BacklightMode)?;
        let manual =
            fresh(d, &c, mode) && mode.observed == SettingValue::Text("permanent_manual".into());
        (!manual).then_some("Backlight Mode isn't permanent manual")
    }

    fn set_step(&mut self, id: &str, step: Step) {
        if let Some(save) = self.page.saves.get_mut(id) {
            save.step = step;
        }
    }

    /// Records an item's outcome. A stored change drops the draft it sent,
    /// unless it was edited since.
    fn settle(&mut self, id: &str, i: usize, status: Status, error: Option<String>) {
        let Some(save) = self.page.saves.get_mut(id) else {
            return;
        };
        let item = &mut save.items[i];
        item.status = status;
        item.error = error;
        save.step = Step::Next { busy: None };
        let (key, draft) = (item.key, item.draft.clone());
        let label = display(catalog::label(key));
        let reason = save.items[i]
            .error
            .as_ref()
            .map(|e| format!(": {e}"))
            .unwrap_or_default();
        if matches!(status, Status::Saved | Status::Applied | Status::NotApplied)
            && let Some(drafts) = self.page.drafts.get_mut(id)
            && draft.is_some()
            && drafts.get(&key) == draft.as_ref()
        {
            drafts.remove(&key);
        }
        let name = self.label(id);
        match status {
            Status::NotSaved => {
                self.note(
                    super::activity::Kind::Bad,
                    format!("Couldn't save {label} for {name}{reason}"),
                );
            }
            Status::NotApplied => self.note(
                super::activity::Kind::Bad,
                format!("Saved {label} for {name}, but it didn't apply{reason}"),
            ),
            _ => {}
        }
    }

    /// Ends a device's Save early: the item in progress is settled by how far
    /// it got, and the rest stay unsent with their drafts.
    fn stop(&mut self, id: &str, why: &str) {
        let Some(save) = self.page.saves.get(id) else {
            return;
        };
        let step = save.step;
        if let Some(i) = save.current()
            && save.items[i].status == Status::Saving
        {
            let reason = (!why.is_empty()).then(|| why.to_owned());
            match step {
                Step::Applying(_) => self.settle(id, i, Status::NotApplied, reason),
                _ => self.settle(id, i, Status::NotSaved, reason),
            }
        }
        if let Some(save) = self.page.saves.get_mut(id) {
            for item in &mut save.items {
                if item.status == Status::Pending {
                    item.status = Status::NotSent;
                }
            }
            save.running = false;
        }
        self.finish_save(id);
    }

    fn finish_save(&mut self, id: &str) {
        if let Some(save) = self.page.saves.get_mut(id) {
            save.running = false;
        }
    }

    /// Records a Save item's reply. A busy device is retried once its
    /// settings work is idle; a lost device or session stops the Save; any
    /// other refusal fails only this item.
    fn save_result(&mut self, id: &str, key: SettingKey, result: &Result<Outcome, Failure>) {
        let Some(save) = self.page.saves.get(id).filter(|s| s.running) else {
            return;
        };
        let Some(i) = save.current().filter(|i| save.items[*i].key == key) else {
            return;
        };
        let Step::Sending { busy, .. } = save.step else {
            return;
        };
        let set = matches!(save.items[i].change, Change::Set { .. });
        match result {
            Err(f) => {
                let code = f.error.wire.as_ref().map(|w| w.code);
                let words = text::error_words(&f.error);
                match code {
                    Some(ErrorCode::Busy) => {
                        let at = Instant::now();
                        let since = busy.unwrap_or(at);
                        self.set_step(id, Step::Busy { since, at });
                    }
                    Some(ErrorCode::NotConnected) | None => {
                        self.settle(id, i, Status::NotSaved, Some(words));
                        self.stop(id, "");
                    }
                    Some(_) => self.settle(id, i, Status::NotSaved, Some(words)),
                }
            }
            Ok(_) => {
                let hidpp = self
                    .state()
                    .and_then(|st| Self::find(&st, id).0.map(|d| d.hidpp_enabled))
                    .unwrap_or(false);
                if set && hidpp {
                    self.set_step(id, Step::Applying(Instant::now()));
                } else {
                    // Forgetting sends nothing to the device, and with
                    // Logitech Features off a set is only stored.
                    self.settle(id, i, Status::Saved, None);
                }
            }
        }
        self.advance(id);
    }

    /// Records a settings command's outcome on the page.
    pub(super) fn settings_result(&mut self, job: &Job, result: &Result<Outcome, Failure>) {
        match &job.command {
            Command::SettingSet(id, key, _) | Command::SettingForget(id, key) if job.save => {
                self.save_result(id, *key, result);
            }
            Command::SettingsRefresh(_) => {
                (self.page.job_note, self.page.job_look) = match result {
                    Err(f) => {
                        let summary = match f.partial.as_deref() {
                            Some(Outcome::Job {
                                counts: Some(c), ..
                            }) => catalog::failure_summary(c),
                            _ => String::new(),
                        };
                        let mut note = format!("✕ {}", text::error_words(&f.error));
                        if !summary.is_empty() {
                            note = format!("{note} ({summary})");
                        }
                        (note, err())
                    }
                    Ok(_) => (String::new(), layout::plain()),
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

    /// Left and Right edit the selected setting: the previous or next choice,
    /// or a fine step. Space turns a toggle over.
    pub(super) fn edit_selected(&mut self, delta: i64, toggle: bool) {
        let Some(st) = self.state() else {
            return;
        };
        let c = cache(&st, &self.page.device);
        let Some(s) = self
            .page
            .key
            .and_then(|k| c.settings.iter().find(|s| s.key == k))
            .cloned()
        else {
            return;
        };
        if !s.writable {
            return;
        }
        let value = self.edit_value(&s);
        let action = match s.kind {
            SettingType::Bool => {
                let next = match (value, toggle) {
                    (SettingValue::Bool(b), true) => !b,
                    (_, true) => true,
                    // On comes first: Left chooses On and Right Off.
                    (_, false) => delta < 0,
                };
                Action::Draft(s.key, SettingValue::Bool(next))
            }
            _ if s.key == SettingKey::WheelThreshold && toggle => {
                let on = matches!(value, SettingValue::Integer(n) if n != 255);
                let next = if on {
                    SettingValue::Integer(255)
                } else {
                    smartshift_on(&s, &value)
                };
                Action::Switch(s.key, next)
            }
            _ if toggle => return,
            _ if !s.choices.is_empty() => {
                let n = s.choices.len() as i64;
                let at = s.choices.iter().position(|c| *c == value);
                let i = match at {
                    Some(i) => (i as i64 + delta).clamp(0, n - 1),
                    None if delta < 0 => n - 1,
                    None => 0,
                };
                Action::Draft(s.key, s.choices[i as usize].clone())
            }
            SettingType::Integer => {
                if s.key == SettingKey::WheelThreshold
                    && !matches!(value, SettingValue::Integer(n) if n != 255)
                {
                    return;
                }
                let (fine, _) = steps(&s);
                Action::Step(s.key, delta.signum() * fine)
            }
            _ => return,
        };
        self.action(action);
    }

    /// Backspace drops the selected setting's staged change.
    pub(super) fn undo_selected(&mut self) {
        if let Some(key) = self.page.key {
            self.action(Action::Undo(key));
        }
    }
}

/// SmartShift's threshold range while it is On: its metadata without the
/// value the device takes as Off.
fn smartshift_range(s: &Setting) -> Setting {
    let mut r = s.clone();
    r.max = Some(r.max.unwrap_or(254).min(254));
    r
}

/// The threshold SmartShift turns On with: the one shown, else the saved or
/// read one, else the highest.
fn smartshift_on(s: &Setting, value: &SettingValue) -> SettingValue {
    [value, &s.desired, &s.observed]
        .into_iter()
        .find(|v| matches!(v, SettingValue::Integer(k) if (1..=254).contains(k)))
        .cloned()
        .unwrap_or(SettingValue::Integer(254))
}

/// Fine and coarse increments around an integer value, and the range ends.
fn stepper(b: &mut Layout, key: &str, s: &Setting, value: &SettingValue) {
    let (fine, coarse) = steps(s);
    let n = match value {
        SettingValue::Integer(n) => Some(*n),
        _ => None,
    };
    let at_min = n.is_some_and(|n| s.min.is_some_and(|m| n <= m));
    let at_max = n.is_some_and(|n| s.max.is_some_and(|m| n >= m));
    b.line(styled(pad_str(key, EDITOR_KEY - 1), dim())); // Buttons add a space.
    let control = |b: &mut Layout, label: String, delta: i64, enabled: bool| {
        button_if(b, &label, Action::Step(s.key, delta), Tone::Normal, enabled);
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
    let unit = catalog::info_for(s.key).0.unit;
    if !unit.is_empty() {
        b.label(unit, dim());
    }
    if let (Some(min), Some(max)) = (s.min, s.max) {
        b.line(Line::from(pad_str("", EDITOR_KEY - 1)));
        let draft = |n| Action::Draft(s.key, SettingValue::Integer(n));
        button_if(b, &format!("Min {min}"), draft(min), Tone::Normal, !at_min);
        button_if(b, &format!("Max {max}"), draft(max), Tone::Normal, !at_max);
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

    #[test]
    fn staged_changes_compare_with_what_the_device_keeps() {
        let mut s = setting(SettingKey::WheelInvert);
        s.writable = true;
        s.observed = SettingValue::Bool(false);
        let set = |v: bool, policy| Change::Set {
            value: SettingValue::Bool(v),
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
        s.managed = true;
        s.desired = SettingValue::Bool(true);
        assert_eq!(effective(&s, Some(&set(true, false))), None);
        assert_eq!(
            effective(&s, Some(&set(false, false))),
            Some(set(false, false))
        );
        assert_eq!(effective(&s, Some(&Change::Forget)), Some(Change::Forget));
        // A value the setter doesn't take is never sent.
        let wrong = Change::Set {
            value: SettingValue::Integer(1),
            policy: true,
        };
        assert_eq!(effective(&s, Some(&wrong)), None);
    }
}
