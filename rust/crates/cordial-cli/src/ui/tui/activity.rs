//! The TUI's activity log keeps its own plain-language wording. The shell
//! prints command results and notices for scripts and scrollback instead.
use super::{Model, layout};
use crate::{
    controller::{Command, Notice, Outcome, Toggle},
    error::Error,
    model::{self, Up},
    ui::{Backend, catalog, text},
};
use cordial_protocol::{self as p, DeviceState, ErrorCode, SettingState, event};
use ratatui::style::Style;
use std::time::Instant;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum Kind {
    Info,
    Good,
    Warn,
    Bad,
}
impl Kind {
    pub fn style(self) -> Style {
        match self {
            Kind::Info => layout::plain(),
            Kind::Good => layout::ok(),
            Kind::Warn => layout::warn(),
            Kind::Bad => layout::err(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct LogEntry {
    pub at: Instant,
    /// Wall-clock time of day as the log shows it.
    pub clock: String,
    pub kind: Kind,
    pub text: String,
}

/// The local time of day for the activity log.
fn clock() -> String {
    chrono::Local::now().format("%H:%M:%S").to_string()
}

pub(super) fn state_words(d: &p::Device) -> String {
    match d.state() {
        DeviceState::Connected => "connected".into(),
        DeviceState::Connecting => "connecting…".into(),
        DeviceState::Disconnecting => "disconnecting…".into(),
        DeviceState::Disconnected => "disconnected".into(),
    }
}

fn device_changes(old: &p::Device, d: &p::Device) -> Vec<(Kind, String)> {
    let name = text::display_name(Some(&d.name));
    let mut out = Vec::new();
    let old_name = text::display_name(Some(&old.name));
    if old_name != name {
        out.push((Kind::Info, format!("{old_name} is now named {name}")));
    }
    if old.state != d.state {
        let kind = match d.state() {
            DeviceState::Connected => Kind::Good,
            _ => Kind::Info,
        };
        out.push((kind, format!("{name} {}", state_words(d))));
    }
    if old.enabled != d.enabled {
        out.push(if d.enabled {
            (Kind::Info, format!("Enabled {name}"))
        } else {
            (Kind::Info, format!("Disabled {name}"))
        });
    }
    if old.inactive != d.inactive
        && let Some(reason) = model::inactive(d)
            .filter(|r| !matches!(r, p::InactiveReason::Disabled | p::InactiveReason::Blocked))
    {
        out.push((
            Kind::Warn,
            format!("{name} is inactive: {}", text::inactive_words(reason)),
        ));
    }
    if old.trusted != d.trusted {
        out.push(if d.trusted {
            (Kind::Info, format!("Trusted {name}"))
        } else {
            (Kind::Info, format!("{name} is no longer trusted"))
        });
    }
    if old.blocked != d.blocked {
        out.push(if d.blocked {
            (Kind::Warn, format!("Blocked {name}"))
        } else {
            (Kind::Info, format!("Unblocked {name}"))
        });
    }
    if old.paused != d.paused {
        out.push(if d.paused {
            (
                Kind::Info,
                format!("Automatic reconnection paused for {name}"),
            )
        } else {
            (
                Kind::Info,
                format!("Automatic reconnection resumed for {name}"),
            )
        });
    }
    if model::hidpp_enabled(old) != model::hidpp_enabled(d) {
        out.push((
            Kind::Info,
            format!(
                "Logitech Features turned {} for {name}",
                text::on_off(model::hidpp_enabled(d))
            ),
        ));
    }
    // Other status changes follow every connection; the details show them.
    if let Up::Error(code) = model::hidpp_up(d)
        && model::hidpp_up(old) != Up::Error(code)
    {
        out.push((
            Kind::Bad,
            format!(
                "Logitech Features failed on {name}: {}",
                text::hidpp_words(code)
            ),
        ));
    }
    out
}

impl<B: Backend> Model<B> {
    pub(super) fn note(&mut self, kind: Kind, text: String) {
        if text.is_empty() {
            return;
        }
        for line in text.split('\n') {
            self.logs.push(LogEntry {
                at: Instant::now(),
                clock: clock(),
                kind,
                text: line.to_owned(),
            });
            if self.event_scroll > 0 {
                // Keep older history still while the reader has scrolled up.
                self.event_scroll += layout::wrap(line, self.event_width.max(1)).len();
            }
        }
        if self.logs.len() > 200 {
            let extra = self.logs.len() - 200;
            self.logs.drain(..extra);
        }
    }

    /// Names a device or candidate, including ones that have since left the
    /// session state, such as a removed bond or a hidden candidate.
    pub(super) fn label(&self, id: &str) -> String {
        if let Some(st) = self.state() {
            match Self::find(&st, id) {
                (Some(d), _) => return text::display_name(Some(&d.name)),
                (_, Some(c)) => return text::display_candidate_name(c),
                _ => {}
            }
        }
        if let Some(d) = self.known.get(id) {
            return text::display_name(Some(&d.name));
        }
        if let Some(name) = self.names.get(id) {
            return name.clone();
        }
        text::display(id)
    }

    pub(super) fn reset_activity(&mut self) {
        self.known.clear();
        self.known_warnings.clear();
        self.names.clear();
        self.found.clear();
    }

    /// Tracks saved devices so later events can say what changed. The first
    /// warning list of a device is only recorded.
    pub(super) fn remember(&mut self) {
        let Some(st) = self.state() else {
            return;
        };
        for d in &st.devices {
            if let Some(w) = st.warnings.get(&d.id) {
                self.known_warnings
                    .entry(d.id.clone())
                    .or_insert_with(|| w.clone());
            }
        }
        self.known_warnings
            .retain(|id, _| st.devices.iter().any(|d| d.id == *id));
        for d in st.devices {
            self.known.entry(d.id.clone()).or_insert(d);
        }
    }

    /// Records adapter events in plain language. Successful responses are
    /// omitted because the device view already reflects them.
    pub(super) fn notice_activity(&mut self, n: &Notice) {
        match n {
            Notice::RefreshFailed(e) => {
                let words = format!("device refresh failed: {}", text::error_words(e));
                self.note(Kind::Bad, text::capitalized(&words));
            }
            Notice::Response(_) => {}
            Notice::Event { event, first, .. } => {
                if let Some(kind) = &event.kind {
                    self.event_activity(kind, *first);
                }
            }
        }
    }

    fn event_activity(&mut self, kind: &event::Kind, first: bool) {
        match kind {
            event::Kind::ScanFound(c) => self.found_candidate(c),
            event::Kind::ScanDone(done) => {
                let text = if done.truncated {
                    "Scan finished; the list is full"
                } else {
                    "Scan finished"
                };
                self.note(Kind::Info, text.into());
            }
            event::Kind::Device(d) => {
                let name = text::display_name(Some(&d.name));
                let old = self.known.insert(d.id.clone(), d.clone());
                match old {
                    Some(old) => {
                        for (kind, text) in device_changes(&old, d) {
                            self.note(kind, text);
                        }
                    }
                    None if first => self.note(Kind::Good, format!("Paired {name}")),
                    None => self.note(Kind::Info, format!("{name} {}", state_words(d))),
                }
            }
            event::Kind::DeviceRemoved(r) => {
                let text = format!("Removed {}", self.label(&r.id));
                self.note(Kind::Info, text);
                self.known.remove(&r.id);
                self.known_warnings.remove(&r.id);
                self.forget_device(&r.id);
            }
            event::Kind::Adapter(a) => {
                let text = format!(
                    "Adapter {} · {}",
                    text::display(&a.name),
                    text::platform_name(a.platform())
                );
                self.note(Kind::Info, text);
            }
            event::Kind::Settings(s) => {
                for setting in &s.settings {
                    self.setting_activity(&s.device, setting);
                }
            }
            event::Kind::Warnings(w) => {
                let name = self.label(&w.device);
                let old = self
                    .known_warnings
                    .insert(w.device.clone(), w.warnings.clone());
                if let Some(old) = old {
                    for warning in w.warnings.iter().filter(|x| !old.contains(x)) {
                        self.note(
                            Kind::Warn,
                            format!("{name}: {}", text::warning_text(warning.code())),
                        );
                    }
                }
            }
            // The pairing command reports how it ended.
            event::Kind::Pairing(_) => {}
        }
    }

    fn found_candidate(&mut self, c: &p::Candidate) {
        if self.found.len() > 256 {
            self.found.clear();
        }
        if self.names.len() > 256 {
            self.names.clear();
        }
        let name = text::display_candidate_name(c);
        self.names.insert(c.id.clone(), name.clone());
        // Names often arrive in a later advertisement; report devices once
        // named or of a known kind, again when a name follows a kind-only
        // report, and hidden unnamed ones not at all.
        let named = text::named(c);
        let shown = named || !matches!(c.kind(), p::Kind::Unknown) || self.show_unnamed;
        let reported = self.found.get(&c.id).copied();
        if shown && (reported.is_none() || named && reported == Some(false)) {
            self.found.insert(c.id.clone(), named);
            let transport = text::transport_name(c.transport());
            self.note(Kind::Info, format!("Found {name} ({transport})"));
        }
    }

    /// Describes a setting's apply outcome when it changes: applied, changed
    /// on the device, or failing. Readings show only on the settings page.
    fn setting_activity(&mut self, device: &str, s: &p::Setting) {
        if catalog::info_for(&s.key).is_none() {
            return;
        }
        if self.page.setting_states.len() > 512 {
            self.page.setting_states.clear();
        }
        let id = format!("{device}/{}", s.key);
        let now = model::applied(s);
        let old = self.page.setting_states.insert(id, now);
        if old == Some(now) {
            return;
        }
        let name = self.label(device);
        let label = text::display(&catalog::label(&s.key));
        match now {
            Some(Ok(SettingState::Applied)) if old == Some(Some(Ok(SettingState::Pending))) => {
                self.note(Kind::Good, format!("Applied {label} on {name}"));
            }
            Some(Ok(SettingState::ChangedOnDevice)) if self.hidpp_off(device) => self.note(
                Kind::Warn,
                format!("{label} on {name} differs from its saved value, which isn't applied while Logitech Features are off"),
            ),
            Some(Ok(SettingState::ChangedOnDevice)) => self.note(
                Kind::Warn,
                format!("{label} was changed on {name}; the saved value is kept"),
            ),
            Some(Err(code)) => self.note(
                Kind::Bad,
                format!("Couldn't apply {label} on {name}: {}", text::hidpp_words(code)),
            ),
            Some(Ok(SettingState::Unsupported)) => self.note(
                Kind::Warn,
                format!("{name} doesn't support its saved {label} now"),
            ),
            _ => {}
        }
    }

    /// A saved device whose HID++ preference is off, so saved settings are
    /// kept but not applied.
    pub(super) fn hidpp_off(&self, id: &str) -> bool {
        self.state()
            .and_then(|st| st.device(id).cloned())
            .is_some_and(|d| !model::hidpp_enabled(&d))
    }

    /// Reports commands started from the TUI. Device events describe
    /// successful changes; failures and results without events are noted.
    pub(super) fn result_activity(&mut self, job: &super::Job, result: &Result<Outcome, Error>) {
        use Command::*;
        if job.answer {
            if let Err(e) = result {
                let words = text::error_words(e);
                self.note(Kind::Bad, format!("Pairing answer not accepted: {words}"));
            }
            return;
        }
        let command = &job.command;
        if matches!(command, SettingsSave { .. }) {
            return;
        }
        let target = match command {
            Get(id)
            | Refresh(id)
            | Pair(id)
            | Connect(id)
            | Disconnect(id)
            | Set(id, ..)
            | Unpair(id) => self.label(id),
            _ => String::new(),
        };
        match result {
            Err(e) => self.failure_activity(command, &target, e),
            Ok(outcome) => self.success_activity(command, &target, outcome),
        }
    }

    fn failure_activity(&mut self, command: &Command, target: &str, error: &Error) {
        use Command::*;
        let words = text::error_words(error);
        let cancelled = error.code_of() == Some(ErrorCode::Cancelled);
        if matches!(command, Pair(_)) && cancelled {
            return self.note(Kind::Info, format!("Pairing {target} cancelled"));
        }
        let what = match command {
            Pair(_) => format!("pair {target}"),
            Connect(_) => format!("connect {target}"),
            Disconnect(_) => format!("disconnect {target}"),
            Set(_, Toggle::Enabled, true) => format!("enable {target}"),
            Set(_, Toggle::Enabled, false) => format!("disable {target}"),
            Set(_, Toggle::Trusted, true) => format!("trust {target}"),
            Set(_, Toggle::Trusted, false) => format!("untrust {target}"),
            Set(_, Toggle::Blocked, true) => format!("block {target}"),
            Set(_, Toggle::Blocked, false) => format!("unblock {target}"),
            Set(_, Toggle::Hidpp, on) => {
                format!("turn Logitech Features {} for {target}", text::on_off(*on))
            }
            Unpair(_) => format!("remove {target}"),
            Scan { .. } | ScanStop => "change scanning".into(),
            Devices => "refresh devices".into(),
            Bootloader => "enter the bootloader".into(),
            CancelPairing => "cancel the pairing".into(),
            Platform(_) => "set the platform".into(),
            Name(_) => "rename the adapter".into(),
            Refresh(_) => format!("refresh the information of {target}"),
            other => command_name(other).into(),
        };
        self.note(Kind::Bad, format!("Couldn't {what}: {words}"));
    }

    fn success_activity(&mut self, command: &Command, target: &str, outcome: &Outcome) {
        use Command::*;
        match (command, outcome) {
            (Scan { .. }, _) => {
                let kind = match self.scan_label.replace(" + ", " and ") {
                    k if k.is_empty() => "nearby".to_string(),
                    k => k,
                };
                self.note(Kind::Info, format!("Scanning for {kind} devices"));
            }
            (ScanStop, Outcome::ScanStopped { was_running: false }) => {
                self.note(Kind::Info, "Scan was already stopped".into());
            }
            (Devices, _) => self.note(Kind::Info, "Device list refreshed".into()),
            (Refresh(_), _) => {
                self.note(
                    Kind::Info,
                    format!("Refreshing the information of {target}"),
                );
            }
            // Events report the saved device; only the result says it won't connect.
            (Pair(_), Outcome::Paired { subject, device }) => {
                if let Some(reason) = device.as_ref().and_then(model::inactive) {
                    let text = text::paired_disabled(subject, reason);
                    self.note(Kind::Warn, text.trim_end_matches('.').into());
                }
            }
            (Bootloader, _) => {
                self.note(
                    Kind::Warn,
                    "Adapter is restarting into its bootloader".into(),
                );
            }
            (Platform(p), _) => {
                let text = format!("Platform set to {}", text::platform_name(*p));
                self.note(Kind::Good, text);
            }
            _ => {}
        }
    }
}

/// What a command does, for wording failures of unusual commands.
fn command_name(command: &Command) -> &'static str {
    use Command::*;
    match command {
        Status => "read the adapter status",
        Get(_) => "read the device details",
        Features(_) => "list the device features",
        Settings(_) => "list the device settings",
        SettingGet(..) => "read the setting",
        Accept(_) | Reject => "answer pairing",
        Warnings(_) => "read the device warnings",
        _ => "run the command",
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn clock_is_local_time_of_day() {
        let before = chrono::Local::now().format("%H:%M:%S").to_string();
        let shown = super::clock();
        let after = chrono::Local::now().format("%H:%M:%S").to_string();
        assert!(shown == before || shown == after, "{shown}");
        assert_eq!(shown.len(), 8);
    }
}
