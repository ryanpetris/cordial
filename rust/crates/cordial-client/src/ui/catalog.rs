//! Presentation of device settings. The adapter reports semantic keys with
//! machine-readable metadata only; names, grouping, choice wording, readout
//! decoding and value parsing belong to the host.
use crate::{
    controller::{Command, DeviceSettings, SettingInput, State, Subject},
    ui::text::{hidpp_words, name, safe, wire},
};
use cordial_protocol::{
    identifiers::ConnectionState,
    messages::Device,
    settings::{
        ObservationSource, Setting, SettingKey, SettingOutcome, SettingScope, SettingState,
        SettingType, SettingValue,
    },
};
use std::fmt::Write;

/// One named field of a decoded readout; unnamed facts form its summary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Fact {
    pub name: String,
    pub value: String,
}
fn fact(name: &str, value: impl Into<String>) -> Fact {
    Fact {
        name: name.into(),
        value: value.into(),
    }
}

type Decode = fn(&Setting, &[u8]) -> Option<Vec<Fact>>;

pub struct Info {
    pub key: SettingKey,
    pub label: &'static str,
    pub category: &'static str,
    /// The unit integer values are in, or "".
    pub unit: &'static str,
    /// Wording of enum tokens that plain capitalization would garble.
    choices: &'static [(&'static str, &'static str)],
    /// Reads a text value the adapter sends as reply bytes in lowercase hex.
    decode: Option<Decode>,
}

const fn info(key: SettingKey, label: &'static str, category: &'static str) -> Info {
    Info {
        key,
        label,
        category,
        unit: "",
        choices: &[],
        decode: None,
    }
}

/// Recognized settings in display order; categories appear in the order of
/// their first setting.
static CATALOG: [Info; 20] = {
    use SettingKey::*;
    [
        Info {
            choices: &[
                ("function_keys", "F1-F12"),
                ("special_actions", "Shortcuts"),
            ],
            ..info(FnRowDefault, "Function Row", "Keyboard")
        },
        info(BacklightEnabled, "Backlight", "Backlight"),
        info(BacklightMode, "Backlight Mode", "Backlight"),
        info(BacklightLevel, "Manual Backlight Level", "Backlight"),
        info(
            BacklightCurrentLevel,
            "Current Backlight Level",
            "Backlight",
        ),
        Info {
            choices: &[
                ("battery", "Off (Battery)"),
                ("saturated", "Automatic (Saturated)"),
            ],
            ..info(BacklightStatus, "Backlight Status", "Backlight")
        },
        info(BacklightEffect, "Backlight Effect", "Backlight"),
        Info {
            unit: "s",
            ..info(
                BacklightDelayHandsOut,
                "Timeout With Hands Away",
                "Backlight",
            )
        },
        Info {
            unit: "s",
            ..info(
                BacklightDelayHandsIn,
                "Timeout With Hands Nearby",
                "Backlight",
            )
        },
        Info {
            unit: "s",
            ..info(
                BacklightDelayPowered,
                "Timeout While Plugged In",
                "Backlight",
            )
        },
        info(BacklightPowerOn, "Backlight at Power-On", "Backlight"),
        info(BacklightCrown, "Crown Backlight", "Backlight"),
        info(BacklightPowerSave, "Backlight Power Saving", "Backlight"),
        Info {
            unit: "DPI",
            ..info(PointerDpi0, "Pointer Speed", "Pointer")
        },
        Info {
            unit: "DPI",
            ..info(PointerDpi1, "Second Sensor Speed", "Pointer")
        },
        info(WheelMode, "Wheel Mode", "Wheel"),
        info(WheelThreshold, "SmartShift", "Wheel"),
        info(WheelInvert, "Reverse Vertical Scrolling", "Wheel"),
        Info {
            decode: Some(wheel_facts),
            ..info(WheelInfo, "Wheel Capabilities", "Wheel")
        },
        info(ThumbwheelInvert, "Reverse Horizontal Scrolling", "Wheel"),
    ]
};

/// A setting's presentation and its display position.
pub fn info_for(key: SettingKey) -> (&'static Info, usize) {
    let i = CATALOG.iter().position(|i| i.key == key).unwrap();
    (&CATALOG[i], i)
}
pub fn label(key: SettingKey) -> &'static str {
    info_for(key).0.label
}
pub fn category(key: SettingKey) -> &'static str {
    info_for(key).0.category
}

/// Settings in display order.
pub fn presented(settings: &[Setting]) -> Vec<&Setting> {
    let mut known: Vec<&Setting> = settings.iter().collect();
    known.sort_by_key(|s| info_for(s.key).1);
    known
}

/// Categories of the settings in display order.
pub fn categories(settings: &[Setting]) -> Vec<&'static str> {
    let mut out = Vec::new();
    for s in presented(settings) {
        let c = category(s.key);
        if !out.contains(&c) {
            out.push(c);
        }
    }
    out
}

/// Words an enum token: the catalog's wording, else the token in title case
/// with spaces for separators.
pub fn choice_words(key: SettingKey, token: &str) -> String {
    if let Some((_, words)) = info_for(key).0.choices.iter().find(|(t, _)| *t == token) {
        return (*words).into();
    }
    token
        .replace(['_', '-'], " ")
        .split(' ')
        .map(|w| {
            let mut chars = w.chars();
            match chars.next() {
                Some(c) if c.is_ascii_lowercase() => {
                    c.to_ascii_uppercase().to_string() + chars.as_str()
                }
                _ => w.to_owned(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// A value as the shell accepts it: on/off, the integer, or the text itself.
pub fn value_string(v: &SettingValue) -> String {
    match v {
        SettingValue::Null => "unavailable".into(),
        SettingValue::Bool(b) => if *b { "on" } else { "off" }.into(),
        SettingValue::Integer(n) => n.to_string(),
        SettingValue::Text(s) => s.clone(),
    }
}

fn hex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(text.get(i..i + 2)?, 16).ok())
        .collect()
}

/// Decodes a hex readout into named fields; None for other settings and for
/// values without the documented layout.
pub fn readout(s: &Setting, v: &SettingValue) -> Option<Vec<Fact>> {
    let decode = info_for(s.key).0.decode?;
    let SettingValue::Text(text) = v else {
        return None;
    };
    if s.kind != SettingType::Text || *text != text.to_lowercase() {
        return None;
    }
    decode(s, &hex(text)?)
}

/// A decoded readout on one line, or the raw value marked unrecognized when it
/// cannot be decoded. Undocumented values are never given a guessed name.
pub fn readout_text(s: &Setting, v: &SettingValue) -> Option<String> {
    if info_for(s.key).0.decode.is_none() || *v == SettingValue::Null {
        return None;
    }
    match readout(s, v) {
        None => Some(format!("Unrecognized: {}", value_string(v))),
        Some(facts) => Some(
            facts
                .iter()
                .filter(|f| f.name.is_empty())
                .map(|f| f.value.as_str())
                .collect::<Vec<_>>()
                .join(", "),
        ),
    }
}

/// The set bits of mask by name, or as "bit N".
fn bit_names(mask: u16, names: &[(u16, &str)]) -> Vec<String> {
    (0..16)
        .filter(|bit| mask & (1 << bit) != 0)
        .map(|bit| {
            names
                .iter()
                .find(|(b, _)| *b == bit)
                .map_or_else(|| format!("bit {bit}"), |(_, n)| (*n).into())
        })
        .collect()
}

fn list_or_none(items: &[String]) -> String {
    if items.is_empty() {
        "none".into()
    } else {
        items.join(", ")
    }
}

/// A HiResWheel getWheelCapability reply: multiplier and capability bits,
/// then from revision 1 ratchets per rotation and diameter.
fn wheel_facts(s: &Setting, b: &[u8]) -> Option<Vec<Fact>> {
    if b.len() != if s.feature_version.0 == 0 { 2 } else { 4 } {
        return None;
    }
    let mut names = vec![(2, "ratchet switch"), (3, "inversion")];
    if s.feature_version.0 >= 1 {
        names.push((4, "statistics"));
    }
    let multiplier = b[0].to_string();
    let mut facts = vec![
        fact("", format!("Multiplier {multiplier}")),
        fact("Resolution Multiplier", &multiplier),
        fact(
            "Capabilities",
            list_or_none(&bit_names(u16::from(b[1]), &names)),
        ),
    ];
    if b.len() == 4 {
        facts.push(fact(
            "",
            format!("{} ratchets per rotation, {} mm", b[2], b[3]),
        ));
        facts.push(fact("Ratchets per Rotation", b[2].to_string()));
        facts.push(fact("Wheel Diameter", format!("{} mm", b[3])));
    }
    Some(facts)
}

/// Where an integer range's steps start.
pub fn base(s: &Setting) -> i64 {
    s.min.unwrap_or(0)
}

/// A setting's legal values.
pub fn legal_text(s: &Setting) -> String {
    if s.kind == SettingType::Bool {
        return "on or off".into();
    }
    if !s.choices.is_empty() {
        let names: Vec<String> = s.choices.iter().map(|c| safe(&value_string(c))).collect();
        return format!("one of {}", names.join(", "));
    }
    if s.kind == SettingType::Integer {
        let mut text = String::from("an integer");
        match (s.min, s.max) {
            (Some(min), Some(max)) => {
                let _ = write!(text, " from {min} through {max}");
            }
            (Some(min), None) => {
                let _ = write!(text, " of at least {min}");
            }
            (None, Some(max)) => {
                let _ = write!(text, " of at most {max}");
            }
            (None, None) => {}
        }
        if let Some(step) = s.step.filter(|s| *s > 1) {
            let _ = write!(text, " in steps of {step}");
        }
        return text;
    }
    "text".into()
}

/// Types a command-line value from a setting's metadata.
fn parse_value(s: &Setting, text: &str) -> Result<SettingValue, String> {
    let key = wire(&s.key);
    let invalid = || format!("{key} takes {}", legal_text(s));
    let v = match s.kind {
        SettingType::Bool => match text.to_lowercase().as_str() {
            "on" | "true" | "yes" => SettingValue::Bool(true),
            "off" | "false" | "no" => SettingValue::Bool(false),
            _ => return Err(format!("{key} takes on or off")),
        },
        SettingType::Integer => SettingValue::Integer(text.parse().map_err(|_| invalid())?),
        SettingType::Enum => s
            .choices
            .iter()
            .find(|c| matches!(c, SettingValue::Text(t) if t.eq_ignore_ascii_case(text)))
            .cloned()
            .ok_or_else(invalid)?,
        SettingType::Text => {
            if text.len() > 128 {
                return Err(format!("{key} takes at most 128 bytes"));
            }
            SettingValue::Text(text.into())
        }
    };
    if !s.accepts(&v) {
        return Err(invalid());
    }
    Ok(v)
}

/// The value to save for a `hidpp setting set`: typed text parsed as the shell
/// accepts it, or a value chosen in the TUI, which must be legal. Read-only
/// information is refused before any value is examined.
pub fn setting_value(s: &Setting, input: &SettingInput) -> Result<SettingValue, String> {
    if !s.writable {
        return Err(format!("{} is read-only information", wire(&s.key)));
    }
    match input {
        SettingInput::Text(text) => parse_value(s, text),
        SettingInput::Value(v) if s.accepts(v) => Ok(v.clone()),
        SettingInput::Value(_) => Err(format!("{} takes {}", wire(&s.key), legal_text(s))),
    }
}

/// A writable setting's apply state.
pub fn apply_state(s: &Setting) -> &'static str {
    match s.state {
        SettingState::Unmanaged => "Default",
        SettingState::Pending => "pending",
        SettingState::Applying => "applying",
        SettingState::Applied => "applied",
        SettingState::ChangedOnDevice => "changed on device",
        SettingState::Unsupported => "unsupported by the device now",
        SettingState::Error => "failed",
        SettingState::Uncertain => "uncertain; apply rereads it",
    }
}

/// How current an observation is.
fn freshness(s: &Setting) -> &'static str {
    match () {
        _ if s.observed == SettingValue::Null => "unavailable",
        _ if !s.fresh => "stale",
        _ if s.observation_source == Some(ObservationSource::Event) => "fresh, reported by device",
        _ => "fresh",
    }
}

fn scope_text(s: &Setting) -> &'static str {
    match s.scope {
        SettingScope::CurrentHost => "this computer's host slot on the device",
        SettingScope::Device => "the whole device, including other computers it is paired with",
    }
}

/// A value as the shell accepts it, except that encoded readouts are decoded.
pub fn value_text(s: &Setting, v: &SettingValue) -> String {
    safe(&readout_text(s, v).unwrap_or_else(|| value_string(v)))
}

/// One setting of `hidpp setting list DEV`.
pub fn setting_line(s: &Setting) -> String {
    let key = safe(&wire(&s.key));
    let label = safe(label(s.key));
    if !s.writable {
        let mut line = format!(
            "  {key}  {label}: {} ({})",
            value_text(s, &s.observed),
            freshness(s)
        );
        if let Some(code) = s.error {
            let _ = write!(line, "; read failed: {}", hidpp_words(code));
        }
        return line;
    }
    let mut line = format!(
        "  {key}  {label}: last observed on device: {} ({})",
        value_text(s, &s.observed),
        freshness(s)
    );
    if s.managed {
        let _ = write!(
            line,
            "; saved on dongle: {}, {}",
            safe(&value_string(&s.desired)),
            apply_state(s)
        );
    } else {
        line.push_str("; Default");
    }
    if let Some(code) = s.error {
        if s.managed {
            let _ = write!(line, ": {}", hidpp_words(code));
        } else {
            let _ = write!(line, "; read failed: {}", hidpp_words(code));
        }
    }
    line
}

/// Labels records that are not a current reading of the device, and saved
/// values that are not being applied.
fn cached_note(d: &Device) -> &'static str {
    if d.state != ConnectionState::Connected {
        " (disconnected; last known values)"
    } else if !d.hidpp_enabled {
        " (HID++ off: values are read, saved values are not applied)"
    } else {
        ""
    }
}

/// `hidpp setting list DEV`, grouped by category.
pub fn setting_list(d: &Device, c: &DeviceSettings) -> String {
    let mut b = format!(
        "Settings of {}: {}{}",
        name(d.name.as_deref()),
        c.state.map(|s| wire(&s)).unwrap_or_default(),
        cached_note(d)
    );
    if let Some(code) = c.error {
        let _ = write!(b, "; error: {}", hidpp_words(code));
    }
    if c.settings.is_empty() {
        b.push_str("\n  No settings discovered.");
    }
    let known = presented(&c.settings);
    for category in categories(&c.settings) {
        let _ = write!(b, "\n{category}");
        for s in known.iter().filter(|s| self::category(s.key) == category) {
            let _ = write!(b, "\n{}", setting_line(s));
        }
    }
    b
}

/// `hidpp setting get`, one field per line.
pub fn setting_detail(d: &Device, s: &Setting) -> String {
    let info = info_for(s.key).0;
    let mut b = format!(
        "Setting {} of {}{}",
        safe(&wire(&s.key)),
        name(d.name.as_deref()),
        cached_note(d)
    );
    let mut field = |key: &str, value: &str| {
        let _ = write!(b, "\n  {key}: {value}");
    };
    field("Label", info.label);
    field("Category", info.category);
    if s.writable {
        field("Values", &legal_text(s));
        field("Applies To", scope_text(s));
    } else {
        field("Values", "read-only information");
    }
    field(
        "Last Observed on Device",
        &format!("{} ({})", value_text(s, &s.observed), freshness(s)),
    );
    if let Some(facts) = readout(s, &s.observed) {
        for f in facts.iter().filter(|f| !f.name.is_empty()) {
            field(&format!("  {}", f.name), &safe(&f.value));
        }
    }
    if info.decode.is_some() && s.observed != SettingValue::Null {
        field("Raw Value", &safe(&value_string(&s.observed)));
    }
    if s.writable {
        if s.managed {
            field(
                "Saved on Dongle",
                &format!("yes, {}", safe(&value_string(&s.desired))),
            );
        } else {
            field("Saved on Dongle", "no (Default)");
        }
        field("State", apply_state(s));
        if s.managed && !d.hidpp_enabled {
            field(
                "Applied",
                "no; HID++ is off, so the saved value is applied when it is turned on",
            );
        }
    }
    if let Some(code) = s.error {
        field(
            if s.managed { "Error" } else { "Read Failed" },
            &hidpp_words(code),
        );
    }
    field(
        "Feature",
        &format!("0x{:04X} version {}", s.feature.0, s.feature_version.0),
    );
    b
}

/// Formats the cached feature inventory for `hidpp feature list DEV`.
pub fn feature_list(d: &Device, c: &DeviceSettings) -> String {
    let mut b = format!(
        "Features last discovered on {}{}",
        name(d.name.as_deref()),
        cached_note(d)
    );
    if c.features.is_empty() {
        b.push_str("\n  None discovered.");
    } else {
        b.push_str("\n  INDEX  FEATURE  VERSION  HANDLER");
    }
    for f in &c.features {
        let handler = if f.supported { "supported" } else { "none" };
        let _ = write!(
            b,
            "\n  {:5}  0x{:04X}   {:7}  {handler}",
            f.index.0, f.id.0, f.version.0
        );
    }
    b
}

fn device<'a>(state: Option<&'a State>, id: &str) -> Option<&'a Device> {
    state?.devices.iter().find(|d| d.device_id.0 == id)
}

/// `hidpp feature list DEV` or `hidpp setting list DEV` from the session's cache.
pub fn subject_text(subject: &Subject, state: Option<&State>, features: bool) -> String {
    let Some(d) = device(state, &subject.id) else {
        return String::new();
    };
    let empty = DeviceSettings::default();
    let c = state
        .and_then(|s| s.settings.get(&d.device_id))
        .unwrap_or(&empty);
    if features {
        feature_list(d, c)
    } else {
        setting_list(d, c)
    }
}

/// The result of `hidpp setting get`, `set` or `forget`.
pub fn setting_result(
    command: &Command,
    subject: &Subject,
    s: &Setting,
    state: Option<&State>,
) -> String {
    let key = safe(&wire(&s.key));
    let hidpp_on = device(state, &subject.id).is_none_or(|d| d.hidpp_enabled);
    match command {
        Command::SettingGet(..) => match device(state, &subject.id) {
            Some(d) => setting_detail(d, s),
            None => setting_line(s),
        },
        Command::SettingSet(..) if !hidpp_on => format!(
            "Saved on dongle: {key} = {} (not applied: HID++ is off).\nThe device keeps its current value. The saved value is applied when HID++ is turned on.",
            safe(&value_string(&s.desired))
        ),
        Command::SettingSet(..) => format!(
            "Saved on dongle: {key} = {} ({}).\nIt is reapplied when the device reconnects with HID++ on, when HID++ is turned on, and after an adapter platform change.",
            safe(&value_string(&s.desired)),
            apply_state(s)
        ),
        Command::SettingForget(..) => {
            format!("Default: forgot the saved value of {key}; the device was left unchanged.")
        }
        _ => String::new(),
    }
}

/// The per-setting results of `hidpp setting refresh` or `hidpp setting apply`, including those
/// before a failure, then the summary counts.
pub fn job_text(
    command: &Command,
    rows: &[(Setting, SettingOutcome)],
    counts: Option<&crate::controller::JobCounts>,
) -> String {
    let mut lines: Vec<String> = rows
        .iter()
        .map(|(s, outcome)| {
            let mut line = format!(
                "  {}  {}: {}",
                safe(&wire(&s.key)),
                wire(outcome),
                value_text(s, &s.observed)
            );
            if let Some(code) = s.error {
                let _ = write!(line, " ({})", hidpp_words(code));
            }
            line
        })
        .collect();
    if let Some(summary) = counts.map(|c| job_summary(command, c)) {
        lines.push(summary);
    }
    lines.join("\n")
}

/// Refresh or Apply failure counts: only those that occurred.
pub fn failure_summary(c: &crate::controller::JobCounts) -> String {
    [
        ("failed", c.failed),
        ("unsupported", c.unsupported),
        ("uncertain", c.uncertain),
    ]
    .iter()
    .filter(|(_, n)| *n > 0)
    .map(|(o, n)| format!("{n} {o}"))
    .collect::<Vec<_>>()
    .join(", ")
}

/// Refresh or Apply counts: those that occurred, and always the main one.
pub fn job_summary(command: &Command, c: &crate::controller::JobCounts) -> String {
    let apply = matches!(command, Command::SettingsApply(_));
    [
        ("read", c.read),
        ("applied", c.applied),
        ("unchanged", c.unchanged),
        ("unsupported", c.unsupported),
        ("failed", c.failed),
        ("uncertain", c.uncertain),
    ]
    .iter()
    .filter(|(o, n)| *n > 0 || *o == "applied" && apply || *o == "read" && !apply)
    .map(|(o, n)| format!("{n} {o}"))
    .collect::<Vec<_>>()
    .join(", ")
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use cordial_protocol::hidpp::{FeatureId, FeatureRevision};

    pub fn setting(key: SettingKey) -> Setting {
        Setting {
            key,
            kind: key.kind(),
            writable: false,
            feature: FeatureId(0x0003),
            feature_version: FeatureRevision(0),
            scope: SettingScope::Device,
            choices: Vec::new(),
            min: None,
            max: None,
            step: None,
            managed: false,
            desired: SettingValue::Null,
            observed: SettingValue::Null,
            fresh: false,
            observed_at_ms: None,
            observation_source: None,
            state: SettingState::Unmanaged,
            error: None,
        }
    }

    #[test]
    fn catalog_covers_every_key_once() {
        for key in SettingKey::ALL {
            assert_eq!(
                CATALOG.iter().filter(|i| i.key == key).count(),
                1,
                "{key:?}"
            );
        }
        assert_eq!(choice_words(SettingKey::WheelMode, "freespin"), "Freespin");
        assert_eq!(
            choice_words(SettingKey::BacklightMode, "permanent_manual"),
            "Permanent Manual"
        );
        assert_eq!(
            choice_words(SettingKey::FnRowDefault, "function_keys"),
            "F1-F12"
        );
    }

    #[test]
    fn readouts_decode_documented_layouts() {
        let mut wheel = setting(SettingKey::WheelInfo);
        wheel.feature = FeatureId(0x2121);
        let v = SettingValue::Text("080c".into());
        assert_eq!(readout_text(&wheel, &v).unwrap(), "Multiplier 8");
        assert!(
            readout(&wheel, &v)
                .unwrap()
                .contains(&fact("Capabilities", "ratchet switch, inversion"))
        );

        let v = SettingValue::Text("zz".into());
        assert_eq!(readout_text(&wheel, &v).unwrap(), "Unrecognized: zz");
    }

    #[test]
    fn values_parse_from_metadata() {
        let mut s = setting(SettingKey::BacklightDelayPowered);
        s.writable = true;
        s.min = Some(5);
        s.max = Some(300);
        s.step = Some(5);
        assert_eq!(parse_value(&s, "25"), Ok(SettingValue::Integer(25)));
        assert_eq!(
            parse_value(&s, "26").unwrap_err(),
            "backlight.delay.powered takes an integer from 5 through 300 in steps of 5"
        );
        let mut m = setting(SettingKey::WheelMode);
        m.writable = true;
        m.choices = vec![
            SettingValue::Text("freespin".into()),
            SettingValue::Text("ratchet".into()),
        ];
        assert_eq!(
            parse_value(&m, "Ratchet"),
            Ok(SettingValue::Text("ratchet".into()))
        );
        assert_eq!(
            parse_value(&m, "x").unwrap_err(),
            "wheel.mode takes one of freespin, ratchet"
        );
        let mut b = setting(SettingKey::WheelInvert);
        b.writable = true;
        assert_eq!(parse_value(&b, "YES"), Ok(SettingValue::Bool(true)));
        assert_eq!(
            parse_value(&b, "2").unwrap_err(),
            "wheel.invert takes on or off"
        );
        assert!(setting_value(&b, &SettingInput::Value(SettingValue::Integer(1))).is_err());
    }

    #[test]
    fn read_only_settings_refuse_any_value() {
        let status = setting(SettingKey::BacklightStatus);
        for input in [
            SettingInput::Text("battery".into()),
            SettingInput::Value(SettingValue::Text("battery".into())),
        ] {
            assert_eq!(
                setting_value(&status, &input).unwrap_err(),
                "backlight.status is read-only information"
            );
        }
        let mut level = setting(SettingKey::BacklightCurrentLevel);
        level.min = Some(0);
        level.max = Some(10);
        level.step = Some(1);
        assert_eq!(
            setting_value(&level, &SettingInput::Text("x".into())).unwrap_err(),
            "backlight.current_level is read-only information"
        );
    }
}
