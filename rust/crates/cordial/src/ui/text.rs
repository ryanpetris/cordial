//! Human-readable text shared by the shell, scripts and the TUI: safe terminal
//! rendering of untrusted strings, the host's explanations of adapter codes,
//! and the shell's line formats.
use crate::{
    controller::{Command, Notice, Outcome, State, Subject, Toggle},
    error::Error,
    model::{self, Prompt, Up},
    profiles,
    settings::{self, Field, Leaf, Value},
    ui::{
        app::words::{self, Scope},
        catalog,
    },
};
use cordial_client::serial::PortInfo;
use cordial_protocol::{
    self as p, CodeKind, DeviceState, ErrorCode, ErrorOutcome, ErrorReason, InactiveReason, Kind,
    OutputMode, OutputState, Platform, ResourceType, Role, Transport, VendorRegistry, event,
    output_status, profile_list_entry,
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

/// What an unnamed nearby device is called, by the kinds it advertised.
pub fn unnamed(kinds: &[Kind]) -> &'static str {
    match (
        kinds.contains(&Kind::Keyboard),
        kinds.contains(&Kind::Mouse),
    ) {
        (true, true) => "Unnamed Keyboard/Mouse",
        (true, false) => "Unnamed Keyboard",
        (false, true) => "Unnamed Mouse",
        (false, false) if kinds.contains(&Kind::Gamepad) => "Unnamed Gamepad",
        (false, false) => "Unnamed Device",
    }
}

/// A nearby device's name for the shell, or its kinds when unnamed.
pub fn candidate_name(c: &p::Candidate) -> String {
    if named(c) {
        name(Some(&c.name))
    } else {
        unnamed(&model::known_kinds(&c.kinds)).into()
    }
}

/// A nearby device's name for the TUI, or its kinds when unnamed.
pub fn display_candidate_name(c: &p::Candidate) -> String {
    if named(c) {
        display_name(Some(&c.name))
    } else {
        unnamed(&model::known_kinds(&c.kinds)).into()
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

pub fn output_mode_name(m: OutputMode) -> &'static str {
    match m {
        OutputMode::Disabled => "Disabled",
        OutputMode::Hid => "HID",
        OutputMode::Xinput => "XInput",
    }
}

/// An output mode as the CLI spells it.
pub fn output_mode_token(m: OutputMode) -> String {
    model::token(m.as_str_name())
}

/// An output's state, or why setting it up failed; unset or unknown reads as unknown.
pub fn output_state(o: &p::OutputStatus) -> Result<OutputState, &p::Error> {
    match &o.status {
        Some(output_status::Status::Error(e)) => Err(e),
        Some(output_status::Status::State(n)) => Ok(OutputState::try_from(*n).unwrap_or_default()),
        None => Ok(OutputState::Unknown),
    }
}

/// An output's state for details.
pub fn output_state_words(o: &p::OutputStatus) -> String {
    match output_state(o) {
        Ok(OutputState::Unknown) => "Unknown".into(),
        Ok(OutputState::Disabled) => "Disabled".into(),
        Ok(OutputState::Disconnected) => "Waiting to Connect".into(),
        Ok(OutputState::Pending) => "Setting Up".into(),
        Ok(OutputState::Active) => "Active".into(),
        Err(e) if model::unsupported(e) && model::plain(e) => "Unsupported".into(),
        Err(e) if e.outcome == ErrorOutcome::Complete as i32 => sentence(&hidpp_error_words(e)),
        Err(e) => format!("Failed: {}", hidpp_error_words(e)),
    }
}

/// An output's state as the CLI spells it.
pub fn output_state_token(o: &p::OutputStatus) -> String {
    match output_state(o) {
        Ok(state) => model::token(state.as_str_name()),
        Err(e) => format!("error:{}", error_token(e)),
    }
}

/// An output's zero-based slot as people count, from one.
pub fn slot_number(slot: u32) -> String {
    (u64::from(slot) + 1).to_string()
}

pub fn role_names(roles: &[Role]) -> String {
    roles
        .iter()
        .filter_map(|r| match r {
            Role::Keyboard => Some("Keyboard"),
            Role::Mouse => Some("Mouse"),
            Role::ConsumerControl => Some("Media Keys"),
            Role::SystemControl => Some("System Keys"),
            Role::Gamepad => Some("Gamepad"),
            Role::Unknown => None,
        })
        .collect::<Vec<_>>()
        .join(", ")
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
        BadRequest => "the adapter couldn't understand the request from Cordial",
        UnknownCommand => {
            "this adapter's firmware doesn't support that action. Update the firmware to use it"
        }
        BadArgs => "the adapter couldn't accept those values",
        TooLong => "the request was too large for the adapter",
        NotReady => "the adapter's Bluetooth or storage isn't ready",
        NotFound => "the adapter has no saved device with that ID",
        NotConnected => "the device isn't connected. Connect it first",
        Busy => "the adapter is busy. Try again when the current operation finishes",
        Disabled => "the device is disabled. Enable it before connecting",
        Blocked => "connections to this device are blocked. Unblock it before connecting",
        Unsupported => "the adapter or device doesn't support this",
        NoCapacity => "the adapter has no room for that right now",
        NoPrompt => "that pairing prompt is no longer waiting for an answer",
        StorageFailed => "the adapter couldn't read or write its saved data",
        Internal => "the adapter hit an unexpected failure",
        InUse => {
            "this profile is still in use. Remove it from every device and interface before deleting it"
        }
        CandidateExpired => "this device is no longer available. Scan again",
        AuthFailed => "Bluetooth authentication with this device failed",
        Rejected => "the pairing was rejected on this computer or on the device",
        Timeout => "this device didn't respond in time",
        Cancelled => "the operation was cancelled",
        ConnectionFailed => "the adapter couldn't connect to this device",
        UnsupportedHid => "the adapter can't use this device's input format",
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
        _ => return None,
    })
}

/// Status labels of the adapter's USB capacity errors, in the order shown. Each applies on its
/// own, and both can apply at once.
pub const USB_CAPACITY: [(ErrorCode, &str); 2] = [
    (
        ErrorCode::UsbRequiredCapacityExceeded,
        "Required Capacity Exceeded",
    ),
    (
        ErrorCode::UsbReservedCapacityExceeded,
        "Reserved Capacity Exceeded",
    ),
];

/// The adapter's USB capacity errors, by their labels.
pub fn usb_capacity(st: &p::Status) -> Vec<(ErrorCode, &'static str)> {
    USB_CAPACITY
        .into_iter()
        .filter(|(code, _)| model::has_error(st, *code))
        .collect()
}

/// A resource's usage in a few words: "3 of 7", or what is known of it; None when nothing is.
pub fn capacity_text(r: &p::ResourceCapacity) -> Option<String> {
    usage(r, |n| n.to_string())
}

/// The adapter's storage usage in bytes, as `capacity_text` words it.
pub fn storage_text(st: &p::Status) -> Option<String> {
    usage(
        model::resource(&st.resources, ResourceType::Storage)?,
        bytes,
    )
}

fn usage(r: &p::ResourceCapacity, unit: impl Fn(u64) -> String) -> Option<String> {
    Some(match (r.count, r.limit) {
        (Some(count), Some(limit)) => format!("{} of {}", unit(count.into()), unit(limit.into())),
        (Some(count), None) => unit(count.into()),
        (None, Some(limit)) => format!("Up to {}", unit(limit.into())),
        (None, None) => return None,
    })
}

/// The validated format of the adapter's mounted storage.
pub fn storage_format(st: &p::Status) -> Option<u32> {
    st.information.as_ref()?.storage_format
}

/// Short explanations of integration and setting failures, as device and
/// setting records carry them.
fn hidpp_text(code: ErrorCode) -> Option<&'static str> {
    use ErrorCode::*;
    Some(match code {
        Timeout => "this device didn't respond in time",
        TransportError => "the adapter couldn't exchange messages with this device",
        DeviceError => "this device reported an error",
        InvalidResponse => "the adapter couldn't understand this device's reply",
        FeatureUnavailable => "this device doesn't have a feature this action needs",
        ProtocolUnsupported => "this device doesn't support this feature",
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

/// Explains an integration, setting or output failure with its details.
pub fn hidpp_error_words(e: &p::Error) -> String {
    described(
        e,
        reason_words(e).map_or_else(|| hidpp_words(e.code()), str::to_owned),
        Scope::Apply,
        related_line,
    )
}

/// An error's code, or None for one this app doesn't know.
fn known_code(e: &p::Error) -> Option<ErrorCode> {
    ErrorCode::try_from(e.code).ok()
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
        (NotFound, Some("file list" | "file get" | "file put" | "file delete")) => {
            Some("the adapter has no file at that path")
        }
        (
            NotFound,
            Some(
                "profile show"
                | "profile copy"
                | "profile delete"
                | "profile rule"
                | "profile rule list"
                | "adapter interface"
                | "adapter settings"
                | "device set profiles",
            ),
        ) => Some("the adapter has no profile with that ID"),
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

/// What failed, when the error's code and reason together say more than its code.
fn reason_words(e: &p::Error) -> Option<&'static str> {
    use {ErrorCode as C, ErrorReason as R};
    Some(
        match (known_code(e)?, ErrorReason::try_from(e.reason).ok()?) {
            (C::NoCapacity, R::EnabledDevices) => {
                "every enabled-device place is in use. Disable another device first"
            }
            (C::NoCapacity, R::Storage) => {
                "the adapter's storage has no room for another paired device. Remove unused devices or saved settings"
            }
            (C::NoCapacity, R::Connections) => {
                "every connection is in use. Disconnect a device first"
            }
            (C::NoCapacity, R::ProfileMemory) => {
                "the adapter doesn't have enough profile memory for this change"
            }
            (C::NoCapacity, R::ScanCandidates) => {
                "the adapter had no room to list every device it found"
            }
            (C::NoCapacity, R::OutputBackend) => {
                "the adapter doesn't have enough USB capacity for this output"
            }
            (C::NotReady, R::Radio) => "the adapter's Bluetooth radio isn't ready",
            (C::NotReady, R::Storage) => "the adapter's storage isn't ready",
            (C::Unsupported, R::OutputBackend) => "the adapter doesn't support this output",
            (C::Unsupported, R::InputLayout) => {
                "the adapter can't use this device's input layout for this output"
            }
            (C::Internal, R::ScanCandidates) => {
                "the adapter couldn't keep track of every device it found"
            }
            (C::StorageFailed | C::Internal | C::Unknown, R::Radio) => {
                "the adapter reported a Bluetooth error"
            }
            _ => return None,
        },
    )
}

/// What an error's reason concerns, when neither its code's nor `reason_words` already say.
fn context_words(e: &p::Error) -> Option<&'static str> {
    use {ErrorCode as C, ErrorReason as R};
    if reason_words(e).is_some() {
        return None;
    }
    let reason = ErrorReason::try_from(e.reason).ok()?;
    let implied = match known_code(e) {
        Some(C::StorageFailed | C::StorageFull) => Some(R::Storage),
        Some(C::ProfileMemoryLow | C::ProfileMemoryExceeded) => Some(R::ProfileMemory),
        Some(C::UsbRequiredCapacityExceeded | C::UsbReservedCapacityExceeded) => {
            Some(R::OutputBackend)
        }
        _ => None,
    };
    if implied == Some(reason) {
        return None;
    }
    Some(match reason {
        R::Unknown => return None,
        R::EnabledDevices => "the adapter reported an error with enabled devices",
        R::Connections => "the adapter reported an error with its connections",
        R::ScanCandidates => "the adapter reported an error with the devices found while searching",
        R::Storage => "the adapter reported an error with its storage",
        R::ProfileMemory => "the adapter reported an error with profile memory",
        R::Radio => "the adapter reported a Bluetooth error",
        R::OutputBackend => "the adapter reported an error with its USB output",
        R::InputLayout => "the adapter reported an error with this device's input layout",
    })
}

/// A shared message as a lowercase CLI clause without its final period.
fn message_clause(message: &str) -> String {
    words::reason(message)
        .split(". ")
        .map(|part| {
            let mut chars = part.chars();
            chars.next().map_or_else(String::new, |first| {
                first.to_lowercase().chain(chars).collect::<String>()
            })
        })
        .collect::<Vec<_>>()
        .join("; ")
}

/// An outcome followed by the failure's explanation and each related failure.
/// A related failure's own details stay inside its parentheses.
fn detailed(e: &p::Error, explanation: String) -> String {
    described(e, explanation, Scope::Other, related_line)
}

/// `detailed`, with each related failure as `related` words it.
fn described(
    e: &p::Error,
    explanation: String,
    scope: Scope,
    related: fn(&p::Error, Scope) -> String,
) -> String {
    let mut parts: Vec<String> = words::outcome_text(e, scope)
        .map(message_clause)
        .into_iter()
        .collect();
    if !words::outcome_covers_failure(e, scope) {
        parts.push(explanation);
        let radio_named = e.reason == ErrorReason::Radio as i32
            && e.outcome == ErrorOutcome::Complete as i32
            && matches!(scope, Scope::Save | Scope::Pair);
        if !radio_named {
            parts.extend(context_words(e).map(str::to_owned));
        }
    }
    let related_scope = if scope == Scope::Pair
        && matches!(
            ErrorOutcome::try_from(e.outcome),
            Ok(ErrorOutcome::Complete | ErrorOutcome::Unknown | ErrorOutcome::Partial)
        ) {
        Scope::Related
    } else {
        scope.related()
    };
    parts.extend(
        e.related
            .iter()
            .map(|r| format!("also ({})", related(r, related_scope))),
    );
    parts.join("; ")
}

/// Explains an adapter error and its related failures in words alone, without any codes.
pub fn plain_text(e: &p::Error) -> String {
    plain_in(e, Scope::Other)
}

fn plain_in(e: &p::Error, scope: Scope) -> String {
    let explanation = match known_code(e) {
        None | Some(ErrorCode::Unknown) => {
            "the adapter reported an error this version of Cordial doesn't recognize".into()
        }
        Some(code) => reason_words(e).map_or_else(|| code_text(code, None), str::to_owned),
    };
    described(e, explanation, scope, plain_in)
}

/// Why a blocked adapter can't be migrated, in words alone.
pub fn blocked_line(e: &Error) -> String {
    e.dongle
        .as_ref()
        .map_or_else(|| e.message.clone(), plain_text)
}

/// A related failure with its code. It concerns a step other than the command's own, such as a
/// failed pairing's cleanup, so it takes the general wording and its outcome stays its own.
fn related_line(r: &p::Error, scope: Scope) -> String {
    let explanation = reason_words(r).map_or_else(|| code_text(r.code(), None), str::to_owned);
    format!(
        "{}: {}",
        code_token(r),
        described(r, explanation, scope, related_line)
    )
}

/// A protocol enum value as the CLI spells it, or its number when this app doesn't know it.
fn detail_token<E: TryFrom<i32>>(n: i32, name: impl Fn(E) -> &'static str) -> String {
    E::try_from(n).map_or_else(|_| n.to_string(), |v| model::token(name(v)))
}

/// An error's code as the CLI spells it, or its number when this app doesn't know it.
fn code_token(e: &p::Error) -> String {
    known_code(e).map_or_else(|| e.code.to_string(), model::code_token)
}

/// An error as the CLI spells it: its code, then its reason and outcome when given, then its
/// related errors in parentheses. Details this app doesn't know keep their numbers.
pub fn error_token(e: &p::Error) -> String {
    let mut token = code_token(e);
    if e.reason != ErrorReason::Unknown as i32 {
        let _ = write!(
            token,
            ":{}",
            detail_token(e.reason, |r: ErrorReason| r.as_str_name())
        );
    }
    if e.outcome != ErrorOutcome::Unspecified as i32 {
        let _ = write!(
            token,
            ":{}",
            detail_token(e.outcome, |o: ErrorOutcome| o.as_str_name())
        );
    }
    if !e.related.is_empty() {
        let related: Vec<String> = e.related.iter().map(error_token).collect();
        let _ = write!(token, "({})", related.join(";"));
    }
    token
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
        InactiveReason::Capacity => "every enabled-device place is in use".into(),
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

/// Explains an adapter error without its code, with its outcome and related failures.
pub fn wire_text(e: &p::Error, command: Option<&str>) -> String {
    let scope = match command {
        Some("pair start") => Scope::Pair,
        Some(
            "adapter name"
            | "adapter platform"
            | "adapter transport"
            | "adapter interface"
            | "adapter settings"
            | "device set"
            | "device set enabled"
            | "device set trusted"
            | "device set blocked"
            | "device set hidpp"
            | "device set profiles"
            | "device set gamepad-output"
            | "device unpair"
            | "setting set"
            | "setting forget"
            | "profile create"
            | "profile copy"
            | "profile delete"
            | "profile rule"
            | "file put"
            | "file delete",
        ) => Scope::Save,
        Some(
            "adapter status" | "device list" | "device get" | "setting list" | "setting get"
            | "feature list" | "profile list" | "profile show" | "profile rule list" | "file list"
            | "file get" | "scan start",
        ) => Scope::Read,
        _ => Scope::Other,
    };
    let explanation = reason_words(e).map_or_else(|| code_text(e.code(), command), str::to_owned);
    let explanation = if known_code(e) == Some(ErrorCode::StorageFailed)
        && e.reason == ErrorReason::Radio as i32
        && e.outcome == ErrorOutcome::NoEffect as i32
    {
        match scope {
            Scope::Save => {
                "the adapter couldn't save the change. The adapter reported a Bluetooth error"
                    .into()
            }
            Scope::Pair => {
                "the adapter couldn't save the device. The adapter reported a Bluetooth error"
                    .into()
            }
            _ => explanation,
        }
    } else {
        explanation
    };
    described(e, explanation, scope, related_line)
}

/// An adapter error with its code before the explanation, as the shell and
/// device details show it.
pub fn wire_line(e: &p::Error, command: Option<&str>) -> String {
    format!("{}: {}", code_token(e), wire_text(e, command))
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
/// Beside Save, when a save's reply was lost to the USB reconnect it caused and the returned
/// adapter doesn't show what was sent.
pub const SAVE_UNCONFIRMED: &str = "Cordial couldn't confirm whether the adapter saved the change. Check the current settings before trying again.";
/// The activity line for the same case.
pub const SAVE_UNCONFIRMED_LOG: &str = "Couldn't confirm whether the adapter saved its settings";

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
pub fn up_token(up: &Up) -> String {
    match up {
        Up::Off => "off".into(),
        Up::Disconnected => "disconnected".into(),
        Up::Starting => "starting".into(),
        Up::Active => "active".into(),
        Up::Unsupported(e) | Up::Error(e) => match error_token(e) {
            token if token == "unsupported" => token,
            token => format!("error:{token}"),
        },
    }
}

/// HID++'s status for details.
pub fn up_words(up: &Up) -> String {
    match up {
        Up::Off => "Off".into(),
        Up::Disconnected => "Waiting to Connect".into(),
        Up::Starting => "Setting Up".into(),
        Up::Active => "Active".into(),
        Up::Unsupported(e) if model::plain(e) => "Unsupported".into(),
        Up::Unsupported(e) | Up::Error(e) if e.outcome == ErrorOutcome::Complete as i32 => {
            sentence(&hidpp_error_words(e))
        }
        Up::Unsupported(e) | Up::Error(e) => format!("Failed: {}", hidpp_error_words(e)),
    }
}

/// The HID++ version a device reported, as "4.5".
pub fn hidpp_protocol_text(d: &p::Device) -> Option<String> {
    model::hidpp_version(d).map(|(major, minor)| format!("{major}.{minor}"))
}

/// The reported security of a device's current link. Only a connected device has one, so a
/// report left over from an earlier link is never shown.
pub fn link_security(d: &p::Device) -> Option<&p::Security> {
    d.security.as_ref().filter(|_| model::connected(d))
}

/// A short summary of link security: encryption and whether pairing was
/// authenticated, or None while encryption isn't reported. Unauthenticated
/// encryption is a valid link, not a failure, and no combination is called
/// simply secure.
pub fn security_summary(s: &p::Security) -> Option<&'static str> {
    Some(match (s.encrypted?, s.authenticated) {
        (false, _) => "Not Encrypted",
        (true, Some(true)) => "Encrypted, Authenticated",
        (true, Some(false)) => "Encrypted, Unauthenticated",
        (true, None) => "Encrypted",
    })
}

/// One link property: its name and its value in words.
pub struct SecurityFact {
    pub label: &'static str,
    pub value: String,
}

/// The link security properties the device reported. Key length is a number, not a verdict,
/// so no length is marked as approved.
pub fn security_facts(s: &p::Security) -> Vec<SecurityFact> {
    let flag = |b| if b { "Yes" } else { "No" }.to_owned();
    [
        ("Encryption", s.encrypted.map(flag)),
        (
            "Authenticated Pairing (MITM Protection)",
            s.authenticated.map(flag),
        ),
        ("Secure Connections", s.secure_connections.map(flag)),
        (
            "Encryption Key",
            s.key_size.map(|b| format!("{} bits", b * 8)),
        ),
    ]
    .into_iter()
    .filter_map(|(label, value)| {
        Some(SecurityFact {
            label,
            value: value?,
        })
    })
    .collect()
}

/// A device's state as the CLI spells it.
pub fn state_token(d: &p::Device) -> String {
    model::token(d.state().as_str_name())
}

/// Why a connected device's profiles aren't loaded, as sentences for its diagnostics.
pub fn profile_error_words(e: &p::Error) -> String {
    match e.code() {
        ErrorCode::ProfileMemoryExceeded => detailed(
            e,
            "The adapter doesn't have room for them. Disconnect another device or give this one fewer profiles".into(),
        ) + ".",
        _ => sentence(&wire_text(e, None)),
    }
}

/// One saved device on a line for the shell.
pub fn device_line(d: &p::Device) -> String {
    let mut policy = String::from(if d.trusted { "trusted" } else { "untrusted" });
    if d.blocked {
        policy.push_str(" blocked");
    }
    if d.paused {
        policy.push_str(" reconnect-paused");
    }
    let mut line = format!(
        "{}  {}  {}  {}  {}",
        d.id,
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
    if let Some(words) = link_security(d).and_then(security_summary) {
        let words = words.to_lowercase().replace(", ", ",").replace(' ', "-");
        let _ = write!(line, " security={words}");
    }
    if let Some(e) = model::last_error(d) {
        let _ = write!(line, " error={}", error_token(e));
    }
    let _ = write!(
        line,
        " hidpp={} hidpp-state={}",
        on_off(model::hidpp_enabled(d)),
        up_token(&model::hidpp_up(d))
    );
    if let Some((major, minor)) = model::hidpp_version(d) {
        let _ = write!(line, " hidpp-protocol={major}.{minor}");
    }
    if model::gamepad(d) {
        let mode = model::gamepad_mode(d);
        let token = mode.map_or_else(|| "unknown".into(), output_mode_token);
        let _ = write!(line, " gamepad-output={token}");
        if let Some(o) = model::gamepad_output(d).filter(|_| model::gamepad_saved(d)) {
            let _ = write!(line, " gamepad-state={}", output_state_token(o));
            if let Some(slot) = o.slot {
                let _ = write!(line, " gamepad-slot={}", slot_number(slot));
            }
        }
    }
    let layers = profiles::layers(d);
    if !layers.is_empty() {
        let ids: Vec<String> = layers.iter().map(u32::to_string).collect();
        let _ = write!(line, " profiles={}", ids.join(","));
    }
    if let Some(e) = model::profile_error(d) {
        let _ = write!(line, " profile-error={}", error_token(e));
    }
    line
}

pub fn candidate_line(c: &p::Candidate) -> String {
    format!(
        "{}  {}  {}  candidate",
        c.id,
        candidate_name(c),
        transport_token(c.transport())
    )
}

/// `adapter status` for the shell. Device counts need the saved devices, once listed.
pub fn adapter_info(port: &str, st: &p::Status, devices: Option<&[p::Device]>) -> String {
    let mut b = format!("Adapter {}", safe(&st.id));
    let mut field = |key: &str, value: &str| {
        let _ = write!(b, "\n  {key}: {}", safe(value));
    };
    if !port.is_empty() {
        field("Port", port);
    }
    let information = st.information.clone().unwrap_or_default();
    if let Some(v) = &information.firmware_version {
        field("Firmware", v);
    }
    if let Some(v) = &information.board_name {
        field("Board", v);
    }
    if let Some(v) = information.storage_format {
        field("Storage Format", &v.to_string());
    }
    field("Name", &display(&st.name));
    field("Platform", &platform_token(st.platform()));
    for t in model::transports(st) {
        if let Some(on) = model::transport_enabled(st, t) {
            field(transport_long(t), enabled_word(on));
        }
    }
    field("Ready", &yes_no(st.ready).to_lowercase());
    for e in &st.errors {
        field("Error", &wire_line(e, None));
    }
    field(
        "Storage Full",
        &yes_no(model::storage_full(st)).to_lowercase(),
    );
    if let Some(v) = storage_text(st) {
        field("Storage", &v);
    }
    for (_, label) in usb_capacity(st) {
        field("USB", label);
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
    if let Some(devices) = devices {
        field("Saved Devices", &devices.len().to_string());
        field(
            "Connected Devices",
            &devices
                .iter()
                .filter(|d| model::connected(d))
                .count()
                .to_string(),
        );
    }
    for t in model::enabled_transports(st) {
        let Some(r) = model::enabled_devices(st, t) else {
            continue;
        };
        let value = match (r.count, r.limit) {
            (Some(count), Some(limit)) => format!(
                "{count} of {limit} in use, {} free",
                limit.saturating_sub(count)
            ),
            (Some(count), None) => format!("{count} in use"),
            (None, Some(limit)) => format!("at most {limit}"),
            (None, None) => continue,
        };
        field(&format!("Enabled Places ({})", transport_name(t)), &value);
    }
    if let Some(v) =
        model::resource(&st.resources, ResourceType::Interfaces).and_then(capacity_text)
    {
        field("USB Interfaces", &v.to_lowercase());
    }
    for m in model::gamepad_support(st) {
        let Some(r) = model::resource(&m.resources, ResourceType::Slots) else {
            continue;
        };
        let value = match (r.count, r.limit) {
            (Some(count), Some(limit)) => format!("{count} of {limit}"),
            (Some(count), None) => count.to_string(),
            (None, Some(limit)) => format!("at most {limit}"),
            (None, None) => continue,
        };
        field(
            &format!("Gamepad Slots ({})", output_mode_name(m.mode())),
            &value,
        );
    }
    b
}

/// `device get` of a saved device, one field per line after bluetoothctl.
pub fn device_info(d: &p::Device) -> String {
    let mut b = format!("Device {} ({})", d.id, transport_token(d.transport()));
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
    if let Some(s) = link_security(d) {
        // The facts sit under the summary when there is one.
        let summary = security_summary(s);
        if let Some(words) = summary {
            field("Link Security", words);
        }
        let indent = if summary.is_some() { "  " } else { "" };
        for f in security_facts(s) {
            field(&format!("{indent}{}", f.label), &f.value);
        }
    }
    field("Bluetooth", &enablement_words(d));
    field("Trusted", &yes_no(d.trusted).to_lowercase());
    field("Blocked", &yes_no(d.blocked).to_lowercase());
    field("Reconnect", if d.paused { "paused" } else { "automatic" });
    field("Logitech Features", on_off(model::hidpp_enabled(d)));
    if let Some(v) = hidpp_protocol_text(d) {
        field("HID++ Protocol", &v);
    }
    field("HID++ Status", &up_words(&model::hidpp_up(d)));
    if model::gamepad(d) {
        let mode = model::gamepad_mode(d).map_or("Unknown", output_mode_name);
        field("Gamepad Output", mode);
        if let Some(o) = model::gamepad_output(d).filter(|_| model::gamepad_saved(d)) {
            field("Gamepad Status", &safe(&output_state_words(o)));
            if let Some(slot) = o.slot {
                field("Gamepad Slot", &slot_number(slot));
            }
        }
    }
    if let Some(e) = model::last_error(d) {
        field("Last Error", &safe(&wire_line(e, None)));
    }
    if let Some(e) = model::profile_error(d).filter(|_| model::connected(d)) {
        field(
            "Profiles",
            &format!(
                "Not Loaded ({}): {}",
                error_token(e),
                safe(&profile_error_words(e))
            ),
        );
    }
    for r in info_rows(d.information.as_ref()) {
        field(&r.label, &safe(&r.value));
    }
    b
}

/// A reading of device information in words.
pub fn reading_value(leaf: Leaf, v: &Value) -> String {
    match (leaf, v) {
        (Leaf::AutoOff, Value::Integer(0)) => "Never".into(),
        (_, Value::Bool(b)) => yes_no(*b).into(),
        (_, Value::Choice(n)) => catalog::choice_words(leaf, *n),
        (_, Value::Integer(n)) => match catalog::unit(leaf) {
            "" => n.to_string(),
            unit => format!("{n} {unit}"),
        },
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
    /// For device lists: "80%", with a mark only while charging; empty when neither applies.
    pub fn compact(&self, charging: &str) -> String {
        let mut text = self.percent.map(|p| format!("{p}%")).unwrap_or_default();
        if self.charging == Some(true) {
            text.push_str(charging);
        }
        text
    }
}

/// The device's battery, or None when it reports neither charge nor charging.
pub fn battery(info: Option<&p::DeviceInformation>) -> Option<Battery> {
    let battery = info?.battery.as_ref()?;
    let b = Battery {
        percent: battery.level.map(i64::from),
        charging: battery.charging,
    };
    (b.percent.is_some() || b.charging.is_some()).then_some(b)
}

/// A labeled line of device information, its value as reported.
#[derive(Clone, Debug, PartialEq)]
pub struct InfoRow {
    pub label: String,
    pub value: String,
}

/// Identity facts that identify the device rather than describe it, shown with its diagnostics.
pub const IDENTIFIERS: [&str; 5] = [
    "Vendor ID Namespace",
    "Vendor ID",
    "Product ID",
    "Product Version",
    "Bootloader",
];

/// The device's identity in display order, each fact labelled, with text as the device reported
/// it.
pub fn identity(info: Option<&p::DeviceInformation>) -> Vec<(&'static str, String)> {
    let Some(id) = info.and_then(|i| i.identity.as_ref()) else {
        return Vec::new();
    };
    let hex = |n: Option<u32>| n.map(|n| format!("0x{n:04X}"));
    let registry = id.vendor.as_ref().and_then(|v| v.registry).map(|r| {
        match VendorRegistry::try_from(r) {
            Ok(VendorRegistry::Usb) => "USB",
            Ok(VendorRegistry::Bluetooth) => "Bluetooth",
            _ => "Unknown",
        }
        .to_owned()
    });
    [
        ("Manufacturer", id.manufacturer.clone()),
        ("Model", id.model.clone()),
        ("Serial Number", id.serial.clone()),
        ("Firmware", id.firmware_version.clone()),
        ("Bootloader", id.bootloader_version.clone()),
        ("Hardware", id.hardware_revision.clone()),
        ("Software", id.software_revision.clone()),
        ("Vendor ID Namespace", registry),
        ("Vendor ID", hex(id.vendor.as_ref().and_then(|v| v.id))),
        ("Product ID", hex(id.product.as_ref().and_then(|p| p.id))),
        (
            "Product Version",
            hex(id.product.as_ref().and_then(|p| p.version)),
        ),
    ]
    .into_iter()
    .filter_map(|(label, value)| Some((label, value?)))
    .collect()
}

/// The reported information in display order: the battery's charge and charging, the identity,
/// then the readings this build knows. A vendor ID is read with its namespace, on one row.
pub fn info_rows(info: Option<&p::DeviceInformation>) -> Vec<InfoRow> {
    let mut rows: Vec<InfoRow> = Vec::new();
    let mut row = |label: &str, value: String| {
        rows.push(InfoRow {
            label: label.into(),
            value,
        })
    };
    if let Some(b) = battery(info) {
        if let Some(p) = b.percent {
            row("Battery", format!("{p}%"));
        }
        if let Some(c) = b.charging {
            row("Charging", yes_no(c).into());
        }
    }
    let identity = identity(info);
    let registry = identity
        .iter()
        .find(|(label, _)| *label == "Vendor ID Namespace")
        .map(|(_, v)| v.clone());
    let vendor = identity.iter().any(|(label, _)| *label == "Vendor ID");
    for (label, value) in identity {
        match (label, &registry) {
            ("Vendor ID Namespace", _) if vendor => {}
            ("Vendor ID", Some(r)) => row(label, format!("{value} ({r})")),
            _ => row(label, value),
        }
    }
    for (field, v) in info.map(settings::readings).unwrap_or_default() {
        row(&catalog::label(&field), reading_value(field.leaf, &v));
    }
    rows
}

/// A device's battery for `device list`, or None when it reports neither charge nor charging.
pub fn battery_list(d: &p::Device) -> Option<String> {
    Some(battery(d.information.as_ref())?.compact("(charging)")).filter(|b| !b.is_empty())
}

pub fn scan_summary(count: u32, error: Option<&p::Error>) -> String {
    let noun = if count == 1 {
        "candidate"
    } else {
        "candidates"
    };
    let mut text = format!("Discovery finished: {count} {noun}.");
    if let Some(e) = error {
        let _ = write!(text, " Error: {}", safe(&wire_line(e, Some("scan start"))));
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
            // A record the listing couldn't read confirms no connection.
            Filter::Connected => {
                d.state() == DeviceState::Connected && !st.unreadable.contains_key(&d.id)
            }
            Filter::Trusted => d.trusted,
        })
        .map(|d| {
            let mut line = device_line(d);
            if let Some(b) = battery_list(d) {
                let _ = write!(line, " battery={b}");
            }
            // The record is the last one read before the listing failed to read it.
            if let Some(e) = st.unreadable.get(&d.id) {
                let _ = write!(line, " read-error={}", error_token(e));
            }
            line
        })
        .collect();
    if matches!(filter, Filter::All | Filter::Saved) {
        lines.extend(
            st.unreadable
                .iter()
                .filter(|(id, _)| st.device(**id).is_none())
                .map(|(id, e)| format!("{id}  unreadable read-error={}", error_token(e))),
        );
    }
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
        .map(|p| format!("{}  {}", safe(&p.port), safe(p.id())))
        .collect::<Vec<_>>()
        .join("\n")
}

/// `adapter list --json`: one object per adapter.
pub fn ports_json(ports: &[PortInfo]) -> String {
    let list: Vec<serde_json::Value> = ports
        .iter()
        .map(|p| serde_json::json!({"port": p.port, "id": p.id()}))
        .collect();
    terminal_json(&serde_json::Value::Array(list).to_string())
}

/// A resolved device or candidate as a result names it.
pub fn label(subject: &Subject) -> String {
    if subject.name.is_empty() {
        subject.id.to_string()
    } else {
        safe(&subject.name)
    }
}

/// A byte count for people: exact below 1 KiB.
fn bytes(n: u64) -> String {
    match n {
        0..1024 => format!("{n} B"),
        1024..1_048_576 => format!("{:.1} KiB", n as f64 / 1024.0),
        _ => format!("{:.1} MiB", n as f64 / 1_048_576.0),
    }
}

/// Bytes in use for `adapter status`: "1.0 KiB of 4.0 KiB in use", or what is known of it.
fn bytes_in_use(r: &p::ResourceCapacity) -> Option<String> {
    Some(match (r.count, r.limit) {
        (Some(used), Some(total)) => {
            format!("{} of {} in use", bytes(used.into()), bytes(total.into()))
        }
        (Some(used), None) => format!("{} in use", bytes(used.into())),
        (None, Some(total)) => bytes(total.into()),
        (None, None) => return None,
    })
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

/// A command's result for the shell and scripts; empty when the command has
/// nothing to add, as for a stopped scan.
pub fn outcome(command: &Command, outcome: &Outcome, state: Option<&State>) -> String {
    match outcome {
        Outcome::Status(status) => {
            let port = state.map_or("", |s| s.port.as_str());
            let devices = state.filter(|s| s.loaded).map(|s| &s.devices[..]);
            let mut b = adapter_info(port, status, devices);
            b.push_str(&profile_fields(status, state));
            b
        }
        Outcome::Profiles(list) => profiles_text(list),
        Outcome::Profile(profile) => match command {
            Command::ProfileCopy(..) => format!(
                "Copied profile to {} ({}).",
                display(&profile.name),
                profile.id
            ),
            Command::ProfileCreate(..) => format!(
                "Created profile {} ({}).",
                display(&profile.name),
                profile.id
            ),
            _ => profile_text(profile),
        },
        Outcome::ProfileDeleted(profile) => format!("Deleted profile {}.", display(&profile.name)),
        Outcome::Rules { profile, rules } => match command {
            Command::RuleChange(_, change) => rule_saved_text(profile, change, rules),
            _ => rules_text(profile, rules),
        },
        Outcome::Interface(interface, status) => interface_text(*interface, status, state),
        Outcome::AdapterSaved(_) => "Saved the adapter settings.".into(),
        Outcome::Files { entries, .. } => {
            entries.iter().map(file_line).collect::<Vec<_>>().join("\n")
        }
        Outcome::FileUploaded { path, bytes } => {
            format!("Uploaded {} to the adapter ({bytes} bytes).", safe(path))
        }
        Outcome::FileDeleted(path) => format!("Deleted {} from the adapter.", safe(path)),
        Outcome::FileSaved {
            path,
            local,
            bytes,
            json,
            unconverted,
        } => {
            let (path, local) = (safe(path), safe(&local.display().to_string()));
            match unconverted {
                Some(why) => format!(
                    "Saved {path} to {local} ({bytes} bytes) without converting it: {}.",
                    why.reason()
                ),
                None if *json => format!("Saved {path} as JSON to {local} ({bytes} bytes)."),
                None => format!("Saved {path} to {local} ({bytes} bytes)."),
            }
        }
        Outcome::Name { name, reset: true } => {
            format!("Adapter name reset to {}.", display(name))
        }
        Outcome::Name { name, reset: false } => format!("Adapter renamed to {}.", display(name)),
        Outcome::Platform(p) => format!("Platform set to {}.", platform_name(*p)),
        Outcome::Transport(t, on) => format!("{} {}.", transport_long(*t), enabled_word(*on)),
        Outcome::Restart => "The adapter is restarting.".into(),
        Outcome::Bootloader => "The adapter is restarting into its bootloader.".into(),
        Outcome::ScanStarted(transports) => format!(
            "Discovery started ({}).",
            transports
                .iter()
                .map(|t| transport_token(*t))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Outcome::ScanFinished { count, error } => scan_summary(*count, error.as_ref()),
        Outcome::ScanStopped { was_running } => {
            if *was_running {
                String::new()
            } else {
                "Discovery is already off.".into()
            }
        }
        Outcome::PairedWithoutDevice { name } => {
            format!("Paired and saved {}.", self::name(Some(name)))
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
                    let mut b = device_info(device);
                    if let Some(st) = state.filter(|st| profiles::available(&st.status)) {
                        let _ = write!(b, "\n  Profiles: {}", layer_names(st, device));
                    }
                    b
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
                Command::GamepadOutput(_, mode) => format!(
                    "Gamepad Output of {label} set to {}.",
                    output_mode_name(*mode)
                ),
                Command::DeviceSave(..) => format!("Saved changes to {label}."),
                Command::Layers(..) => {
                    let words = match state {
                        Some(st) => layer_names(st, device),
                        None => id_list(profiles::layers(device)),
                    };
                    format!("Profiles of {label} set to {words}.")
                }
                _ => String::new(),
            }
        }
        Outcome::Unpaired(subject) => format!("Forgot {}.", label(subject)),
        Outcome::Refreshing(subject) => {
            format!("Reading current information from {}.", label(subject))
        }
        Outcome::Settings(subject) => catalog::subject_text(subject, state),
        Outcome::Setting { subject, setting } => match state.and_then(|s| s.device(subject.id)) {
            Some(d) => catalog::setting_detail(d, setting),
            None => String::new(),
        },
        Outcome::Saved {
            subject,
            set,
            forget,
            settings,
        } => catalog::saved_text(subject, set, forget, settings, state),
        Outcome::Features { subject, features } => match state.and_then(|s| s.device(subject.id)) {
            Some(d) => catalog::feature_list(d, features),
            None => String::new(),
        },
    }
}

/// Profile IDs separated by commas, or `none`.
fn id_list(ids: &[u32]) -> String {
    if ids.is_empty() {
        return "none".into();
    }
    ids.iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

/// A device's layers for the shell: each profile's name and ID in order, or `none`.
pub fn layer_names(st: &State, d: &p::Device) -> String {
    let layers = profiles::layers(d);
    if layers.is_empty() {
        return "none".into();
    }
    layers
        .iter()
        .map(|id| match st.profile(*id) {
            Some(profile) => format!("{} ({id})", safe(&profile.name)),
            None => id.to_string(),
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// A profile ID with its name when known, as `adapter status` names an interface's profile.
fn profile_ref(state: Option<&State>, id: u32) -> String {
    match state.and_then(|st| st.profile(id)) {
        _ if id == 0 => "none".into(),
        Some(profile) => format!("{} ({id})", safe(&profile.name)),
        None => id.to_string(),
    }
}

/// The adapter's profile support and configuration interfaces, as `adapter status` adds them.
fn profile_fields(status: &p::Status, state: Option<&State>) -> String {
    if !profiles::available(status) {
        return String::new();
    }
    let mut b = String::new();
    if let Some(v) = profiles::resource(status, ResourceType::Memory).and_then(bytes_in_use) {
        let _ = write!(b, "\n  Profile Memory: {v}");
    }
    if let Some(max) = profiles::max_layers(status) {
        let _ = write!(b, "\n  Profiles Per Device: {max}");
    }
    for s in profiles::interfaces(status) {
        let _ = write!(
            b,
            "\n  {}: {}, profile {}",
            profiles::interface_label(s.interface()),
            interface_words(s),
            profile_ref(state, s.profile)
        );
    }
    b
}

/// An interface's saved preference, and whether an enabled one isn't exposed over USB.
fn interface_words(s: &p::ConfigurationInterfaceSupport) -> &'static str {
    match (s.enabled, s.active) {
        (true, false) => "on (inactive)",
        (on, _) => on_off(on),
    }
}

/// A configuration interface's saved preferences after a change. Whether the adapter exposes it
/// follows from the save, so it isn't known yet.
fn interface_text(
    interface: p::ConfigurationInterface,
    status: &p::Status,
    state: Option<&State>,
) -> String {
    let label = profiles::interface_label(interface);
    match profiles::interface(status, interface) {
        Some(s) => format!(
            "{label} {}, profile {}.",
            on_off(s.enabled),
            profile_ref(state, s.profile)
        ),
        None => format!("Saved {label}."),
    }
}

/// A profile's roles as the shell lists them: lowercase tokens, or `none`.
fn role_tokens(profile: &p::Profile) -> String {
    let roles: Vec<String> = profiles::roles(profile)
        .iter()
        .map(|r| model::token(r.as_str_name()))
        .collect();
    if roles.is_empty() {
        "none".into()
    } else {
        roles.join(",")
    }
}

/// One profile as `profile list` shows it: ID, roles and name, tab-separated.
pub fn profile_line(profile: &p::Profile) -> String {
    format!(
        "{}\t{}\t{}",
        profile.id,
        role_tokens(profile),
        display(&profile.name)
    )
}

/// `profile show`: a profile's name, ID and roles.
fn profile_text(profile: &p::Profile) -> String {
    let roles = role_names(&profiles::roles(profile));
    format!(
        "Profile {}\n  ID: {}\n  Roles: {}",
        display(&profile.name),
        profile.id,
        if roles.is_empty() { "none" } else { &roles }
    )
}

/// `profile list`: profiles and unreadable ones, with why, in ID order.
fn profiles_text(list: &p::ProfileList) -> String {
    let mut b = "Profiles".to_owned();
    if list.entries.is_empty() {
        b.push_str("\n  No profiles.");
    }
    for entry in &list.entries {
        match &entry.entry {
            Some(profile_list_entry::Entry::Profile(profile)) => {
                let _ = write!(b, "\n  {}", profile_line(profile));
            }
            Some(profile_list_entry::Entry::Error(r)) => {
                let _ = write!(b, "\n  {}\tunreadable", r.id);
                if let Some(e) = &r.error {
                    let _ = write!(b, ": {}", safe(&wire_line(e, Some("profile list"))));
                }
            }
            None => {}
        }
    }
    b
}

/// `profile rule list`: every rule of a profile, in the form the commands take.
fn rules_text(profile: &p::Profile, rules: &[p::ProfileRule]) -> String {
    let mut b = format!("Rules of {}", display(&profile.name));
    if rules.is_empty() {
        b.push_str("\n  No rules.");
    }
    for rule in rules {
        let _ = write!(b, "\n  {}", profiles::rule_words(rule));
    }
    b
}

/// The result of saving or forgetting a rule: the input's rule as saved, or that it has none.
fn rule_saved_text(
    profile: &p::Profile,
    change: &p::ProfileRuleChange,
    rules: &[p::ProfileRule],
) -> String {
    let at = profiles::change_target(change);
    let name = display(&profile.name);
    let saved = rules.iter().find(|r| Some(profiles::rule_ref(r)) == at);
    match (saved, at) {
        (Some(rule), _) => format!("Saved in {name}: {}", profiles::rule_words(rule)),
        (None, Some(at)) => {
            format!("{name} has no rule for {}.", profiles::input_words(&at))
        }
        (None, None) => String::new(),
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
/// prefix where the shell uses one. A save cut off by the connection's end may have been stored.
pub fn failure_text(command: &Command, error: &Error) -> String {
    if matches!(
        command,
        Command::ProfileDelete(_) | Command::ProfileDeletePlanned(_)
    ) && error.unconfirmed()
    {
        return deletion_uncertain(error);
    }
    if command.saves() && error.unconfirmed() {
        return "the adapter couldn't confirm whether the change was saved".into();
    }
    safe(&error_line(error))
}

/// Names the unconfirmed deletion step and whether earlier changes completed.
pub(crate) fn deletion_uncertain(error: &Error) -> String {
    let (_, incomplete) = crate::commands::deletion_failure(error);
    let result = match error.command {
        Some("device set profiles") => {
            "the adapter couldn't confirm whether the device's profiles were updated. Profile deletion did not proceed"
        }
        Some("adapter interface") => {
            "the adapter couldn't confirm whether its configuration interfaces were updated. Profile deletion did not proceed"
        }
        Some("profile delete") => "the adapter couldn't confirm whether the profile was deleted",
        _ => {
            "the adapter couldn't finish preparing the profile deletion. Profile deletion did not proceed"
        }
    };
    let completed = if incomplete {
        ". Earlier changes were saved"
    } else {
        ""
    };
    format!("{result}{completed}. Check the current settings before trying again")
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
    changed: &[Field],
    state: Option<&State>,
) -> Option<String> {
    match kind {
        event::Kind::SessionEnding(_) => None,
        event::Kind::ScanFound(c) if first => Some(format!("[NEW] {}", candidate_line(c))),
        event::Kind::ScanFound(_) => None,
        event::Kind::ScanDone(done) => Some(scan_summary(done.count, done.error.as_ref())),
        event::Kind::Device(d) if first => Some(format!("[NEW] {}", device_line(d))),
        event::Kind::Device(d) => Some(format!("[CHG] {}", device_line(d))),
        event::Kind::DeviceRemoved(r) => Some(format!("[DEL] {}", r.id)),
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
            for s in profiles::interfaces(a) {
                let _ = write!(
                    line,
                    " {}={},{}",
                    profiles::interface_word(s.interface()),
                    on_off(s.enabled),
                    s.profile
                );
                if s.enabled && !s.active {
                    line.push_str(",inactive");
                }
            }
            let errors: Vec<String> = a.errors.iter().map(error_token).collect();
            if !errors.is_empty() {
                let _ = write!(line, " errors={}", errors.join(","));
            }
            Some(line)
        }
        event::Kind::SettingsChanged(s) => {
            let d = state?.device(s.device)?;
            let all = settings::settings(&s.changed);
            let lines: Vec<String> = catalog::presented(&all)
                .into_iter()
                .filter(|setting| changed.contains(&setting.field))
                .map(|setting| format!("[CHG] {}{}", s.device, catalog::setting_line(d, setting)))
                .collect();
            (!lines.is_empty()).then(|| lines.join("\n"))
        }
        event::Kind::Pairing(pairing) => {
            let prompt = model::prompt(pairing)?;
            Some(prompt_line(pairing.candidate, &prompt))
        }
        event::Kind::Profile(profile) => Some(format!("[CHG] Profile {}", profile_line(profile))),
        event::Kind::ProfileRemoved(r) => Some(format!("[DEL] Profile {}", r.id)),
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
pub fn prompt_line(candidate: u32, prompt: &Prompt) -> String {
    let place = match prompt {
        Prompt::ShowCode(..) => "Enter on the peripheral",
        Prompt::ConfirmCode(_) => "Compare with the peripheral; answer yes or no",
        Prompt::EnterCode(_) => "Enter on this computer",
    };
    let mut what = prompt_token(prompt).to_owned();
    if let Some(v) = prompt_value(prompt).filter(|v| !v.is_empty()) {
        let _ = write!(what, " {}", safe(v));
    }
    format!("[pair {candidate}] {place}: {what}")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An error with only a code.
    fn err(code: ErrorCode) -> p::Error {
        p::Error {
            code: code as i32,
            ..Default::default()
        }
    }

    #[test]
    fn gamepad_output_reads_its_mode_state_and_slot() {
        let keyboard = p::Device {
            id: 1,
            kinds: vec![Kind::Keyboard as i32],
            ..Default::default()
        };
        assert!(!device_line(&keyboard).contains("gamepad"));
        assert!(!device_info(&keyboard).contains("Gamepad"));
        let mut pad = p::Device {
            id: 2,
            roles: vec![Role::Gamepad as i32],
            outputs: vec![p::OutputStatus {
                role: Role::Gamepad as i32,
                mode: OutputMode::Disabled as i32,
                status: Some(output_status::Status::State(OutputState::Disabled as i32)),
                slot: None,
            }],
            ..Default::default()
        };
        assert_eq!(model::gamepad_mode(&pad), Some(OutputMode::Disabled));
        assert!(device_line(&pad).ends_with(" gamepad-output=disabled"));
        assert!(device_info(&pad).contains("\n  Gamepad Output: Disabled"));
        assert!(!device_info(&pad).contains("Gamepad Status"));
        pad.output_preferences = vec![p::OutputPreference {
            role: Role::Gamepad as i32,
            mode: OutputMode::Xinput as i32,
        }];
        pad.outputs[0] = p::OutputStatus {
            role: Role::Gamepad as i32,
            mode: OutputMode::Xinput as i32,
            status: Some(output_status::Status::State(OutputState::Active as i32)),
            slot: Some(1),
        };
        assert!(
            device_line(&pad)
                .ends_with(" gamepad-output=xinput gamepad-state=active gamepad-slot=2")
        );
        let info = device_info(&pad);
        assert!(
            info.contains(
                "\n  Gamepad Output: XInput\n  Gamepad Status: Active\n  Gamepad Slot: 2"
            )
        );
        // A mode this client doesn't know is neither Disabled nor hidden, even on a keyboard.
        let mut unknown = keyboard.clone();
        unknown.output_preferences = vec![p::OutputPreference {
            role: Role::Gamepad as i32,
            mode: 7,
        }];
        unknown.outputs = vec![p::OutputStatus {
            role: Role::Gamepad as i32,
            mode: 7,
            status: Some(output_status::Status::State(OutputState::Pending as i32)),
            slot: None,
        }];
        assert_eq!(model::gamepad_mode(&unknown), None);
        assert!(device_line(&unknown).ends_with(" gamepad-output=unknown gamepad-state=pending"));
        assert!(
            device_info(&unknown)
                .contains("\n  Gamepad Output: Unknown\n  Gamepad Status: Setting Up")
        );

        let mut status = p::Status::default();
        assert!(!adapter_info("", &status, None).contains("Gamepad"));
        let slots = |count, limit| {
            vec![p::ResourceCapacity {
                r#type: ResourceType::Slots as i32,
                count,
                limit,
            }]
        };
        status.output_support = vec![p::OutputSupport {
            role: Role::Gamepad as i32,
            modes: vec![
                p::OutputModeSupport {
                    mode: OutputMode::Disabled as i32,
                    resources: slots(Some(0), Some(0)),
                },
                p::OutputModeSupport {
                    mode: OutputMode::Hid as i32,
                    resources: slots(Some(2), Some(4)),
                },
                p::OutputModeSupport {
                    mode: OutputMode::Xinput as i32,
                    resources: slots(None, Some(4)),
                },
            ],
        }];
        let info = adapter_info("", &status, None);
        assert!(
            info.ends_with("\n  Gamepad Slots (HID): 2 of 4\n  Gamepad Slots (XInput): at most 4"),
            "{info}"
        );
    }

    #[test]
    fn readings_read_never_at_zero_and_unknown_enums_as_unknown() {
        assert_eq!(reading_value(Leaf::AutoOff, &Value::Integer(0)), "Never");
        assert_eq!(reading_value(Leaf::AutoOff, &Value::Integer(600)), "600 s");
        assert_eq!(reading_value(Leaf::WheelMode, &Value::Choice(0)), "Unknown");
        assert_eq!(
            reading_value(Leaf::WheelMode, &Value::Choice(77)),
            "Unknown"
        );
        assert_eq!(reading_value(Leaf::WheelInvert, &Value::Bool(false)), "No");
    }

    #[test]
    fn information_rows_follow_the_typed_records() {
        let info = p::DeviceInformation {
            identity: Some(p::DeviceIdentity {
                manufacturer: Some("Logi".into()),
                vendor: Some(p::VendorIdentity {
                    registry: Some(VendorRegistry::Bluetooth as i32),
                    id: Some(0x046d),
                }),
                product: Some(p::ProductIdentity {
                    id: Some(0xb023),
                    version: None,
                }),
                ..Default::default()
            }),
            battery: Some(p::BatteryInformation {
                level: Some(0),
                charging: None,
            }),
            pointer_sensors: vec![
                p::PointerSensorInformation {
                    index: 0,
                    dpi: Some(1000),
                },
                p::PointerSensorInformation {
                    index: 1,
                    dpi: None,
                },
            ],
            wheel: Some(p::WheelInformation {
                diameter: Some(24),
                free_spin_threshold: Some(255),
                ..Default::default()
            }),
            ..Default::default()
        };
        let rows: Vec<(String, String)> = info_rows(Some(&info))
            .into_iter()
            .map(|r| (r.label, r.value))
            .collect();
        let expected = [
            ("Battery", "0%"),
            ("Manufacturer", "Logi"),
            ("Vendor ID", "0x046D (Bluetooth)"),
            ("Product ID", "0xB023"),
            ("Pointer Sensitivity", "1000 DPI"),
            ("Automatic Free Spin Threshold", "255"),
            ("Wheel Diameter", "24 mm"),
        ];
        assert_eq!(rows, expected.map(|(l, v)| (l.to_owned(), v.to_owned())));
        assert!(info_rows(None).is_empty());
    }

    #[test]
    fn errors_show_every_detail() {
        let e = p::Error {
            code: ErrorCode::Timeout as i32,
            reason: ErrorReason::Connections as i32,
            outcome: ErrorOutcome::Unknown as i32,
            related: vec![p::Error {
                code: ErrorCode::StorageFailed as i32,
                reason: ErrorReason::Storage as i32,
                ..Default::default()
            }],
        };
        assert_eq!(
            error_token(&e),
            "timeout:connections:unknown(storage_failed:storage)"
        );
        assert_eq!(
            wire_line(&e, None),
            "timeout: the operation's outcome is unknown; this device didn't respond in time; the adapter reported an error with its connections; also (storage_failed: the adapter couldn't read or write its saved data)"
        );
        // A related failure's outcome stays with it.
        let pairing = p::Error {
            code: ErrorCode::AuthFailed as i32,
            outcome: ErrorOutcome::NoEffect as i32,
            related: vec![p::Error {
                code: ErrorCode::Internal as i32,
                reason: ErrorReason::Radio as i32,
                outcome: ErrorOutcome::Unknown as i32,
                ..Default::default()
            }],
            ..Default::default()
        };
        assert_eq!(
            wire_line(&pairing, Some("pair start")),
            "auth_failed: Bluetooth authentication with this device failed; also (internal: pairing failed; the adapter couldn't confirm whether it removed the incomplete pairing; the adapter reported a Bluetooth error)"
        );
        // A related failure's outcome doesn't become the command's own.
        let cleanup = p::Error {
            code: ErrorCode::AuthFailed as i32,
            outcome: ErrorOutcome::NoEffect as i32,
            related: vec![p::Error {
                code: ErrorCode::StorageFailed as i32,
                outcome: ErrorOutcome::Unknown as i32,
                ..Default::default()
            }],
            ..Default::default()
        };
        assert_eq!(
            wire_line(&cleanup, Some("pair start")),
            "auth_failed: Bluetooth authentication with this device failed; also (storage_failed: pairing failed; the adapter couldn't confirm whether it removed the incomplete pairing; the adapter couldn't read or write its saved data)"
        );
        // A reason gives context without replacing what the code means.
        let starting = p::Error {
            code: ErrorCode::NotReady as i32,
            reason: ErrorReason::Radio as i32,
            ..Default::default()
        };
        assert_eq!(
            wire_line(&starting, None),
            "not_ready: the adapter's Bluetooth radio isn't ready"
        );
        let usb = p::Error {
            code: ErrorCode::UsbRequiredCapacityExceeded as i32,
            reason: ErrorReason::OutputBackend as i32,
            ..Default::default()
        };
        assert_eq!(
            wire_line(&usb, None),
            "usb_required_capacity_exceeded: the adapter doesn't have enough USB capacity for every required interface"
        );
        // An unknown save is reported as unknown, not as failed.
        let unknown = p::Error {
            code: ErrorCode::StorageFailed as i32,
            outcome: ErrorOutcome::Unknown as i32,
            ..Default::default()
        };
        assert_eq!(
            wire_text(&unknown, Some("setting set")),
            "the adapter couldn't confirm whether the change was saved; check the current settings before trying again"
        );
        assert_eq!(
            wire_text(&unknown, Some("pair start")),
            "the adapter couldn't confirm whether it saved the device; check its saved devices before pairing again; the adapter couldn't read or write its saved data"
        );
        // Details this build doesn't know keep their numbers, and their explanation adds nothing.
        let newer = p::Error {
            code: 99,
            reason: 98,
            outcome: 97,
            related: vec![p::Error {
                code: ErrorCode::Timeout as i32,
                reason: 96,
                ..Default::default()
            }],
        };
        assert_eq!(error_token(&newer), "99:98:97(timeout:96)");
        assert!(
            wire_line(&newer, None).starts_with("99: the adapter reported error"),
            "{}",
            wire_line(&newer, None)
        );
    }

    #[test]
    fn confirmed_saves_and_pairings_keep_their_codes_and_radio_failure() {
        let radio = p::Error {
            code: ErrorCode::StorageFailed as i32,
            reason: ErrorReason::Radio as i32,
            outcome: ErrorOutcome::Complete as i32,
            ..Default::default()
        };
        assert_eq!(error_token(&radio), "storage_failed:radio:complete");
        assert_eq!(
            wire_line(&radio, Some("device set enabled")),
            "storage_failed: the change was saved, but the adapter reported a Bluetooth error"
        );
        assert_eq!(
            wire_line(&radio, Some("pair start")),
            "storage_failed: the device was paired, but the adapter reported a Bluetooth error"
        );
        let applied = p::Error {
            code: ErrorCode::Internal as i32,
            reason: ErrorReason::Unknown as i32,
            ..radio.clone()
        };
        assert_eq!(
            hidpp_error_words(&applied),
            "the change took effect, but the adapter reported an error"
        );
        assert_eq!(
            up_words(&Up::Error(applied.clone())),
            "The change took effect, but the adapter reported an error."
        );
        assert_eq!(
            output_state_words(&p::OutputStatus {
                status: Some(output_status::Status::Error(applied.clone())),
                ..Default::default()
            }),
            "The change took effect, but the adapter reported an error."
        );
        let related = p::Error {
            related: vec![p::Error {
                code: ErrorCode::StorageFull as i32,
                outcome: ErrorOutcome::NoEffect as i32,
                ..Default::default()
            }],
            ..radio
        };
        assert_eq!(
            error_token(&related),
            "storage_failed:radio:complete(storage_full:no_effect)"
        );
        assert_eq!(
            wire_line(&related, Some("device set enabled")),
            "storage_failed: the change was saved, but the adapter reported a Bluetooth error; also (storage_full: the adapter's storage is full)"
        );
        let paired = p::Error {
            related: vec![p::Error {
                code: ErrorCode::StorageFailed as i32,
                outcome: ErrorOutcome::Unknown as i32,
                ..Default::default()
            }],
            ..related
        };
        assert_eq!(
            wire_line(&paired, Some("pair start")),
            "storage_failed: the device was paired, but the adapter reported a Bluetooth error; also (storage_failed: a related operation's outcome is unknown; the adapter couldn't read or write its saved data)"
        );
        let concrete = p::Error {
            code: ErrorCode::Timeout as i32,
            ..applied
        };
        assert_eq!(
            wire_text(&concrete, Some("setting set")),
            "the change was saved, but the adapter reported an error; this device didn't respond in time"
        );
    }

    #[test]
    fn device_get_reports_read_outcomes() {
        for (outcome, expected) in [
            (
                ErrorOutcome::Unknown,
                "the adapter couldn't confirm whether the results are complete",
            ),
            (
                ErrorOutcome::Partial,
                "the adapter returned only some of the results",
            ),
        ] {
            let error = p::Error {
                code: ErrorCode::StorageFailed as i32,
                outcome: outcome as i32,
                ..Default::default()
            };
            assert!(wire_line(&error, Some("device get")).contains(expected));
        }
    }

    #[test]
    fn partial_saves_and_unconfirmed_reads_keep_distinct_outcomes() {
        let partial = p::Error {
            code: ErrorCode::StorageFailed as i32,
            reason: ErrorReason::Storage as i32,
            outcome: ErrorOutcome::Partial as i32,
            ..Default::default()
        };
        assert_eq!(
            wire_text(&partial, Some("setting set")),
            "the adapter confirmed only part of the change; check the current settings before trying again"
        );
        assert_eq!(
            wire_text(&partial, Some("setting list")),
            "the adapter returned only some of the results; the adapter couldn't read or write its saved data"
        );
        let unknown = p::Error {
            outcome: ErrorOutcome::Unknown as i32,
            ..partial
        };
        assert_eq!(
            wire_text(&unknown, Some("setting set")),
            "the adapter couldn't confirm whether the change was saved; check the current settings before trying again"
        );
        assert_eq!(
            wire_text(&unknown, Some("setting list")),
            "the adapter couldn't confirm whether the results are complete; the adapter couldn't read or write its saved data"
        );
        assert_eq!(
            wire_text(&unknown, None),
            "the operation's outcome is unknown; the adapter couldn't read or write its saved data"
        );
        let refused = p::Error {
            reason: ErrorReason::Radio as i32,
            outcome: ErrorOutcome::NoEffect as i32,
            ..unknown
        };
        assert_eq!(
            wire_text(&refused, Some("setting set")),
            "the adapter couldn't save the change. The adapter reported a Bluetooth error"
        );
        assert_eq!(
            wire_text(&refused, Some("pair start")),
            "the adapter couldn't save the device. The adapter reported a Bluetooth error"
        );
        let future = p::Error {
            code: 99,
            reason: 98,
            outcome: ErrorOutcome::Complete as i32,
            ..Default::default()
        };
        assert_eq!(error_token(&future), "99:98:complete");
        assert!(
            wire_line(&future, Some("setting set"))
                .starts_with("99: the change was saved, but the adapter reported an error;")
        );
    }

    #[test]
    fn a_device_the_listing_couldnt_read_keeps_its_last_record() {
        let mut st = State::default();
        st.devices.push(p::Device {
            id: 3,
            name: "Desk".into(),
            ..Default::default()
        });
        let read = p::Error {
            code: ErrorCode::StorageFailed as i32,
            reason: ErrorReason::Storage as i32,
            outcome: ErrorOutcome::NoEffect as i32,
            related: Vec::new(),
        };
        st.unreadable.insert(3, read.clone());
        st.unreadable.insert(5, read);
        let list = devices(&st, Filter::Saved);
        assert!(
            list.contains(" read-error=storage_failed:storage:no_effect"),
            "{list}"
        );
        assert!(
            list.contains("\n5  unreadable read-error=storage_failed:storage:no_effect"),
            "{list}"
        );
        assert_eq!(list.matches("unreadable").count(), 1, "{list}");
    }

    #[test]
    fn unsupported_outputs_and_integrations_keep_their_labels() {
        let output = |error: p::Error| p::OutputStatus {
            role: Role::Gamepad as i32,
            mode: OutputMode::Hid as i32,
            status: Some(output_status::Status::Error(error)),
            slot: None,
        };
        let layout = p::Error {
            code: ErrorCode::Unsupported as i32,
            reason: ErrorReason::InputLayout as i32,
            ..Default::default()
        };
        assert_eq!(output_state_words(&output(layout.clone())), "Unsupported");
        assert_eq!(
            output_state_token(&output(layout)),
            "error:unsupported:input_layout"
        );
        assert_eq!(
            output_state_words(&output(err(ErrorCode::Timeout))),
            "Failed: this device didn't respond in time"
        );
        assert_eq!(output_state_token(&p::OutputStatus::default()), "unknown");
        assert_eq!(
            up_token(&Up::Unsupported(err(ErrorCode::Unsupported))),
            "unsupported"
        );
        assert_eq!(
            up_token(&Up::Unsupported(err(ErrorCode::ProtocolUnsupported))),
            "error:protocol_unsupported"
        );
        assert_eq!(
            up_words(&Up::Unsupported(err(ErrorCode::ProtocolUnsupported))),
            "Unsupported"
        );
    }

    #[test]
    fn device_errors_split_into_connection_and_profile_errors() {
        let mut d = p::Device {
            errors: vec![
                err(ErrorCode::Timeout),
                err(ErrorCode::ProfileMemoryExceeded),
            ],
            ..Default::default()
        };
        assert_eq!(model::last_error(&d), Some(&err(ErrorCode::Timeout)));
        assert_eq!(
            model::profile_error(&d),
            Some(&err(ErrorCode::ProfileMemoryExceeded))
        );
        assert!(device_line(&d).contains(" error=timeout"));
        assert!(device_line(&d).contains(" profile-error=profile_memory_exceeded"));
        d.state = DeviceState::Connected as i32;
        assert!(device_info(&d).contains(
            "Profiles: Not Loaded (profile_memory_exceeded): The adapter doesn't have room for them."
        ));
        d.errors = vec![err(ErrorCode::ProfileLoadFailed)];
        assert_eq!(model::last_error(&d), None);
        assert!(
            device_info(&d).contains("The adapter couldn't read one of this device's profiles.")
        );
    }

    #[test]
    fn adapter_status_shows_errors_and_resources_independently() {
        let mut status = p::Status {
            information: Some(p::AdapterInformation {
                firmware_version: Some("1.0".into()),
                board_name: None,
                development_build: Some(false),
                storage_format: Some(0),
                target_storage_format: Some(2),
            }),
            transports: vec![p::TransportSupport {
                transport: Transport::Ble as i32,
                enabled: Some(true),
                resources: vec![p::ResourceCapacity {
                    r#type: ResourceType::Devices as i32,
                    count: Some(0),
                    limit: None,
                }],
            }],
            resources: vec![p::ResourceCapacity {
                r#type: ResourceType::Interfaces as i32,
                count: None,
                limit: Some(8),
            }],
            ..Default::default()
        };
        let info = adapter_info("", &status, None);
        assert!(info.contains("\n  Firmware: 1.0\n"), "{info}");
        assert!(!info.contains("Board"));
        assert!(info.contains("\n  Storage Full: no"));
        assert!(!info.contains("USB:"));
        // A zero count is a count, not unknown.
        assert!(
            info.contains("\n  Enabled Places (BLE): 0 in use"),
            "{info}"
        );
        assert!(info.contains("\n  USB Interfaces: up to 8"), "{info}");
        status.errors = vec![
            err(ErrorCode::StorageFull),
            err(ErrorCode::UsbRequiredCapacityExceeded),
            err(ErrorCode::UsbReservedCapacityExceeded),
        ];
        let info = adapter_info("", &status, None);
        assert!(info.contains("\n  Storage Full: yes"));
        assert!(
            info.contains("\n  Error: storage_full: the adapter's storage is full\n"),
            "{info}"
        );
        assert!(
            info.contains("\n  USB: Required Capacity Exceeded\n  USB: Reserved Capacity Exceeded")
        );
        status.errors = vec![err(ErrorCode::UsbReservedCapacityExceeded)];
        let info = adapter_info("", &status, None);
        assert!(!info.contains("Required") && info.contains("Reserved Capacity Exceeded"));
        let line = event_line(&event::Kind::Adapter(status), false, &[], None).unwrap();
        assert!(
            line.ends_with(" errors=usb_reserved_capacity_exceeded"),
            "{line}"
        );
    }

    #[test]
    fn adapter_status_shows_what_is_known_of_storage() {
        let storage = |count, limit| p::ResourceCapacity {
            r#type: ResourceType::Storage as i32,
            count,
            limit,
        };
        let mut st = State::default();
        let info = adapter_info("", &st.status, None);
        assert!(!info.contains("Storage Format") && !info.contains("Storage:"));
        assert!(
            !info.contains("Port") && !info.contains("Devices:"),
            "{info}"
        );
        st.event(&event::Kind::Adapter(p::Status {
            information: Some(p::AdapterInformation {
                storage_format: Some(0),
                ..Default::default()
            }),
            resources: vec![storage(Some(0), Some(0))],
            ..Default::default()
        }));
        st.port = "simulated".into();
        let status = Outcome::Status(st.status.clone());
        let info = outcome(&Command::Status, &status, Some(&st));
        // Zero is known, and saved devices are counted only once listed.
        assert!(info.contains("\n  Port: simulated\n"), "{info}");
        assert!(info.contains("\n  Storage Format: 0\n"), "{info}");
        assert!(info.contains("\n  Storage: 0 B of 0 B\n"), "{info}");
        assert!(!info.contains("Saved Devices") && !info.contains("Connected Devices"));
        st.loaded = true;
        let info = outcome(&Command::Status, &status, Some(&st));
        assert!(
            info.contains("\n  Saved Devices: 0\n  Connected Devices: 0"),
            "{info}"
        );
        // Each value is shown without the other.
        st.event(&event::Kind::Adapter(p::Status {
            resources: vec![storage(Some(12_288), None)],
            ..Default::default()
        }));
        let info = adapter_info("", &st.status, None);
        assert!(info.contains("\n  Storage: 12.0 KiB\n"), "{info}");
        assert!(!info.contains("Storage Format"));
        st.status.resources = vec![storage(None, Some(1_048_576))];
        let info = adapter_info("", &st.status, None);
        assert!(info.contains("\n  Storage: Up to 1.0 MiB\n"), "{info}");
        st.status.resources = vec![storage(None, None)];
        assert!(!adapter_info("", &st.status, None).contains("Storage:"));
    }

    #[test]
    fn unknown_device_facts_are_left_out() {
        let mut d = p::Device {
            id: 1,
            state: DeviceState::Connected as i32,
            integrations: vec![p::Integration {
                kind: p::IntegrationKind::Hidpp as i32,
                enabled: true,
                status: Some(p::integration::Status::State(
                    p::IntegrationState::Starting as i32,
                )),
                ..Default::default()
            }],
            information: Some(p::DeviceInformation {
                battery: Some(p::BatteryInformation {
                    level: None,
                    charging: Some(false),
                }),
                ..Default::default()
            }),
            ..Default::default()
        };
        let info = device_info(&d);
        assert!(!info.contains("HID++ Protocol"), "{info}");
        assert!(info.contains("\n  HID++ Status: Setting Up"), "{info}");
        assert!(
            !info.contains("Link Security") && !info.contains("Battery"),
            "{info}"
        );
        assert!(info.contains("\n  Charging: No"), "{info}");
        let line = device_line(&d);
        assert!(!line.contains("security=") && !line.contains("hidpp-protocol"));
        assert_eq!(battery_list(&d), None);
        d.information.as_mut().unwrap().battery = Some(p::BatteryInformation {
            level: None,
            charging: Some(true),
        });
        assert_eq!(battery_list(&d).as_deref(), Some("(charging)"));
        d.security = Some(p::Security {
            encrypted: Some(false),
            secure_connections: Some(false),
            ..Default::default()
        });
        let info = device_info(&d);
        assert!(
            info.contains(
                "\n  Link Security: Not Encrypted\n    Encryption: No\n    Secure Connections: No\n"
            ),
            "{info}"
        );
        assert!(!info.contains("Not Reported"), "{info}");
        assert!(device_line(&d).contains(" security=not-encrypted"));
        // Unreported encryption has no summary, but the other reported facts stay.
        d.security = Some(p::Security {
            authenticated: Some(false),
            key_size: Some(16),
            ..Default::default()
        });
        let info = device_info(&d);
        assert!(
            info.contains(
                "\n  Authenticated Pairing (MITM Protection): No\n  Encryption Key: 128 bits\n"
            ),
            "{info}"
        );
        assert!(
            !info.contains("Link Security") && !info.contains("Encryption:"),
            "{info}"
        );
        assert!(!info.contains("Not Reported"), "{info}");
        assert!(!device_line(&d).contains("security="));
        d.security = Some(p::Security::default());
        assert!(!device_info(&d).contains("Link Security"));
        assert!(!device_line(&d).contains("security="));
        // A disconnected device's security belongs to an earlier link.
        d.state = DeviceState::Disconnected as i32;
        assert!(!device_info(&d).contains("Link Security"));
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
