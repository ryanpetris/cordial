//! Shared words for adapter codes and records.
//! Messages are complete sentences; labels and status words are Title Case.
use crate::{error::Error, model, ui::text};
use cordial_protocol::{
    self as p, DeviceState, ErrorCode, ErrorOutcome, ErrorReason, InactiveReason, Kind,
    ResourceType, Role, Transport,
};

fn error_clause(code: ErrorCode) -> &'static str {
    use ErrorCode::*;
    match code {
        Unknown | Internal => "the adapter hit an unexpected failure",
        BadRequest => "the adapter couldn't understand the request from Cordial",
        UnknownCommand => {
            "this adapter's firmware doesn't support that action. Update the firmware to use it"
        }
        BadArgs => "the adapter couldn't accept those values",
        TooLong => "the request was too large for the adapter",
        NotReady => "the adapter's Bluetooth or storage isn't ready. Try again in a moment",
        NotFound => "the adapter couldn't find this device, setting or profile",
        NotConnected => "the device isn't connected. Connect it first",
        Busy => "the adapter is busy. Try again when the current operation finishes",
        Disabled => "this device is turned off in Cordial. Turn on “Use This Device” first",
        Blocked => "connections to this device are blocked. Unblock it before connecting",
        Unsupported => "the adapter or device doesn't support this action",
        NoCapacity => "the adapter has no room for that right now",
        NoPrompt => "that pairing prompt is no longer waiting for an answer",
        StorageFailed => "the adapter couldn't save the change",
        InUse => {
            "this profile is still in use. Remove it from every device and interface before deleting it"
        }
        CandidateExpired => "this device is no longer available. Search again",
        AuthFailed => "Bluetooth authentication with this device failed",
        Rejected => "the pairing was rejected on this computer or on the device",
        Timeout => "this device didn't respond in time",
        Cancelled => "the operation was cancelled",
        ConnectionFailed => "the adapter couldn't connect to this device",
        UnsupportedHid => "the adapter can't use this device's input format",
        ProtocolUnsupported => "this device doesn't support this feature",
        FeatureUnavailable => "this device doesn't have a feature this action needs",
        TransportError => "the adapter couldn't exchange messages with this device",
        DeviceError => "this device reported an error",
        InvalidResponse => "the adapter couldn't understand this device's reply",
        ReadbackMismatch => "the device reported a different value from the one requested",
        StorageFull => "the adapter's storage is full",
        ProfileMemoryLow => "the adapter is almost out of profile memory",
        UsbRequiredCapacityExceeded => {
            "the adapter doesn't have enough USB capacity for every required interface"
        }
        UsbReservedCapacityExceeded => {
            "the adapter doesn't have enough USB capacity for every reserved interface"
        }
        ProfileMemoryExceeded => "the adapter doesn't have room for this device's profiles",
        ProfileLoadFailed => "the adapter couldn't read one of this device's profiles",
        StorageMigrationRequired => "the adapter's saved data must be migrated before use",
    }
}

/// Wording for a code whose reason changes what failed.
fn specific(code: ErrorCode, reason: ErrorReason) -> Option<&'static str> {
    use {ErrorCode as C, ErrorReason as R};
    Some(match (code, reason) {
        (C::NoCapacity, R::EnabledDevices) => {
            "every enabled-device place is in use. Turn off another device first"
        }
        (C::NoCapacity, R::Storage) => {
            "the adapter's storage is full. Remove an unused device or forget a saved setting, then try again"
        }
        (C::NoCapacity, R::Connections) => "every connection is in use. Disconnect a device first",
        (C::NoCapacity, R::ProfileMemory) => {
            "the adapter doesn't have enough profile memory for this change"
        }
        (C::NoCapacity, R::ScanCandidates) => {
            "the adapter had no room to list every device it found"
        }
        (C::NoCapacity, R::OutputBackend) => {
            "the adapter doesn't have enough USB capacity for this output"
        }
        (C::NotReady, R::Radio) => {
            "the adapter's Bluetooth radio isn't ready. Try again in a moment"
        }
        (C::NotReady, R::Storage) => "the adapter's storage isn't ready. Try again in a moment",
        (C::Unsupported, R::OutputBackend) => "the adapter doesn't support this output",
        (C::Unsupported, R::InputLayout) => {
            "the adapter can't use this device's input layout for this output"
        }
        (C::Internal, R::ScanCandidates) => {
            "the adapter couldn't keep track of every device it found"
        }
        _ => return None,
    })
}

/// What a reason concerns, for a code whose wording doesn't already say.
fn context(reason: ErrorReason) -> Option<&'static str> {
    Some(match reason {
        ErrorReason::Unknown => return None,
        ErrorReason::EnabledDevices => "enabled devices",
        ErrorReason::Connections => "the adapter's connections",
        ErrorReason::ScanCandidates => "the devices found while searching",
        ErrorReason::Storage => "the adapter's storage",
        ErrorReason::ProfileMemory => "profile memory",
        ErrorReason::Radio => "the adapter's Bluetooth radio",
        ErrorReason::OutputBackend => "the adapter's USB output",
        ErrorReason::InputLayout => "this device's input layout",
    })
}

/// The reason a code's wording already names.
fn implied(code: ErrorCode) -> Option<ErrorReason> {
    use ErrorCode::*;
    Some(match code {
        StorageFailed | StorageFull => ErrorReason::Storage,
        ProfileMemoryLow | ProfileMemoryExceeded => ErrorReason::ProfileMemory,
        UsbRequiredCapacityExceeded | UsbReservedCapacityExceeded => ErrorReason::OutputBackend,
        _ => return None,
    })
}

/// What an error's outcome is about: a change saved on the adapter, a change applied to the
/// device, a read, a pairing's saved device, the cleanup after a failed pairing, or another
/// operation, as for related failures with no more specific scope.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Scope {
    Save,
    Apply,
    Read,
    Pair,
    Cleanup,
    Other,
}

impl Scope {
    /// The scope of the related failures of an error in this scope.
    pub(crate) fn related(self) -> Self {
        match self {
            Scope::Read => Scope::Read,
            Scope::Pair => Scope::Cleanup,
            _ => Scope::Other,
        }
    }
}

fn outcome_text(outcome: ErrorOutcome, scope: Scope) -> Option<&'static str> {
    use ErrorOutcome::*;
    Some(match (scope, outcome) {
        (_, Unspecified | NoEffect) => return None,
        (Scope::Save, Unknown) => {
            "The adapter couldn't confirm whether the change was saved. Check before trying again."
        }
        (Scope::Save, Partial) => "The adapter confirmed saving only part of the change.",
        (Scope::Save, Complete) => "The adapter saved the change despite this.",
        (Scope::Apply, Unknown) => "The adapter couldn't confirm whether the change took effect.",
        (Scope::Apply, Partial) => "Only part of the change is confirmed to have taken effect.",
        (Scope::Apply, Complete) => "The change took effect despite this.",
        (Scope::Read, Unknown) => "The adapter couldn't confirm whether the results are complete.",
        (Scope::Read, Partial) => "The adapter returned only some of the results.",
        (Scope::Read, Complete) => "The results are complete despite this.",
        (Scope::Pair, Unknown) => {
            "The adapter couldn't confirm whether it saved the device. Check its saved devices before pairing again."
        }
        (Scope::Pair, Partial) => "The adapter confirmed saving only part of the pairing.",
        (Scope::Pair, Complete) => "The adapter saved the device despite this.",
        (Scope::Cleanup, Unknown) => {
            "The adapter couldn't confirm whether it finished cleaning up."
        }
        (Scope::Cleanup, Partial) => "The adapter finished only part of the cleanup.",
        (Scope::Cleanup, Complete) => "The adapter finished cleaning up despite this.",
        (Scope::Other, Unknown) => "Its outcome is unknown.",
        (Scope::Other, Partial) => "It partly completed.",
        (Scope::Other, Complete) => "Its intended effects completed.",
    })
}

/// A clause as a complete sentence: capitalized, with a final period.
fn sentence(clause: &str) -> String {
    format!("{}.", text::capitalized(clause))
}

pub fn code_text(code: ErrorCode) -> String {
    sentence(error_clause(code))
}

/// A message as a reason inserted into other text, such as "Failed: {reason}", without its final
/// period.
pub fn reason(message: &str) -> &str {
    message.strip_suffix('.').unwrap_or(message)
}

/// The error's outcome, or None for one this app doesn't know.
fn outcome(e: &p::Error) -> Option<ErrorOutcome> {
    ErrorOutcome::try_from(e.outcome).ok()
}

pub use model::plain;

/// What failed, as a clause, with what its reason concerns.
fn failure_clause(e: &p::Error, scope: Scope) -> String {
    let Ok(code) = ErrorCode::try_from(e.code) else {
        return error_clause(ErrorCode::Unknown).into();
    };
    let why = ErrorReason::try_from(e.reason).unwrap_or_default();
    if let Some(words) = specific(code, why) {
        return words.into();
    }
    // Only an outcome of no effect confirms that nothing was saved.
    let none = outcome(e) == Some(ErrorOutcome::NoEffect);
    let clause = match (code, scope) {
        (ErrorCode::StorageFailed, Scope::Read) => reason(READ_FAILED),
        (ErrorCode::StorageFailed, Scope::Save) if none => error_clause(code),
        (ErrorCode::StorageFailed, Scope::Pair) if none => "the adapter couldn't save the device",
        (ErrorCode::StorageFailed, _) => "the adapter couldn't read or write its saved data",
        _ => error_clause(code),
    };
    match context(why).filter(|_| implied(code) != Some(why)) {
        Some(context) => format!("{clause}. This involves {context}"),
        None => clause.into(),
    }
}

/// A record's read failure as its placeholder's status: "Couldn't Read", with the error when it
/// says more than that.
pub fn couldnt_read(e: Option<&p::Error>) -> String {
    match e.map(|e| error_text(e, Scope::Read)) {
        Some(text) if text != READ_FAILED => format!("Couldn't Read: {}", reason(&text)),
        _ => "Couldn't Read".into(),
    }
}

/// An adapter error as sentences: what failed, what is known of its effects, then each related
/// failure with its own outcome.
pub fn error_text(e: &p::Error, scope: Scope) -> String {
    sentences(e, scope, false)
}

fn sentences(e: &p::Error, scope: Scope, also: bool) -> String {
    let clause = failure_clause(e, scope);
    let mut parts = vec![if also {
        format!("Also, {clause}.")
    } else {
        sentence(&clause)
    }];
    parts.extend(
        outcome(e)
            .and_then(|o| outcome_text(o, scope))
            .map(str::to_owned),
    );
    parts.extend(
        e.related
            .iter()
            .map(|r| sentences(r, scope.related(), true)),
    );
    parts.join(" ")
}

pub const GONE: &str = "This device or adapter is no longer available.";
pub const ADAPTER_GONE: &str = "This adapter is no longer available.";
pub const CLOSED: &str = "The adapter disconnected before the change finished.";
pub const READ_CLOSED: &str = "The adapter disconnected before it answered.";
pub const UNEXPECTED: &str = "The adapter returned an unexpected result.";
pub const NOT_PLUGGED_IN: &str = "The adapter isn't plugged in.";
/// A failed read of the adapter's saved data, where the save wording doesn't apply.
pub const READ_FAILED: &str = "The adapter couldn't read its saved data. Try again.";

/// Why Connect couldn't open an adapter's port.
pub fn open_failed(adapter: &str) -> String {
    format!(
        "Cordial couldn't connect to {adapter}. Close any other app that uses it, then try again."
    )
}

/// Why Connect didn't reach the adapter it opened the port for.
pub fn other_adapter(adapter: &str) -> String {
    format!("Cordial couldn't connect to {adapter}. Another adapter answered on its port.")
}

pub const STORAGE_FULL: &str = "Storage Full";
pub const REQUIRED_CAPACITY: &str = text::USB_CAPACITY[0].1;
pub const STORAGE_FULL_ATTENTION: &str = "The adapter's storage is full. Remove an unused device or forget a saved setting to pair another device.";
pub const SETTINGS_READ_FAILED: &str = "The adapter couldn't read the device's settings.";
pub const DEVICE_READ_FAILED: &str = "The adapter couldn't read this device's saved data.";
pub const SETTINGS_SAVE_FAILED: &str = "The adapter couldn't save the settings.";
pub const FORGET_TEXT: &str =
    "Forgetting this device deletes its pairing and saved settings from the adapter.";
pub const ADAPTER_NAME_INVALID: &str = "Enter an adapter name of up to 64 bytes.";
pub const PROFILE_NAME_INVALID: &str = "Enter a profile name of up to 64 bytes.";

/// A failed action as a sentence: the adapter's refusal, or the local reason.
pub fn failure(e: &Error) -> String {
    failure_in(e, Scope::Save)
}

/// A failed action as `failure` words it, with the adapter's outcome in `scope`.
pub fn failure_in(e: &Error, scope: Scope) -> String {
    if let Some(w) = &e.dongle {
        return error_text(w, scope);
    }
    match e.message.as_str() {
        "the connection to the adapter closed" => CLOSED.into(),
        "the adapter returned an unexpected result" => UNEXPECTED.into(),
        m if m == crate::commands::STARTING => code_text(ErrorCode::NotReady),
        "the control session is unavailable; select an adapter" => ADAPTER_GONE.into(),
        m => text::sentence(&text::display(m)),
    }
}

/// A failed read: a storage failure reads as one of the saved data, not of a save.
pub fn read_failure(e: &Error) -> String {
    match &e.dongle {
        Some(w) => error_text(w, Scope::Read),
        None if e.message == "the connection to the adapter closed" => READ_CLOSED.into(),
        None => failure(e),
    }
}

/// A failure that refuses work on a disabled transport names the transport; otherwise it reads
/// as `failure_in` words it.
pub fn transport_failure(e: &Error, status: &p::Status, t: Transport, scope: Scope) -> String {
    let refused = e
        .dongle
        .as_ref()
        .is_some_and(|w| w.code() == ErrorCode::Unsupported && plain(w));
    if refused && model::transport_disabled(status, t) {
        return transport_disabled_text(t);
    }
    failure_in(e, scope)
}

pub fn transport_name(t: Transport) -> &'static str {
    text::transport_long(t)
}

pub fn transport_disabled_text(t: Transport) -> String {
    text::transport_disabled_sentence(t)
}

/// Why `d` is inactive, naming its Bluetooth type where that is the reason.
pub fn inactive_text(d: &p::Device) -> String {
    let t = model::transport(d.transport);
    match (model::inactive(d), t) {
        (Some(InactiveReason::UnsupportedTransport), Some(t)) => {
            format!("This adapter doesn't support {}.", transport_name(t))
        }
        (Some(InactiveReason::TransportDisabled), Some(t)) => transport_disabled_text(t),
        (Some(InactiveReason::UnsupportedTransport), None) => {
            "This adapter doesn't support the device's Bluetooth type.".into()
        }
        (Some(InactiveReason::Blocked), _) => "Connections to this device are blocked.".into(),
        (Some(InactiveReason::Disabled), _) => "It is turned off.".into(),
        (Some(InactiveReason::Capacity), _) => {
            "The adapter can't enable another device. Turn off another device first.".into()
        }
        _ => "The adapter isn't using this device.".into(),
    }
}

/// The device's state in a few words, as lists show it.
pub fn device_status(d: &p::Device) -> &'static str {
    if d.blocked {
        return "Blocked";
    }
    match d.state() {
        DeviceState::Connected => "Connected",
        DeviceState::Connecting => "Connecting…",
        DeviceState::Disconnecting => "Disconnecting…",
        DeviceState::Disconnected => match model::inactive(d) {
            None => "Disconnected",
            Some(_) if d.enabled => "Inactive",
            Some(_) => "Disabled",
        },
    }
}

/// A device record's name, cleaned; records without one read "Unnamed device".
pub fn device_name(d: &p::Device) -> String {
    let name = clean(&d.name);
    if name.is_empty() {
        "Unnamed device".into()
    } else {
        name
    }
}

/// A scan candidate's name, cleaned; one without a name reads "Unnamed Device".
pub fn candidate_name(c: &p::Candidate) -> String {
    let name = clean(&c.name);
    if name.is_empty() {
        "Unnamed Device".into()
    } else {
        name
    }
}

/// A text field an adapter reported, cleaned; None when absent or blank.
pub fn reported(value: Option<&str>) -> Option<String> {
    value.map(clean).filter(|v| !v.is_empty())
}

/// Removes control and format characters, including bidirectional overrides, from untrusted
/// text, and trims it.
pub fn clean(s: &str) -> String {
    use unicode_properties::{GeneralCategory, UnicodeGeneralCategory};
    let out: String = s
        .chars()
        .filter_map(|c| match c.general_category() {
            GeneralCategory::Control | GeneralCategory::LineSeparator => Some(' '),
            GeneralCategory::ParagraphSeparator => Some(' '),
            GeneralCategory::Format => None,
            _ => Some(c),
        })
        .collect();
    out.trim().to_owned()
}

/// What the device or candidate is shown as.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DisplayKind {
    Keyboard,
    Mouse,
    KeyboardMouse,
    Gamepad,
    Other,
}

/// What a device or candidate is shown as: from what it says it is, else from the input its HID
/// descriptor produces.
pub fn display_kind(kinds: &[i32], roles: &[i32]) -> DisplayKind {
    let kinds = model::known_kinds(kinds);
    let roles = model::known_roles(roles);
    let keyboard =
        kinds.contains(&Kind::Keyboard) || (kinds.is_empty() && roles.contains(&Role::Keyboard));
    let mouse = kinds.contains(&Kind::Mouse) || (kinds.is_empty() && roles.contains(&Role::Mouse));
    let gamepad =
        kinds.contains(&Kind::Gamepad) || (kinds.is_empty() && roles.contains(&Role::Gamepad));
    match (keyboard, mouse) {
        (true, true) => DisplayKind::KeyboardMouse,
        (true, false) => DisplayKind::Keyboard,
        (false, true) => DisplayKind::Mouse,
        _ if gamepad => DisplayKind::Gamepad,
        _ => DisplayKind::Other,
    }
}

pub fn kind_text(kind: DisplayKind) -> &'static str {
    match kind {
        DisplayKind::Keyboard => "Keyboard",
        DisplayKind::Mouse => "Mouse",
        DisplayKind::KeyboardMouse => "Keyboard and Mouse",
        DisplayKind::Gamepad => "Gamepad",
        DisplayKind::Other => "Other",
    }
}

pub fn role_text(r: Role) -> Option<&'static str> {
    Some(match r {
        Role::Keyboard => "Keyboard",
        Role::Mouse => "Mouse",
        Role::ConsumerControl => "Media Keys",
        Role::SystemControl => "System Keys",
        Role::Gamepad => "Gamepad",
        Role::Unknown => return None,
    })
}

/// Why a connected device's profiles aren't loaded. The short wording of a lack of room is for a
/// plain error whose reason, if any, is profile memory.
pub fn profile_error_text(e: &p::Error) -> String {
    let room = e.code() == ErrorCode::ProfileMemoryExceeded
        && plain(e)
        && [
            ErrorReason::Unknown as i32,
            ErrorReason::ProfileMemory as i32,
        ]
        .contains(&e.reason);
    if room {
        "The adapter doesn't have room for them. Disconnect another device or give this one fewer profiles.".into()
    } else {
        error_text(e, Scope::Apply)
    }
}

/// A failure as "Failed: {reason}".
pub fn failed_text(e: &p::Error) -> String {
    format!("Failed: {}", reason(&error_text(e, Scope::Apply)))
}

/// HID++'s state, as Diagnostics shows it.
pub fn integration_text(i: &p::Integration) -> String {
    match model::up(i) {
        model::Up::Off => "Off".into(),
        model::Up::Disconnected => "Waiting to Connect".into(),
        model::Up::Starting => "Setting Up".into(),
        model::Up::Active => "Active".into(),
        model::Up::Unsupported(e) if model::plain(&e) => "Unsupported".into(),
        model::Up::Unsupported(e) | model::Up::Error(e) => failed_text(&e),
    }
}

/// The link security facts the device reported, labelled.
pub fn security_facts(s: &p::Security) -> Vec<(&'static str, String)> {
    let flag = |b| if b { "Yes" } else { "No" }.to_owned();
    [
        ("Encrypted", s.encrypted.map(flag)),
        ("Authenticated Pairing", s.authenticated.map(flag)),
        ("Secure Connections", s.secure_connections.map(flag)),
        (
            "Encryption Key",
            s.key_size.map(|n| format!("{}-bit", n * 8)),
        ),
    ]
    .into_iter()
    .filter_map(|(label, value)| Some((label, value?)))
    .collect()
}

/// The low battery threshold, in percent, the desktop application uses by default.
pub const LOW_BATTERY_PERCENT: i64 = 20;
/// At most this charged is critically low.
pub const CRITICAL_PERCENT: i64 = 5;

/// A device's battery, with whether each reading is current: readings are current only while
/// the device is connected.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Battery {
    pub percent: Option<i64>,
    pub charging: Option<bool>,
    pub fresh: bool,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum Level {
    Ok,
    Low,
    Critical,
}

impl Battery {
    /// The device's last reading; fresh only while `current`, the record is the adapter's
    /// latest, and the device is connected.
    pub fn of(d: &p::Device, current: bool) -> Option<Self> {
        let b = text::battery(d.information.as_ref())?;
        Some(Self {
            percent: b.percent,
            charging: b.charging,
            fresh: current && model::connected(d),
        })
    }

    /// "45%", "45% · Charging" or "Charging"; None when nothing is known.
    pub fn text(&self) -> Option<String> {
        let mut parts = Vec::new();
        if let Some(p) = self.percent {
            parts.push(format!("{p}%"));
        }
        if self.charging == Some(true) {
            parts.push("Charging".into());
        }
        (!parts.is_empty()).then(|| parts.join(" · "))
    }

    /// The level of a current reading; None without a current percent.
    pub fn level(&self) -> Option<Level> {
        if !self.fresh {
            return None;
        }
        if self.charging == Some(true) {
            return Some(Level::Ok);
        }
        let p = self.percent?;
        Some(match p {
            p if p <= CRITICAL_PERCENT => Level::Critical,
            p if p <= LOW_BATTERY_PERCENT => Level::Low,
            _ => Level::Ok,
        })
    }

    pub fn low(&self) -> bool {
        matches!(self.level(), Some(Level::Low | Level::Critical))
    }

    /// Whether the shown reading is only last known.
    pub fn stale(&self) -> bool {
        !self.fresh
    }
}

/// How full the adapter's profile memory is, as a whole percentage rounded half up; None while
/// its use or a nonzero budget is unknown.
pub fn memory_percent(status: &p::Status) -> Option<u32> {
    let memory = crate::profiles::resource(status, ResourceType::Memory)?;
    let used = u64::from(memory.count?) * 100;
    let budget = u64::from(memory.limit.filter(|b| *b > 0)?);
    Some(((used * 2 + budget) / (budget * 2)) as u32)
}

/// The adapter reports its profile memory almost full.
pub fn memory_alert(status: &p::Status) -> bool {
    model::has_error(status, ErrorCode::ProfileMemoryLow)
}

/// The wheel's capability readings as labelled figures.
pub fn wheel_figures(info: Option<&p::DeviceInformation>) -> Vec<(&'static str, String)> {
    let Some(w) = info.and_then(|i| i.wheel.as_ref()) else {
        return Vec::new();
    };
    [
        ("Resolution Multiplier", w.resolution_multiplier, ""),
        ("Ratchets per Rotation", w.ratchets_per_rotation, ""),
        ("Wheel Diameter", w.diameter, " mm"),
    ]
    .into_iter()
    .filter_map(|(label, v, unit)| Some((label, format!("{}{unit}", v?))))
    .collect()
}

/// Makes a pairing code easy to read and compare, with its characters spaced apart.
pub fn spaced(value: &str) -> String {
    let v = text::display(value);
    v.chars().map(String::from).collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_rounds_half_up() {
        let status = |count, limit| p::Status {
            profile_support: Some(p::ProfileSupport {
                resources: vec![p::ResourceCapacity {
                    r#type: ResourceType::Memory as i32,
                    count,
                    limit,
                }],
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(memory_percent(&status(Some(1), Some(200))), Some(1));
        assert_eq!(memory_percent(&status(Some(1), Some(201))), Some(0));
        assert_eq!(memory_percent(&status(Some(0), Some(0))), None);
        // An unknown count isn't a count of zero.
        assert_eq!(memory_percent(&status(None, Some(200))), None);
        assert_eq!(memory_percent(&status(Some(0), Some(200))), Some(0));
        // The adapter decides when memory is low.
        let mut low = status(Some(1), Some(200));
        assert!(!memory_alert(&low));
        low.errors.push(p::Error {
            code: ErrorCode::ProfileMemoryLow as i32,
            ..Default::default()
        });
        assert!(memory_alert(&low));
    }

    #[test]
    fn messages_are_sentences_and_reasons_drop_the_period() {
        assert_eq!(
            code_text(ErrorCode::Busy),
            "The adapter is busy. Try again when the current operation finishes."
        );
        assert_eq!(
            reason(&code_text(ErrorCode::TransportError)),
            "The adapter couldn't exchange messages with this device"
        );
        let storage = Error::dongle(
            p::Error {
                code: ErrorCode::StorageFailed as i32,
                ..Default::default()
            },
            Some("setting list"),
        );
        assert_eq!(read_failure(&storage), READ_FAILED);
        // Only an outcome of no effect confirms the save failed.
        assert_eq!(
            failure(&storage),
            "The adapter couldn't read or write its saved data."
        );
        let refused = p::Error {
            code: ErrorCode::StorageFailed as i32,
            outcome: ErrorOutcome::NoEffect as i32,
            ..Default::default()
        };
        assert_eq!(
            failure(&Error::dongle(refused, None)),
            "The adapter couldn't save the change."
        );
        let closed = Error::new("the connection to the adapter closed");
        assert_eq!(read_failure(&closed), READ_CLOSED);
        assert_eq!(failure(&closed), CLOSED);
        assert_eq!(
            other_adapter("Desk"),
            "Cordial couldn't connect to Desk. Another adapter answered on its port."
        );
    }

    #[test]
    fn outcomes_are_reported_whatever_the_code_and_related_failures_follow() {
        let e = p::Error {
            code: ErrorCode::Timeout as i32,
            outcome: ErrorOutcome::Unknown as i32,
            related: vec![p::Error {
                code: ErrorCode::TransportError as i32,
                ..Default::default()
            }],
            ..Default::default()
        };
        assert_eq!(
            error_text(&e, Scope::Save),
            "This device didn't respond in time. The adapter couldn't confirm whether the change was saved. Check before trying again. Also, the adapter couldn't exchange messages with this device."
        );
        assert_eq!(
            error_text(&e, Scope::Apply),
            "This device didn't respond in time. The adapter couldn't confirm whether the change took effect. Also, the adapter couldn't exchange messages with this device."
        );
        let unknown = p::Error {
            code: ErrorCode::StorageFailed as i32,
            outcome: ErrorOutcome::Unknown as i32,
            ..Default::default()
        };
        assert_eq!(
            failure(&Error::dongle(unknown, None)),
            "The adapter couldn't read or write its saved data. The adapter couldn't confirm whether the change was saved. Check before trying again."
        );
        let radio = p::Error {
            code: ErrorCode::Internal as i32,
            reason: ErrorReason::Radio as i32,
            outcome: ErrorOutcome::Partial as i32,
            related: vec![p::Error {
                code: ErrorCode::NoCapacity as i32,
                reason: ErrorReason::ScanCandidates as i32,
                ..Default::default()
            }],
        };
        assert_eq!(
            error_text(&radio, Scope::Read),
            "The adapter hit an unexpected failure. This involves the adapter's Bluetooth radio. The adapter returned only some of the results. Also, the adapter had no room to list every device it found."
        );
        let starting = p::Error {
            code: ErrorCode::NotReady as i32,
            reason: ErrorReason::Radio as i32,
            ..Default::default()
        };
        assert_eq!(
            error_text(&starting, Scope::Save),
            "The adapter's Bluetooth radio isn't ready. Try again in a moment."
        );
        let newer = p::Error {
            code: 99,
            reason: 98,
            outcome: 97,
            ..Default::default()
        };
        assert_eq!(
            error_text(&newer, Scope::Save),
            "The adapter hit an unexpected failure."
        );
    }

    #[test]
    fn related_failures_keep_their_own_scope() {
        let cleanup = p::Error {
            code: ErrorCode::StorageFailed as i32,
            reason: ErrorReason::Storage as i32,
            outcome: ErrorOutcome::Unknown as i32,
            ..Default::default()
        };
        let e = p::Error {
            code: ErrorCode::AuthFailed as i32,
            outcome: ErrorOutcome::NoEffect as i32,
            related: vec![cleanup],
            ..Default::default()
        };
        assert_eq!(
            error_text(&e, Scope::Pair),
            "Bluetooth authentication with this device failed. Also, the adapter couldn't read or write its saved data. The adapter couldn't confirm whether it finished cleaning up."
        );
        let saved = p::Error {
            code: ErrorCode::StorageFailed as i32,
            reason: ErrorReason::Storage as i32,
            outcome: ErrorOutcome::Complete as i32,
            ..Default::default()
        };
        assert_eq!(
            error_text(&saved, Scope::Pair),
            "The adapter couldn't read or write its saved data. The adapter saved the device despite this."
        );
        let unsupported = p::Error {
            code: ErrorCode::Unsupported as i32,
            outcome: ErrorOutcome::Unknown as i32,
            ..Default::default()
        };
        assert!(!plain(&unsupported));
        assert!(plain(&p::Error {
            code: ErrorCode::Unsupported as i32,
            outcome: 97,
            ..Default::default()
        }));
    }

    #[test]
    fn gamepads_show_as_gamepads() {
        let gamepad = Kind::Gamepad as i32;
        assert_eq!(display_kind(&[gamepad], &[]), DisplayKind::Gamepad);
        assert_eq!(
            display_kind(&[], &[Role::Gamepad as i32]),
            DisplayKind::Gamepad
        );
        assert_eq!(
            display_kind(&[Kind::Keyboard as i32, gamepad], &[]),
            DisplayKind::Keyboard
        );
        assert_eq!(role_text(Role::Gamepad), Some("Gamepad"));
        assert_eq!(text::unnamed(&[Kind::Gamepad]), "Unnamed Gamepad");
    }

    #[test]
    fn a_lack_of_profile_room_is_short_only_when_plain() {
        let room = p::Error {
            code: ErrorCode::ProfileMemoryExceeded as i32,
            reason: ErrorReason::ProfileMemory as i32,
            ..Default::default()
        };
        let short = "The adapter doesn't have room for them. Disconnect another device or give this one fewer profiles.";
        assert_eq!(profile_error_text(&room), short);
        let unknown = p::Error {
            outcome: ErrorOutcome::Unknown as i32,
            ..room.clone()
        };
        assert_eq!(
            profile_error_text(&unknown),
            "The adapter doesn't have room for this device's profiles. The adapter couldn't confirm whether the change took effect."
        );
        let elsewhere = p::Error {
            reason: ErrorReason::Storage as i32,
            ..room
        };
        assert_ne!(profile_error_text(&elsewhere), short);
    }

    #[test]
    fn clean_removes_controls_and_formats() {
        assert_eq!(clean(" a\u{1b}b\u{202e}c "), "a bc");
        assert_eq!(clean("x\u{2028}y"), "x y");
    }

    #[test]
    fn battery_levels_need_a_current_reading() {
        let b = |percent, charging, fresh| Battery {
            percent,
            charging,
            fresh,
        };
        assert_eq!(b(Some(5), None, true).level(), Some(Level::Critical));
        assert_eq!(b(Some(20), None, true).level(), Some(Level::Low));
        assert_eq!(b(Some(21), None, true).level(), Some(Level::Ok));
        assert_eq!(b(Some(3), Some(true), true).level(), Some(Level::Ok));
        assert_eq!(b(Some(3), None, false).level(), None);
        assert_eq!(
            b(Some(45), Some(true), true).text().unwrap(),
            "45% · Charging"
        );
    }
}
