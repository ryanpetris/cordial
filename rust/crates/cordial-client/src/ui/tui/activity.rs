//! The TUI's activity log keeps its own plain-language wording. The shell
//! prints command results and notices for scripts and scrollback instead.
use super::{Model, layout};
use crate::{
    client::Envelope,
    controller::{Command, Failure, Notice, Outcome},
    ui::{Backend, catalog, text},
};
use cordial_protocol::{
    errors::ErrorCode,
    identifiers::{ConnectionState, NormalizationState, Reconnect, SettingsState},
    messages::{Candidate, Device, DeviceKind, Message, SettingChunk},
    payloads::{AdapterSettings, BootloaderMode, DeviceUnpaired},
    settings::SettingState,
};
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

#[derive(serde::Deserialize)]
struct DeviceData {
    device: Device,
}

pub(super) fn state_words(d: &Device) -> String {
    match d.state {
        ConnectionState::Connected => "connected".into(),
        ConnectionState::Connecting => "connecting…".into(),
        ConnectionState::Disconnecting => "disconnecting…".into(),
        ConnectionState::Disconnected => "disconnected".into(),
    }
}

fn device_changes(old: &Device, d: &Device) -> Vec<(Kind, String)> {
    let name = text::display_name(d.name.as_deref());
    let mut out = Vec::new();
    let old_name = text::display_name(old.name.as_deref());
    if old_name != name {
        out.push((Kind::Info, format!("{old_name} is now named {name}")));
    }
    if old.state != d.state {
        let kind = match d.state {
            ConnectionState::Connected => Kind::Good,
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
    if old.enabled_reason != d.enabled_reason
        && let Some(reason) = d
            .enabled_reason
            .filter(|r| *r != cordial_protocol::errors::DisabledReason::Disabled)
    {
        out.push((
            Kind::Warn,
            format!("{name} is inactive: {}", text::disabled_words(reason)),
        ));
    }
    if old.validation_error != d.validation_error
        && let Some(v) = d.validation_error
    {
        out.push((Kind::Bad, format!("{name}: {}", text::validation_words(v))));
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
    if old.reconnect != d.reconnect {
        out.push(if d.reconnect == Reconnect::Paused {
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
    for w in &d.warnings {
        if !old.warnings.contains(w) {
            out.push((Kind::Warn, format!("{name}: {}", text::warning_text(*w))));
        }
    }
    if old.hidpp_enabled != d.hidpp_enabled {
        out.push((
            Kind::Info,
            format!(
                "Logitech Features turned {} for {name}",
                text::on_off(d.hidpp_enabled)
            ),
        ));
    }
    // Other status changes follow every connection; the details show them.
    // A special-key failure does not mean device settings failed, or the
    // reverse.
    let words = text::hidpp_error_text;
    if d.normalization_state == NormalizationState::Error
        && (old.normalization_state != NormalizationState::Error
            || words(old.normalization_error) != words(d.normalization_error))
    {
        out.push((
            Kind::Bad,
            format!(
                "Special keys failed on {name}: {}",
                words(d.normalization_error)
            ),
        ));
    }
    if d.settings_state == SettingsState::Error
        && (old.settings_state != SettingsState::Error
            || words(old.settings_error) != words(d.settings_error))
    {
        out.push((
            Kind::Bad,
            format!(
                "Device settings failed on {name}: {}",
                words(d.settings_error)
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
                (Some(d), _) => return text::display_name(d.name.as_deref()),
                (_, Some(c)) => return text::display_candidate_name(c),
                _ => {}
            }
        }
        if let Some(d) = self.known.get(id) {
            return text::display_name(d.name.as_deref());
        }
        if let Some(name) = self.names.get(id) {
            return name.clone();
        }
        text::display(id)
    }

    pub(super) fn reset_activity(&mut self) {
        self.known.clear();
        self.names.clear();
        self.found.clear();
        self.found_scan = 0;
    }

    /// Restarts change tracking from the current device view, for when events
    /// stopped describing every change (paused updates or lost events).
    pub(super) fn resync(&mut self) {
        self.known.clear();
        self.page.setting_states.clear();
        self.remember();
    }

    /// Tracks saved devices so later events can say what changed.
    pub(super) fn remember(&mut self) {
        if let Some(st) = self.state() {
            for d in st.devices {
                self.known.entry(d.device_id.0.clone()).or_insert(d);
            }
        }
    }

    /// Records adapter notifications in plain language. Successful responses
    /// are omitted because the device view already reflects them.
    pub(super) fn notice_activity(&mut self, n: &Notice) {
        match n {
            Notice::RefreshFailed(e) => {
                let words = format!("device refresh failed: {}", text::error_words(e));
                self.note(Kind::Bad, text::capitalized(&words));
            }
            Notice::RequestPending { .. } => {}
            Notice::BondSaved(id) => {
                if self.state().is_some_and(|st| !st.monitor) {
                    let text = format!("Paired {}; connecting…", self.label(&id.0));
                    self.note(Kind::Good, text);
                }
            }
            Notice::MonitorExpired => {
                self.note(
                    Kind::Warn,
                    "Live updates expired; renewing them and refreshing devices".into(),
                );
                self.resync();
            }
            Notice::Skipped(count) => {
                // These notifications were never applied: the view reloads.
                let noun = if *count == 1 { "update" } else { "updates" };
                let text = format!("Missed {count} {noun}; refreshing the device list");
                self.note(Kind::Warn, text);
                self.resync();
            }
            Notice::Message { envelope, .. } => self.message_activity(envelope),
            Notice::StorageEntries { ticket, entries } => self.files_entries(*ticket, entries),
            Notice::StorageProgress { ticket, bytes } => self.files_progress(*ticket, *bytes),
        }
        self.remember();
    }

    fn message_activity(&mut self, e: &Envelope) {
        match &e.message {
            Message::Response {
                ok, done, error, ..
            } if e.command == Some("discovery.scan") && *done => match (ok, error) {
                (true, _) => self.note(Kind::Info, "Scan finished".into()),
                (false, Some(w)) if w.code == ErrorCode::Cancelled => {
                    self.note(Kind::Info, "Scan stopped".into());
                }
                (false, Some(w)) => {
                    let words = text::display(&text::wire_text(w, Some("discovery.scan")));
                    self.note(Kind::Bad, format!("Scan failed: {words}"));
                }
                _ => {}
            },
            Message::Response { .. } => {}
            Message::Event {
                event, request_id, ..
            } => match event.as_str() {
                "discovery.result" => {
                    let Ok(c) = e.decode::<Candidate>() else {
                        return;
                    };
                    let request = request_id.map_or(0, |r| r.get());
                    self.found_candidate(request, &c);
                }
                "device.paired" | "device.changed" | "device.connected" | "device.disconnected" => {
                    let Ok(DeviceData { device: mut d }) = e.decode() else {
                        return;
                    };
                    // Named as everywhere else: a reported name comes only
                    // as device information, never with this record.
                    if let Some(st) = self.state()
                        && let (Some(saved), _) = Self::find(&st, &d.device_id.0)
                    {
                        d.name = saved.name.clone();
                    }
                    let name = text::display_name(d.name.as_deref());
                    let old = self.known.insert(d.device_id.0.clone(), d.clone());
                    if event == "device.paired" {
                        self.note(Kind::Good, format!("Paired {name}"));
                    } else if let Some(old) = old {
                        for (kind, text) in device_changes(&old, &d) {
                            self.note(kind, text);
                        }
                    } else {
                        self.note(Kind::Info, format!("{name} {}", state_words(&d)));
                    }
                }
                "device.unpaired" => {
                    if let Ok(DeviceUnpaired { device_id, .. }) = e.decode() {
                        let device_id = device_id.0;
                        let text = format!("Removed {}", self.label(&device_id));
                        self.note(Kind::Info, text);
                        self.known.remove(&device_id);
                        self.forget_device(&device_id);
                    }
                }
                "adapter.changed" => {
                    if let Ok(AdapterSettings {
                        name,
                        host_platform,
                        ..
                    }) = e.decode()
                    {
                        let text = format!(
                            "Adapter {} · {}",
                            text::display(&name),
                            text::platform_name(host_platform)
                        );
                        self.note(Kind::Info, text);
                    }
                }
                "hidpp.setting.changed" => {
                    if let Ok(data) = e.decode::<SettingChunk>() {
                        self.setting_activity(&data);
                    }
                }
                // Local loss is reported once, with its count, by Notice::Skipped.
                "events.lost" => {
                    self.note(
                        Kind::Warn,
                        "Missed some updates; refreshing the device list".into(),
                    );
                    self.resync();
                }
                _ => {}
            },
        }
    }

    fn found_candidate(&mut self, request: u32, c: &Candidate) {
        if request != self.found_scan || self.found.len() > 256 {
            self.found.clear();
            self.found_scan = request;
        }
        if self.names.len() > 256 {
            self.names.clear();
        }
        let id = c.candidate_id.0.clone();
        let name = text::display_candidate_name(c);
        self.names.insert(id.clone(), name.clone());
        // Names often arrive in a later advertisement; report devices once
        // named or of a known kind, again when a name follows a kind-only
        // report, and hidden unnamed ones not at all.
        let named = text::named(c);
        let shown = named || c.kind != DeviceKind::Unknown || self.show_unnamed;
        let reported = self.found.get(&id).copied();
        if shown && (reported.is_none() || named && reported == Some(false)) {
            self.found.insert(id, named);
            let transport = text::transport_name(c.transport);
            self.note(Kind::Info, format!("Found {name} ({transport})"));
        }
    }

    /// Describes a setting.changed notification when it reports an outcome: a
    /// value applied, changed on the device, or failing. Readings show only on
    /// the settings page.
    fn setting_activity(&mut self, e: &SettingChunk) {
        let s = &e.setting;
        if self.page.setting_states.len() > 512 {
            self.page.setting_states.clear();
        }
        let id = format!("{}/{}", e.device_id.0, text::wire(&s.key));
        let old = self.page.setting_states.insert(id, s.state);
        if old == Some(s.state) {
            return;
        }
        let name = self.label(&e.device_id.0);
        let label = text::display(catalog::label(s.key));
        match s.state {
            SettingState::Applied if old == Some(SettingState::Applying) => {
                self.note(Kind::Good, format!("Applied {label} on {name}"));
            }
            SettingState::ChangedOnDevice if self.hidpp_off(&e.device_id.0) => self.note(
                Kind::Warn,
                format!("{label} on {name} differs from its saved value, which isn't applied while Logitech Features are off"),
            ),
            SettingState::ChangedOnDevice => self.note(
                Kind::Warn,
                format!("{label} was changed on {name}; the saved value is kept"),
            ),
            SettingState::Error => {
                let reason = s
                    .error
                    .map(|code| format!(": {}", text::hidpp_words(code)))
                    .unwrap_or_default();
                self.note(Kind::Bad, format!("Couldn't apply {label} on {name}{reason}"));
            }
            SettingState::Uncertain => self.note(
                Kind::Warn,
                format!("{label} on {name} may not have been applied"),
            ),
            SettingState::Unsupported if s.managed => self.note(
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
            .and_then(|st| st.devices.into_iter().find(|d| d.device_id.0 == id))
            .is_some_and(|d| !d.hidpp_enabled)
    }

    /// Reports commands started from the TUI. With live updates on, device
    /// events describe successful changes; otherwise the result does.
    pub(super) fn result_activity(&mut self, job: &super::Job, result: &Result<Outcome, Failure>) {
        use Command::*;
        if job.answer {
            if let Err(f) = result {
                let words = text::error_words(&f.error);
                self.note(Kind::Bad, format!("Pairing answer not accepted: {words}"));
            }
            return;
        }
        let command = &job.command;
        if matches!(
            command,
            SettingSet(..) | SettingForget(..) | SettingsRefresh(_) | SettingsApply(_)
        ) {
            return self.settings_command_activity(command, result);
        }
        let target = match command {
            Info(id)
            | DeviceInfoRefresh(id)
            | Pair(id)
            | Connect(id)
            | Disconnect(id)
            | Enabled(id, _)
            | Trusted(id, _)
            | Blocked(id, _)
            | Remove(id)
            | Hidpp(id, _) => self.label(id),
            _ => String::new(),
        };
        self.command_activity(command, &target, result);
        if self
            .state()
            .is_some_and(|st| matches!(command, Monitor(_)) || !st.monitor)
        {
            self.resync(); // No events describe this change.
        }
    }

    fn command_activity(
        &mut self,
        command: &Command,
        target: &str,
        result: &Result<Outcome, Failure>,
    ) {
        use Command::*;
        let failure = match result {
            Err(f) => f,
            Ok(outcome) => return self.success_activity(command, target, outcome),
        };
        let words = text::error_words(&failure.error);
        let cancelled = failure
            .error
            .wire
            .as_ref()
            .is_some_and(|w| w.code == ErrorCode::Cancelled);
        // Pairing saves the bond before connecting; report a failed or
        // cancelled connection as such rather than as a failed pairing.
        if let Some(bonded) = &failure.bonded {
            let name = self.label(&bonded.0);
            if cancelled {
                self.note(
                    Kind::Info,
                    format!("Paired {name}; connecting was cancelled"),
                );
            } else {
                self.note(
                    Kind::Bad,
                    format!("Paired {name}, but couldn't connect: {words}"),
                );
            }
            return;
        }
        match command {
            Pair(_) if cancelled => {
                return self.note(Kind::Info, format!("Pairing {target} cancelled"));
            }
            Connect(_) if cancelled => {
                return self.note(Kind::Info, format!("Connecting {target} cancelled"));
            }
            // The adapter may have applied the change before a later step
            // failed; the error itself says which.
            Monitor(_) => return self.note(Kind::Bad, format!("Live updates: {words}")),
            _ => {}
        }
        let what = match command {
            Pair(_) => format!("pair {target}"),
            Connect(_) => format!("connect {target}"),
            Disconnect(_) => format!("disconnect {target}"),
            Enabled(_, true) => format!("enable {target}"),
            Enabled(_, false) => format!("disable {target}"),
            Trusted(_, true) => format!("trust {target}"),
            Trusted(_, false) => format!("untrust {target}"),
            Blocked(_, true) => format!("block {target}"),
            Blocked(_, false) => format!("unblock {target}"),
            Remove(_) => format!("remove {target}"),
            Scan(_) | ScanOff => "change scanning".into(),
            Devices => "refresh devices".into(),
            Bootloader => "enter the bootloader".into(),
            Cancel(_) => "cancel the request".into(),
            Hidpp(_, on) => format!("turn Logitech Features {} for {target}", text::on_off(*on)),
            Platform(_) => "set the platform".into(),
            Name(_) => "rename the adapter".into(),
            DeviceInfoRefresh(_) => format!("refresh the information of {target}"),
            other => command_name(other).into(),
        };
        self.note(Kind::Bad, format!("Couldn't {what}: {words}"));
    }

    fn success_activity(&mut self, command: &Command, target: &str, outcome: &Outcome) {
        use Command::*;
        match (command, outcome) {
            (Scan(_), _) => {
                let kind = match self.scan_label.replace(" + ", " and ") {
                    k if k.is_empty() => "nearby".to_string(),
                    k => k,
                };
                return self.note(Kind::Info, format!("Scanning for {kind} devices"));
            }
            (ScanOff, Outcome::ScanStopped { was_running: false }) => {
                return self.note(Kind::Info, "Scan was already stopped".into());
            }
            (ScanOff, _) => return,
            (Monitor(true), _) => return self.note(Kind::Info, "Live updates on".into()),
            (Monitor(false), _) => {
                return self.note(
                    Kind::Warn,
                    "Live updates paused; saved devices update only after your own actions".into(),
                );
            }
            (Devices, _) => return self.note(Kind::Info, "Device list refreshed".into()),
            (DeviceInfoRefresh(_), _) => {
                return self.note(Kind::Info, format!("Refreshed the information of {target}"));
            }
            // Events report the saved device; only the result says it won't connect.
            (Pair(_), Outcome::PairedDisabled(subject, reason)) => {
                let text = text::paired_disabled(subject, *reason);
                return self.note(Kind::Warn, text.trim_end_matches('.').into());
            }
            (Bootloader, Outcome::Bootloader { mode }) => {
                let mode = match mode {
                    BootloaderMode::Bootsel => "BOOTSEL",
                    BootloaderMode::Download => "download",
                };
                return self.note(
                    Kind::Warn,
                    format!("Adapter is restarting into {mode} mode"),
                );
            }
            _ => {}
        }
        if self.state().is_some_and(|st| st.monitor) {
            return;
        }
        let done = match command {
            Pair(_) => format!("Paired and connected {target}"),
            Connect(_) => format!("{target} connected"),
            Disconnect(_) => format!("{target} disconnected"),
            Enabled(_, true) => format!("Enabled {target}"),
            Enabled(_, false) => format!("Disabled {target}"),
            Trusted(_, true) => format!("Trusted {target}"),
            Trusted(_, false) => format!("{target} is no longer trusted"),
            Blocked(_, true) => format!("Blocked {target}"),
            Blocked(_, false) => format!("Unblocked {target}"),
            Remove(_) => format!("Removed {target}"),
            Hidpp(_, on) => format!(
                "Logitech Features turned {} for {target}",
                text::on_off(*on)
            ),
            Platform(p) => format!("Platform set to {}", text::platform_name(*p)),
            _ => return,
        };
        self.note(Kind::Good, done);
    }

    /// Reports Refresh and Apply started from the TUI. Only failures are
    /// noted, with the counts of what failed; the settings page's Save queue
    /// reports its own outcomes.
    fn settings_command_activity(&mut self, command: &Command, result: &Result<Outcome, Failure>) {
        use Command::*;
        let (SettingsRefresh(device) | SettingsApply(device)) = command else {
            return;
        };
        let Err(f) = result else {
            return;
        };
        let name = self.label(device);
        let summary = match f.partial.as_deref() {
            Some(Outcome::Job {
                counts: Some(c), ..
            }) => catalog::failure_summary(c),
            _ => String::new(),
        };
        let mut what = match command {
            SettingsRefresh(_) => format!("read the settings of {name}"),
            _ => format!("apply every saved value on {name}"),
        };
        if !summary.is_empty() {
            what = format!("{what} ({summary})");
        }
        let words = text::error_words(&f.error);
        self.note(Kind::Bad, format!("Couldn't {what}: {words}"));
    }
}

/// What a command does, for wording failures of unusual commands.
fn command_name(command: &Command) -> &'static str {
    use Command::*;
    match command {
        Status => "read the adapter status",
        Capabilities => "read the adapter capabilities",
        Info(_) => "read the device details",
        DeviceInfo(_) => "read the device information",
        Features(_) => "list the device features",
        Settings(_) => "list the device settings",
        SettingGet(..) => "read the setting",
        PairReply { .. } => "answer pairing",
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
