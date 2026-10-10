//! The TUI's words for adapter codes and records, the same words the desktop application uses.
//! Messages are complete sentences; labels and status words are Title Case.
use crate::{error::Error, model, ui::text};
use cordial_protocol::{
    self as p, CapacityReason, DeviceState, ErrorCode, InactiveReason, Kind, Role, Transport, keys,
    value::Value,
};

fn error_clause(code: ErrorCode) -> &'static str {
    use ErrorCode::*;
    match code {
        Unknown | Internal => "the adapter hit an unexpected failure",
        BadRequest => "the adapter rejected a malformed request",
        UnknownCommand => "this adapter's firmware doesn't support that command",
        BadArgs => "the adapter rejected the command's arguments",
        TooLong => "the request was too large for the adapter",
        NotReady => "the adapter's Bluetooth or storage isn't ready. Try again in a moment",
        NotFound => "the adapter couldn't find this device, setting or profile",
        NotConnected => "the device isn't connected. Connect it first",
        Busy => "the adapter is busy. Try again when the current operation finishes",
        Disabled => "the device is turned off in Cordial; turn on “Use This Device” first",
        Blocked => "connections to this device are blocked. Unblock it before connecting",
        Unsupported => "the adapter or device doesn't support this action",
        NoCapacity => "the adapter has no room for that right now",
        NoPrompt => "that pairing prompt is no longer waiting for an answer",
        StorageFailed => "the adapter couldn't save the change. Your saved data hasn't changed",
        InUse => {
            "this profile is still in use. Remove it from every device and interface before deleting it"
        }
        CandidateExpired => "this device is no longer available. Search again",
        AuthFailed => "Bluetooth authentication failed",
        Rejected => "authentication was rejected by you or the device",
        Timeout => "the operation timed out",
        Cancelled => "the operation was cancelled",
        ConnectionFailed => "the Bluetooth link or HID setup failed",
        UnsupportedHid => "the device's HID format isn't supported",
        ProtocolUnsupported => "not supported",
        FeatureUnavailable => "the device doesn't provide a feature this action needs",
        TransportError => "couldn't send",
        DeviceError => "device error",
        InvalidResponse => "unexpected reply",
        ReadbackMismatch => "the device reported a different value from the one requested",
    }
}

fn capacity_clause(reason: CapacityReason) -> &'static str {
    match reason {
        CapacityReason::Unknown => error_clause(ErrorCode::NoCapacity),
        CapacityReason::Enabled => {
            "every enabled-device place is in use; turn off another device first"
        }
        CapacityReason::Storage => {
            "the adapter's storage is full. Remove an unused device or forget a saved setting, then try again"
        }
        CapacityReason::Connections => "every connection is in use; disconnect a device first",
        CapacityReason::ProfileMemory => {
            "the adapter doesn't have enough profile memory for this change"
        }
    }
}

pub fn code_text(code: ErrorCode) -> String {
    text::capitalized(error_clause(code))
}

/// An adapter error as a sentence.
pub fn error_text(e: &p::Error) -> String {
    match e.code() {
        ErrorCode::NoCapacity => text::capitalized(capacity_clause(e.reason())),
        ErrorCode::StorageFailed if e.outcome_unknown => {
            "The adapter couldn't confirm whether the change was saved. Check before trying again."
                .into()
        }
        code => code_text(code),
    }
}

pub const GONE: &str = "This device or adapter is no longer available.";
pub const ADAPTER_GONE: &str = "This adapter is no longer available.";
pub const CLOSED: &str = "The adapter disconnected before the change finished.";
pub const UNEXPECTED: &str = "The adapter returned an unexpected result.";
pub const NOT_PLUGGED_IN: &str = "The adapter isn't plugged in.";
pub const IN_USE_ELSEWHERE: &str = "Couldn't connect. Is another program using it?";
pub const STORAGE_FULL: &str = "Storage Full";
pub const STORAGE_FULL_ATTENTION: &str = "The adapter's storage is full. Remove an unused device or forget a saved setting to pair another device.";
pub const WARNINGS_READ_FAILED: &str = "The adapter couldn't read the device's warnings.";
pub const SETTINGS_READ_FAILED: &str = "The adapter couldn't read the device's settings.";
pub const SETTINGS_SAVE_FAILED: &str = "The adapter couldn't save the settings.";
pub const RECONNECT_TITLE: &str = "USB Reconnect Required";
pub const RECONNECT_TEXT: &str =
    "The adapter will disconnect from this computer for a moment after it saves these changes.";
pub const FORGET_TEXT: &str =
    "Forgetting this device deletes its pairing and saved settings from the adapter.";
pub const ADAPTER_NAME_INVALID: &str = "Enter an adapter name of up to 64 bytes.";
pub const PROFILE_NAME_INVALID: &str = "Enter a profile name of up to 64 bytes.";

/// A failed action as a sentence: the adapter's refusal, or the local reason.
pub fn failure(e: &Error) -> String {
    if let Some(w) = &e.dongle {
        return error_text(w);
    }
    match e.message.as_str() {
        "the connection to the adapter closed" => CLOSED.into(),
        "the adapter returned an unexpected result" => UNEXPECTED.into(),
        m if m == crate::commands::STARTING => code_text(ErrorCode::NotReady),
        "the control session is unavailable; select an adapter" => ADAPTER_GONE.into(),
        m => text::sentence(&text::display(m)),
    }
}

/// A failure that refuses work on a disabled transport names the transport.
pub fn transport_failure(e: &Error, status: &p::Status, t: Transport) -> String {
    if e.code_of() == Some(ErrorCode::Unsupported) && model::transport_disabled(status, t) {
        return transport_disabled_text(t);
    }
    failure(e)
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
    match (keyboard, mouse) {
        (true, true) => DisplayKind::KeyboardMouse,
        (true, false) => DisplayKind::Keyboard,
        (false, true) => DisplayKind::Mouse,
        _ => DisplayKind::Other,
    }
}

pub fn kind_text(kind: DisplayKind) -> &'static str {
    match kind {
        DisplayKind::Keyboard => "Keyboard",
        DisplayKind::Mouse => "Mouse",
        DisplayKind::KeyboardMouse => "Keyboard and Mouse",
        DisplayKind::Other => "Other",
    }
}

pub fn role_text(r: Role) -> Option<&'static str> {
    Some(match r {
        Role::Keyboard => "Keyboard",
        Role::Mouse => "Mouse",
        Role::ConsumerControl => "Media Keys",
        Role::SystemControl => "System Keys",
        Role::Unknown => return None,
    })
}

/// Why a connected device's profiles aren't loaded.
pub fn profile_error_text(code: ErrorCode) -> String {
    match code {
        ErrorCode::NoCapacity => "The adapter doesn't have room for them. Disconnect another device or give this one fewer profiles.".into(),
        ErrorCode::StorageFailed => "The adapter couldn't read one of this device's profiles.".into(),
        code => code_text(code),
    }
}

/// HID++'s state, as Diagnostics shows it.
pub fn integration_text(i: &p::Integration) -> String {
    match model::up(i) {
        model::Up::Off => "Off".into(),
        model::Up::Disconnected => "Waiting to Connect".into(),
        model::Up::Starting => "Setting Up".into(),
        model::Up::Active => "Active".into(),
        model::Up::Unsupported => "Unsupported".into(),
        model::Up::Error(code) => format!("Failed: {}", code_text(code)),
    }
}

/// The detected HID++ version, as "4.5".
pub fn version_text(d: &p::Device) -> String {
    match model::hidpp_version(d) {
        Some((major, minor)) => format!("{major}.{minor}"),
        None => "Unknown".into(),
    }
}

/// The link security facts, labelled.
pub fn security_facts(s: &p::Security) -> [(&'static str, String); 4] {
    let flag = |v: Option<bool>| match v {
        None => "Not Reported".to_owned(),
        Some(true) => "Yes".into(),
        Some(false) => "No".into(),
    };
    [
        ("Encrypted", flag(s.encrypted)),
        ("Authenticated Pairing", flag(s.authenticated)),
        ("Secure Connections", flag(s.secure_connections)),
        (
            "Encryption Key",
            match s.key_size {
                Some(n) => format!("{}-bit", n * 8),
                None => "Not Reported".into(),
            },
        ),
    ]
}

/// Labels of the information keys, in display order.
pub const INFO_LABELS: [(&str, &str); 11] = [
    (keys::DEVICE_MANUFACTURER, "Manufacturer"),
    (keys::DEVICE_MODEL, "Model"),
    (keys::DEVICE_SERIAL, "Serial Number"),
    (keys::FIRMWARE_VERSION, "Firmware"),
    (keys::BOOTLOADER_VERSION, "Bootloader"),
    (keys::HARDWARE_REVISION, "Hardware"),
    (keys::SOFTWARE_REVISION, "Software"),
    (keys::VENDOR_REGISTRY, "Vendor ID Namespace"),
    (keys::VENDOR_ID, "Vendor ID"),
    (keys::PRODUCT_ID, "Product ID"),
    (keys::PRODUCT_VERSION, "Product Version"),
];

pub fn info_label(key: &str) -> &'static str {
    INFO_LABELS
        .iter()
        .find(|(k, _)| *k == key)
        .map_or("", |(_, l)| l)
}

/// An information value as shown.
pub fn info_value(key: &str, v: &Value) -> String {
    match (key, v) {
        (keys::VENDOR_ID | keys::PRODUCT_ID | keys::PRODUCT_VERSION, Value::Integer(n)) => {
            format!("0x{n:04X}")
        }
        (keys::VENDOR_REGISTRY, Value::Text(t)) if t == "usb" => "USB".into(),
        (keys::VENDOR_REGISTRY, _) => "Bluetooth".into(),
        (_, Value::Bool(b)) => if *b { "Yes" } else { "No" }.into(),
        (_, Value::Color(c)) => format!("#{c:06X}"),
        (_, Value::Integer(n)) => n.to_string(),
        (_, Value::Text(t)) => text::display(t),
    }
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
    pub fn of(d: &p::Device) -> Option<Self> {
        let b = text::battery(&d.info)?;
        Some(Self {
            percent: b.percent,
            charging: b.charging,
            fresh: model::connected(d),
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

/// How full the adapter's profile memory is, as a whole percentage rounded half up.
pub fn memory_percent(status: &p::Status) -> Option<u32> {
    let support = status
        .profile_support
        .as_ref()
        .filter(|s| s.memory_budget > 0)?;
    let used = u64::from(support.memory_used) * 100;
    let budget = u64::from(support.memory_budget);
    Some(((used * 2 + budget) / (budget * 2)) as u32)
}

/// At or above this share of the profile memory budget, the adapter needs attention.
pub const MEMORY_ALERT_PERCENT: u64 = 85;

pub fn memory_alert(status: &p::Status) -> bool {
    status.profile_support.as_ref().is_some_and(|s| {
        s.memory_budget > 0
            && u64::from(s.memory_used) * 100 >= u64::from(s.memory_budget) * MEMORY_ALERT_PERCENT
    })
}

/// The wheel's capability readings as labelled figures.
pub fn wheel_figures(info: &[p::Info]) -> Vec<(&'static str, String)> {
    [
        (
            keys::WHEEL_RESOLUTION_MULTIPLIER,
            "Resolution Multiplier",
            "",
        ),
        (
            keys::WHEEL_RATCHETS_PER_ROTATION,
            "Ratchets per Rotation",
            "",
        ),
        (keys::WHEEL_DIAMETER, "Wheel Diameter", " mm"),
    ]
    .into_iter()
    .filter_map(|(key, label, unit)| {
        let v = model::info(info, key)?;
        let shown = match v {
            Value::Integer(n) => n.to_string(),
            Value::Text(t) => text::display(t),
            Value::Bool(b) => if *b { "Yes" } else { "No" }.into(),
            Value::Color(c) => format!("#{c:06X}"),
        };
        Some((label, format!("{shown}{unit}")))
    })
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
        let status = |used, budget| p::Status {
            profile_support: Some(p::ProfileSupport {
                memory_used: used,
                memory_budget: budget,
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(memory_percent(&status(1, 200)), Some(1));
        assert_eq!(memory_percent(&status(1, 201)), Some(0));
        assert_eq!(memory_percent(&status(0, 0)), None);
        assert!(memory_alert(&status(85, 100)));
        assert!(!memory_alert(&status(84, 100)));
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
