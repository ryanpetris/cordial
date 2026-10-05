//! Drawing: the header, device list, details, activity log, dialogs and
//! menus, painted into a cell buffer with the highlighted control reversed.
use super::{
    Action, Area, Dialog, MIN_HEIGHT, MIN_WIDTH, Menu, Model, can_set_platform, code_kind,
    connected,
    layout::{
        self, Choice, Hit, Layout, Styled, Tone, accent, beside, bold, dim, err, inherit, join,
        line_width, ok, pad, pad_str, span, spread, strip, styled, title, truncate, warn,
    },
    pair_blocked, pending_for,
    profiles::layered,
    scan_choices, scan_name, scanning_name,
    settings::settings_busy,
};
use crate::view::Item;
use crate::{
    controller::{DeviceUpdate, State},
    model::{self, Prompt, Up},
    profiles,
    ui::{
        Backend,
        text::{
            self, display, display_candidate_name, display_name, platform_name, role_names,
            transport_long, transport_name, warning_context, warning_label, warning_text,
        },
    },
};
use cordial_protocol::{
    self as p, CodeKind, DeviceState, InactiveReason, Platform, Transport, keys,
};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
    text::Line,
};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

/// The shortest pane that shows a line between its borders.
const PANE_MIN: usize = 3;

pub(super) fn spinner() -> char {
    const FRAMES: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis());
    FRAMES[(ms / 100 % 10) as usize]
}

pub(super) fn device_status(d: &p::Device) -> (&'static str, Style) {
    match () {
        _ if d.blocked => ("⊘ Blocked", err()),
        _ if d.state() == DeviceState::Connected => ("● Connected", ok()),
        _ if d.state() == DeviceState::Connecting => ("◌ Connecting", warn()),
        _ if d.state() == DeviceState::Disconnecting => ("◌ Disconnecting", warn()),
        _ if model::inactive(d) == Some(InactiveReason::UnsupportedTransport) => {
            ("! Unsupported", warn())
        }
        _ if !d.enabled => ("○ Disabled", dim()),
        _ if model::inactive(d).is_some() => ("! Inactive", warn()),
        _ => ("○ Disconnected", dim()),
    }
}

/// The saved device card's label column: every label and value in it lines
/// up past the longest label the card can show, and when that leaves too
/// little room each value sits indented under its own label instead.
#[derive(Clone, Copy)]
struct Column {
    kw: usize,
    stacked: bool,
}

impl Column {
    const LABELS: [&str; 10] = [
        "Status",
        "Security",
        "Type",
        "Use This Device",
        "Automatic Connections",
        "Block Connections",
        "Reconnect",
        "Logitech Features",
        "Lock Indicators",
        "ID",
    ];

    /// Sized for the card's own labels and `more`, such as Device Info's.
    fn new<'a>(width: usize, more: impl IntoIterator<Item = &'a str>) -> Self {
        let kw = Self::LABELS
            .into_iter()
            .chain(more)
            .map(text::width)
            .max()
            .unwrap_or(0)
            + 2;
        Self {
            kw,
            stacked: width < kw + 16,
        }
    }
    fn field(self, b: &mut Layout, label: &str, value: &str, look: Style) {
        if self.stacked {
            b.line(styled(label.to_string(), dim()));
            b.field_at("", 2, value, look);
        } else {
            b.field_at(label, self.kw, value, look);
        }
    }
    /// A further line of the field above, aligned with its value.
    fn more(self, b: &mut Layout, mark: Option<(&str, Style)>, value: &str, look: Style) {
        let lead = if self.stacked { 2 } else { self.kw };
        let mut prefix = Line::from(pad_str("", lead));
        if let Some((mark, style)) = mark {
            prefix.spans.push(span(mark.to_string(), style));
        }
        b.hang(prefix, value, look);
    }
    /// On and Off options; `actions` choose On and Off. They stay in place, dim and without
    /// targets, while `enabled` is false, and are drawn as a plain value when the adapter
    /// doesn't offer changing it.
    fn on_off_if(
        self,
        b: &mut Layout,
        label: &str,
        on: bool,
        actions: Option<(Action, Action)>,
        enabled: bool,
    ) {
        let Some((on_action, off_action)) = actions else {
            self.field(b, label, if on { "On" } else { "Off" }, layout::plain());
            return;
        };
        let options = layout::on_off(Some(on), on_action, off_action);
        if self.stacked {
            b.line(styled(label.to_string(), dim()));
            b.choice_if("", 2, options, enabled);
        } else {
            b.choice_if(label, self.kw, options, enabled);
        }
    }
}

/// Why a saved device is inactive.
fn enablement_section(b: &mut Layout, d: &p::Device) {
    match model::inactive(d) {
        None | Some(InactiveReason::Disabled) => {}
        Some(InactiveReason::TransportDisabled) => {
            b.para(&text::transport_disabled_sentence(d.transport()), warn())
        }
        Some(reason) => b.para(
            &text::capitalized(&format!("{}.", text::inactive_words(reason, d.transport()))),
            warn(),
        ),
    }
}

/// Enabled devices of each transport against how many can be enabled.
fn capacity_section(b: &mut Layout, st: &State) {
    let limits: Vec<(Transport, u32)> = model::enabled_transports(&st.status)
        .into_iter()
        .filter_map(|t| Some((t, model::max_enabled(&st.status, t)?)))
        .collect();
    if limits.is_empty() {
        return;
    }
    b.row();
    b.line(styled("Active Devices", title()));
    for (t, max) in limits {
        let enabled = st
            .devices
            .iter()
            .filter(|d| d.enabled && d.transport == t as i32)
            .count() as u32;
        b.hang(
            styled(format!("{}  ", transport_long(t)), dim()),
            &format!("{enabled} of {max}"),
            if enabled >= max {
                warn()
            } else {
                layout::plain()
            },
        );
    }
}

/// A device's battery for the device list, such as "80%↑" or "?" while the
/// charge is unknown: a warning when it runs low.
fn battery_tag(d: &p::Device) -> Option<(String, Style)> {
    let b = text::battery(&d.info)?;
    let look = if b.low() { warn() } else { layout::plain() };
    Some((b.compact("↑"), look))
}

/// The device's own report: battery, identity and firmware.
fn info_section(b: &mut Layout, c: Column, st: &State, d: &p::Device, rows: Vec<text::InfoRow>) {
    let busy = pending_for(st, "device refresh", d.id);
    if rows.is_empty() && !busy {
        return;
    }
    b.row();
    b.line(styled("Device Info", title()));
    if busy {
        c.more(b, None, &format!("{} Reading…", spinner()), warn());
    }
    for r in rows {
        c.field(b, &r.label, &r.value, layout::plain());
    }
}

/// A device's diagnostics: its warnings, HID++ details, link security and identifiers.
fn diagnostics(b: &mut Layout, st: &State, d: &p::Device) {
    let ids: Vec<_> = text::info_rows(&d.info)
        .into_iter()
        .filter(|r| text::IDENTIFIER_KEYS.contains(&r.key.as_str()))
        .collect();
    let warnings = st.warnings_of(d.id);
    let labels = warnings
        .iter()
        .map(|w| warning_label(w.code()))
        .chain(ids.iter().map(|r| r.label.as_str()))
        .chain(["HID++ Protocol", "Security", "ID", "Last Error", "Status"]);
    let c = Column::new(b.width, labels);
    // Sections are separated by a blank row; a section with nothing to show is left out.
    let mut started = false;
    let mut section = |b: &mut Layout| {
        if std::mem::replace(&mut started, true) {
            b.row();
        }
    };
    if !warnings.is_empty() {
        section(b);
        b.line(styled("Device Warnings", title()));
    }
    for warning in warnings {
        c.field(
            b,
            warning_label(warning.code()),
            warning_text(warning.code()),
            warn(),
        );
        c.more(b, None, &warning_context(warning), dim());
    }
    if let Some(code) = model::last_error(d) {
        section(b);
        b.line(styled("Connection", title()));
        let e = p::Error {
            code: code as i32,
            ..Default::default()
        };
        c.field(b, "Last Error", &text::wire_text(&e, None), err());
    }
    if let Some(code) = model::profile_error(d).filter(|_| connected(d)) {
        section(b);
        b.line(styled("Profiles", title()));
        c.field(b, "Status", "Not Loaded", err());
        c.more(b, None, &text::profile_error_words(code), dim());
    }
    let hidpp = [protocol_status(d), hidpp_status(d)];
    if hidpp.iter().any(Option::is_some) {
        section(b);
        b.line(styled("Logitech Features", title()));
        for (text, look) in hidpp.into_iter().flatten() {
            b.line(styled(text, look));
        }
    }
    if text::link_security(d).is_some() {
        section(b);
        security_section(b, c, d);
    }
    section(b);
    b.line(styled("Identifiers", title()));
    for r in ids {
        c.field(b, &r.label, &r.value, layout::plain());
    }
    c.field(b, "ID", &d.id.to_string(), dim());
}

/// A short tag for a device's current link security in the device list, or
/// `None` when the device is not connected.
fn security_tag(d: &p::Device) -> Option<(&'static str, Style)> {
    let s = text::link_security(d)?;
    Some(match s.map(|s| (s.encrypted, s.authenticated)) {
        Some((Some(false), _)) => ("Unencrypted", warn()),
        Some((Some(true), Some(true))) => ("Enc · Auth", layout::plain()),
        Some((Some(true), Some(false))) => ("Enc · Unauth", layout::plain()),
        Some((Some(true), None)) => ("Encrypted", layout::plain()),
        Some((None, _)) | None => ("Unreported", dim()),
    })
}

/// The current link's security: a summary, then each property with
/// unreported ones marked. Nothing is shown for a device that is not
/// connected, so an earlier link's report never appears.
fn security_section(b: &mut Layout, c: Column, d: &p::Device) {
    let Some(s) = text::link_security(d) else {
        return;
    };
    let look = match s.map(|s| s.encrypted) {
        Some(Some(true)) => layout::plain(),
        Some(Some(false)) => warn(),
        _ => dim(),
    };
    let summary = match s {
        Some(_) => text::capitalized(text::security_summary(s)),
        None => "Not Reported".into(),
    };
    c.field(b, "Security", &summary, look);
    for f in s.map(text::security_facts).into_iter().flatten() {
        let (mark, look) = match f.reading {
            text::Reading::Yes => ("● ", ok()),
            text::Reading::No if f.label == "Encryption" => ("○ ", warn()),
            text::Reading::No => ("○ ", layout::plain()),
            text::Reading::Value => ("· ", layout::plain()),
            text::Reading::NotReported => ("? ", dim()),
        };
        c.more(
            b,
            Some((mark, look)),
            &format!("{}: {}", f.label, f.value),
            look,
        );
    }
}

/// RSSI as four bars on `base`, which may carry the row background.
fn signal(rssi: Option<i32>, base: Style) -> Vec<ratatui::text::Span<'static>> {
    let Some(rssi) = rssi else {
        return vec![span("—", inherit(dim(), base))];
    };
    let n = match rssi {
        r if r >= -60 => 4,
        r if r >= -70 => 3,
        r if r >= -80 => 2,
        _ => 1,
    };
    let bars: Vec<char> = "▂▄▆█".chars().collect();
    vec![
        span(bars[..n].iter().collect::<String>(), base),
        span(bars[n..].iter().collect::<String>(), inherit(dim(), base)),
    ]
}

/// The HID++ version the current link reported.
fn protocol_status(d: &p::Device) -> Option<(String, Style)> {
    let (mark, look) = match (model::hidpp_version(d), model::hidpp_up(d)) {
        (Some(_), _) => ("●", ok()),
        (None, Up::Starting) => ("◌", warn()),
        (None, Up::Unsupported) => ("○", dim()),
        _ => return None,
    };
    Some((
        format!("{mark} HID++ {}", text::hidpp_protocol_text(d)),
        look,
    ))
}

/// Logitech Features as a whole: off, waiting, starting, active or failed.
pub(super) fn hidpp_status(d: &p::Device) -> Option<(String, Style)> {
    Some(match model::hidpp_up(d) {
        Up::Off => return None,
        Up::Disconnected => ("○ Waiting to Connect".into(), dim()),
        Up::Starting => ("◌ Setting Up Logitech Features…".into(), warn()),
        Up::Active => ("● Logitech Features Active".into(), ok()),
        Up::Unsupported => ("○ Logitech Features Unsupported".into(), dim()),
        Up::Error(code) => (
            format!("✕ Logitech Features Failed: {}", text::hidpp_words(code)),
            err(),
        ),
    })
}

/// The Logitech Features preference, protocol detection and readiness. The
/// preference is saved per device, so it stays changeable while the device
/// is disconnected. Its options are dim and have no targets while `idle` is
/// false.
fn hidpp_section(b: &mut Layout, c: Column, on: bool, staged: bool, idle: bool) {
    let actions = Some((Action::Hidpp(true), Action::Hidpp(false)));
    c.on_off_if(b, "Logitech Features", on, actions, idle);
    if staged {
        c.more(b, None, "✎ Changed", accent());
    }
}

/// Actions drawn as the details card's On and Off options rather than buttons.
fn policy_option(action: &Action) -> bool {
    matches!(
        action,
        Action::Enable
            | Action::Disable
            | Action::Trust
            | Action::Untrust
            | Action::Block
            | Action::Unblock
    )
}

pub(super) struct Control {
    pub label: &'static str,
    pub action: Action,
    pub tone: Tone,
    pub right: bool,
}

/// Makes a pairing code easy to read and compare: 482916 → "4 8 2   9 1 6".
fn spaced(value: &str) -> String {
    let v = display(value);
    if v.chars().count() > 16 || v.contains('\\') {
        return v;
    }
    let digits = v.len() == 6 && v.bytes().all(|b| b.is_ascii_digit());
    let mut out = String::new();
    for (i, c) in v.chars().enumerate() {
        if i > 0 {
            out.push(' ');
            if i == 3 && digits {
                out.push_str("  ");
            }
        }
        out.push(c);
    }
    out
}

/// The full-screen interface's help; the shell has its own.
const HELP: &[(&str, &[(&str, &str)])] = &[
    (
        "Devices",
        &[
            (
                "Saved",
                "Saved on the adapter with their settings, including devices that are disabled or use a Bluetooth transport this build doesn't support. Trusted, enabled saved devices reconnect by themselves, even while this app is closed.",
            ),
            (
                "Disabled",
                "Saved with its bond and settings, but not used for connections. Enable it to connect again. Enabling needs a free enabled-device place; the adapter never disables another device to make one.",
            ),
            (
                "Nearby",
                "Found by the current scan. Every listed nearby device offers Pair; the adapter recognizes a saved device only once pairing identifies it. Unnamed devices are listed by kind, such as Unnamed Keyboard; Show Unnamed Devices also lists those of unknown kind until this app closes.",
            ),
            (
                "Battery",
                "The battery's charge, such as 80%↑; ↑ is charging.",
            ),
        ],
    ),
    (
        "Actions",
        &[
            (
                "Pair",
                "Pair a nearby device, save it on the adapter and connect it. Pairing a saved device again renews its bond and keeps its settings. When every enabled-device place is in use, the device is saved disabled instead. Pair is unavailable when the adapter has no room for another paired device.",
            ),
            (
                "Use This Device: On",
                "Use a saved device for connections again, if an enabled-device place is free.",
            ),
            (
                "Use This Device: Off",
                "Disconnect the device and stop using it for connections. Its bond and settings are kept.",
            ),
            (
                "Connect",
                "Connect a saved device now and resume automatic reconnection.",
            ),
            (
                "Disconnect",
                "Disconnect and pause automatic reconnection until the device is connected again or the adapter restarts.",
            ),
            (
                "Automatic Connections: On",
                "Let the device reconnect without being asked.",
            ),
            (
                "Automatic Connections: Off",
                "Stop future automatic connections; the current one is kept.",
            ),
            (
                "Block Connections: On",
                "Refuse every connection from the device until it is unblocked. The bond is kept.",
            ),
            (
                "Block Connections: Off",
                "Allow connections from the device again.",
            ),
            (
                "Forget Device",
                "Disconnect and delete the saved bond. The device may also need its old pairing cleared before it is used again.",
            ),
            (
                "Hide",
                "Drop a nearby device from the list until it is found again.",
            ),
            (
                "Logitech Features",
                "On lets the adapter use Logitech HID++ for special keys and to apply saved device settings while the device is connected. Saved for each device, and can be changed while it is disconnected.",
            ),
            (
                "Settings…",
                "Open a saved device's settings. Opening and browsing them never changes the device or what is saved.",
            ),
            (
                "Profiles…",
                "Show and change the profiles a saved device uses, in the order they apply.",
            ),
            (
                "Diagnostics…",
                "Show the selected saved device's warnings, HID++ status, link security and identifiers.",
            ),
        ],
    ),
    (
        "Diagnostics",
        &[(
            "Refresh",
            "Ask a connected device for its current battery charge, model, firmware and other information. The adapter keeps this information only in memory; it is never saved.",
        )],
    ),
    (
        "Device Settings",
        &[
            (
                "Current",
                "Current: the value last read from the device. It is marked Last Known while the device is disconnected or has not been read again.",
            ),
            (
                "Saved",
                "\"Saved\" means the adapter reapplies the saved value when the device reconnects with Logitech Features on, when Logitech Features are turned on, and after an adapter platform change, overriding changes made on the device or from another computer meanwhile. With Logitech Features off the saved value waits.",
            ),
            (
                "Save",
                "Choose a new value with the controls, then Save stores it on the adapter and applies it. With Logitech Features off, Save only stores it; it is applied when Logitech Features are turned on. Nothing is sent until you click Save; Discard discards the change.",
            ),
            (
                "Forget Saved Value",
                "The adapter stops applying a value and the device keeps its current one.",
            ),
            (
                "Refresh",
                "Read current values from the connected device. Nothing is changed.",
            ),
            (
                "Changed on Device",
                "\"Changed on Device\" means the device was changed, for example with its own controls. Such changes are shown but never saved or corrected until saved values are applied again.",
            ),
        ],
    ),
    (
        "Adapter",
        &[
            (
                "Files…",
                "Browse the adapter's filesystem from Adapters › Files… and download a file to this computer. Click a directory to open it, or a file to choose where to save it. The download is kept only when every byte arrives, and an existing local file is replaced only after you confirm. Files work even when the adapter's Bluetooth isn't ready. In Files: ↑↓ select, Enter opens or downloads, Backspace goes up, r refreshes, Esc closes.",
            ),
            (
                "Platform",
                "The computer's system: Linux, Windows or macOS. Special keys on devices that use HID++ send that system's standard shortcuts. To change the platform, select the adapter in Adapters. The adapter saves the platform, even when no devices are paired.",
            ),
            (
                "Profiles…",
                "Create, copy and delete profiles, and choose the profiles VIA and Vial edit.",
            ),
        ],
    ),
    (
        "Mouse and Keyboard",
        &[
            (
                "Mouse",
                "Click buttons, options and rows. Scroll a pane with the wheel or the ▲/▼ markers on its border.",
            ),
            ("↑ ↓", "Select a device; j/k, Home and End also work."),
            (
                "Enter",
                "Pair the selected Nearby device, or connect the selected saved one.",
            ),
            (
                "s",
                "Start or stop a scan of both transports. The Scan menu picks one transport.",
            ),
            (
                "p",
                "Pair the selected Nearby device. Saved devices don't pair.",
            ),
            ("c", "Connect the selected saved device."),
            ("d", "Disconnect the selected device."),
            ("e", "Enable or disable the selected saved device."),
            ("t", "Trust or untrust the selected device."),
            ("b", "Block or unblock the selected device."),
            ("x", "Forget the selected saved device."),
            ("h", "Hide the selected nearby device."),
            (
                "o",
                "Open the selected saved device's settings; Esc goes back.",
            ),
            ("i", "Open the selected saved device's diagnostics."),
            ("a", "Select the adapter."),
            ("r", "Refresh the device list."),
            (
                "Tab",
                "Move between controls; Shift+Tab moves back. Enter activates the highlighted control, and Enter or Space chooses a highlighted On or Off option such as Show Unnamed Devices.",
            ),
            ("y n", "Answer confirmations and code comparisons."),
            ("PgUp PgDn", "Scroll the activity log."),
            ("Esc", "Close a menu or dialog, or cancel a pairing prompt."),
            ("? q", "Show this help, or quit."),
            (
                "Ctrl+C",
                "Quit from anywhere, including dialogs and text fields. Like Quit, it ends this app's adapter session and anything it started; saved devices keep working.",
            ),
        ],
    ),
    (
        "Closing",
        &[(
            "Quit",
            "Anything this app started, such as a scan, stops. Saved devices stay paired and keep working.",
        )],
    ),
];

/// A help row as the adapter offers it: `None` hides a row that describes
/// nothing it supports. Local navigation is described without the adapter.
fn help_text(st: &State, section: &str, key: &str, text: &str) -> Option<String> {
    let scans = scan_choices(st);
    // Hiding is local, so it needs only nearby devices to hide.
    let hide = !scans.is_empty() || !st.candidates.is_empty();
    let shown = match (section, key) {
        (_, "s") => {
            return match scans[..] {
                [] => None,
                [only] => Some(format!(
                    "Start or stop a scan of {} devices.",
                    scan_name(only)
                )),
                _ => Some(text.to_owned()),
            };
        }
        ("Devices", "Nearby") | ("Actions", "Hide") | (_, "h") => hide,
        ("Actions", "Pair") | (_, "p") => !scans.is_empty() || !st.candidates.is_empty(),
        ("Adapter", "Files…") => model::development(&st.status),
        (_, "Profiles…") => profiles::available(&st.status),
        _ => true,
    };
    shown.then(|| text.to_owned())
}

/// Whether the running pairing is with this candidate.
pub(super) fn pairing_with(st: &State, candidate: u32) -> bool {
    st.available
        && st
            .pairing
            .as_ref()
            .is_some_and(|p| p.candidate == candidate && model::pairing_running(p))
}

/// A drawn frame: lines exactly as tall as the screen, with any overlay and
/// what the footer shows.
struct Frame {
    lines: Vec<Styled>,
    overlay: Option<(Layout, usize, usize, bool)>,
    footer: Option<Styled>,
}

impl<B: Backend> Model<B> {
    /// Draws a titled box of exactly w×h cells. The body scrolls by the
    /// area's offset, counted from its end when `from_end` is set; pinned
    /// lines stay at the bottom. Arrows on the border scroll when the body
    /// does not fit.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn frame(
        &mut self,
        heading: &str,
        mut body: Layout,
        mut pinned: Layout,
        area: Option<Area>,
        from_end: bool,
        w: usize,
        h: usize,
    ) -> Layout {
        let mut out = Layout::new(w);
        let (inner, rows) = (w.saturating_sub(4), h.saturating_sub(2));
        let n = pinned.lines.len();
        if n > 0 && (n > rows || n == rows && !body.lines.is_empty()) {
            // Too short to pin the controls: scroll them with the body
            // instead of dropping any.
            if !body.lines.is_empty() {
                body.row();
            }
            body.add(std::mem::take(&mut pinned));
        }
        let mut fixed = pinned.lines.len();
        if fixed > 0 && !body.lines.is_empty() && rows.saturating_sub(fixed) > 1 {
            fixed += 1;
        }
        fixed = fixed.min(rows);
        // Keep pinned lines next to short content.
        let visible = (rows - fixed).min(body.lines.len());
        if let Some(area) = area {
            self.steps
                .insert(area, visible.saturating_sub(1).clamp(1, 3));
        }
        let over = body.lines.len() - visible;
        let mut none = 0;
        let offset = match area {
            Some(area) => self.scroll(area),
            None => &mut none,
        };
        *offset = (*offset).min(over);
        let start = if from_end { over - *offset } else { *offset };
        let border =
            |out: &mut Layout, corner: &str, end: &str, heading: &str, more: bool, up: bool| {
                let y = out.lines.len();
                let mut line = styled(format!("{corner}─"), dim());
                let mut used = 2;
                if !heading.is_empty() {
                    let room = w.saturating_sub(8).saturating_sub(if more { 9 } else { 0 });
                    let heading = layout::truncate_str(heading, room.max(1));
                    used += text::width(&heading) + 2;
                    line = join(
                        line,
                        Line::from(vec![
                            span(" ", Style::new()),
                            span(heading, title()),
                            span(" ", Style::new()),
                        ]),
                    );
                }
                let mut mark = Line::default();
                if more
                    && w.saturating_sub(used + 1) >= 10
                    && let Some(area) = area
                {
                    let arrow = if up { "▲" } else { "▼" };
                    mark = Line::from(vec![
                        span(" ", Style::new()),
                        span(format!("{arrow} more"), accent()),
                        span(" ", Style::new()),
                        span("─", dim()),
                    ]);
                    out.hits.push(Hit {
                        x: w - 9,
                        y,
                        w: 6,
                        action: Action::Scroll(area, up),
                    });
                }
                let fill = w.saturating_sub(used + 1 + line_width(&mark));
                line = join(line, styled("─".repeat(fill), dim()));
                line = join(line, mark);
                line = join(line, styled(end.to_owned(), dim()));
                out.lines.push(line);
            };
        border(&mut out, "╭", "╮", heading, start > 0, true);
        let side = || span("│", dim());
        let put = |out: &mut Layout, s: Styled| {
            let mut l = Line::from(vec![side(), span(" ", Style::new())]);
            l = join(l, pad(truncate(s, inner), inner));
            l.spans.push(span(" ", Style::new()));
            l.spans.push(side());
            out.lines.push(l);
        };
        let Layout {
            lines: body_lines,
            hits: body_hits,
            ..
        } = body;
        for s in body_lines.into_iter().skip(start).take(visible) {
            put(&mut out, s);
        }
        for hh in body_hits {
            if hh.y >= start && hh.y < start + visible {
                out.hits.push(Hit {
                    x: hh.x + 2,
                    y: hh.y - start + 1,
                    w: hh.w.min(inner.saturating_sub(hh.x)),
                    action: hh.action,
                });
            }
        }
        let skip = pinned.lines.len().saturating_sub(fixed);
        let shown = pinned.lines.len() - skip;
        let top = 1 + visible + fixed - shown;
        if fixed > shown {
            put(&mut out, Line::default());
        }
        for s in pinned.lines.into_iter().skip(skip) {
            put(&mut out, s);
        }
        for hh in pinned.hits {
            if hh.y >= skip {
                out.hits.push(Hit {
                    x: hh.x + 2,
                    y: top + hh.y - skip,
                    w: hh.w.min(inner.saturating_sub(hh.x)),
                    action: hh.action,
                });
            }
        }
        while out.lines.len() + 1 < h {
            put(&mut out, Line::default());
        }
        border(&mut out, "╰", "╯", "", start < over, false);
        if let Some(area) = area {
            for y in 1..=rows {
                out.hits.push(Hit {
                    x: 1,
                    y,
                    w: w.saturating_sub(2),
                    action: Action::Wheel(area),
                });
            }
        }
        // A box with no room for both borders is cut to its height.
        out.lines.truncate(h);
        out.hits.retain(|hh| hh.y < h);
        out
    }

    /// Paints the frame into `buf`, which covers the whole terminal.
    pub(crate) fn render(&mut self, buf: &mut Buffer) {
        buf.reset();
        let frame = if self.width < MIN_WIDTH || self.height < MIN_HEIGHT {
            self.hits = vec![Hit {
                x: 0,
                y: 0,
                w: self.width.min(6),
                action: Action::Quit,
            }];
            self.hovered();
            Frame {
                lines: vec![
                    Line::from("[Quit]"),
                    Line::from("Resize to at least 40 columns and 10 rows."),
                ],
                overlay: None,
                footer: None,
            }
        } else {
            self.screen()
        };
        let area = buf.area;
        let put = |buf: &mut Buffer, x: usize, y: usize, line: &Styled| {
            if y < usize::from(area.height) && x < usize::from(area.width) {
                buf.set_line(x as u16, y as u16, line, area.width - x as u16);
            }
        };
        for (y, line) in frame.lines.iter().enumerate() {
            put(buf, 0, y, line);
        }
        if let Some((overlay, x, y, dimmed)) = &frame.overlay {
            if *dimmed {
                for cell in buf.content.iter_mut() {
                    cell.set_style(Style::reset().add_modifier(Modifier::DIM));
                }
            }
            // Styles merge on draw, so clear the overlay's area first.
            let under = Rect::new(
                *x as u16,
                *y as u16,
                overlay.width as u16,
                overlay.lines.len() as u16,
            )
            .intersection(area);
            buf.set_style(under, Style::reset());
            for (i, line) in overlay.lines.iter().enumerate() {
                put(buf, *x, y + i, line);
            }
        }
        if let Some(footer) = &frame.footer {
            let y = usize::from(area.height).saturating_sub(1);
            buf.set_style(Rect::new(0, y as u16, area.width, 1), Style::reset());
            for x in 0..area.width {
                buf[(x, y as u16)].set_symbol(" ");
            }
            put(buf, 0, y, footer);
        }
        // The highlighted control is drawn in reverse video.
        if let Some(h) = self.focus_hit()
            && h.y < usize::from(area.height)
        {
            let end = (h.x + h.w).min(usize::from(area.width));
            for x in h.x..end {
                buf[(x as u16, h.y as u16)]
                    .set_style(Style::reset().add_modifier(Modifier::REVERSED));
            }
        }
    }

    fn screen(&mut self) -> Frame {
        let st = self.state();
        let mut base = Layout::new(self.width);
        self.header(&mut base, st.as_ref());
        let body_h = self.height.saturating_sub(base.lines.len() + 1);
        let mut activity = false;
        match &st {
            Some(st) if !self.preparing => activity = self.body(&mut base, st, body_h),
            _ => {
                for _ in 0..body_h {
                    base.line(Line::default());
                }
            }
        }
        base.line(Line::default());
        let mut hits = std::mem::take(&mut base.hits);
        let mut overlay = None;
        let (mut footer, mut modal) = (true, false);
        if let Some(b) = self.dialog_box(st.as_ref()) {
            let x = self.width.saturating_sub(b.width) / 2;
            let y = self.height.saturating_sub(b.lines.len()) / 2;
            footer = y + b.lines.len() < self.height;
            modal = true;
            hits = b
                .hits
                .iter()
                .map(|h| Hit {
                    x: h.x + x,
                    y: h.y + y,
                    ..h.clone()
                })
                .collect();
            overlay = Some((b, x, y, true));
        } else if let Some((b, x, y)) = self.menu_box(st.as_ref(), &hits) {
            modal = true;
            hits = b
                .hits
                .iter()
                .map(|h| Hit {
                    x: h.x + x,
                    y: h.y + y,
                    ..h.clone()
                })
                .collect();
            overlay = Some((b, x, y, false));
        } else {
            self.menu = None; // Its button is gone, for example after losing the adapter.
        }
        self.hits = hits;
        self.hovered();
        self.sync_focus(); // Before the hints, which name what Enter presses.
        let footer = footer.then(|| self.footer(st.as_ref(), activity, modal));
        Frame {
            lines: base.lines,
            overlay,
            footer,
        }
    }

    /// Hints for this frame's controls. Without an activity pane, a failure
    /// shows for 20 seconds from when it is first drawn, or until a key is
    /// pressed after that, keeping the way to help.
    fn footer(&mut self, st: Option<&State>, activity: bool, modal: bool) -> Styled {
        let mut line = styled(format!(" {}", self.hints(st)), dim());
        // Routine entries after a failure do not hide it.
        let failure = self
            .logs
            .iter()
            .rev()
            .find(|e| e.kind >= super::activity::Kind::Warn)
            .cloned();
        if let (false, false, Some(e)) = (activity, modal, failure)
            && self.news_seen.is_none_or(|seen| e.at > seen)
        {
            if self.news_shown != Some(e.at) {
                self.news_shown = Some(e.at);
                self.news_shown_at = Some(Instant::now());
            }
            if self
                .news_shown_at
                .is_some_and(|t| t.elapsed().as_secs() < 20)
            {
                let tail = if self.width < 60 {
                    "? help "
                } else {
                    "? help · q quit "
                };
                line = spread(
                    Line::from(vec![
                        span(" ", Style::new()),
                        span(e.text.clone(), e.kind.style()),
                    ]),
                    styled(tail, dim()),
                    self.width,
                );
            }
        }
        truncate(line, self.width)
    }

    fn header(&mut self, l: &mut Layout, st: Option<&State>) {
        let mut parts = Vec::new();
        if !self.port.is_empty() {
            parts.push(display(&self.port));
        }
        if let Some(st) = st {
            parts.push(display(&st.status.name));
            if let Some(board) = model::info_text(&st.status.info, keys::BOARD_NAME) {
                parts.push(display(board));
            }
        }
        let mut heading = Line::from(vec![span(" ", Style::new()), span("Cordial", title())]);
        if !parts.is_empty() {
            heading
                .spans
                .push(span(format!(" · {}", parts.join(" · ")), dim()));
        }
        let status = join(self.status(st), Line::from(" "));
        l.line(spread(heading, status, l.width));
        let mut bar = Layout::new(l.width.saturating_sub(2));
        let mut right = Layout::new(bar.width);
        let prepared = st.is_some() && !self.preparing;
        right.button("Help", Action::Help, Tone::Normal);
        right.button("Quit", Action::Quit, Tone::Normal);
        match st {
            Some(st) if prepared && !st.available => bar.row(),
            Some(st) if prepared && st.scanning.is_some() => {
                bar.button("Stop Scan", Action::ScanOff, Tone::Normal);
                let label = format!("Scanning {}", scanning_name(st));
                let room = bar
                    .width
                    .saturating_sub(line_width(&bar.lines[0]) + line_width(&right.lines[0]) + 3);
                if room >= 6 {
                    let text =
                        layout::truncate_str(&format!("{} {}…", spinner(), label.trim()), room);
                    bar.lines[0].spans.push(span(format!(" {text}"), accent()));
                }
            }
            // One transport scans directly; two open a menu of both and each.
            Some(st) if prepared => match scan_choices(st)[..] {
                [] => {}
                [only] => {
                    let label = format!("Scan {}", scan_name(only));
                    bar.button(&label, Action::Scan(only), Tone::Primary);
                }
                _ => bar.button("Scan ▾", Action::Menu(Menu::Scan), Tone::Primary),
            },
            _ => {}
        }
        bar.align_right(right);
        bar.indent();
        l.add(bar);
    }

    fn status(&self, st: Option<&State>) -> Styled {
        if self.quitting {
            return styled("Closing…", dim());
        }
        if self.opening.is_some() || self.preparing {
            return styled("◌ Connecting…", warn());
        }
        let Some(st) = st else {
            return styled("No Adapter", dim());
        };
        let s = match () {
            _ if !st.available => styled("✕ Disconnected", err()),
            _ if self.unready.is_some() || !st.status.ready => styled("! Not Ready", warn()),
            _ => styled("● Ready", ok()),
        };
        let enabled = st
            .devices
            .iter()
            .filter(|d| d.enabled && model::inactive(d).is_none())
            .count();
        let count = format!(" · {} Saved · {enabled} Enabled", st.devices.len());
        join(s, styled(count, dim()))
    }

    /// Fills the space between the header and footer: devices and details
    /// side by side (stacked when narrow) above the activity log.
    fn body(&mut self, l: &mut Layout, st: &State, mut h: usize) -> bool {
        let w = l.width;
        if !st.available {
            let mut banner = Layout::new(w.saturating_sub(2));
            let (mut text, mut choose) = (
                "✕ Cordial lost its connection to the adapter. The displayed information may be out of date.",
                "Choose Adapter…",
            );
            if text::width(text) + 30 > banner.width {
                (text, choose) = ("✕ Adapter Lost", "Adapters"); // One line when narrow.
            }
            banner.line(styled(text, err().add_modifier(Modifier::BOLD)));
            banner.button("Reconnect", Action::Reopen, Tone::Primary);
            banner.button(choose, Action::Adapters, Tone::Normal);
            banner.indent();
            h = h.saturating_sub(banner.lines.len());
            l.add(banner);
        }
        if let Some(why) = self.unready.clone().filter(|_| st.available) {
            let mut banner = Layout::new(w.saturating_sub(2));
            banner.hang(
                Line::default(),
                &format!("! The adapter isn't ready: {why}"),
                warn().add_modifier(Modifier::BOLD),
            );
            if !self.files.open && self.offers(st, &Action::FilesOpen) {
                banner.button("Files…", Action::FilesOpen, Tone::Primary);
            }
            banner.button("Reconnect", Action::Reopen, Tone::Normal);
            banner.button("Choose Adapter…", Action::Adapters, Tone::Normal);
            banner.indent();
            h = h.saturating_sub(banner.lines.len());
            l.add(banner);
        }
        // The Files page, or a device's settings page, takes the place of the
        // list and details.
        let files = self.files_open(st);
        let page = !files && self.settings_open(st);
        if w >= 90 {
            let act = if h >= 11 {
                (h / 3).clamp(4, 12)
            } else if h >= 8 {
                3
            } else {
                0
            };
            let list = w * 11 / 20;
            let (a, b) = if files {
                (
                    self.files_pane(st, list, h - act),
                    self.transfer_pane(st, w - list, h - act),
                )
            } else if page {
                (
                    self.settings_pane(st, list, h - act),
                    self.editor_pane(st, w - list, h - act),
                )
            } else {
                (
                    self.list_column(st, list, h - act),
                    self.details_pane(st, w - list, h - act),
                )
            };
            l.add(beside(a, b));
            if act > 0 {
                let pane = self.activity_pane(w, act);
                l.add(pane);
            }
            return act > 0;
        }
        let act = if h >= 13 { (h / 4).clamp(4, 10) } else { 0 };
        let rest = h - act;
        if rest < 6 {
            // Only a lost adapter's banner leaves this little room, and
            // device actions cannot run then; keep the list.
            let pane = if files {
                self.files_pane(st, w, rest)
            } else if page {
                self.settings_pane(st, w, rest)
            } else {
                self.list_column(st, w, rest)
            };
            l.add(pane);
            return false;
        }
        // Details shrink to their content so the list keeps the space.
        let (_, card, actions) = if files {
            self.transfer(st, w)
        } else if page {
            self.editor(st, w)
        } else {
            self.details(st, w)
        };
        let mut need = card.lines.len() + actions.lines.len() + 2;
        let mut floor = 3;
        if !actions.lines.is_empty() {
            need += 1;
            floor = actions.lines.len() + 3;
        }
        // The list keeps room for its Devices and Adapters panes when the details still fit.
        let list = if !files && !page && rest >= 3 * PANE_MIN {
            2 * PANE_MIN
        } else {
            PANE_MIN
        };
        let details = need.min(rest / 2).max(floor).min(rest - list);
        let (a, b) = if files {
            (
                self.files_pane(st, w, rest - details),
                self.transfer_pane(st, w, details),
            )
        } else if page {
            (
                self.settings_pane(st, w, rest - details),
                self.editor_pane(st, w, details),
            )
        } else {
            (
                self.list_column(st, w, rest - details),
                self.details_pane(st, w, details),
            )
        };
        l.add(a);
        l.add(b);
        if act > 0 {
            let pane = self.activity_pane(w, act);
            l.add(pane);
        }
        act > 0
    }

    /// The Devices pane above the Adapters pane, together exactly `h` lines tall. The Devices
    /// pane keeps at least half of them. A column too short for both boxes shows one Devices
    /// pane that lists the adapter and its actions after the devices, so they stay reachable.
    fn list_column(&mut self, st: &State, w: usize, h: usize) -> Layout {
        if h < 2 * PANE_MIN {
            let (mut b, mut selected_line) = self.devices_body(st, w);
            b.row();
            b.line(styled("Adapters", dim()));
            if self.selected == Some(Item::Adapter) {
                selected_line = Some(b.lines.len());
            }
            let (row, pinned) = self.adapters_body(st, w);
            b.add(row);
            b.add(pinned);
            self.reveal_selection(selected_line, h);
            return self.frame(
                "Devices",
                b,
                Layout::default(),
                Some(Area::Devices),
                false,
                w,
                h,
            );
        }
        let adapters = self.adapters_pane(st, w, h - h.div_ceil(2));
        let mut column = self.devices_pane(st, w, h - adapters.lines.len());
        column.add(adapters);
        column
    }

    /// Scrolls the device list to the selected row once after the selection moves.
    fn reveal_selection(&mut self, selected_line: Option<usize>, h: usize) {
        if self.reveal
            && let Some(line) = selected_line
        {
            self.reveal = false;
            self.device_scroll = self
                .device_scroll
                .max(line.saturating_sub(h.saturating_sub(3)))
                .min(line);
        }
    }

    fn devices_pane(&mut self, st: &State, w: usize, h: usize) -> Layout {
        let (b, selected_line) = self.devices_body(st, w);
        self.reveal_selection(selected_line, h);
        self.frame(
            "Devices",
            b,
            Layout::default(),
            Some(Area::Devices),
            false,
            w,
            h,
        )
    }

    /// The device list's rows, and the line of the selected one.
    fn devices_body(&mut self, st: &State, w: usize) -> (Layout, Option<usize>) {
        self.drop_hidden_selection();
        let mut b = Layout::new(w.saturating_sub(4));
        let transport_w = if b.width < 36 { 0 } else { 9 };
        let status_w = 15;
        // Link security gets a column only when names keep room beside it.
        let security_w = if b.width < 53 { 0 } else { 13 };
        // The battery gets a column sized to the widest report when names
        // keep room beside it.
        let battery = |d: &p::Device| battery_tag(d);
        let battery_w = st
            .devices
            .iter()
            .filter_map(|d| battery(d).map(|(t, _)| layout::line_width(&styled(t, dim())) + 1))
            .max()
            .filter(|w| b.width >= 44 + w)
            .unwrap_or(0)
            .min(16);
        let name_w = b
            .width
            .saturating_sub(2 + transport_w + status_w + battery_w + security_w)
            .max(4);
        let mut shown: Vec<Item> = Vec::new();
        let mut selected_line = None;
        // Each column is drawn on the row's base so the selection spans it.
        let mut row = |m: &Self,
                       b: &mut Layout,
                       id: Item,
                       name: &str,
                       transport: &str,
                       status: Vec<ratatui::text::Span<'static>>,
                       base: Style| {
            shown.push(id);
            let look = if Some(id) == m.selected {
                selected_line = Some(b.lines.len());
                inherit(bold(), base)
            } else {
                base
            };
            let marker = if Some(id) == m.selected { "▌ " } else { "  " };
            let mut text = Line::from(vec![
                span(marker, inherit(accent(), base)),
                span(
                    pad_str(&layout::truncate_str(name, name_w - 1), name_w),
                    look,
                ),
            ]);
            if transport_w > 0 {
                text.spans
                    .push(span(pad_str(transport, transport_w), inherit(dim(), base)));
            }
            text.spans.extend(status);
            let fill = b.width.saturating_sub(line_width(&text));
            text.spans.push(span(" ".repeat(fill), base));
            b.control(text, Action::Select(id));
        };
        let busy = |text: &str, base: Style| {
            let text = format!("{} {text}", spinner());
            vec![span(pad_str(&text, status_w), inherit(warn(), base))]
        };
        let base_of = |m: &Self, id: Item| {
            if Some(id) == m.selected {
                layout::selected()
            } else {
                layout::plain()
            }
        };
        if !st.devices.is_empty() {
            b.line(styled("Saved", dim()));
        }
        for d in &st.devices {
            let id = Item::Device(d.id);
            let base = base_of(self, id);
            let status = if pending_for(st, "device connect", d.id) {
                busy("Connecting…", base)
            } else if pending_for(st, "device disconnect", d.id) {
                busy("Disconnecting…", base)
            } else {
                let (text, look) = device_status(d);
                vec![span(pad_str(text, status_w), inherit(look, base))]
            };
            let mut status = status;
            if battery_w > 0 {
                let (tag, look) = battery(d).unwrap_or_default();
                status.push(span(
                    pad_str(&layout::truncate_str(&tag, battery_w - 1), battery_w),
                    inherit(look, base),
                ));
            }
            if security_w > 0
                && let Some((tag, look)) = security_tag(d)
            {
                status.push(span(format!(" {tag}"), inherit(look, base)));
            }
            row(
                self,
                &mut b,
                id,
                &display_name(Some(&d.name)),
                transport_name(d.transport()),
                status,
                base,
            );
        }
        if !st.devices.is_empty() {
            b.row();
        }
        b.line(styled("Nearby", dim()));
        let hidden = self.unnamed_hidden();
        if !scan_choices(st).is_empty() || !st.candidates.is_empty() || hidden > 0 {
            b.choice(
                "Show Unnamed Devices ",
                21,
                layout::on_off(
                    Some(self.show_unnamed),
                    Action::ShowUnnamed(true),
                    Action::ShowUnnamed(false),
                ),
            );
        }
        let mut nearby = 0;
        // Every listed candidate stays here and offers Pair.
        for c in &st.candidates {
            nearby += 1;
            let id = Item::Candidate(c.id);
            let base = base_of(self, id);
            let status = if pairing_with(st, c.id) {
                busy("Pairing…", base)
            } else {
                signal(c.rssi, base)
            };
            row(
                self,
                &mut b,
                id,
                &display_candidate_name(c),
                transport_name(c.transport()),
                status,
                base,
            );
        }
        // Hidden entries are counted below instead of calling the list empty.
        if nearby == 0 && hidden == 0 {
            let text = if st.scanning.is_some() {
                "  Looking for devices…"
            } else if scan_choices(st).is_empty() {
                "  No Nearby Devices"
            } else {
                "  Use Scan to find nearby devices"
            };
            b.line(styled(text, dim()));
        }
        if hidden > 0 {
            let noun = if hidden == 1 { "device" } else { "devices" };
            b.line(styled(format!("  {hidden} unnamed {noun} hidden"), dim()));
        }
        if st.last_scan.is_some_and(|s| s.truncated) {
            b.hang(
                Line::from("  "),
                "List is full. Stop and restart the scan to refresh it.",
                warn(),
            );
        }
        // A pairing whose device is no longer listed stays cancellable.
        if let Some(pairing) = st
            .pairing
            .as_ref()
            .filter(|p| st.available && model::pairing_running(p))
            && !shown.contains(&Item::Candidate(pairing.candidate))
        {
            b.line(styled(
                format!(
                    "{} Pairing {}…",
                    spinner(),
                    self.label(Item::Candidate(pairing.candidate))
                ),
                warn(),
            ));
            b.button("Cancel Pairing", Action::CancelPairing, Tone::Normal);
        }
        (b, selected_line)
    }

    fn details_pane(&mut self, st: &State, w: usize, h: usize) -> Layout {
        let (heading, b, actions) = self.details(st, w);
        self.frame(&heading, b, actions, Some(Area::Details), false, w, h)
    }

    /// The right pane's content for the selection: the adapter's page or its Profiles view, a
    /// device's Profiles view, or the selected device's field card and its actions.
    fn details(&self, st: &State, w: usize) -> (String, Layout, Layout) {
        if self.selected == Some(Item::Adapter) {
            if self.adapter_profiles_open(st) {
                return self.adapter_profiles(st, w);
            }
            return self.adapter_page(st, w);
        }
        if let Some(d) = self.layers_open(st) {
            return self.device_layers(st, d, w);
        }
        let mut b = Layout::new(w.saturating_sub(4));
        let mut actions = Layout::new(w.saturating_sub(4));
        let mut heading = "Details".to_string();
        match Self::find(st, self.selected) {
            (Some(d), _) => {
                heading = display_name(Some(&d.name));
                let rows: Vec<_> = text::info_rows(&d.info)
                    .into_iter()
                    .filter(|r| !text::IDENTIFIER_KEYS.contains(&r.key.as_str()))
                    .collect();
                let c = Column::new(b.width, rows.iter().map(|r| r.label.as_str()));
                let (text, look) = device_status(d);
                c.field(&mut b, "Status", text, look);
                enablement_section(&mut b, d);
                let mut kind = transport_long(d.transport()).to_string();
                let roles = role_names(&model::roles(d));
                if !roles.is_empty() {
                    kind = format!("{roles} · {kind}");
                }
                c.field(&mut b, "Type", &kind, layout::plain());
                // Choices stage until Save; nothing changes while a save runs.
                let values = self.device_values(st, d);
                let draft = self.device_draft(st, d);
                let saving = self.device_saving(d.id);
                let policy = |b: &mut Layout, label, on, actions, staged: bool| {
                    c.on_off_if(b, label, on, Some(actions), !saving);
                    if staged {
                        c.more(b, None, "✎ Changed", accent());
                    }
                };
                policy(
                    &mut b,
                    "Use This Device",
                    values.enabled == Some(true),
                    (Action::Enable, Action::Disable),
                    draft.enabled.is_some(),
                );
                policy(
                    &mut b,
                    "Automatic Connections",
                    values.trusted == Some(true),
                    (Action::Trust, Action::Untrust),
                    draft.trusted.is_some(),
                );
                policy(
                    &mut b,
                    "Block Connections",
                    values.blocked == Some(true),
                    (Action::Block, Action::Unblock),
                    draft.blocked.is_some(),
                );
                if model::inactive(d).is_some() {
                    c.field(&mut b, "Reconnect", "Not While Inactive", dim());
                } else if d.paused {
                    c.field(&mut b, "Reconnect", "Paused Until Connect", warn());
                } else {
                    c.field(&mut b, "Reconnect", "Automatic", layout::plain());
                }
                // Logitech Features wait while the device's settings work runs.
                let idle = settings_busy(st, d, self.saving(d.id)).is_empty() && !saving;
                hidpp_section(
                    &mut b,
                    c,
                    values.hidpp == Some(true),
                    draft.hidpp.is_some(),
                    idle,
                );
                if saving {
                    c.more(&mut b, None, &format!("{} Saving…", spinner()), warn());
                } else if let Some(e) = self.details_error(st, d) {
                    c.more(&mut b, None, &format!("✕ Couldn't Save: {e}"), err());
                }
                info_section(&mut b, c, st, d, rows);
            }
            (_, Some(c)) => {
                heading = display_candidate_name(c);
                b.field("Status", "Nearby", layout::plain());
                if !pairing_with(st, c.id) {
                    if model::storage_full(&st.status) {
                        b.field("Pair", "Storage Full", warn());
                    } else if model::transport_disabled(&st.status, c.transport()) {
                        b.para(&text::transport_disabled_sentence(c.transport()), warn());
                    } else if st.pairing.as_ref().is_some_and(model::pairing_running) {
                        b.field("Pair", "Pairing in Progress", warn());
                    } else {
                        b.para(
                            "Pair saves it. If it is already saved, pairing renews its bond and keeps its settings.",
                            dim(),
                        );
                    }
                }
                b.field("Type", transport_long(c.transport()), layout::plain());
                let mut strength = Line::from(signal(c.rssi, layout::plain()));
                if let Some(rssi) = c.rssi {
                    strength
                        .spans
                        .push(span(format!(" {rssi} dBm"), Style::new()));
                }
                b.line(join(styled(pad_str("Signal", 11), dim()), strength));
                b.field("ID", &c.id.to_string(), dim());
            }
            _ => b.para("Select a device to see its details and actions.", dim()),
        }
        // Policy actions are drawn as On and Off options in the card; their
        // shortcuts still use these actions.
        for a in self
            .device_actions(st)
            .into_iter()
            .filter(|a| !policy_option(&a.action))
        {
            if a.right {
                actions.button_right(a.label, a.action, a.tone);
            } else {
                actions.button(a.label, a.action, a.tone);
            }
        }
        (heading, b, actions)
    }

    /// The adapter's page: its name, platform and transports, staged until Save, and the use of
    /// its enabled-device places, with Profiles… to show its Profiles view.
    fn adapter_page(&self, st: &State, w: usize) -> (String, Layout, Layout) {
        let mut body = Layout::new(w.saturating_sub(4));
        let mut pinned = Layout::new(w.saturating_sub(4));
        body.field("Name", &display(&st.status.name), layout::plain());
        if can_set_platform(st) && !self.renaming() {
            body.button("Rename", Action::Rename, Tone::Normal);
        }
        body.row();
        let draft = self.adapter_draft(st);
        let idle = can_set_platform(st) && !self.adapter_saving();
        let changed = |b: &mut Layout, kw: usize| {
            b.hang(Line::from(pad_str("", kw)), "✎ Changed", accent());
        };
        let platform = draft.platform.unwrap_or(st.status.platform());
        if !st.status.ready {
            body.field(
                "Platform",
                "Unavailable until adapter storage is ready",
                warn(),
            );
        } else if can_set_platform(st) {
            let options = [Platform::Linux, Platform::Windows, Platform::Mac]
                .into_iter()
                .map(|p| Choice {
                    label: platform_name(p).into(),
                    action: Action::Platform(p),
                    chosen: platform == p,
                })
                .collect();
            body.choice_if("Platform", 11, options, idle);
        } else {
            body.field("Platform", platform_name(platform), layout::plain());
        }
        if draft.platform.is_some() {
            changed(&mut body, 11);
        }
        body.row();
        body.para("The computer's system. Special keys on every device using HID++ send its standard shortcuts. Saved on the adapter for all devices, including ones paired later.", dim());
        // One On and Off choice per transport the firmware can enable and disable, labelled by
        // its name.
        const KW: usize = "Bluetooth Classic".len() + 2;
        for t in model::transports(&st.status) {
            let Some(saved) = model::transport_enabled(&st.status, t) else {
                continue;
            };
            let staged = draft
                .transports
                .iter()
                .find(|(r, _)| *r == t)
                .map(|(_, on)| *on);
            let on = staged.unwrap_or(saved);
            let label = transport_long(t);
            body.row();
            if !st.status.ready {
                body.field_at(
                    label,
                    KW,
                    "Unavailable until adapter storage is ready",
                    warn(),
                );
            } else if can_set_platform(st) {
                let options = layout::on_off(
                    Some(on),
                    Action::Transport(t, true),
                    Action::Transport(t, false),
                );
                body.choice_if(label, KW, options, idle);
            } else {
                body.field_at(label, KW, if on { "On" } else { "Off" }, layout::plain());
            }
            if staged.is_some() {
                changed(&mut body, KW);
            }
        }
        body.row();
        if self.adapter_saving() {
            body.para(&format!("{} Saving…", spinner()), warn());
        } else if let Some(e) = self.adapter_error(st) {
            body.para(&format!("✕ {e}"), err());
        } else {
            self.refusal_hint(st, &mut body);
        }
        capacity_section(&mut body, st);
        self.adapter_buttons(st, &mut pinned);
        if profiles::available(&st.status) {
            pinned.button("Profiles…", Action::Profiles, Tone::Normal);
        }
        (display(&st.status.name), body, pinned)
    }

    /// The Adapters pane: the connected adapter, which selects its page, and the adapter's own
    /// actions. It is at most `max` lines tall, and at least [`PANE_MIN`]; its actions scroll
    /// into view when they don't fit.
    fn adapters_pane(&mut self, st: &State, w: usize, max: usize) -> Layout {
        let (b, pinned) = self.adapters_body(st, w);
        let h = (b.lines.len() + pinned.lines.len() + 3)
            .min(max)
            .max(PANE_MIN);
        self.frame("Adapters", b, pinned, Some(Area::Adapters), false, w, h)
    }

    /// The connected adapter's row, and its actions.
    fn adapters_body(&self, st: &State, w: usize) -> (Layout, Layout) {
        let mut b = Layout::new(w.saturating_sub(4));
        let selected = self.selected == Some(Item::Adapter);
        let base = if selected {
            layout::selected()
        } else {
            layout::plain()
        };
        let look = if selected {
            inherit(bold(), base)
        } else {
            base
        };
        let (status, status_look) = match () {
            _ if !st.available => ("✕ Disconnected", err()),
            _ if self.unready.is_some() || !st.status.ready => ("! Not Ready", warn()),
            _ => ("● Ready", ok()),
        };
        let marker = if selected { "▌ " } else { "  " };
        let name_w = b.width.saturating_sub(2 + 15).max(4);
        let mut row = Line::from(vec![
            span(marker, inherit(accent(), base)),
            span(
                pad_str(
                    &layout::truncate_str(&display(&st.status.name), name_w - 1),
                    name_w,
                ),
                look,
            ),
            span(status, inherit(status_look, base)),
        ]);
        let fill = b.width.saturating_sub(line_width(&row));
        row.spans.push(span(" ".repeat(fill), base));
        b.control(row, Action::Select(Item::Adapter));
        let mut pinned = Layout::new(b.width);
        let actions = [
            ("Disconnect", Action::AdapterDisconnect, Tone::Normal),
            ("Choose Adapter…", Action::Adapters, Tone::Normal),
            ("Refresh", Action::Refresh, Tone::Normal),
            ("Files…", Action::FilesOpen, Tone::Normal),
            ("Enter Bootloader…", Action::Bootloader, Tone::Danger),
        ];
        for (label, action, tone) in actions {
            if self.offers(st, &action) {
                pinned.button(label, action, tone);
            }
        }
        (b, pinned)
    }

    /// The controls for the selected device in its current state. The
    /// details card draws them and keyboard shortcuts consult them directly,
    /// so a key never acts on an older frame's buttons.
    pub(super) fn device_actions(&self, st: &State) -> Vec<Control> {
        let mut out = Vec::new();
        let mut add = |label, action, tone, right| {
            if self.offers(st, &action) {
                out.push(Control {
                    label,
                    action,
                    tone,
                    right,
                })
            }
        };
        match Self::find(st, self.selected) {
            (Some(d), _) => {
                match d.state() {
                    DeviceState::Connected | DeviceState::Connecting => {
                        add("Disconnect", Action::Disconnect, Tone::Normal, false)
                    }
                    _ if super::connect_blocked(d).is_none() => {
                        add("Connect", Action::Connect, Tone::Primary, false)
                    }
                    _ => {}
                }
                // The policy actions stage the opposite of the staged or saved value.
                let values = self.device_values(st, d);
                if values.enabled == Some(true) {
                    add("Disable", Action::Disable, Tone::Normal, false);
                } else {
                    add("Enable", Action::Enable, Tone::Normal, false);
                }
                // Saved devices never offer pairing; it starts from Nearby.
                if values.trusted == Some(true) {
                    add("Untrust", Action::Untrust, Tone::Normal, false);
                } else {
                    add("Trust", Action::Trust, Tone::Normal, false);
                }
                if values.blocked == Some(true) {
                    add("Unblock", Action::Unblock, Tone::Normal, false);
                } else {
                    add("Block", Action::Block, Tone::Normal, false);
                }
                let draft = self.device_draft(st, d);
                let details = DeviceUpdate {
                    layers: None,
                    ..draft
                };
                if !details.is_empty() && !self.device_saving(d.id) {
                    add("Save", Action::DeviceSave, Tone::Primary, false);
                    add("Discard", Action::DeviceDiscard, Tone::Normal, false);
                }
                add("Settings…", Action::DeviceSettings, Tone::Normal, false);
                if layered(st, d) {
                    add("Profiles…", Action::DeviceProfiles, Tone::Normal, false);
                }
                add("Diagnostics…", Action::Diagnostics, Tone::Normal, false);
                add("Forget Device", Action::Remove, Tone::Danger, true);
            }
            (_, Some(c)) => {
                if pairing_with(st, c.id) {
                    add("Cancel Pairing", Action::CancelPairing, Tone::Normal, false);
                } else if pair_blocked(st, c).is_none() {
                    // The details card says why Pair is unavailable.
                    add("Pair", Action::Pair, Tone::Primary, false);
                }
                add("Hide", Action::Hide, Tone::Normal, true);
            }
            _ => {}
        }
        out
    }

    fn activity_pane(&mut self, w: usize, h: usize) -> Layout {
        let mut b = Layout::new(w.saturating_sub(4));
        self.event_width = b.width.saturating_sub(10);
        for e in &self.logs {
            b.hang(
                styled(format!("{}  ", e.clock), dim()),
                &e.text,
                e.kind.style(),
            );
        }
        self.frame(
            "Activity",
            b,
            Layout::default(),
            Some(Area::Events),
            true,
            w,
            h,
        )
    }

    /// The open drop-down menu and its position under its button.
    fn menu_box(&mut self, st: Option<&State>, hits: &[Hit]) -> Option<(Layout, usize, usize)> {
        let menu = self.menu?;
        // A menu whose button is gone, as after losing the adapter, closes
        // rather than keep the keys.
        let (Some(anchor), Some(st)) = (hits.iter().find(|h| h.action == Action::Menu(menu)), st)
        else {
            self.menu = None;
            return None;
        };
        let anchor = anchor.clone();
        let items: Vec<(&str, Action, Tone)> = match menu {
            Menu::Scan => scan_choices(st)
                .into_iter()
                .map(|t| {
                    let label = match t {
                        None => "Bluetooth LE and Classic",
                        Some(Transport::Ble) => "Bluetooth LE Only",
                        Some(_) => "Classic Only",
                    };
                    (label, Action::Scan(t), Tone::Normal)
                })
                .collect(),
        };
        let w = items
            .iter()
            .map(|(label, ..)| text::width(label) + 4)
            .max()
            .unwrap_or(0)
            .min(self.width);
        let mut body = Layout::new(w.saturating_sub(4));
        let count = items.len();
        for (label, action, tone) in items {
            let look = if tone == Tone::Danger {
                err()
            } else {
                layout::plain()
            };
            body.control(styled(label, look), action);
        }
        let b = self.frame("", body, Layout::default(), None, false, w, count + 2);
        let x = anchor.x;
        Some((b, x.min(self.width.saturating_sub(w)), anchor.y + 1))
    }

    /// The modal drawn over the main view, if any.
    fn dialog_box(&mut self, st: Option<&State>) -> Option<Layout> {
        let a = self.auth();
        let mut w = self.width.min(60);
        let mut body = Layout::new(w - 4);
        let mut pinned = Layout::new(w - 4);
        let mut heading = String::new();
        if let Some((candidate, prompt)) = &a {
            let name = self.label(Item::Candidate(*candidate));
            heading = format!("Pair With {name}");
            let code = |body: &mut Layout, value: &str| {
                let value = spaced(value);
                body.row();
                let indent = body.width.saturating_sub(text::width(&value)) / 2;
                body.line(join(Line::from(" ".repeat(indent)), styled(value, title())));
            };
            match prompt {
                Prompt::ShowCode(_, value) => {
                    body.para(
                        &format!("Enter this code on {name}, then press Enter on it."),
                        layout::plain(),
                    );
                    code(&mut body, value);
                    pinned.button("Cancel Pairing", Action::CancelDialog, Tone::Normal);
                }
                Prompt::ConfirmCode(value) => {
                    body.para(
                        &format!("Check that {name} shows the same code:"),
                        layout::plain(),
                    );
                    code(&mut body, value);
                    pinned.button("Codes Match", Action::Accept, Tone::Primary);
                    pinned.button("Codes Differ", Action::Reject, Tone::Danger);
                }
                Prompt::EnterCode(_) => {
                    let prompt = if code_kind(prompt) == CodeKind::Pin {
                        format!("Enter the PIN for {name}.")
                    } else {
                        format!("Enter the six-digit passkey shown on {name}.")
                    };
                    body.para(&prompt, layout::plain());
                    let field = self.field_line(body.width.saturating_sub(3));
                    body.control(field, Action::Input);
                    pinned.button("Pair", Action::Accept, Tone::Primary);
                    pinned.button("Reject", Action::CancelDialog, Tone::Normal);
                }
            }
            if !self.form_err.is_empty() {
                body.para(&self.form_err.clone(), err());
            }
        } else {
            match self.dialog.clone() {
                Some(Dialog::Diagnostics) => {
                    let st = st?;
                    heading = "Diagnostics".into();
                    if let (Some(d), _) = Self::find(st, self.selected) {
                        heading = format!("Diagnostics · {}", display_name(Some(&d.name)));
                        if pending_for(st, "device refresh", d.id) {
                            body.para(&format!("{} Reading…", spinner()), warn());
                        }
                        diagnostics(&mut body, st, d);
                        if connected(d) {
                            pinned.button("Refresh", Action::RefreshInfo, Tone::Normal);
                        }
                    } else {
                        body.para("Select a saved device to see its diagnostics.", dim());
                    }
                    pinned.button("Close", Action::CancelDialog, Tone::Normal);
                }
                Some(Dialog::Help) => {
                    w = self.width.min(80);
                    body.width = w - 4;
                    pinned.width = w - 4;
                    // With an adapter, only what it offers is described.
                    let mut first = true;
                    for (section, rows) in HELP {
                        let rows: Vec<(&str, String)> = rows
                            .iter()
                            .filter_map(|(key, text)| {
                                let text = match st {
                                    Some(st) => help_text(st, section, key, text)?,
                                    None => (*text).to_owned(),
                                };
                                Some((*key, text))
                            })
                            .collect();
                        if rows.is_empty() {
                            continue;
                        }
                        if !std::mem::take(&mut first) {
                            body.row();
                        }
                        body.line(styled(*section, title()));
                        for (key, text) in rows {
                            // A key too long for the column gets a line of its own.
                            if text::width(key) > 10 {
                                body.line(styled(key.to_string(), bold()));
                                body.hang(styled(pad_str("", 12), bold()), &text, layout::plain());
                            } else {
                                body.hang(styled(pad_str(key, 12), bold()), &text, layout::plain());
                            }
                        }
                    }
                    pinned.button("Close", Action::CancelDialog, Tone::Normal);
                }
                Some(Dialog::Rename) => {
                    heading = "Rename Adapter".into();
                    let field = self.field_line(body.width.saturating_sub(3));
                    body.control(field, Action::Input);
                    if !self.form_err.is_empty() {
                        body.para(&format!("✕ {}", self.form_err), err());
                    }
                    if self.renaming() {
                        body.para("Saving…", warn());
                    } else if st.is_some_and(can_set_platform) {
                        pinned.button("Rename", Action::SaveName, Tone::Primary);
                        pinned.button("Reset to Default", Action::ResetName, Tone::Normal);
                    }
                    pinned.button("Cancel", Action::CancelDialog, Tone::Normal);
                }
                Some(
                    Dialog::ProfileName(_)
                    | Dialog::ProfileDelete(_)
                    | Dialog::SaveAdapter
                    | Dialog::ProfilePick(_),
                ) => {
                    let st = st?;
                    self.profiles_dialog(st, &mut heading, &mut body, &mut pinned);
                }
                Some(Dialog::Bootloader) => {
                    heading = "Enter Bootloader".into();
                    body.para("Restart the adapter into USB programming mode?", bold());
                    body.row();
                    body.para("Keyboard and mouse input stops until the adapter restarts. Saved bonds are kept and no firmware is installed.", layout::plain());
                    pinned.button("Enter Bootloader", Action::Confirm, Tone::Danger);
                    pinned.button("Cancel", Action::CancelDialog, Tone::Normal);
                }
                Some(Dialog::Replace(target)) => {
                    heading = "Replace File".into();
                    let name = display(&target.local.display().to_string());
                    body.para(&format!("{name} already exists. Replace it?"), bold());
                    body.row();
                    body.para(
                        "The existing file is replaced only after the whole download succeeds.",
                        layout::plain(),
                    );
                    pinned.button("Replace", Action::Confirm, Tone::Danger);
                    pinned.button("Cancel", Action::CancelDialog, Tone::Normal);
                }
                Some(Dialog::Remove(id)) => {
                    heading = format!("Forget “{}”?", self.label(Item::Device(id)));
                    body.para(
                        "Forgetting this device deletes its pairing and saved settings from the adapter.",
                        layout::plain(),
                    );
                    pinned.button("Forget", Action::Confirm, Tone::Danger);
                    pinned.button("Cancel", Action::CancelDialog, Tone::Normal);
                }
                None => {
                    let gate = self.gate()?;
                    let mut right = Layout::new(pinned.width);
                    match gate {
                        "loading" => {
                            heading = "Cordial".into();
                            body.para(&format!("{} Looking for adapters…", spinner()), warn());
                        }
                        "connecting" => {
                            heading = "Connecting".into();
                            body.para(
                                &format!("{} Connecting to {}…", spinner(), display(&self.port)),
                                warn(),
                            );
                            body.row();
                            let detail = if self.opening.is_some() || self.reconnecting() {
                                "Opening the port and checking the adapter.".to_string()
                            } else if st.is_some_and(|s| !s.status.ready) {
                                "Waiting for adapter… Its radio and saved devices are starting."
                                    .into()
                            } else {
                                "Loading the adapter's saved devices.".into()
                            };
                            body.para(&detail, dim());
                        }
                        _ => {
                            w = self.width.min(68);
                            body.width = w - 4;
                            pinned.width = w - 4;
                            right.width = w - 4;
                            heading = "Choose Adapter".into();
                            if let Some(e) = &self.last_err {
                                body.para(
                                    &format!("Couldn't open {}: {e}", display(&self.port)),
                                    err(),
                                );
                                body.row();
                            }
                            let port_w = (body.width / 2).saturating_sub(2).clamp(8, 24);
                            if !self.ports.is_empty() {
                                body.line(styled(
                                    format!("  {} SERIAL", pad_str("PORT", port_w)),
                                    dim(),
                                ));
                            }
                            for p in self.ports.clone() {
                                let marker = if self.session.is_some() && p.port == self.port {
                                    span("● ", accent())
                                } else {
                                    span("  ", Style::new())
                                };
                                let line = Line::from(vec![
                                    marker,
                                    span(
                                        format!(
                                            "{} {}",
                                            pad_str(
                                                &layout::truncate_str(&display(&p.port), port_w),
                                                port_w
                                            ),
                                            display(p.id())
                                        ),
                                        Style::new(),
                                    ),
                                ]);
                                body.control(line, Action::Port(p.port.clone()));
                            }
                            if let Some(e) = &self.list_err {
                                body.para(
                                    &format!(
                                        "Cordial couldn't list adapters. {} Select Refresh to try again.",
                                        text::sentence(&display(e))
                                    ),
                                    err(),
                                );
                            } else if self.ports.is_empty() {
                                body.para("No adapters found. Connect one and click Refresh, or start cordial with --port for another serial port.", dim());
                            }
                            pinned.button("Refresh", Action::RefreshPorts, Tone::Primary);
                            if self.session.is_some() {
                                right.button("Back", Action::CancelDialog, Tone::Normal);
                            }
                        }
                    }
                    right.button("Help", Action::Help, Tone::Normal);
                    right.button("Quit", Action::Quit, Tone::Normal);
                    pinned.align_right(right);
                }
            }
        }
        if heading.is_empty() {
            heading = "Help".into();
        }
        let h = self.height.min(body.lines.len() + pinned.lines.len() + 3);
        Some(self.frame(&heading, body, pinned, Some(Area::Dialog), false, w, h))
    }

    /// The pairing-code field: its prompt, the visible text and a cursor
    /// while it has focus.
    pub(super) fn field_line(&mut self, w: usize) -> Styled {
        edit_line(&mut self.form, self.form_focused, w)
    }
}

/// A text field: its prompt, the visible text and a cursor while it has focus.
pub(super) fn edit_line(field: &mut crate::ui::field::Field, focused: bool, w: usize) -> Styled {
    let (value, column) = field.view(w.saturating_sub(3).max(1));
    let mut line = Line::from(vec![span("> ", Style::new())]);
    if !focused {
        line.spans.push(span(value, Style::new()));
        return line;
    }
    // The cursor is drawn in reverse video on its character or a space.
    let before: String = strip(&Line::from(value.clone()))
        .chars()
        .scan(0, |used, c| {
            *used += text::width(&c.to_string());
            (*used <= column).then_some(c)
        })
        .collect();
    let rest = &value[before.len()..];
    let mut chars = rest.chars();
    let at = chars.next().map_or(" ".to_string(), |c| c.to_string());
    line.spans.push(span(before, Style::new()));
    line.spans
        .push(span(at, Style::new().add_modifier(Modifier::REVERSED)));
    line.spans
        .push(span(chars.as_str().to_owned(), Style::new()));
    line
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_are_spaced() {
        assert_eq!(spaced("482916"), "4 8 2   9 1 6");
        assert_eq!(spaced("1234"), "1 2 3 4");
        assert_eq!(spaced("a\u{202e}"), "a\\u202e");
    }

    #[test]
    fn help_never_pairs_from_saved() {
        let st = crate::ui::command::tests::state();
        for (section, rows) in HELP {
            for (key, text) in *rows {
                let shown = help_text(&st, section, key, text).unwrap_or_default();
                for words in [text, &shown.as_str()] {
                    let lower = words.to_lowercase();
                    assert!(
                        !lower.contains("saved one again")
                            && !lower.contains("this app recognizes"),
                        "{section} {key}: {words}"
                    );
                }
            }
        }
        let (_, keys) = HELP
            .iter()
            .find(|(s, _)| *s == "Mouse and Keyboard")
            .unwrap();
        for key in ["Enter", "p"] {
            let text = keys.iter().find(|(k, _)| *k == key).unwrap().1;
            let shown = help_text(&st, "Mouse and Keyboard", key, text).unwrap();
            assert!(shown.contains("Nearby"), "{key}: {shown}");
        }
        // On and Off option keys are described wherever the controls are.
        let tab = keys.iter().find(|(k, _)| *k == "Tab").unwrap().1;
        let shown = help_text(&st, "Mouse and Keyboard", "Tab", tab).unwrap();
        assert!(shown.contains("Enter or Space chooses"), "{shown}");
    }

    #[test]
    fn help_describes_profiles_only_with_profile_support() {
        let mut st = crate::ui::command::tests::state();
        let entries = || {
            HELP.iter().flat_map(|(section, rows)| {
                rows.iter()
                    .filter(|(key, _)| *key == "Profiles…")
                    .map(move |(key, text)| (*section, *key, *text))
            })
        };
        assert_eq!(entries().count(), 2);
        for (section, key, text) in entries() {
            assert!(help_text(&st, section, key, text).is_none(), "{section}");
        }
        crate::ui::command::tests::with_profiles(&mut st.status);
        for (section, key, text) in entries() {
            assert!(help_text(&st, section, key, text).is_some(), "{section}");
        }
    }
}
