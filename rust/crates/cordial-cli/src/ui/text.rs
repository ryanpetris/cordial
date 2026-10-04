//! Human-readable text shared by the shell, scripts and the TUI: safe terminal
//! rendering of untrusted strings, the host's explanations of adapter codes,
//! and the shell's line formats.
use crate::{
    controller::{Command, Notice, Outcome, State, Subject, Toggle},
    error::Error,
    model::{self, Prompt, Up},
    ui::catalog,
};
use cordial_client::serial::PortInfo;
use cordial_protocol::{
    self as p, CapacityReason, CodeKind, DeviceState, ErrorCode, InactiveReason, Platform,
    ReportType, Role, Transport, WarningCode, event, keys, value::Value,
};
use ratatui::buffer::CellWidth;
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
pub fn named(c: &p::Candidate) -> bool {
    !c.name.is_empty()
}

/// What an unnamed nearby device is called, by the kind it advertised.
pub fn unnamed(kind: p::Kind) -> &'static str {
    match kind {
        p::Kind::Keyboard => "Unnamed Keyboard",
        p::Kind::Mouse => "Unnamed Mouse",
        p::Kind::KeyboardMouse => "Unnamed Keyboard/Mouse",
        p::Kind::Unknown | p::Kind::Other => "Unnamed Device",
    }
}

/// A nearby device's name for the shell, or its kind when unnamed.
pub fn candidate_name(c: &p::Candidate) -> String {
    if named(c) {
        name(Some(&c.name))
    } else {
        unnamed(c.kind()).into()
    }
}

/// A nearby device's name for the TUI, or its kind when unnamed.
pub fn display_candidate_name(c: &p::Candidate) -> String {
    if named(c) {
        display_name(Some(&c.name))
    } else {
        unnamed(c.kind()).into()
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
        Transport::Unspecified => "Unknown",
    }
}

pub fn transport_long(t: Transport) -> &'static str {
    match t {
        Transport::Ble => "Bluetooth LE",
        Transport::Classic => "Bluetooth Classic",
        Transport::Unspecified => "Unknown Transport",
    }
}

/// A transport as the CLI spells it.
pub fn transport_token(t: Transport) -> String {
    model::token(t.as_str_name())
}

pub fn role_names(roles: &[Role]) -> String {
    roles
        .iter()
        .filter_map(|r| match r {
            Role::Keyboard => Some("Keyboard"),
            Role::Mouse => Some("Mouse"),
            Role::ConsumerControl => Some("Media keys"),
            Role::SystemControl => Some("System keys"),
            Role::Unknown => None,
        })
        .collect::<Vec<_>>()
        .join(", ")
}

pub fn warning_text(w: WarningCode) -> &'static str {
    match w {
        WarningCode::NumericSelectorUnsupported => {
            "The adapter can't derive a value from this numeric selector field."
        }
        WarningCode::PointerSelectorUnsupported => {
            "The adapter can't forward pointer coordinates from a selector field."
        }
        WarningCode::BufferedInputUnsupported => {
            "The adapter can't interpret this input field's custom byte format."
        }
        WarningCode::IndicatorReportTooLarge => {
            "The indicator report is larger than the Bluetooth connection allows."
        }
        WarningCode::IndicatorReadUnsupported => {
            "The device doesn't support reading this indicator report."
        }
        WarningCode::IndicatorWriteUnsupported => {
            "The device doesn't support writing this indicator report."
        }
        WarningCode::BufferedIndicatorUnsupported => {
            "The adapter can't interpret this indicator field's custom byte format."
        }
        WarningCode::IndicatorStateUnknown => {
            "The device doesn't report the indicator state needed for this update."
        }
        WarningCode::IndicatorReadFailed => "The adapter couldn't read the indicator report.",
        WarningCode::IndicatorWriteFailed => "The adapter couldn't update the indicator lights.",
        WarningCode::IndicatorArrayFull => {
            "The indicator report has no room for all active lights."
        }
        WarningCode::IndicatorRelativeSelectorUnsupported => {
            "The adapter can't forward this relative indicator selector."
        }
        WarningCode::IndicatorModeUnsupported => {
            "The indicator doesn't provide a usable on/off control."
        }
        WarningCode::IndicatorNonlinearUnsupported => {
            "The adapter can't convert this indicator's nonlinear values."
        }
        WarningCode::IndicatorScaleUnsupported => {
            "The indicator's units don't provide a usable value conversion."
        }
        WarningCode::IndicatorRangeUnsupported => {
            "The indicator's value range can't represent both states."
        }
        WarningCode::Unknown => "The adapter can't use part of this device.",
    }
}

pub fn warning_label(w: WarningCode) -> &'static str {
    match w {
        WarningCode::NumericSelectorUnsupported
        | WarningCode::PointerSelectorUnsupported
        | WarningCode::BufferedInputUnsupported => "Input Field",
        WarningCode::Unknown => "Device Warning",
        _ => "Lock Indicators",
    }
}

pub fn warning_context(w: &p::DeviceWarning) -> String {
    let mut context = format!("Service {}", w.service);
    let kind = match w.report_type() {
        ReportType::Input => Some("Input Report"),
        ReportType::Output => Some("Output Report"),
        ReportType::Feature => Some("Feature Report"),
        ReportType::Unknown => None,
    };
    match (kind, w.report_id) {
        (Some(kind), Some(id)) => {
            let _ = write!(context, ", {kind} {id}");
        }
        (Some(kind), None) => {
            let _ = write!(context, ", {kind}");
        }
        (None, Some(id)) => {
            let _ = write!(context, ", Report {id}");
        }
        (None, None) => {}
    }
    if let Some(bit) = w.bit_offset {
        let _ = write!(context, ", Bit {bit}");
    }
    if let Some(page) = w.usage_page {
        let _ = write!(context, ", Usage {page:04X}");
        if let Some(usage) = w.usage {
            let _ = write!(context, ":{usage:04X}");
        }
    }
    context
}

/// A warning on one line: its label, explanation and where on the device it is.
pub fn warning_line(w: &p::DeviceWarning) -> String {
    format!(
        "{}: {} {}",
        warning_label(w.code()),
        warning_text(w.code()),
        warning_context(w)
    )
}

pub fn platform_name(p: Platform) -> &'static str {
    match p {
        Platform::Linux => "Linux",
        Platform::Windows => "Windows",
        Platform::Mac => "macOS",
    }
}

/// A platform as the CLI spells it.
pub fn platform_token(p: Platform) -> String {
    model::token(p.as_str_name())
}

/// The host's explanation of an adapter error code. The adapter sends codes
/// only; every explanation is the host's own.
fn general_text(code: ErrorCode) -> Option<&'static str> {
    use ErrorCode::*;
    Some(match code {
        BadRequest => "the adapter rejected a malformed request",
        UnknownCommand => "this adapter's firmware doesn't support that command",
        BadArgs => "the adapter rejected the command's arguments",
        TooLong => "the request was too large for the adapter",
        NotReady => "the adapter's Bluetooth or storage isn't ready",
        NotFound => "the adapter has no saved device with that ID",
        NotConnected => "the device isn't connected. Connect it first",
        Busy => "the adapter is busy. Try again when the current operation finishes",
        Disabled => "the device is disabled; enable it before connecting",
        Blocked => "connections to this device are blocked. Unblock it before connecting",
        Unsupported => "the adapter or device doesn't support this",
        NoCapacity => "the adapter has no room for that right now",
        NoPrompt => "that pairing prompt is no longer waiting for an answer",
        StorageFailed => "the adapter couldn't read or write its saved data",
        Internal => "the adapter hit an unexpected failure",
        CandidateExpired => "this device is no longer available. Scan again",
        AuthFailed => "Bluetooth authentication failed",
        Rejected => "authentication was rejected by the user or the device",
        Timeout => "the operation's deadline expired",
        Cancelled => "the operation was cancelled",
        ConnectionFailed => "the Bluetooth link or HID setup failed",
        UnsupportedHid => "the device's HID format isn't supported",
        _ => return None,
    })
}

/// Short explanations of integration and setting failures, as device and
/// setting records carry them.
fn hidpp_text(code: ErrorCode) -> Option<&'static str> {
    use ErrorCode::*;
    Some(match code {
        Timeout => "no response",
        TransportError => "couldn't send",
        DeviceError => "device error",
        InvalidResponse => "unexpected reply",
        FeatureUnavailable => "the device doesn't provide a feature this needs",
        ProtocolUnsupported => "not supported",
        NotConnected => "the device disconnected",
        Unsupported => "not supported by the device now",
        ReadbackMismatch => "the device reported a different value from the one requested",
        _ => return None,
    })
}

/// Explains an integration or setting failure: its short wording, else the
/// general explanation.
pub fn hidpp_words(code: ErrorCode) -> String {
    hidpp_text(code)
        .or_else(|| general_text(code))
        .map(str::to_owned)
        .unwrap_or_else(|| unknown_code(code))
}

fn unknown_code(code: ErrorCode) -> String {
    format!(
        "the adapter reported error {}",
        quote(&model::code_token(code))
    )
}

/// The command-specific or general explanation of an error code.
fn code_text(code: ErrorCode, command: Option<&str>) -> String {
    use ErrorCode::*;
    let specific = match (code, command) {
        (NotFound, Some("setting get" | "setting set" | "setting forget")) => {
            Some("the adapter doesn't know that setting for the device, or the device isn't saved")
        }
        (NotFound, Some("pair start")) => general_text(CandidateExpired),
        (NotFound, Some("file list" | "file get")) => Some("the adapter has no file at that path"),
        (Unsupported, Some("scan start" | "pair start")) => {
            Some("this build does not support the requested Bluetooth transport")
        }
        (Unsupported, Some("setting set")) => {
            Some("the device can't use this setting or value right now")
        }
        _ => None,
    };
    specific
        .or_else(|| general_text(code))
        .or_else(|| hidpp_text(code))
        .map(str::to_owned)
        .unwrap_or_else(|| unknown_code(code))
}

/// Why a capacity error happened.
pub fn capacity_words(reason: CapacityReason) -> &'static str {
    match reason {
        CapacityReason::Enabled => {
            "every enabled-device place is in use; disable another device first"
        }
        CapacityReason::Storage => {
            "the adapter's storage has no room for another paired device; remove unused devices or saved settings"
        }
        CapacityReason::Connections => "every connection is in use; disconnect a device first",
        CapacityReason::Unknown => "the adapter has no room for that right now",
    }
}

/// Why work on a transport is refused, and its saved devices are unused, while the adapter has
/// it disabled.
pub fn transport_disabled(t: Transport) -> String {
    format!("{} is disabled on the adapter", transport_long(t))
}

/// `transport_disabled` as sentences, as the TUI's details of a device or candidate state it.
pub fn transport_disabled_sentence(t: Transport) -> String {
    format!(
        "{} is disabled. Enable it in the adapter settings.",
        transport_long(t)
    )
}

/// Why a saved device of `transport` is not used for connections.
pub fn inactive_words(reason: InactiveReason, transport: Transport) -> String {
    match reason {
        InactiveReason::UnsupportedTransport => {
            "this build doesn't support its Bluetooth transport".into()
        }
        InactiveReason::TransportDisabled => transport_disabled(transport),
        InactiveReason::Blocked => "it is blocked".into(),
        InactiveReason::Disabled => "it is disabled".into(),
        InactiveReason::Capacity => {
            "every enabled-device place is in use; disable another device to make room".into()
        }
        InactiveReason::Unknown => "the adapter isn't using this device".into(),
    }
}

/// A saved device's Bluetooth enablement: preference, then why it isn't used.
pub fn enablement_words(d: &p::Device) -> String {
    match (d.enabled, model::inactive(d)) {
        (_, None) => "enabled".into(),
        (false, Some(InactiveReason::Disabled)) => "disabled".into(),
        (preferred, Some(reason)) => format!(
            "{}, not active: {}",
            if preferred { "enabled" } else { "disabled" },
            inactive_words(reason, d.transport())
        ),
    }
}

/// A transport's setting as the CLI spells it.
pub fn enabled_word(on: bool) -> &'static str {
    if on { "enabled" } else { "disabled" }
}

/// Explains an adapter error without its code.
pub fn wire_text(e: &p::Error, command: Option<&str>) -> String {
    match e.code() {
        ErrorCode::NoCapacity => format!("the adapter has no room: {}", capacity_words(e.reason())),
        ErrorCode::StorageFailed if e.outcome_unknown => "save status unknown: the adapter couldn't confirm whether the change was saved, and didn't apply it; check the current state before trying again".into(),
        code => code_text(code, command),
    }
}

/// An adapter error with its code before the explanation, as the shell and
/// device details show it.
pub fn wire_line(e: &p::Error, command: Option<&str>) -> String {
    format!("{}: {}", model::code_token(e.code()), wire_text(e, command))
}

/// An error for the shell, unescaped. An adapter error names its code
/// before the explanation; a local message that wraps one keeps its context
/// with the code's token expanded.
pub fn error_line(e: &Error) -> String {
    match &e.dongle {
        Some(w) => {
            let code = model::code_token(w.code());
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
    match &e.dongle {
        Some(w) if e.message == model::code_token(w.code()) => display(&wire_text(w, e.command)),
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

/// A reason as a sentence: capitalized and ending with a period.
pub fn sentence(s: &str) -> String {
    let s = capitalized(s);
    if s.ends_with('.') { s } else { format!("{s}.") }
}

/// HID++'s status as the CLI spells it.
pub fn up_token(up: Up) -> String {
    match up {
        Up::Off => "off".into(),
        Up::Disconnected => "disconnected".into(),
        Up::Starting => "starting".into(),
        Up::Active => "active".into(),
        Up::Unsupported => "unsupported".into(),
        Up::Error(code) => format!("error:{}", model::code_token(code)),
    }
}

/// HID++'s status for details.
pub fn up_words(up: Up) -> String {
    match up {
        Up::Off => "Off".into(),
        Up::Disconnected => "Waiting to Connect".into(),
        Up::Starting => "Setting Up".into(),
        Up::Active => "Active".into(),
        Up::Unsupported => "Unsupported".into(),
        Up::Error(code) => format!("Failed: {}", hidpp_words(code)),
    }
}

/// The HID++ version a device reported, as details show it.
pub fn hidpp_protocol_text(d: &p::Device) -> String {
    match (model::hidpp_version(d), model::hidpp_up(d)) {
        (Some((major, minor)), _) => format!("{major}.{minor}"),
        (None, Up::Unsupported) => "Unavailable".into(),
        (None, Up::Starting) => "Checking".into(),
        _ => "Unknown".into(),
    }
}

/// The security of a device's current link. Only a connected device has one, so a report
/// left over from an earlier link is never shown.
pub fn link_security(d: &p::Device) -> Option<Option<&p::Security>> {
    model::connected(d).then_some(d.security.as_ref())
}

/// A short summary of link security: encryption and whether pairing was
/// authenticated. Unauthenticated encryption is a valid link, not a failure,
/// and no combination is called simply secure.
pub fn security_summary(s: Option<&p::Security>) -> &'static str {
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
pub fn security_facts(s: &p::Security) -> [SecurityFact; 4] {
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
            value: s
                .key_size
                .map_or("Not Reported".into(), |b| format!("{} bits", b * 8)),
            reading: s.key_size.map_or(Reading::NotReported, |_| Reading::Value),
        },
    ]
}

/// A device's state as the CLI spells it.
pub fn state_token(d: &p::Device) -> String {
    model::token(d.state().as_str_name())
}

/// One saved device on a line for the shell.
pub fn device_line(d: &p::Device, warnings: usize) -> String {
    let mut policy = String::from(if d.trusted { "trusted" } else { "untrusted" });
    if d.blocked {
        policy.push_str(" blocked");
    }
    if d.paused {
        policy.push_str(" reconnect-paused");
    }
    let mut line = format!(
        "{}  {}  {}  {}  {}",
        safe(&d.id),
        name(Some(&d.name)),
        transport_token(d.transport()),
        state_token(d),
        policy
    );
    let _ = write!(
        line,
        " bluetooth={}",
        if d.enabled { "enabled" } else { "disabled" }
    );
    if let Some(reason) = model::inactive(d).filter(|r| *r != InactiveReason::Disabled) {
        let _ = write!(line, " inactive={}", model::token(reason.as_str_name()));
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
    if warnings > 0 {
        let _ = write!(line, " warnings={warnings}");
    }
    if let Some(code) = model::last_error(d) {
        let _ = write!(line, " error={}", model::code_token(code));
    }
    let _ = write!(
        line,
        " hidpp={} hidpp-state={}",
        on_off(model::hidpp_enabled(d)),
        up_token(model::hidpp_up(d))
    );
    if let Some((major, minor)) = model::hidpp_version(d) {
        let _ = write!(line, " hidpp-protocol={major}.{minor}");
    }
    line
}

pub fn candidate_line(c: &p::Candidate) -> String {
    format!(
        "{}  {}  {}  candidate",
        safe(&c.id),
        candidate_name(c),
        transport_token(c.transport())
    )
}

/// `adapter status` for the shell.
pub fn adapter_info(port: &str, st: &p::Status, devices: &[p::Device]) -> String {
    let mut b = format!("Adapter {}", safe(&st.id));
    let mut field = |key: &str, value: &str| {
        let _ = write!(b, "\n  {key}: {}", safe(value));
    };
    field("Port", port);
    if let Some(v) = model::info_text(&st.info, keys::FIRMWARE_VERSION) {
        field("Firmware", v);
    }
    if let Some(v) = model::info_text(&st.info, keys::BOARD_NAME) {
        field("Board", v);
    }
    field("Name", &display(&st.name));
    field("Platform", &platform_token(st.platform()));
    for t in model::transports(st) {
        if let Some(on) = model::transport_enabled(st, t) {
            field(transport_long(t), enabled_word(on));
        }
    }
    field("Ready", &yes_no(st.ready).to_lowercase());
    if let Some(full) = model::info_bool(&st.info, keys::STORAGE_FULL) {
        field("Storage Full", &yes_no(full).to_lowercase());
    }
    let transports: Vec<&str> = model::transports(st)
        .into_iter()
        .map(transport_name)
        .collect();
    field(
        "Transports",
        &if transports.is_empty() {
            "none".to_owned()
        } else {
            transports.join(", ")
        },
    );
    field("Saved Devices", &devices.len().to_string());
    field(
        "Connected Devices",
        &devices
            .iter()
            .filter(|d| model::connected(d))
            .count()
            .to_string(),
    );
    for t in model::enabled_transports(st) {
        let enabled = devices
            .iter()
            .filter(|d| d.enabled && d.transport == t as i32)
            .count() as u32;
        let value = match model::max_enabled(st, t) {
            Some(max) => format!(
                "{enabled} of {max} in use, {} free",
                max.saturating_sub(enabled)
            ),
            None => format!("{enabled} in use"),
        };
        field(&format!("Enabled Places ({})", transport_name(t)), &value);
    }
    b
}

pub fn candidate_info(c: &p::Candidate) -> String {
    let mut b = format!(
        "Candidate {} ({})\n  Name: {}\n  State: discovered",
        safe(&c.id),
        transport_token(c.transport()),
        candidate_name(c)
    );
    if let Some(rssi) = c.rssi {
        let _ = write!(b, "\n  Signal: {rssi} dBm");
    }
    b
}

/// `device get` of a saved device, one field per line after bluetoothctl.
pub fn device_info(d: &p::Device, warnings: &[p::DeviceWarning]) -> String {
    let mut b = format!(
        "Device {} ({})",
        safe(&d.id),
        transport_token(d.transport())
    );
    let mut field = |key: &str, value: &str| {
        let _ = write!(b, "\n  {key}: {value}");
    };
    field("Name", &name(Some(&d.name)));
    let roles = model::roles(d);
    if !roles.is_empty() {
        let roles: Vec<String> = roles
            .iter()
            .map(|r| model::token(r.as_str_name()))
            .collect();
        field("Roles", &roles.join(", "));
    }
    field("State", &state_token(d));
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
    field("Trusted", &yes_no(d.trusted).to_lowercase());
    field("Blocked", &yes_no(d.blocked).to_lowercase());
    field("Reconnect", if d.paused { "paused" } else { "automatic" });
    field("Logitech Features", on_off(model::hidpp_enabled(d)));
    field("HID++ Protocol", &hidpp_protocol_text(d));
    field("HID++ Status", &up_words(model::hidpp_up(d)));
    for warning in warnings {
        field(
            warning_label(warning.code()),
            &format!(
                "{} {}",
                warning_text(warning.code()),
                warning_context(warning)
            ),
        );
    }
    if let Some(code) = model::last_error(d) {
        let e = p::Error {
            code: code as i32,
            ..Default::default()
        };
        field("Last Error", &safe(&wire_line(&e, None)));
    }
    for r in info_rows(&d.info) {
        field(&r.label, &r.value);
    }
    b
}

/// A value of an information key in words.
pub fn info_value(key: &str, v: &Value) -> String {
    match (key, v) {
        (keys::VENDOR_ID | keys::PRODUCT_ID | keys::PRODUCT_VERSION, Value::Integer(n)) => {
            format!("0x{n:04X}")
        }
        (keys::BATTERY_LEVEL, Value::Integer(n)) => format!("{n}%"),
        (keys::POWER_AUTO_OFF, Value::Integer(0)) => "Never".into(),
        (_, Value::Bool(b)) => yes_no(*b).into(),
        (_, Value::Text(t)) if model::key(key).is_some_and(|(k, _)| k.kind == keys::Kind::Enum) => {
            safe(&catalog::choice_words(key, t))
        }
        (_, Value::Text(t)) => safe(t),
        (_, Value::Integer(n)) => match catalog::unit(key) {
            "" => n.to_string(),
            unit => format!("{n} {unit}"),
        },
        (_, v) => safe(&catalog::value_string(v)),
    }
}

/// A device's battery: its charge and whether it charges, each None while
/// unknown. Unknown is never shown as 0% or not charging.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Battery {
    pub percent: Option<i64>,
    pub charging: Option<bool>,
}
impl Battery {
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
pub fn battery(info: &[p::Info]) -> Option<Battery> {
    let b = Battery {
        percent: model::info_integer(info, keys::BATTERY_LEVEL),
        charging: model::info_bool(info, keys::BATTERY_CHARGING),
    };
    (b.percent.is_some() || b.charging.is_some()).then_some(b)
}

/// A labeled line of device information.
#[derive(Clone, Debug, PartialEq)]
pub struct InfoRow {
    /// The information key the row shows.
    pub key: String,
    pub label: String,
    pub value: String,
}

/// Information keys that identify the device rather than describe it, shown with its
/// diagnostics.
pub const IDENTIFIER_KEYS: [&str; 5] = [
    keys::VENDOR_REGISTRY,
    keys::VENDOR_ID,
    keys::PRODUCT_ID,
    keys::PRODUCT_VERSION,
    keys::BOOTLOADER_VERSION,
];

/// The reported information in display order: the battery's charge and
/// charging, then the other keys this build knows. With either battery value
/// reported, both rows are shown and an unknown one reads "Unknown". A vendor
/// ID is read with its namespace, on one row.
pub fn info_rows(info: &[p::Info]) -> Vec<InfoRow> {
    let mut rows: Vec<InfoRow> = Vec::new();
    if let Some(b) = battery(info) {
        rows.push(InfoRow {
            key: keys::BATTERY_LEVEL.into(),
            label: catalog::label(keys::BATTERY_LEVEL),
            value: b.percent_words(),
        });
        rows.push(InfoRow {
            key: keys::BATTERY_CHARGING.into(),
            label: catalog::label(keys::BATTERY_CHARGING),
            value: b.charging_words(),
        });
    }
    let registry = model::info(info, keys::VENDOR_REGISTRY);
    let vendor = model::info(info, keys::VENDOR_ID).is_some();
    for i in catalog::presented_info(info) {
        let key = i.key.as_str();
        match key {
            keys::BATTERY_LEVEL | keys::BATTERY_CHARGING => continue,
            keys::VENDOR_REGISTRY if vendor => continue,
            _ => {}
        }
        let Some(v) = i.value.as_ref().and_then(|v| v.value.as_ref()) else {
            continue;
        };
        let mut value = info_value(key, v);
        if key == keys::VENDOR_ID
            && let Some(r) = registry
        {
            value = format!("{value} ({})", info_value(keys::VENDOR_REGISTRY, r));
        }
        rows.push(InfoRow {
            key: key.into(),
            label: catalog::label(key),
            value,
        });
    }
    rows
}

/// A device's battery for `device list`, or None when it reports none.
pub fn battery_list(d: &p::Device) -> Option<String> {
    Some(battery(&d.info)?.compact("(charging)"))
}

pub fn scan_summary(count: u32, truncated: bool) -> String {
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
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Filter {
    #[default]
    All,
    Saved,
    Enabled,
    Connected,
    Trusted,
}

/// `device list` for the shell: saved devices, then this session's candidates,
/// which are never associated with saved devices.
pub fn devices(st: &State, filter: Filter) -> String {
    let mut lines: Vec<String> = st
        .devices
        .iter()
        .filter(|d| match filter {
            Filter::All | Filter::Saved => true,
            Filter::Enabled => d.enabled,
            Filter::Connected => d.state() == DeviceState::Connected,
            Filter::Trusted => d.trusted,
        })
        .map(|d| {
            let mut line = device_line(d, st.warnings_of(&d.id).len());
            if let Some(b) = battery_list(d) {
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

/// `adapter list`: attached adapters.
pub fn ports(ports: &[PortInfo]) -> String {
    if ports.is_empty() {
        return "No Cordial adapters found. Use --port for an explicit serial path.".into();
    }
    ports
        .iter()
        .map(|p| format!("{}  {}", safe(&p.port), safe(&p.id)))
        .collect::<Vec<_>>()
        .join("\n")
}

/// `adapter list --json`: one object per adapter.
pub fn ports_json(ports: &[PortInfo]) -> String {
    let list: Vec<serde_json::Value> = ports
        .iter()
        .map(|p| serde_json::json!({"port": p.port, "id": p.id}))
        .collect();
    terminal_json(&serde_json::Value::Array(list).to_string())
}

/// A resolved device or candidate as a result names it.
pub fn label(subject: &Subject) -> String {
    if subject.name.is_empty() {
        safe(&subject.id)
    } else {
        safe(&subject.name)
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

/// One `file list` row: type, size and name, tab-separated.
pub fn file_line(e: &p::FileEntry) -> String {
    format!(
        "{}\t{}\t{}",
        if e.directory { "directory" } else { "file" },
        e.size,
        safe(&e.name)
    )
}

/// `warning list DEV`.
pub fn warnings_text(subject: &Subject, warnings: &[p::DeviceWarning]) -> String {
    let mut b = format!("Warnings of {}", label(subject));
    if warnings.is_empty() {
        b.push_str("\n  No warnings.");
    }
    for w in warnings {
        let _ = write!(b, "\n  {}", warning_line(w));
    }
    b
}

/// A command's result for the shell and scripts; empty when the command has
/// nothing to add, as for a stopped scan.
pub fn outcome(command: &Command, outcome: &Outcome, state: Option<&State>) -> String {
    match outcome {
        Outcome::Status(status) => {
            let (port, devices) =
                state.map_or(("", &[][..]), |s| (s.port.as_str(), &s.devices[..]));
            adapter_info(port, status, devices)
        }
        Outcome::Files { entries, .. } => {
            entries.iter().map(file_line).collect::<Vec<_>>().join("\n")
        }
        Outcome::FileSaved { path, local, bytes } => format!(
            "Saved {} to {} ({bytes} bytes).",
            safe(path),
            safe(&local.display().to_string())
        ),
        Outcome::Name(name) if matches!(command, Command::Name(None)) => {
            format!("Adapter name reset to {}.", display(name))
        }
        Outcome::Name(name) => format!("Adapter renamed to {}.", display(name)),
        Outcome::Platform(p) => format!("Platform set to {}.", platform_name(*p)),
        Outcome::Transport(t, on) => format!("{} {}.", transport_long(*t), enabled_word(*on)),
        Outcome::Bootloader => "The adapter is restarting into its bootloader.".into(),
        Outcome::ScanStarted(transports) => format!(
            "Discovery started ({}).",
            transports
                .iter()
                .map(|t| transport_token(*t))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Outcome::ScanFinished { count, truncated } => scan_summary(*count, *truncated),
        Outcome::ScanStopped { was_running } => {
            if *was_running {
                String::new()
            } else {
                "Discovery is already off.".into()
            }
        }
        Outcome::Paired { subject, device } => {
            match device
                .as_ref()
                .and_then(|d| Some((model::inactive(d)?, d.transport())))
            {
                Some((reason, transport)) => paired_disabled(subject, reason, transport),
                None => format!("Paired and saved {}.", label(subject)),
            }
        }
        Outcome::Answered => "Pairing answer sent.".into(),
        Outcome::PairingCancelled => "Pairing cancellation requested.".into(),
        Outcome::Devices => state.map_or_else(String::new, |s| devices(s, Filter::All)),
        Outcome::Device { subject, device } => {
            let label = label(subject);
            match command {
                Command::Get(_) => {
                    device_info(device, state.map_or(&[][..], |s| s.warnings_of(&device.id)))
                }
                Command::Connect(_) => format!("Connecting to {label}."),
                Command::Disconnect(_) => format!("Disconnected {label}."),
                Command::Set(_, Toggle::Enabled, true) => format!("Enabled {label}."),
                Command::Set(_, Toggle::Enabled, false) => format!("Disabled {label}."),
                Command::Set(_, Toggle::Trusted, true) => format!("Trusted {label}."),
                Command::Set(_, Toggle::Trusted, false) => format!("Removed trust for {label}."),
                Command::Set(_, Toggle::Blocked, true) => format!("Blocked {label}."),
                Command::Set(_, Toggle::Blocked, false) => format!("Unblocked {label}."),
                Command::Set(_, Toggle::Hidpp, on) => {
                    format!("HID++ turned {} for {label}.", on_off(*on))
                }
                _ => String::new(),
            }
        }
        Outcome::Candidate(c) => candidate_info(c),
        Outcome::Hidden(subject) => format!("Candidate hidden: {}", safe(&subject.id)),
        Outcome::Unpaired(subject) => format!("Forgot {}.", label(subject)),
        Outcome::Refreshing(subject) => {
            format!("Reading current information from {}.", label(subject))
        }
        Outcome::Warnings { subject, warnings } => warnings_text(subject, warnings),
        Outcome::Settings(subject) => catalog::subject_text(subject, state),
        Outcome::Setting { subject, setting } => match state.and_then(|s| s.device(&subject.id)) {
            Some(d) => catalog::setting_detail(d, setting),
            None => String::new(),
        },
        Outcome::Saved {
            subject,
            set,
            forget,
            settings,
        } => catalog::saved_text(subject, set, forget, settings, state),
        Outcome::Features { subject, features } => {
            match state.and_then(|s| s.device(&subject.id)) {
                Some(d) => catalog::feature_list(d, features),
                None => String::new(),
            }
        }
    }
}

/// A completed pairing whose device the adapter doesn't use now.
pub fn paired_disabled(subject: &Subject, reason: InactiveReason, transport: Transport) -> String {
    let label = label(subject);
    match reason {
        InactiveReason::Disabled => format!("Paired and saved {label}; enable it to connect."),
        reason => format!(
            "Paired and saved {label}; it can't connect now because {}.",
            inactive_words(reason, transport)
        ),
    }
}

/// A failed command's error line for the shell and scripts. The caller adds the `Error: `
/// prefix where the shell uses one.
pub fn failure_text(error: &Error) -> String {
    safe(&error_line(error))
}

/// A notice for the shell and scripts, or None when it has nothing to show:
/// callers report their own results.
pub fn notice(n: &Notice, state: Option<&State>) -> Option<String> {
    match n {
        Notice::Event {
            event,
            first,
            changed,
        } => event
            .kind
            .as_ref()
            .and_then(|k| event_line(k, *first, changed, state)),
        Notice::Response(_) => None,
        Notice::RefreshFailed(e) => Some(format!(
            "Error: device refresh failed: {}",
            safe(&error_line(e))
        )),
    }
}

fn event_line(
    kind: &event::Kind,
    first: bool,
    changed: &[String],
    state: Option<&State>,
) -> Option<String> {
    let warnings = |id: &str| state.map_or(0, |s| s.warnings_of(id).len());
    match kind {
        event::Kind::ScanFound(c) if first => Some(format!("[NEW] {}", candidate_line(c))),
        event::Kind::ScanFound(_) => None,
        event::Kind::ScanDone(done) => Some(scan_summary(done.count, done.truncated)),
        event::Kind::Device(d) if first => {
            Some(format!("[NEW] {}", device_line(d, warnings(&d.id))))
        }
        event::Kind::Device(d) => Some(format!("[CHG] {}", device_line(d, warnings(&d.id)))),
        event::Kind::DeviceRemoved(r) => Some(format!("[DEL] {}", safe(&r.id))),
        event::Kind::Adapter(a) => {
            let mut line = format!(
                "[CHG] Adapter name={} platform={}",
                quote(&a.name),
                platform_token(a.platform())
            );
            for t in model::transports(a) {
                if let Some(on) = model::transport_enabled(a, t) {
                    let _ = write!(line, " {}={}", transport_token(t), enabled_word(on));
                }
            }
            Some(line)
        }
        event::Kind::Settings(s) => {
            let d = state?.device(&s.device)?;
            let lines: Vec<String> = catalog::presented(&s.settings)
                .into_iter()
                .filter(|setting| changed.contains(&setting.key))
                .map(|setting| {
                    format!(
                        "[CHG] {}{}",
                        safe(&s.device),
                        catalog::setting_line(d, setting)
                    )
                })
                .collect();
            (!lines.is_empty()).then(|| lines.join("\n"))
        }
        event::Kind::Warnings(w) if w.warnings.is_empty() => {
            Some(format!("[CHG] {} warnings cleared", safe(&w.device)))
        }
        event::Kind::Warnings(w) => Some(
            w.warnings
                .iter()
                .map(|warning| format!("[CHG] {} {}", safe(&w.device), warning_line(warning)))
                .collect::<Vec<_>>()
                .join("\n"),
        ),
        event::Kind::Pairing(pairing) => {
            let prompt = model::prompt(pairing)?;
            Some(prompt_line(&pairing.candidate, &prompt))
        }
    }
}

/// The token of a pairing prompt, such as `enter_passkey`.
pub fn prompt_token(prompt: &Prompt) -> &'static str {
    match prompt {
        Prompt::EnterCode(CodeKind::Pin) => "enter_pin",
        Prompt::EnterCode(_) => "enter_passkey",
        Prompt::ConfirmCode(_) => "confirm_passkey",
        Prompt::ShowCode(CodeKind::Pin, _) => "display_pin",
        Prompt::ShowCode(..) => "display_passkey",
    }
}

/// The value a prompt shows, if any.
pub fn prompt_value(prompt: &Prompt) -> Option<&str> {
    match prompt {
        Prompt::ConfirmCode(v) | Prompt::ShowCode(_, v) => Some(v),
        Prompt::EnterCode(_) => None,
    }
}

/// A pairing prompt for the shell: what to enter or compare, where, and for
/// which candidate.
pub fn prompt_line(candidate: &str, prompt: &Prompt) -> String {
    let place = match prompt {
        Prompt::ShowCode(..) => "Enter on the peripheral",
        Prompt::ConfirmCode(_) => "Compare with the peripheral; answer yes or no",
        Prompt::EnterCode(_) => "Enter on this computer",
    };
    let mut what = prompt_token(prompt).to_owned();
    if let Some(v) = prompt_value(prompt).filter(|v| !v.is_empty()) {
        let _ = write!(what, " {}", safe(v));
    }
    format!("[pair {}] {place}: {what}", safe(candidate))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_off_information_reads_never_at_zero() {
        assert_eq!(
            info_value(keys::POWER_AUTO_OFF, &Value::Integer(0)),
            "Never"
        );
        assert_eq!(
            info_value(keys::POWER_AUTO_OFF, &Value::Integer(600)),
            "600 s"
        );
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
        let back: serde_json::Value = serde_json::from_str(&shown).unwrap();
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
    fn quoting_escapes_quotes_backslashes_and_controls() {
        assert_eq!(quote("Test \"kb\"\\"), "\"Test \\\"kb\\\"\\\\\"");
        assert_eq!(quote("a\u{7}\u{202e}"), "\"a\\x07\\u202e\"");
    }
}
