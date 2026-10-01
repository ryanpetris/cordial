//! Human-readable text shared by the shell, scripts and the TUI: safe terminal
//! rendering of untrusted strings, the host's explanations of adapter codes,
//! and the shell's line formats.
use crate::{
    client::{Envelope, Error},
    controller::{Command, DeviceInfoView, Notice, Outcome, State, Subject},
    transport::PortInfo,
    ui::catalog,
};
use cordial_protocol::{
    errors::{
        CapacityReason, DisabledReason, ErrorCode, PairUnavailable, StorageOutcome,
        ValidationError, WarningCode,
    },
    hidpp::ProtocolState,
    identifiers::{
        ConnectionState, DeviceId, HostPlatform, NormalizationState, PairingState, Role,
        SettingsState, Transport,
    },
    info::{DeviceInfo, InfoField, InfoKey},
    messages::{
        Candidate, Capabilities, Capability, ConnectionSecurity, Device, DeviceKind, Prompt,
        SettingChunk, Status, WireError,
    },
    payloads::{AdapterSettings, BootloaderMode, DeviceUnpaired, FileEntry, ScanEnd},
    settings::SettingValue,
};
use ratatui::buffer::CellWidth;
use serde::Serialize;
use serde_json::Value;
use std::fmt::Write;
use unicode_properties::{GeneralCategoryGroup, UnicodeGeneralCategory};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthChar;

/// Printable: letters, marks, numbers, punctuation, symbols and the ASCII
/// space. Controls, format characters (including
/// bidirectional controls and tags), other separators, private-use and
/// unassigned code points are not.
fn printable(c: char) -> bool {
    c == ' '
        || !matches!(
            c.general_category_group(),
            GeneralCategoryGroup::Separator | GeneralCategoryGroup::Other
        )
}

/// Escapes characters that could control or reorder terminal output.
pub fn safe(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if printable(c) {
            out.push(c);
        } else {
            let _ = write!(out, "\\u{:04x}", c as u32);
        }
    }
    out
}

/// `safe` for each line of multiline text.
pub fn safe_lines(s: &str) -> String {
    s.split('\n').map(safe).collect::<Vec<_>>().join("\n")
}

/// Preserves JSON values and envelopes while escaping Unicode terminal
/// controls that JSON permits as literal characters. ASCII bytes are unchanged.
pub fn terminal_json(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if c as u32 >= 0x7f && !printable(c) {
            let mut units = [0u16; 2];
            for unit in c.encode_utf16(&mut units) {
                let _ = write!(out, "\\u{unit:04x}");
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Display width in cells, measured as the renderer draws text.
pub fn width(s: &str) -> usize {
    s.graphemes(true)
        .filter(|g| !g.contains(char::is_control))
        .map(|g| usize::from(g.cell_width()))
        .sum()
}

fn char_width(c: char) -> usize {
    c.width().unwrap_or(0)
}

/// Escapes characters whose width depends on the terminal: the trailing parts
/// of grapheme clusters measured differently by grapheme-aware and per-code-point
/// terminals, such as emoji variation selectors, a leading cluster that would
/// join the text drawn before it, and prepended characters that would join the
/// text after them. Layout arithmetic then matches either kind of terminal,
/// keeping click targets on their labels.
pub fn settle(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let escape = |out: &mut String, c: char| {
        if c as u32 > 0xffff {
            let _ = write!(out, "\\U{:08x}", c as u32); // Unambiguous beside hex digits.
        } else {
            let _ = write!(out, "\\u{:04x}", c as u32);
        }
    };
    let joins_next = |c: char| {
        let s = format!("{c}a");
        s.graphemes(true)
            .next()
            .is_some_and(|g| g.len() > c.len_utf8())
    };
    for (i, cluster) in s.graphemes(true).enumerate() {
        let joined = format!("a{cluster}");
        let whole = i == 0 && joined.graphemes(true).next().is_some_and(|g| g.len() > 1);
        let mixed =
            usize::from(cluster.cell_width()) != cluster.chars().map(char_width).sum::<usize>();
        for (j, c) in cluster.chars().enumerate() {
            let alone = c.to_string();
            let odd = usize::from(alone.cell_width()) != char_width(c);
            if whole || joins_next(c) || mixed && (j > 0 || odd) {
                escape(&mut out, c);
            } else {
                out.push(c);
            }
        }
    }
    out
}

/// Escapes untrusted text for the TUI.
pub fn display(s: &str) -> String {
    settle(&safe(s))
}

/// A device name for the shell.
pub fn name(n: Option<&str>) -> String {
    match n {
        Some(n) if !n.is_empty() => safe(n),
        _ => "(unnamed)".into(),
    }
}

/// A device name for the TUI.
pub fn display_name(n: Option<&str>) -> String {
    match n {
        Some(n) if !n.is_empty() => display(n),
        _ => "(unnamed)".into(),
    }
}

/// Whether a nearby device advertised a name.
pub fn named(c: &Candidate) -> bool {
    c.name.as_deref().is_some_and(|n| !n.is_empty())
}

/// What an unnamed nearby device is called, by the kind it advertised.
pub fn unnamed(kind: DeviceKind) -> &'static str {
    match kind {
        DeviceKind::Keyboard => "Unnamed Keyboard",
        DeviceKind::Mouse => "Unnamed Mouse",
        DeviceKind::KeyboardMouse => "Unnamed Keyboard/Mouse",
        DeviceKind::Unknown => "Unnamed Device",
    }
}

/// A nearby device's name for the shell, or its kind when unnamed.
pub fn candidate_name(c: &Candidate) -> String {
    if named(c) {
        name(c.name.as_deref())
    } else {
        unnamed(c.kind).into()
    }
}

/// A nearby device's name for the TUI, or its kind when unnamed.
pub fn display_candidate_name(c: &Candidate) -> String {
    if named(c) {
        display_name(c.name.as_deref())
    } else {
        unnamed(c.kind).into()
    }
}

/// The wire spelling of a protocol value, such as `connected` or `ble`.
pub fn wire<T: Serialize>(value: &T) -> String {
    match serde_json::to_value(value) {
        Ok(Value::String(s)) => s,
        Ok(other) => other.to_string(),
        Err(_) => String::new(),
    }
}

/// Double-quotes text, escaping quotes, backslashes and nonprintable
/// characters.
pub fn quote(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            c if printable(c) => out.push(c),
            c if (c as u32) < 0x80 => {
                let _ = write!(out, "\\x{:02x}", c as u32);
            }
            c if (c as u32) <= 0xffff => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => {
                let _ = write!(out, "\\U{:08x}", c as u32);
            }
        }
    }
    out.push('"');
    out
}

pub fn on_off(b: bool) -> &'static str {
    if b { "on" } else { "off" }
}

pub fn yes_no(b: bool) -> &'static str {
    if b { "Yes" } else { "No" }
}

pub fn transport_name(t: Transport) -> &'static str {
    match t {
        Transport::Ble => "BLE",
        Transport::Classic => "Classic",
    }
}

pub fn transport_long(t: Transport) -> &'static str {
    match t {
        Transport::Ble => "Bluetooth LE",
        Transport::Classic => "Bluetooth Classic",
    }
}

pub fn role_names(roles: &[Role]) -> String {
    roles
        .iter()
        .map(|r| match r {
            Role::Keyboard => "Keyboard",
            Role::Mouse => "Mouse",
            Role::ConsumerControl => "Media keys",
        })
        .collect::<Vec<_>>()
        .join(", ")
}

pub fn warning_text(w: WarningCode) -> &'static str {
    match w {
        WarningCode::UnsupportedFields => "Some input fields are unsupported",
        WarningCode::LedOutputUnavailable => "Lock indicator output is unsupported",
    }
}

pub fn platform_name(p: HostPlatform) -> &'static str {
    match p {
        HostPlatform::Linux => "Linux",
        HostPlatform::Windows => "Windows",
        HostPlatform::Mac => "Mac",
    }
}

/// The host's explanation of an adapter error code. The adapter sends codes
/// only; every explanation is the host's own.
fn general_text(code: ErrorCode) -> Option<&'static str> {
    use ErrorCode::*;
    Some(match code {
        InvalidRequest => "the adapter rejected a malformed request",
        InvalidJson => "the adapter couldn't parse a request",
        MessageTooLarge => "a request exceeded the adapter's line limit",
        UnsupportedVersion => "the adapter doesn't support this protocol version",
        UnknownCommand => "this adapter's firmware doesn't support that command",
        InvalidArgs => "the adapter rejected the command's arguments",
        Busy => "the adapter is busy with a conflicting operation; try again when it finishes",
        NotFound => "the adapter has no saved device with that ID",
        Blocked => "the device is blocked; unblock it before connecting",
        HeartbeatRequired => "the adapter stopped hearing from this app; try again",
        ClientTimeout => "stopped because the adapter stopped hearing from this app",
        CandidateExpired => "that nearby device is no longer available; scan again",
        Disabled => "the device is disabled; enable it before connecting",
        PairingRequired => {
            "the device needs pairing again; put it in pairing mode, scan, then pair it again"
        }
        Capacity => "the adapter has no room for that right now",
        UnsupportedTransport => "this build does not support the requested Bluetooth transport",
        UnsupportedHid => "the device's HID format isn't supported",
        AuthenticationFailed => "Bluetooth authentication failed",
        AuthenticationRejected => "authentication was rejected by the user or the device",
        StalePrompt => "that pairing prompt is no longer waiting for an answer",
        ConnectionFailed => "the Bluetooth link or HID setup failed",
        RadioUnavailable => "the adapter's Bluetooth controller isn't ready",
        InputOverflow => {
            "input was dropped because the computer didn't read it before the adapter's queue filled"
        }
        StorageFailed => "the adapter couldn't save the change; what it had saved is unchanged",
        StorageFull => {
            "the adapter's storage is full, so the change wasn't saved or applied; remove unused devices or set saved settings back to Default to make room"
        }
        Timeout => "the operation's deadline expired",
        Cancelled => "the operation was cancelled",
        NotPending => "that request has already finished",
        NotCancellable => "that operation can't be cancelled",
        SessionFault => {
            "the adapter stopped this session because its output stalled; reopen the port"
        }
        InternalError => "the adapter hit an unexpected failure",
        HidppDisabled => "HID++ is off for this device; turn it on to apply saved settings",
        NotConnected => "the device isn't connected; connect it first",
        ReadOnly => "that setting is read-only information from the device",
        SettingsUnavailable => {
            "the adapter hasn't read this device's settings yet; it reads them when the device connects"
        }
        UnsupportedSetting => "the device doesn't support that setting or value now",
        SettingsLimit => {
            "the adapter can't save more settings for this device; set another one back to Default first"
        }
        SettingsRefreshFailed => "some settings couldn't be read",
        SettingsApplyFailed => "some saved values couldn't be applied or confirmed",
        _ => return None,
    })
}

/// Short explanations of HID++ and setting diagnostics, as device and setting
/// records carry them.
fn hidpp_text(code: ErrorCode) -> Option<&'static str> {
    use ErrorCode::*;
    Some(match code {
        HidppTimeout => "no response",
        HidppTransportError => "couldn't send",
        HidppDeviceError => "device error",
        HidppInvalidResponse => "unexpected reply",
        HidppResetUnavailable | HidppControlsUnavailable => "special keys unavailable",
        HidppProtocolUnsupported | HidppReportsUnavailable => "not supported",
        HidppDisabled => "HID++ is off",
        NotConnected => "the device disconnected",
        UnsupportedSetting => "not supported by the device now",
        ReadbackMismatch => "the device reported a different value after the change",
        FeatureSetUnavailable => "the device's feature list couldn't be read",
        BacklightModeSelectionRequired => {
            "the backlight is in temporary manual mode; choose a backlight mode first"
        }
        BacklightPermanentManualRequired => {
            "the level only applies in permanent manual backlight mode"
        }
        NativeStandardResolutionRequired => {
            "the wheel isn't in its native standard-resolution mode"
        }
        NativeRoutingRequired => "the thumbwheel isn't in its native mode",
        _ => return None,
    })
}

/// Explains a HID++ or setting diagnostic: its short wording, else the
/// general explanation.
pub fn hidpp_words(code: ErrorCode) -> String {
    hidpp_text(code)
        .or_else(|| general_text(code))
        .map(str::to_owned)
        .unwrap_or_else(|| format!("the adapter reported error {}", quote(&wire(&code))))
}

/// Explains a HID++ runtime error of a device record.
pub fn hidpp_error_text(code: Option<ErrorCode>) -> String {
    code.map_or_else(|| "unknown error".into(), hidpp_words)
}

pub fn hidpp_protocol_text(protocol: ProtocolState) -> String {
    match protocol {
        ProtocolState::Unknown => "Unknown".into(),
        ProtocolState::Probing => "Checking".into(),
        ProtocolState::Detected { major, minor } => format!("{major}.{minor}"),
        ProtocolState::Unavailable => "Unavailable".into(),
        ProtocolState::Error { code } => format!("Failed: {}", hidpp_words(code)),
    }
}

/// The command-specific or general explanation of an error code.
fn code_text(code: ErrorCode, command: Option<&str>) -> String {
    use ErrorCode::*;
    let specific = match (code, command) {
        (NotFound, Some("hidpp.setting.get" | "hidpp.setting.set" | "hidpp.setting.forget")) => {
            Some("the adapter doesn't know that setting for the device, or the device isn't saved")
        }
        (StorageFailed, Some("adapter.wait_ready")) => {
            Some("the adapter's saved-device storage couldn't be initialized")
        }
        (RadioUnavailable, Some("adapter.wait_ready")) => {
            Some("the adapter's Bluetooth controller couldn't be initialized")
        }
        (Timeout, Some("adapter.wait_ready")) => Some("the adapter didn't finish starting in time"),
        _ => None,
    };
    specific
        .or_else(|| general_text(code))
        .or_else(|| hidpp_text(code))
        .map(str::to_owned)
        .unwrap_or_else(|| format!("the adapter reported error {}", quote(&wire(&code))))
}

const OUTCOMES: [&str; 6] = [
    "read",
    "applied",
    "unchanged",
    "unsupported",
    "failed",
    "uncertain",
];

fn scalar(v: Option<&Value>) -> Option<String> {
    match v? {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

/// Error details as short phrases: named IDs, outcome counts, then any other
/// plain values by name.
fn details(e: &WireError) -> Option<serde_json::Map<String, Value>> {
    match serde_json::to_value(e.details.as_ref()?).ok()? {
        Value::Object(details) => Some(details),
        _ => None,
    }
}

fn facts(e: &WireError) -> Vec<String> {
    let Some(details) = details(e) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (key, name) in [
        ("device_id", "device"),
        ("candidate_id", "candidate"),
        ("request_id", "request"),
        ("key", "setting"),
    ] {
        if let Some(v) = scalar(details.get(key)) {
            out.push(format!("{name} {v}"));
        }
    }
    let counted = matches!(
        e.code,
        ErrorCode::SettingsRefreshFailed | ErrorCode::SettingsApplyFailed
    );
    if counted {
        for o in OUTCOMES {
            if let Some(v) = scalar(details.get(o)).filter(|v| v != "0") {
                out.push(format!("{v} {o}"));
            }
        }
        if let Some(v) = scalar(details.get("count")) {
            out.push(format!("of {v}"));
        }
    }
    let mut rest: Vec<&String> = details
        .keys()
        .filter(|k| {
            !matches!(
                k.as_str(),
                "device_id" | "candidate_id" | "request_id" | "key" | "revision"
            ) && !(counted && (*k == "count" || OUTCOMES.contains(&k.as_str())))
        })
        .collect();
    rest.sort();
    for k in rest {
        if let Some(v) = scalar(details.get(k)) {
            out.push(format!("{} {v}", k.replace('_', " ")));
        }
    }
    out
}

/// Why a capacity error happened, from its `details.reason`.
pub fn capacity_words(reason: CapacityReason) -> &'static str {
    match reason {
        CapacityReason::EnabledFull => {
            "every enabled-device place is in use; disable another device first"
        }
        CapacityReason::StorageFull => {
            "the adapter's storage has no room for another paired device; remove unused devices or saved settings"
        }
        CapacityReason::SetupCapacity => {
            "the adapter's Bluetooth stack has no pairing place available"
        }
        CapacityReason::ConnectionsFull => "every connection is in use; disconnect a device first",
    }
}

/// Why status says Pair is unavailable for a transport.
pub fn pair_unavailable_words(reason: PairUnavailable) -> &'static str {
    match reason {
        PairUnavailable::StorageFull => "storage is full; remove unused devices or saved settings",
        PairUnavailable::SetupCapacity => "the Bluetooth stack has no pairing place available",
        PairUnavailable::ConnectionsFull => "every connection is in use; disconnect a device first",
        PairUnavailable::PairingActive => "another pairing is in progress",
        PairUnavailable::RadioUnavailable => "Bluetooth isn't ready",
        PairUnavailable::StorageUnavailable => "adapter storage isn't ready",
    }
}

/// Why Pair is unavailable for a transport, as a short label.
pub fn pair_unavailable_label(reason: PairUnavailable) -> &'static str {
    match reason {
        PairUnavailable::StorageFull => "Storage Full",
        PairUnavailable::SetupCapacity => "No Pairing Slot",
        PairUnavailable::ConnectionsFull => "All Connections in Use",
        PairUnavailable::PairingActive => "Pairing in Progress",
        PairUnavailable::RadioUnavailable => "Bluetooth Not Ready",
        PairUnavailable::StorageUnavailable => "Storage Not Ready",
    }
}

/// One transport's room for new pairings as a short label. Estimates are
/// advisory and never combined across transports.
pub fn pairing_room_label(p: &cordial_protocol::messages::PairingCapacity) -> String {
    match (p.available, p.reason) {
        (true, _) if p.estimated_additional > 0 => {
            format!("About {} More", p.estimated_additional)
        }
        (true, _) => "Available".into(),
        (false, Some(reason)) => pair_unavailable_label(reason).into(),
        (false, None) => "Unavailable".into(),
    }
}

/// Why a saved device is not active for connections.
pub fn disabled_words(reason: DisabledReason) -> &'static str {
    match reason {
        DisabledReason::UnsupportedTransport => {
            "this build doesn't support its Bluetooth transport"
        }
        DisabledReason::Invalid => "its saved record is invalid",
        DisabledReason::Blocked => "it is blocked",
        DisabledReason::Disabled => "it is disabled",
        DisabledReason::Capacity => {
            "every enabled-device place is in use; disable another device to make room"
        }
    }
}

/// A saved record's validation problem in words.
pub fn validation_words(error: ValidationError) -> &'static str {
    match error {
        ValidationError::BondMissing => "its saved bond is missing; pair it again",
        ValidationError::BondCorrupt => "its saved bond is damaged; pair it again",
        ValidationError::BondMismatch => {
            "its saved bond belongs to a different identity; pair it again"
        }
        ValidationError::DeviceCorrupt => "its saved device record is damaged",
        ValidationError::ReadFailed => "the adapter couldn't read its saved record",
    }
}

/// A saved device's Bluetooth enablement: preference, then why it isn't active.
pub fn enablement_words(d: &Device) -> String {
    match (d.enabled, d.enabled_reason) {
        (_, None) => "enabled".into(),
        (false, Some(DisabledReason::Disabled)) => "disabled".into(),
        (preferred, Some(reason)) => format!(
            "{}, not active: {}",
            if preferred { "enabled" } else { "disabled" },
            disabled_words(reason)
        ),
    }
}

/// Explanations chosen by an error's details rather than its code alone.
fn detail_text(e: &WireError) -> Option<(String, &'static str)> {
    let details = details(e)?;
    match e.code {
        ErrorCode::Capacity => {
            let reason = serde_json::from_value(details.get("reason")?.clone()).ok()?;
            Some((
                format!("the adapter has no room: {}", capacity_words(reason)),
                "reason",
            ))
        }
        ErrorCode::StorageFailed => {
            let outcome = serde_json::from_value(details.get("outcome")?.clone()).ok()?;
            Some((
                match outcome {
                    StorageOutcome::NotSaved => general_text(ErrorCode::StorageFailed)?.into(),
                    StorageOutcome::Unknown => "save status unknown: the adapter couldn't confirm whether the change was saved, and didn't apply it; check the current state before trying again".into(),
                },
                "outcome",
            ))
        }
        // Storage full always means not saved; the outcome adds nothing.
        ErrorCode::StorageFull => {
            let _: StorageOutcome = serde_json::from_value(details.get("outcome")?.clone()).ok()?;
            Some((general_text(ErrorCode::StorageFull)?.into(), "outcome"))
        }
        _ => None,
    }
}

/// Explains an adapter error with its details, without the code.
pub fn wire_text(e: &WireError, command: Option<&str>) -> String {
    let explained = detail_text(e);
    let mut text = explained
        .as_ref()
        .map_or_else(|| code_text(e.code, command), |(t, _)| t.clone());
    let mut facts = facts(e);
    if let Some((_, key)) = explained {
        let prefix = format!("{key} ");
        facts.retain(|f| !f.starts_with(&prefix));
    }
    if !facts.is_empty() {
        let _ = write!(text, " ({})", facts.join(", "));
    }
    text
}

/// An adapter error with its code before the explanation, as the shell and
/// device details show it.
pub fn wire_line(e: &WireError, command: Option<&str>) -> String {
    format!("{}: {}", wire(&e.code), wire_text(e, command))
}

/// A host error for the shell, unescaped. An adapter error names its code
/// before the explanation; a local message that wraps one keeps its context
/// with the code's token expanded.
pub fn error_line(e: &Error) -> String {
    match &e.wire {
        Some(w) => {
            let code = wire(&w.code);
            let line = wire_line(w, e.command);
            if e.message == code {
                line
            } else if e.message.contains(&code) {
                e.message.replacen(&code, &line, 1)
            } else {
                format!("{}: {}", e.message, line)
            }
        }
        None => e.message.clone(),
    }
}

/// Explains an error for the TUI: an adapter error without its code.
pub fn error_words(e: &Error) -> String {
    match &e.wire {
        Some(w) if e.message == wire(&w.code) => display(&wire_text(w, e.command)),
        _ => display(&error_line(e)),
    }
}

/// Sentence-cases TUI activity text.
pub fn capitalized(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

fn quoted_error(code: ErrorCode) -> String {
    quote(&hidpp_words(code))
}

/// The observed security of a device's current link. Only a connected
/// device has one, so a report left over from an earlier link is never shown.
pub fn link_security(d: &Device) -> Option<Option<&ConnectionSecurity>> {
    (d.state == ConnectionState::Connected).then_some(d.security.as_ref())
}

/// A short summary of link security: encryption and whether pairing was
/// authenticated. Unauthenticated encryption is a valid link, not a failure,
/// and no combination is called simply secure.
pub fn security_summary(s: Option<&ConnectionSecurity>) -> &'static str {
    let Some(s) = s else {
        return "Security Not Reported";
    };
    match (s.encrypted, s.authenticated) {
        (Some(false), _) => "Not Encrypted",
        (None, _) => "Encryption Not Reported",
        (Some(true), Some(true)) => "Encrypted, Authenticated",
        (Some(true), Some(false)) => "Encrypted, Unauthenticated",
        (Some(true), None) => "Encrypted",
    }
}

/// What a link property reports: a yes/no fact, a plain value such as the
/// key length, or nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reading {
    Yes,
    No,
    Value,
    NotReported,
}

/// One link property: its name, the value in words, and what it reports.
pub struct SecurityFact {
    pub label: &'static str,
    pub value: String,
    pub reading: Reading,
}

/// Every link security property, each marked when not reported. Key length
/// is a number, not a verdict, so no length is marked as approved.
pub fn security_facts(s: &ConnectionSecurity) -> [SecurityFact; 5] {
    let flag = |label, v: Option<bool>| SecurityFact {
        label,
        value: v
            .map_or("Not Reported", |b| if b { "Yes" } else { "No" })
            .into(),
        reading: match v {
            Some(true) => Reading::Yes,
            Some(false) => Reading::No,
            None => Reading::NotReported,
        },
    };
    [
        flag("Encryption", s.encrypted),
        flag("Authenticated Pairing (MITM Protection)", s.authenticated),
        flag("Secure Connections", s.secure_connections),
        SecurityFact {
            label: "Encryption Key",
            value: s.key_size.map_or("Not Reported".into(), |b| {
                format!("{} bits", u16::from(b) * 8)
            }),
            reading: s.key_size.map_or(Reading::NotReported, |_| Reading::Value),
        },
        flag("Saved Bond", s.bonded),
    ]
}

/// A saved device's pairing state in words.
pub fn pairing_words(state: PairingState) -> &'static str {
    match state {
        PairingState::Paired => "paired",
        PairingState::NeedsPairing => "needs pairing again",
    }
}

/// One saved device on a line for the shell.
pub fn device_line(d: &Device) -> String {
    let mut policy = String::from(if d.trusted { "trusted" } else { "untrusted" });
    if d.blocked {
        policy.push_str(" blocked");
    }
    if d.reconnect == cordial_protocol::identifiers::Reconnect::Paused {
        policy.push_str(" reconnect-paused");
    }
    let mut line = format!(
        "{}  {}  {}  {}  {}  {}",
        safe(&d.device_id.0),
        name(d.name.as_deref()),
        wire(&d.transport),
        wire(&d.pairing_state),
        wire(&d.state),
        policy
    );
    let _ = write!(
        line,
        " bluetooth={}",
        if d.enabled { "enabled" } else { "disabled" }
    );
    if let Some(reason) = d.enabled_reason.filter(|r| *r != DisabledReason::Disabled) {
        let _ = write!(line, " inactive={}", wire(&reason));
    }
    if let Some(v) = d.validation_error {
        let _ = write!(line, " record={}", wire(&v));
    }
    if let Some(s) = link_security(d) {
        let words = match s {
            Some(_) => security_summary(s)
                .to_lowercase()
                .replace(", ", ",")
                .replace(' ', "-"),
            None => "not-reported".into(),
        };
        let _ = write!(line, " security={words}");
    }
    if !d.warnings.is_empty() {
        let warnings: Vec<String> = d.warnings.iter().map(wire).collect();
        let _ = write!(line, " warnings={}", safe(&warnings.join(",")));
    }
    if let Some(e) = &d.last_error {
        let _ = write!(line, " error={}", wire(&e.code));
    }
    // The saved preference and the runtime states are separate: an enabled
    // device is pending until it connects, and may turn out unsupported.
    let protocol = match d.hidpp_protocol {
        ProtocolState::Unknown => "unknown".into(),
        ProtocolState::Probing => "probing".into(),
        ProtocolState::Detected { major, minor } => format!("{major}.{minor}"),
        ProtocolState::Unavailable => "unavailable".into(),
        ProtocolState::Error { code } => format!("error:{}", wire(&code)),
    };
    let _ = write!(
        line,
        " hidpp={} hidpp-protocol={protocol}",
        on_off(d.hidpp_enabled)
    );
    if d.hidpp_enabled
        || d.normalization_state != NormalizationState::Off
        || d.settings_state != SettingsState::Off
    {
        let _ = write!(
            line,
            " normalization={} settings={}",
            wire(&d.normalization_state),
            wire(&d.settings_state)
        );
    }
    if let Some(code) = d.normalization_error {
        let _ = write!(line, " normalization-error={}", quoted_error(code));
    }
    if let Some(code) = d.settings_error {
        let _ = write!(line, " settings-error={}", quoted_error(code));
    }
    line
}

pub fn candidate_line(c: &Candidate) -> String {
    format!(
        "{}  {}  {}  candidate",
        safe(&c.candidate_id.0),
        candidate_name(c),
        wire(&c.transport)
    )
}

/// Status's advisory Pair availability for a transport: None when available.
pub fn pair_blocked(st: &Status, transport: Transport) -> Option<String> {
    match st.pairing(transport) {
        Some(p) if !p.available => Some(match p.reason {
            Some(reason) => pair_unavailable_words(reason).into(),
            None => "the adapter reports pairing unavailable".into(),
        }),
        _ => None,
    }
}

/// Advisory pairing estimate and availability of one transport.
pub fn pairing_capacity_words(p: &cordial_protocol::messages::PairingCapacity) -> String {
    let estimate = match p.estimated_additional {
        1 => "about 1 more device".to_owned(),
        n => format!("about {n} more devices"),
    };
    match (p.available, p.reason) {
        (true, _) => format!("available, {estimate}"),
        (false, Some(reason)) => format!(
            "unavailable ({}), {estimate}",
            pair_unavailable_words(reason)
        ),
        (false, None) => format!("unavailable, {estimate}"),
    }
}

/// A capability as the shell and TUI name it.
pub fn capability_words(c: Capability) -> &'static str {
    match c {
        Capability::Classic => "Bluetooth Classic",
        Capability::Ble => "Bluetooth LE",
        Capability::Debug => "development functions",
        Capability::StorageManagement => "file access",
    }
}

/// `adapter capabilities`: each wire name with its meaning.
pub fn capabilities_info(caps: &Capabilities) -> String {
    if caps.0.is_empty() {
        return "Capabilities: none".into();
    }
    let mut b = String::from("Capabilities:");
    for c in &caps.0 {
        let _ = write!(b, "\n  {}  {}", wire(c), capability_words(*c));
    }
    b
}

/// `adapter status` for the shell.
pub fn adapter_info(port: &str, st: &Status, caps: &Capabilities) -> String {
    let mut b = format!("Adapter {}", safe(&st.adapter_id));
    let mut field = |key: &str, value: &str| {
        let _ = write!(b, "\n  {key}: {}", safe(value));
    };
    field("Port", port);
    field("Firmware", &st.firmware_version);
    field("Hardware", &st.hardware_config);
    field("Build Profile", &wire(&st.build_profile));
    // Before storage loads, the reported platform is not the saved value.
    let platform = if st.storage_ready {
        wire(&st.host_platform)
    } else {
        "unavailable (adapter storage not ready)".into()
    };
    field("Name", &display(&st.name));
    field("Platform", &platform);
    field("Radio Ready", &yes_no(st.radio_ready).to_lowercase());
    field("Storage Ready", &yes_no(st.storage_ready).to_lowercase());
    field("Saved Devices", &st.counts.saved.to_string());
    field("Paired Devices", &st.counts.paired.to_string());
    field(
        "Enabled Devices",
        &format!(
            "{} active of {} enabled",
            st.counts.enabled, st.counts.preferred_enabled
        ),
    );
    field("Connected Devices", &st.counts.connected.to_string());
    for c in &st.capacity.enabled {
        let transports: Vec<&str> = c.transports.iter().map(|t| transport_name(*t)).collect();
        field(
            &format!("Enabled Places ({})", transports.join(" + ")),
            &format!(
                "{} of {} in use, {} free",
                c.enabled,
                c.limit,
                c.limit.saturating_sub(c.enabled)
            ),
        );
    }
    for p in &st.capacity.pairing {
        field(
            &format!("Pairing ({})", transport_name(p.transport)),
            &pairing_capacity_words(p),
        );
    }
    if st.capacity.pairing.len() > 1 {
        field(
            "Pairing Estimates",
            "shared between transports; don't add them",
        );
    }
    field("Monitoring", on_off(st.monitor));
    let names: Vec<String> = caps.0.iter().map(wire).collect();
    field(
        "Capabilities",
        &if names.is_empty() {
            "none".to_owned()
        } else {
            names.join(", ")
        },
    );
    field(
        "Capacity",
        &format!(
            "up to {} saved devices, {} connections, {} discovery candidates",
            st.limits.saved_devices, st.limits.active_connections, st.limits.scan_candidates
        ),
    );
    field(
        "Pending Request Limit",
        &st.limits.max_pending_requests.to_string(),
    );
    field(
        "HID++ Limits per Device",
        &format!(
            "{} settings, {} saved settings",
            st.limits.hidpp_settings, st.limits.hidpp_saved_settings
        ),
    );
    if st.pending.is_empty() {
        field("Pending Operations", "none");
    } else {
        b.push_str("\n  Pending Operations:");
        for p in &st.pending {
            let target = p
                .device_id
                .as_ref()
                .map(|d| d.0.as_str())
                .or(p.candidate_id.as_ref().map(|c| c.0.as_str()))
                .unwrap_or("");
            let _ = write!(
                b,
                "\n    [request {}] {}",
                p.id.get(),
                safe(format!("{} {target}", p.cmd).trim())
            );
        }
    }
    b
}

pub fn candidate_info(c: &Candidate) -> String {
    let mut b = format!(
        "Candidate {} ({})\n  Name: {}\n  State: discovered",
        safe(&c.candidate_id.0),
        wire(&c.transport),
        candidate_name(c)
    );
    if let Some(rssi) = c.rssi {
        let _ = write!(b, "\n  Signal: {rssi} dBm");
    }
    b
}

/// `info` of a saved device, one field per line after bluetoothctl.
pub fn device_info(d: &Device) -> String {
    let mut b = format!("Device {} ({})", safe(&d.device_id.0), wire(&d.transport));
    let mut field = |key: &str, value: &str| {
        let _ = write!(b, "\n  {key}: {value}");
    };
    field("Name", &name(d.name.as_deref()));
    field("Pairing", pairing_words(d.pairing_state));
    if !d.roles.is_empty() {
        let roles: Vec<String> = d.roles.iter().map(wire).collect();
        field("Roles", &roles.join(", "));
    }
    field("State", &wire(&d.state));
    match link_security(d) {
        None => field("Link Security", "not connected"),
        Some(None) => field("Link Security", security_summary(None)),
        Some(Some(s)) => {
            field("Link Security", security_summary(Some(s)));
            for f in security_facts(s) {
                field(&format!("  {}", f.label), &f.value);
            }
        }
    }
    field("Bluetooth", &enablement_words(d));
    if !d.transport_supported {
        field(
            "Transport",
            "not supported by this build; bond and settings kept",
        );
    }
    if let Some(v) = d.validation_error {
        field("Saved Record", validation_words(v));
    }
    field("Trusted", &yes_no(d.trusted).to_lowercase());
    field("Blocked", &yes_no(d.blocked).to_lowercase());
    field("Reconnect", &wire(&d.reconnect));
    field("Logitech Features", on_off(d.hidpp_enabled));
    field("HID++ Protocol", &hidpp_protocol_text(d.hidpp_protocol));
    field("Special-Key Translation", &wire(&d.normalization_state));
    if let Some(code) = d.normalization_error {
        field("Special-Key Translation Error", &hidpp_words(code));
    }
    field("Device Settings", &wire(&d.settings_state));
    if let Some(code) = d.settings_error {
        field("Device Settings Error", &hidpp_words(code));
    }
    if !d.warnings.is_empty() {
        let warnings: Vec<String> = d.warnings.iter().map(wire).collect();
        field("Warnings", &warnings.join(", "));
    }
    if let Some(e) = &d.last_error {
        field("Last Error", &safe(&wire_line(e, None)));
    }
    b
}

/// A device information field's name. Instances after the first are
/// numbered, so a second firmware component reads "Firmware 2".
pub fn info_label(key: InfoKey, instance: u8) -> String {
    let base = match key {
        InfoKey::Name => "Reported Name",
        InfoKey::Kind => "Device Type",
        InfoKey::Manufacturer => "Manufacturer",
        InfoKey::Model => "Model",
        InfoKey::Serial => "Serial Number",
        InfoKey::Firmware => "Firmware",
        InfoKey::Hardware => "Hardware",
        InfoKey::Software => "Software",
        InfoKey::VendorIdNamespace => "Vendor ID Namespace",
        InfoKey::VendorId => "Vendor ID",
        InfoKey::ProductId => "Product ID",
        InfoKey::ProductVersion => "Product Version",
        InfoKey::BatteryPercent => "Battery",
        InfoKey::BatteryCharging => "Charging",
    };
    match instance {
        0 => base.into(),
        n => format!("{base} {}", n + 1),
    }
}

/// A field's value in words; None when the device doesn't report it.
pub fn info_value(f: &InfoField) -> Option<String> {
    if !f.available {
        return None;
    }
    Some(match (&f.key, &f.value) {
        (InfoKey::Kind, SettingValue::Text(k)) => match k.as_str() {
            "keyboard" => "Keyboard".into(),
            "mouse" => "Mouse".into(),
            "keyboard_mouse" => "Keyboard and mouse".into(),
            _ => "Other".into(),
        },
        (
            InfoKey::VendorId | InfoKey::ProductId | InfoKey::ProductVersion,
            SettingValue::Integer(n),
        ) => {
            format!("0x{n:04X}")
        }
        (InfoKey::VendorIdNamespace, SettingValue::Text(n)) => match n.as_str() {
            "usb" => "USB".into(),
            _ => "Bluetooth".into(),
        },
        (InfoKey::BatteryPercent, SettingValue::Integer(n)) => format!("{n}%"),
        (InfoKey::BatteryCharging, SettingValue::Bool(b)) => yes_no(*b).into(),
        (_, SettingValue::Text(t)) => safe(t),
        (_, v) => safe(&catalog::value_string(v)),
    })
}

/// A device's battery: its charge and whether it charges, each None while
/// unknown. Unknown is never shown as 0% or not charging.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Battery {
    pub percent: Option<i64>,
    pub charging: Option<bool>,
    pub percent_fresh: bool,
    pub charging_fresh: bool,
}
impl Battery {
    /// Every known value is a current reading.
    pub fn fresh(&self) -> bool {
        (self.percent.is_none() || self.percent_fresh)
            && (self.charging.is_none() || self.charging_fresh)
    }
    /// At most 10% charged.
    pub fn low(&self) -> bool {
        self.percent.is_some_and(|p| p <= 10)
    }
    /// For details: "80%" or "Unknown".
    pub fn percent_words(&self) -> String {
        match self.percent {
            Some(p) => format!("{p}%"),
            None => "Unknown".into(),
        }
    }
    /// For details: "Yes", "No" or "Unknown".
    pub fn charging_words(&self) -> String {
        match self.charging {
            Some(c) => yes_no(c).into(),
            None => "Unknown".into(),
        }
    }
    /// For device lists: "80%" or "?", with a mark only while charging.
    pub fn compact(&self, charging: &str) -> String {
        let mut text = match self.percent {
            Some(p) => format!("{p}%"),
            None => "?".into(),
        };
        if self.charging == Some(true) {
            text.push_str(charging);
        }
        text
    }
}

/// The device's battery, or None when it reports neither charge nor charging.
pub fn battery(fields: &[InfoField]) -> Option<Battery> {
    let mut b = Battery::default();
    for f in fields.iter().filter(|f| f.available && f.instance == 0) {
        match (&f.key, &f.value) {
            (InfoKey::BatteryPercent, SettingValue::Integer(n)) => {
                b.percent = Some(*n);
                b.percent_fresh = f.fresh;
            }
            (InfoKey::BatteryCharging, SettingValue::Bool(c)) => {
                b.charging = Some(*c);
                b.charging_fresh = f.fresh;
            }
            _ => {}
        }
    }
    (b.percent.is_some() || b.charging.is_some()).then_some(b)
}

/// A labeled line of device information.
#[derive(Clone, Debug, PartialEq)]
pub struct InfoRow {
    pub label: String,
    pub value: String,
    pub fresh: bool,
}

/// The reported information in display order: the battery's charge and
/// charging, then the other available fields. With either battery value
/// reported, both rows are shown and an unknown one reads "Unknown". Other
/// unreported fields are left out, and so is the name, which already names
/// the device.
pub fn info_rows(fields: &[InfoField]) -> Vec<InfoRow> {
    let mut rows: Vec<InfoRow> = Vec::new();
    if let Some(b) = battery(fields) {
        rows.push(InfoRow {
            label: info_label(InfoKey::BatteryPercent, 0),
            value: b.percent_words(),
            fresh: b.percent.is_none() || b.percent_fresh,
        });
        rows.push(InfoRow {
            label: info_label(InfoKey::BatteryCharging, 0),
            value: b.charging_words(),
            fresh: b.charging.is_none() || b.charging_fresh,
        });
    }
    let mut others: Vec<&InfoField> = fields.iter().filter(|f| f.available).collect();
    others.sort_by_key(|f| (crate::view::info_order(f.key), f.instance));
    let namespace = others
        .iter()
        .find(|f| f.key == InfoKey::VendorIdNamespace)
        .copied();
    let vendor = others.iter().any(|f| f.key == InfoKey::VendorId);
    for f in others {
        match f.key {
            // The name is the device's name wherever it is shown.
            InfoKey::Name | InfoKey::BatteryPercent | InfoKey::BatteryCharging => continue,
            // A vendor ID is read with its namespace, on one row.
            InfoKey::VendorIdNamespace if vendor => continue,
            _ => {}
        }
        let Some(mut value) = info_value(f) else {
            continue;
        };
        let mut fresh = f.fresh;
        if f.key == InfoKey::VendorId
            && let Some(ns) = namespace
            && let Some(words) = info_value(ns)
        {
            value = format!("{value} ({words})");
            fresh &= ns.fresh;
        }
        rows.push(InfoRow {
            label: info_label(f.key, f.instance),
            value,
            fresh,
        });
    }
    rows
}

/// A device's battery for `device list`, or None when it reports none.
pub fn battery_list(info: Option<&DeviceInfoView>) -> Option<String> {
    let b = battery(&info?.fields)?;
    let mut w = b.compact("(charging)");
    if !b.fresh() {
        w.push_str("(stale)");
    }
    Some(w)
}

/// `device info DEV`: every reported field, one per line; values that are
/// not current readings are marked as last known.
pub fn info_text(subject: &Subject, info: Option<&DeviceInfoView>) -> String {
    let mut b = format!("Information of {}", label(subject));
    let Some(info) = info else {
        b.push_str("\n  Not read yet.");
        return b;
    };
    if !info.current {
        b.push_str(" (may be out of date; refreshing)");
    }
    let rows = info_rows(&info.fields);
    if rows.is_empty() {
        b.push_str("\n  The device reports no information now.");
    }
    for r in rows {
        let _ = write!(b, "\n  {}: {}", r.label, r.value);
        if !r.fresh {
            b.push_str(" (last known)");
        }
    }
    b
}

/// A device.info.changed event: the fields it sets and clears.
fn info_change_line(info: &DeviceInfo) -> String {
    let parts: Vec<String> = info
        .fields
        .iter()
        .map(|f| {
            let label = info_label(f.key, f.instance);
            match info_value(f) {
                Some(v) => format!("{label}: {v}"),
                None => format!("{label}: not reported"),
            }
        })
        .collect();
    format!("[CHG] {} {}", safe(&info.device_id.0), parts.join("; "))
}

pub fn scan_summary(count: u64, truncated: bool) -> String {
    let noun = if count == 1 {
        "candidate"
    } else {
        "candidates"
    };
    let mut text = format!("Discovery finished: {count} {noun}.");
    if truncated {
        text.push_str(" Candidate limit reached; restart discovery to look for more devices.");
    }
    text
}

/// Which saved devices `device list` lists; candidates only without a filter.
/// Every filter reads the session's cached Saved list.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Filter {
    #[default]
    All,
    Saved,
    Paired,
    Enabled,
    Connected,
    Trusted,
}

/// `device list` for the shell: saved devices, then this session's candidates,
/// which are never associated with saved devices.
pub fn devices(st: &State, filter: Filter) -> String {
    use cordial_protocol::identifiers::ConnectionState;
    let mut lines: Vec<String> = st
        .devices
        .iter()
        .filter(|d| match filter {
            Filter::All | Filter::Saved => true,
            Filter::Paired => d.pairing_state == PairingState::Paired,
            Filter::Enabled => d.enabled,
            Filter::Connected => d.state == ConnectionState::Connected,
            Filter::Trusted => d.trusted,
        })
        .map(|d| {
            let mut line = device_line(d);
            if let Some(b) = battery_list(st.info.get(&d.device_id)) {
                let _ = write!(line, " battery={b}");
            }
            line
        })
        .collect();
    if filter == Filter::All {
        lines.extend(st.candidates.iter().map(candidate_line));
    }
    if lines.is_empty() {
        return "No matching devices.".into();
    }
    lines.join("\n")
}

/// `list`: attached adapters.
pub fn ports(ports: &[PortInfo]) -> String {
    if ports.is_empty() {
        return "No Cordial adapters found. Use --port for an explicit serial path.".into();
    }
    ports
        .iter()
        .map(|p| format!("{}  {}", safe(&p.port), safe(&p.serial)))
        .collect::<Vec<_>>()
        .join("\n")
}

/// A resolved device or candidate as a result names it.
pub fn label(subject: &Subject) -> String {
    match subject.name.as_deref() {
        Some(n) if !n.is_empty() => safe(n),
        _ => safe(&subject.id),
    }
}

fn bootloader_mode(mode: BootloaderMode) -> &'static str {
    match mode {
        BootloaderMode::Bootsel => "BOOTSEL mode",
        BootloaderMode::Download => "download mode",
    }
}

/// A byte count for people: exact below 1 KiB.
pub fn bytes(n: u64) -> String {
    match n {
        0..1024 => format!("{n} B"),
        1024..1_048_576 => format!("{:.1} KiB", n as f64 / 1024.0),
        _ => format!("{:.1} MiB", n as f64 / 1_048_576.0),
    }
}

/// One `storage ls` row: type, size and name, tab-separated.
pub fn file_line(e: &FileEntry) -> String {
    format!("{}\t{}\t{}", wire(&e.kind), e.size, safe(&e.name))
}

/// A command's result for the shell and scripts; empty when the command has
/// nothing to add, as for a stopped scan.
pub fn outcome(command: &Command, outcome: &Outcome, state: Option<&State>) -> String {
    match outcome {
        Outcome::Status(status) => {
            // The session's status carries the newest platform report.
            let empty = Capabilities::default();
            let (port, st, caps) = match state {
                Some(s) => (s.port.as_str(), &s.status, &s.capabilities),
                None => ("", status, &empty),
            };
            adapter_info(port, st, caps)
        }
        Outcome::Capabilities(caps) => capabilities_info(caps),
        // Rows were printed as they arrived.
        Outcome::StorageListed { .. } => String::new(),
        Outcome::StorageSaved { path, local, bytes } => format!(
            "Saved {} to {} ({bytes} bytes).",
            safe(path),
            safe(&local.display().to_string())
        ),
        Outcome::Name(name) if matches!(command, Command::Name(None)) => {
            format!("Adapter name reset to {}.", display(name))
        }
        Outcome::Name(name) => format!("Adapter renamed to {}.", display(name)),
        Outcome::Platform(p) => format!("Platform set to {}.", platform_name(*p)),
        Outcome::Monitor(on) => {
            format!("Monitoring {}.", if *on { "enabled" } else { "disabled" })
        }
        Outcome::ScanStarted { request, transport } => format!(
            "[request {}] Discovery started ({}).",
            request.get(),
            wire(transport)
        ),
        Outcome::ScanFinished { count, truncated } => scan_summary(*count, *truncated),
        Outcome::ScanStopped { was_running } => {
            if *was_running {
                String::new()
            } else {
                "Discovery is already off.".into()
            }
        }
        Outcome::Devices => state.map_or_else(String::new, |s| devices(s, Filter::All)),
        Outcome::Device {
            device: Some(d), ..
        } if matches!(command, Command::Info(_)) => device_info(d),
        Outcome::Device { subject, .. } => {
            let label = label(subject);
            match command {
                Command::Connect(_) | Command::Pair(_) => format!("Connected to {label}."),
                Command::Disconnect(_) => format!("Disconnected {label}."),
                Command::Enabled(_, true) => format!("Enabled {label}."),
                Command::Enabled(_, false) => format!("Disabled {label}."),
                Command::Trusted(_, true) => format!("Trusted {label}."),
                Command::Trusted(_, false) => format!("Removed trust for {label}."),
                Command::Blocked(_, true) => format!("Blocked {label}."),
                Command::Blocked(_, false) => format!("Unblocked {label}."),
                Command::Remove(_) => format!("Forgot {label}."),
                Command::Hidpp(_, on) => format!("HID++ turned {} for {label}.", on_off(*on)),
                _ => String::new(),
            }
        }
        Outcome::Candidate(c) => candidate_info(c),
        Outcome::Hidden(subject) => format!("Candidate hidden: {}", safe(&subject.id)),
        Outcome::Connected(subject) => format!("Connected to {}.", label(subject)),
        Outcome::PairedDisabled(subject, reason) => paired_disabled(subject, *reason),
        Outcome::CancelRequested(id) => format!("Cancellation requested for request {}.", id.get()),
        Outcome::ReplySent => "Pairing answer sent.".into(),
        Outcome::DeviceInfo(subject) => info_text(
            subject,
            state.and_then(|s| s.info.get(&DeviceId(subject.id.clone()))),
        ),
        Outcome::Features(subject) => catalog::subject_text(subject, state, true),
        Outcome::Settings(subject) => catalog::subject_text(subject, state, false),
        Outcome::Setting { subject, setting } => {
            catalog::setting_result(command, subject, setting, state)
        }
        Outcome::Job { rows, counts, .. } => catalog::job_text(command, rows, counts.as_ref()),
        Outcome::Bootloader { mode } => {
            format!("Adapter is restarting into {}.", bootloader_mode(*mode))
        }
    }
}

/// A committed pairing whose device may not connect now.
pub fn paired_disabled(subject: &Subject, reason: Option<DisabledReason>) -> String {
    let label = label(subject);
    match reason {
        None | Some(DisabledReason::Disabled) => {
            format!("Paired and saved {label}; enable it to connect.")
        }
        Some(reason) => format!(
            "Paired and saved {label}; it can't connect now because {}.",
            disabled_words(reason)
        ),
    }
}

/// A failed command for the shell and scripts: any partial result, then the
/// error line. The caller adds the `Error: ` prefix where the shell uses one.
/// A pairing whose bond was saved before its connection failed says so, so
/// the failure never reads as a failed pairing.
pub fn failure_text(
    command: &Command,
    failure: &crate::controller::Failure,
    state: Option<&State>,
) -> (String, String) {
    let partial = failure
        .partial
        .as_ref()
        .map(|o| outcome(command, o, state))
        .unwrap_or_default();
    let mut error = error_line(&failure.error);
    if let Some(id) = &failure.bonded {
        error = format!("bond saved for {}, but connecting failed: {error}", id.0);
    }
    (partial, safe(&error))
}

fn decode<T: serde::de::DeserializeOwned>(e: &Envelope) -> Option<T> {
    e.decode().ok()
}

#[derive(serde::Deserialize)]
struct DeviceData {
    device: Device,
}

/// A notice for the shell and scripts, or None when it has
/// nothing to show: callers report their own results, and internal startup,
/// heartbeat and refresh responses stay quiet.
pub fn notice(n: &Notice, state: Option<&State>) -> Option<String> {
    match n {
        Notice::RequestPending {
            id,
            command,
            target,
        } => Some(safe(&format!(
            "[request {}] {command} {target} pending.",
            id.get()
        ))),
        Notice::BondSaved(id) => Some(safe(&format!(
            "Bond saved for {}. Waiting for HID input readiness.",
            id.0
        ))),
        Notice::MonitorExpired => {
            Some("Monitoring expired; renewing subscription and device state.".into())
        }
        Notice::Skipped(n) => Some(missed(*n)),
        Notice::RefreshFailed(e) => Some(format!(
            "Error: device refresh failed: {}",
            safe(&error_line(e))
        )),
        Notice::Message {
            envelope,
            background,
        } => message(envelope, *background, state),
        Notice::StorageEntries { entries, .. } => {
            Some(entries.iter().map(file_line).collect::<Vec<_>>().join("\n"))
        }
        Notice::StorageProgress { .. } => None,
    }
}

/// A device event's record, named as the session names it: a reported name
/// arrives only as device information, never with the record.
fn resolved(mut d: Device, state: Option<&State>) -> Device {
    if let Some(saved) = state.and_then(|s| s.devices.iter().find(|s| s.device_id == d.device_id)) {
        d.name = saved.name.clone();
    }
    d
}

fn message(e: &Envelope, background: bool, state: Option<&State>) -> Option<String> {
    use cordial_protocol::messages::Message;
    match &e.message {
        Message::Response {
            id,
            ok,
            done,
            error,
            ..
        } => {
            // Only background operations, such as an interactive scan, finish
            // through notices.
            if !background || !done || e.internal {
                return None;
            }
            let scan = e.command == Some("discovery.scan");
            if !ok {
                let error = error.as_ref()?;
                if scan && error.code == ErrorCode::Cancelled {
                    return Some("Discovery stopped.".into());
                }
                return Some(format!(
                    "[request {}] {}",
                    id.get(),
                    safe(&wire_line(error, e.command))
                ));
            }
            if scan {
                return Some(match decode::<ScanEnd>(e) {
                    Some(r) => scan_summary(r.count as u64, r.truncated),
                    None => "Discovery finished; its summary could not be read.".into(),
                });
            }
            Some(format!(
                "[request {}] {} completed.",
                id.get(),
                safe(e.command.unwrap_or("request"))
            ))
        }
        Message::Event {
            event, request_id, ..
        } => match event.as_str() {
            "discovery.result" => {
                decode::<Candidate>(e).map(|c| format!("[NEW] {}", candidate_line(&c)))
            }
            "device.paired" => decode::<DeviceData>(e)
                .map(|d| format!("[NEW] {}", device_line(&resolved(d.device, state)))),
            "device.changed" | "device.connected" | "device.disconnected" => {
                decode::<DeviceData>(e)
                    .map(|d| format!("[CHG] {}", device_line(&resolved(d.device, state))))
            }
            "device.unpaired" => {
                decode::<DeviceUnpaired>(e).map(|d| format!("[DEL] {}", safe(&d.device_id.0)))
            }
            "device.info.changed" => decode::<DeviceInfo>(e).map(|i| info_change_line(&i)),
            "adapter.changed" => decode::<AdapterSettings>(e).map(|a| {
                format!(
                    "[CHG] Adapter name={} platform={}",
                    quote(&a.name),
                    wire(&a.host_platform)
                )
            }),
            "hidpp.setting.changed" => decode::<SettingChunk>(e).map(|s| {
                format!(
                    "[CHG] {}{}",
                    safe(&s.device_id.0),
                    catalog::setting_line(&s.setting)
                )
            }),
            "pairing.prompt" | "pairing.display" => {
                let p = decode::<Prompt>(e)?;
                Some(prompt_line(request_id.map_or(0, |r| r.get()), &p))
            }
            "events.lost" => Some("Notifications lost; refreshing the device list.".into()),
            // Local loss is reported once, with its count, by Notice::Skipped.
            _ => None,
        },
    }
}

/// Notifications this host dropped before applying them: the device view is
/// no longer current and is being reloaded.
fn missed(n: u64) -> String {
    let noun = if n == 1 {
        "notification"
    } else {
        "notifications"
    };
    format!("{n} {noun} missed; refreshing the device list.")
}

/// An authentication prompt for the shell: what to enter or compare, where,
/// and which request it belongs to.
pub fn prompt_line(request: u32, p: &Prompt) -> String {
    use cordial_protocol::messages::PromptMethod;
    let place = match p.method {
        m if m.display() => "Enter on the peripheral",
        PromptMethod::ConfirmPasskey => "Compare with the peripheral; answer yes or no",
        _ => "Enter on this computer",
    };
    let mut what = safe(&wire(&p.method));
    if let Some(v) = p.value.as_deref().filter(|v| !v.is_empty()) {
        let _ = write!(what, " {}", safe(v));
    }
    format!(
        "[pair {request} {}] {place}: {what} (expires in {}s)",
        safe(&p.prompt_id),
        p.expires_in_ms / 1000
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use cordial_protocol::info::InfoField;

    fn field(key: InfoKey, instance: u8, value: SettingValue) -> InfoField {
        InfoField {
            key,
            instance,
            value,
            available: true,
            fresh: true,
        }
    }

    /// Event lines name a device as the session does, since a reported name
    /// never arrives with the device record.
    #[test]
    fn device_events_use_the_session_name() {
        let mut st = crate::ui::command::tests::state();
        st.devices[0].name = Some("Reported".into());
        let d = crate::ui::command::tests::device("d_1", "Saved");
        let envelope = Envelope {
            message: cordial_protocol::messages::Message::event(
                "device.changed".into(),
                None,
                serde_json::json!({"revision":4,"device":d}),
            ),
            raw: String::new(),
            command: None,
            internal: false,
            sequence: 0,
        };
        let n = Notice::Message {
            envelope,
            background: false,
        };
        assert!(
            notice(&n, Some(&st))
                .unwrap()
                .starts_with("[CHG] d_1  Reported  ")
        );
        assert!(notice(&n, None).unwrap().starts_with("[CHG] d_1  Saved  "));
    }

    /// Rendering depends only on the reported fields: the same values in any
    /// order give the same text, whichever backend or protocol supplied them.
    #[test]
    fn device_info_renders_source_neutral_fields() {
        let fields = vec![
            field(InfoKey::BatteryPercent, 0, SettingValue::Integer(80)),
            field(InfoKey::BatteryCharging, 0, SettingValue::Bool(true)),
            field(
                InfoKey::Kind,
                0,
                SettingValue::Text("keyboard_mouse".into()),
            ),
            field(InfoKey::VendorId, 0, SettingValue::Integer(0x46d)),
            field(InfoKey::Firmware, 1, SettingValue::Text("RBM 1.2".into())),
            field(
                InfoKey::VendorIdNamespace,
                0,
                SettingValue::Text("usb".into()),
            ),
            field(InfoKey::Name, 0, SettingValue::Text("Combo".into())),
            InfoField::unknown(InfoKey::Serial, 0),
        ];
        let subject = Subject {
            id: "d_1".into(),
            name: Some("Combo".into()),
        };
        let view = |fields: Vec<InfoField>| DeviceInfoView {
            current: true,
            revision: 1,
            fields,
        };
        let text = info_text(&subject, Some(&view(fields.clone())));
        let mut reversed = fields.clone();
        reversed.reverse();
        assert_eq!(text, info_text(&subject, Some(&view(reversed))));
        assert_eq!(
            text,
            "Information of Combo\n  Battery: 80%\n  Charging: Yes\n  Device Type: Keyboard and mouse\n  Firmware 2: RBM 1.2\n  Vendor ID: 0x046D (USB)"
        );
        let mut stale = fields.clone();
        stale[0].fresh = false;
        let text = info_text(
            &subject,
            Some(&DeviceInfoView {
                current: false,
                ..view(stale)
            }),
        );
        assert!(text.contains("Battery: 80% (last known)"), "{text}");
        assert!(text.contains("may be out of date"), "{text}");
        assert_eq!(battery_list(Some(&view(fields))).unwrap(), "80%(charging)");
        // A namespace without its vendor ID is shown on its own.
        let rows = info_rows(&[field(
            InfoKey::VendorIdNamespace,
            0,
            SettingValue::Text("bluetooth".into()),
        )]);
        assert_eq!(rows[0].label, "Vendor ID Namespace");
        assert_eq!(rows[0].value, "Bluetooth");
        assert_eq!(battery_list(Some(&view(Vec::new()))), None);
        assert!(info_text(&subject, None).contains("Not read yet"));
        let change = DeviceInfo {
            revision: 2,
            device_id: cordial_protocol::identifiers::DeviceId("d_1".into()),
            fields: vec![
                InfoField::unknown(InfoKey::BatteryPercent, 0),
                field(InfoKey::BatteryCharging, 0, SettingValue::Bool(false)),
            ],
        };
        assert_eq!(
            info_change_line(&change),
            "[CHG] d_1 Battery: not reported; Charging: No"
        );
    }

    /// An unreported battery value reads unknown, never 0% or not charging,
    /// and a reported "not charging" is told apart from unknown.
    #[test]
    fn battery_unknown_is_never_zero_or_not_charging() {
        let view = |fields: Vec<InfoField>| DeviceInfoView {
            current: true,
            revision: 1,
            fields,
        };
        let rows = |fields: &[InfoField]| -> Vec<(String, String)> {
            info_rows(fields)
                .into_iter()
                .map(|r| (r.label, r.value))
                .collect()
        };
        let pair = |a: &str, b: &str| (a.to_string(), b.to_string());
        let charging_only = vec![
            InfoField::unknown(InfoKey::BatteryPercent, 0),
            field(InfoKey::BatteryCharging, 0, SettingValue::Bool(false)),
        ];
        assert_eq!(
            rows(&charging_only),
            [pair("Battery", "Unknown"), pair("Charging", "No")]
        );
        assert_eq!(battery_list(Some(&view(charging_only))).unwrap(), "?");
        let percent_only = vec![field(InfoKey::BatteryPercent, 0, SettingValue::Integer(0))];
        assert_eq!(
            rows(&percent_only),
            [pair("Battery", "0%"), pair("Charging", "Unknown")]
        );
        assert_eq!(battery_list(Some(&view(percent_only))).unwrap(), "0%");
        let neither = vec![
            InfoField::unknown(InfoKey::BatteryPercent, 0),
            InfoField::unknown(InfoKey::BatteryCharging, 0),
        ];
        assert!(rows(&neither).is_empty());
        assert_eq!(battery_list(Some(&view(neither))), None);
        // A last-known charge is marked stale; an unknown one is not.
        let mut stale = vec![
            field(InfoKey::BatteryPercent, 0, SettingValue::Integer(40)),
            field(InfoKey::BatteryCharging, 0, SettingValue::Bool(true)),
        ];
        stale[1].fresh = false;
        let info = info_rows(&stale);
        assert!(info[0].fresh && !info[1].fresh);
        assert_eq!(
            battery_list(Some(&view(stale))).unwrap(),
            "40%(charging)(stale)"
        );
    }

    #[test]
    fn unnamed_candidates_are_labelled_by_kind_and_all_listed() {
        use crate::ui::command::tests::{candidate, state};
        let mut st = state();
        let kinds = [
            (DeviceKind::Keyboard, "Unnamed Keyboard"),
            (DeviceKind::Mouse, "Unnamed Mouse"),
            (DeviceKind::KeyboardMouse, "Unnamed Keyboard/Mouse"),
            (DeviceKind::Unknown, "Unnamed Device"),
        ];
        for (i, (kind, label)) in kinds.into_iter().enumerate() {
            let mut c = candidate(&format!("c_u{i}"), "");
            c.kind = kind;
            if i % 2 == 0 {
                c.name = None; // Missing and empty names read alike.
            }
            assert_eq!(candidate_name(&c), label);
            assert_eq!(display_candidate_name(&c), label);
            assert!(candidate_info(&c).contains(&format!("  Name: {label}")));
            st.candidates.push(c);
        }
        // A name wins over the kind.
        let named = candidate("c_n", "Desk mouse");
        assert_eq!(candidate_name(&named), "Desk mouse");
        // The shell lists every candidate, including unknown unnamed ones.
        let list = devices(&st, Filter::All);
        assert!(
            list.contains("c_u3  Unnamed Device  ble  candidate"),
            "{list}"
        );
        assert!(
            list.contains("c_u2  Unnamed Keyboard/Mouse  ble  candidate"),
            "{list}"
        );
    }

    #[test]
    fn link_security_is_described_without_overclaiming() {
        use crate::ui::command::tests::device;
        let full = device("d_1", "Keyboard");
        assert!(device_line(&full).contains(" security=encrypted,unauthenticated "));
        let info = device_info(&full);
        for line in [
            "  Link Security: Encrypted, Unauthenticated",
            "    Encryption: Yes",
            "    Authenticated Pairing (MITM Protection): No",
            "    Secure Connections: Yes",
            "    Encryption Key: 128 bits",
            "    Saved Bond: Yes",
            "  Trusted: yes",
        ] {
            assert!(info.contains(line), "{line}:\n{info}");
        }
        assert!(!info.to_lowercase().contains(": secure\n"));

        let mut d = full.clone();
        let s = d.security.as_mut().unwrap();
        s.authenticated = Some(true);
        s.secure_connections = None;
        s.key_size = None;
        s.bonded = Some(false);
        assert!(device_line(&d).contains(" security=encrypted,authenticated "));
        let info = device_info(&d);
        for line in [
            "    Authenticated Pairing (MITM Protection): Yes",
            "    Secure Connections: Not Reported",
            "    Encryption Key: Not Reported",
            "    Saved Bond: No",
        ] {
            assert!(info.contains(line), "{line}:\n{info}");
        }

        d.security.as_mut().unwrap().key_size = Some(7);
        assert!(device_info(&d).contains("    Encryption Key: 56 bits\n"));

        let s = d.security.as_mut().unwrap();
        s.authenticated = None;
        assert_eq!(security_summary(d.security.as_ref()), "Encrypted");
        d.security.as_mut().unwrap().encrypted = None;
        assert!(device_line(&d).contains(" security=encryption-not-reported "));
        d.security.as_mut().unwrap().encrypted = Some(false);
        assert!(device_line(&d).contains(" security=not-encrypted "));

        d.security = None;
        assert!(device_line(&d).contains(" security=not-reported "));
        assert!(
            device_info(&d).contains(
                "  Link Security: Security Not Reported\n  Bluetooth: enabled\n  Trusted"
            )
        );

        // Connection state governs: a leftover report is not shown.
        for state in [
            ConnectionState::Disconnected,
            ConnectionState::Connecting,
            ConnectionState::Disconnecting,
        ] {
            let mut d = full.clone();
            d.state = state;
            assert!(!device_line(&d).contains("security="));
            let info = device_info(&d);
            assert!(info.contains("  Link Security: not connected\n"), "{info}");
            assert!(!info.contains("Encryption"), "{info}");
        }
    }

    #[test]
    fn hostile_names_are_escaped() {
        let hostile = "Test\u{9b}2J\u{202e}键\u{e0001}board";
        let shown = safe(hostile);
        assert!(!shown.contains(['\u{9b}', '\u{202e}', '\u{e0001}']));
        assert!(
            shown.contains("\\u009b") && shown.contains("\\u202e") && shown.contains("\\ue0001")
        );
        assert!(shown.contains('键'));
        assert_eq!(safe_lines("first\nsecond\u{202e}"), "first\nsecond\\u202e");
        assert_eq!(safe("a\u{a0}b c\u{7}"), "a\\u00a0b c\\u0007");
    }

    #[test]
    fn json_keeps_values() {
        let hostile = "Test\u{9b}2J\u{202e}键\u{e0001}board";
        let raw = serde_json::json!({"name": hostile}).to_string();
        let shown = terminal_json(&raw);
        assert!(!shown.contains(['\u{9b}', '\u{202e}', '\u{e0001}']));
        assert!(shown.contains("\\udb40\\udc01"));
        let back: Value = serde_json::from_str(&shown).unwrap();
        assert_eq!(back["name"], hostile);
        assert_eq!(terminal_json("{\"a\":\"b\"}"), "{\"a\":\"b\"}");
    }

    #[test]
    fn settle_escapes_ambiguous_widths() {
        // A leading combining mark would join the text drawn before it.
        assert_eq!(settle("\u{301}abc"), "\\u0301abc");
        // A variation selector widens its base only in grapheme-aware terminals.
        let heart = settle("\u{2764}\u{fe0f}!");
        assert_eq!(heart, "\u{2764}\\ufe0f!");
        assert_eq!(width(&heart), width("\u{2764}") + 7);
        // Ordinary wide and combined text is unchanged.
        assert_eq!(settle("键e\u{301}"), "键e\u{301}");
    }

    #[test]
    fn errors_name_codes_and_facts() {
        let e = WireError {
            code: ErrorCode::SettingsApplyFailed,
            details: Some(
                serde_json::from_value(
                    serde_json::json!({"device_id":"d_1","applied":2,"failed":1,"read":0,
                    "unchanged":0,"unsupported":0,"uncertain":0,"count":3,"revision":9}),
                )
                .unwrap(),
            ),
        };
        assert_eq!(
            wire_line(&e, Some("hidpp.setting.apply")),
            "settings_apply_failed: some saved values couldn't be applied or confirmed (device d_1, 2 applied, 1 failed, of 3)"
        );
        let e = WireError {
            code: ErrorCode::Busy,
            details: Some(serde_json::from_value(serde_json::json!({"device_id":"d_9"})).unwrap()),
        };
        assert!(wire_text(&e, None).ends_with("(device d_9)"));
        let ready = WireError::from(ErrorCode::Timeout);
        assert_eq!(
            wire_text(&ready, Some("adapter.wait_ready")),
            "the adapter didn't finish starting in time"
        );
        assert_eq!(hidpp_words(ErrorCode::HidppTimeout), "no response");
        assert_eq!(
            hidpp_words(ErrorCode::Busy),
            general_text(ErrorCode::Busy).unwrap()
        );
    }

    #[test]
    fn wrapped_errors_keep_context() {
        let mut e = Error::new("busy");
        e.wire = Some(Box::new(ErrorCode::Busy.into()));
        assert!(error_line(&e).starts_with("busy: the adapter is busy"));
        assert!(error_words(&e).starts_with("the adapter is busy"));
        e.message = "adapter not ready: busy; adapter status still works".into();
        assert!(error_line(&e).starts_with("adapter not ready: busy: the adapter is busy"));
        assert!(error_line(&e).ends_with("; adapter status still works"));
        assert_eq!(error_line(&Error::new("plain")), "plain");
    }

    fn event(name: &str, data: Value) -> Notice {
        Notice::Message {
            envelope: Envelope {
                message: cordial_protocol::messages::Message::event(name.into(), None, data),
                raw: String::new(),
                command: None,
                internal: false,
                sequence: 0,
            },
            background: false,
        }
    }

    #[test]
    fn notification_loss_is_reported_once_and_truthfully() {
        let local = notice(&Notice::Skipped(3), None).unwrap();
        assert_eq!(local, "3 notifications missed; refreshing the device list.");
        assert_eq!(
            notice(&Notice::Skipped(1), None).unwrap(),
            "1 notification missed; refreshing the device list."
        );
        let marker = event("local.events_lost", serde_json::json!({"dropped": 3}));
        assert_eq!(
            notice(&marker, None),
            None,
            "the local marker repeats Skipped"
        );
        let firmware = event(
            "events.lost",
            serde_json::json!({"revision": 9, "dropped": 2}),
        );
        assert_eq!(
            notice(&firmware, None).unwrap(),
            "Notifications lost; refreshing the device list."
        );
    }

    #[test]
    fn pairing_errors_never_suggest_connect() {
        use cordial_protocol::errors::ErrorCode;
        let required = general_text(ErrorCode::PairingRequired).unwrap();
        assert!(required.contains("pairing mode") && required.contains("pair it again"));
        let disabled = general_text(ErrorCode::Disabled).unwrap();
        assert!(disabled.contains("enable it"));
        let mut d = crate::ui::command::tests::device("d_1", "Kbd");
        d.pairing_state = cordial_protocol::identifiers::PairingState::NeedsPairing;
        assert!(device_info(&d).contains("Pairing: needs pairing again"));
    }

    #[test]
    fn storage_outcomes_are_explained_without_raw_details() {
        let wire = |code, details| WireError {
            code,
            details: Some(serde_json::from_value(details).unwrap()),
        };
        let unknown = wire_text(
            &wire(
                ErrorCode::StorageFailed,
                serde_json::json!({"outcome":"unknown"}),
            ),
            None,
        );
        assert!(
            unknown.starts_with("save status unknown") && !unknown.contains("outcome"),
            "{unknown}"
        );
        let full = wire_text(
            &wire(
                ErrorCode::StorageFull,
                serde_json::json!({"outcome":"not_saved"}),
            ),
            Some("hidpp.setting.set"),
        );
        assert!(
            full.starts_with("the adapter's storage is full") && !full.contains("outcome"),
            "{full}"
        );
        let plain = wire_text(
            &wire(
                ErrorCode::StorageFull,
                serde_json::json!({"outcome":"not_saved"}),
            ),
            None,
        );
        assert!(!plain.contains('('), "{plain}");
        let not_saved = wire_text(
            &wire(
                ErrorCode::StorageFailed,
                serde_json::json!({"outcome":"not_saved"}),
            ),
            None,
        );
        assert!(
            not_saved.contains("what it had saved is unchanged"),
            "{not_saved}"
        );
    }

    #[test]
    fn bonded_failures_keep_the_bond() {
        let mut error = Error::new("timeout");
        error.wire = Some(Box::new(ErrorCode::Timeout.into()));
        let failure = crate::controller::Failure {
            error,
            bonded: Some(cordial_protocol::identifiers::DeviceId("d_7".into())),
            partial: None,
        };
        let (partial, line) = failure_text(&Command::Pair("c_1".into()), &failure, None);
        assert!(partial.is_empty());
        assert_eq!(
            line,
            "bond saved for d_7, but connecting failed: timeout: the operation's deadline expired"
        );
    }

    #[test]
    fn quoting_escapes_quotes_backslashes_and_controls() {
        assert_eq!(quote("Test \"kb\"\\"), "\"Test \\\"kb\\\"\\\\\"");
        assert_eq!(quote("a\u{7}\u{202e}"), "\"a\\x07\\u202e\"");
    }
}
