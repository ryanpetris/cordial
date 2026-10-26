//! Presentation of settings and the readings beside them. The adapter sends typed settings
//! objects and information; names, grouping, choice wording and value parsing belong to the host.
//! Fields of integrations this build doesn't know are never shown.
use crate::{
    controller::{SettingInput, State, Subject},
    model::{self, Up},
    settings::{self, Field, Leaf, Setting, Type, Value},
    ui::text::{hidpp_error_words, name, safe},
};
use cordial_protocol::{self as p, IntegrationKind, SettingState};
use std::fmt::Write;

pub struct Info {
    pub label: &'static str,
    /// The settings page's group.
    pub category: &'static str,
    /// The unit integer values are in, or "".
    pub unit: &'static str,
    /// Wording of enum values that plain capitalization would garble.
    choices: &'static [(&'static str, &'static str)],
}

const fn info(label: &'static str, category: &'static str) -> Info {
    Info {
        label,
        category,
        unit: "",
        choices: &[],
    }
}

/// A leaf's presentation; categories appear in the order of their first leaf.
pub fn info_for(leaf: Leaf) -> Info {
    use Leaf::*;
    match leaf {
        FunctionRow => Info {
            choices: &[
                ("function_keys", "F1-F12"),
                ("special_actions", "Shortcuts"),
            ],
            ..info("Function Row", "Keyboard")
        },
        KeyboardPlatform => Info {
            choices: &[
                ("windows", "Windows"),
                ("windows_embedded", "Windows Embedded"),
                ("linux", "Linux"),
                ("chrome_os", "ChromeOS"),
                ("android", "Android"),
                ("mac", "macOS"),
                ("ios", "iOS"),
                ("webos", "webOS"),
                ("tizen", "Tizen"),
            ],
            ..info("Keyboard Platform", "Keyboard")
        },
        BacklightEnabled => info("Backlight", "Backlight"),
        BacklightMode => info("Backlight Mode", "Backlight"),
        Brightness => info("Brightness", "Backlight"),
        CurrentBrightness => info("Current Brightness", "Backlight"),
        BacklightStatus => Info {
            choices: &[
                ("battery", "Off (Battery)"),
                ("saturated", "Automatic (Saturated)"),
            ],
            ..info("Backlight Status", "Backlight")
        },
        BacklightEffect => info("Backlight Effect", "Backlight"),
        HandsAway => Info {
            unit: "s",
            ..info("Hands Away Timeout", "Backlight")
        },
        HandsNearby => Info {
            unit: "s",
            ..info("Hands Nearby Timeout", "Backlight")
        },
        ExternalPower => Info {
            unit: "s",
            ..info("External Power Timeout", "Backlight")
        },
        StartupEffect => info("Startup Effect", "Backlight"),
        DialEffect => info("Dial Lighting Effect", "Backlight"),
        PowerSave => info("Backlight Power Saving", "Backlight"),
        Dpi => Info {
            unit: "DPI",
            ..info("Pointer Sensitivity", "Pointer")
        },
        WheelMode => info("Wheel Mode", "Wheel"),
        FreeSpinThreshold => info("Automatic Free Spin Threshold", "Wheel"),
        WheelInvert => info("Reverse Vertical Scrolling", "Wheel"),
        ResolutionMultiplier => info("Resolution Multiplier", "Wheel"),
        RatchetsPerRotation => info("Ratchets per Rotation", "Wheel"),
        Diameter => Info {
            unit: "mm",
            ..info("Wheel Diameter", "Wheel")
        },
        ThumbwheelInvert => info("Reverse Horizontal Scrolling", "Wheel"),
        AutoOff => Info {
            unit: "s",
            ..info("Automatic Power-Off", "Power")
        },
    }
}

/// A field's label. Sensors after the first are numbered, so the second reads
/// "Pointer Sensitivity 2".
pub fn label(field: &Field) -> String {
    let label = info_for(field.leaf).label;
    match field.index {
        Some(n) if n > 0 => format!("{label} {}", u64::from(n) + 1),
        _ => label.into(),
    }
}

pub fn category(leaf: Leaf) -> &'static str {
    info_for(leaf).category
}

pub fn unit(leaf: Leaf) -> &'static str {
    info_for(leaf).unit
}

/// Whether this build shows fields of the integration.
fn known(field: &Field) -> bool {
    field.integration == IntegrationKind::Hidpp as i32
}

/// Settings this build presents, in display order.
pub fn presented(settings: &[Setting]) -> Vec<&Setting> {
    let mut known: Vec<&Setting> = settings.iter().filter(|s| known(&s.field)).collect();
    known.sort_by_key(|s| s.field);
    known
}

/// Categories of the settings in display order.
pub fn categories(settings: &[Setting]) -> Vec<&'static str> {
    let mut out = Vec::new();
    for s in presented(settings) {
        let c = category(s.field.leaf);
        if !out.contains(&c) {
            out.push(c);
        }
    }
    out
}

/// Words an enum value: the catalog's wording, else the value in title case with spaces for
/// separators.
pub fn choice_words(leaf: Leaf, n: i32) -> String {
    let token = leaf.choice_token(n);
    if let Some((_, words)) = info_for(leaf).choices.iter().find(|(t, _)| *t == token) {
        return (*words).into();
    }
    token
        .split('_')
        .map(|w| {
            let mut chars = w.chars();
            match chars.next() {
                Some(c) => c.to_ascii_uppercase().to_string() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// A value as the shell accepts it: on/off, the integer, or the enum value's name.
pub fn value_string(leaf: Leaf, v: &Value) -> String {
    match v {
        Value::Bool(b) => if *b { "on" } else { "off" }.into(),
        Value::Integer(n) => n.to_string(),
        Value::Choice(n) => leaf.choice_token(*n),
    }
}

/// An optional value as the shell shows it.
pub fn maybe_value(leaf: Leaf, v: Option<&Value>) -> String {
    v.map_or_else(|| "unavailable".into(), |v| value_string(leaf, v))
}

/// A setting's legal values.
pub fn legal_text(s: &Setting) -> String {
    let choices = settings::choices(s);
    match settings::kind(s) {
        Type::Bool => "on or off".into(),
        _ if !choices.is_empty() => {
            let names: Vec<String> = choices
                .iter()
                .map(|c| value_string(s.field.leaf, c))
                .collect();
            format!("one of {}", names.join(", "))
        }
        Type::Integer => {
            let mut text = String::from("an integer");
            if let Some((min, max, step)) = settings::range(s) {
                let _ = write!(text, " from {min} through {max}");
                if step > 1 {
                    let _ = write!(text, " in steps of {step}");
                }
            }
            text
        }
        Type::Choice => "one of no values".into(),
    }
}

/// Why a value for `s` was refused: the values it takes, or that it has none.
fn rejected(s: &Setting) -> String {
    let path = s.field.path();
    if settings::kind(s) == Type::Choice && settings::choices(s).is_empty() {
        format!("{path} has no values to choose from")
    } else {
        format!("{path} takes {}", legal_text(s))
    }
}

/// Types a command-line value from a setting's type and limits.
fn parse_value(s: &Setting, text: &str) -> Result<Value, String> {
    let invalid = || rejected(s);
    let v = match settings::kind(s) {
        Type::Bool => match text.to_lowercase().as_str() {
            "on" | "true" | "yes" => Value::Bool(true),
            "off" | "false" | "no" => Value::Bool(false),
            _ => return Err(format!("{} takes on or off", s.field.path())),
        },
        Type::Integer => Value::Integer(text.parse().map_err(|_| invalid())?),
        Type::Choice => settings::choices(s)
            .into_iter()
            .find(|c| value_string(s.field.leaf, c).eq_ignore_ascii_case(text))
            .ok_or_else(invalid)?,
    };
    if !settings::accepts(s, &v) {
        return Err(invalid());
    }
    Ok(v)
}

/// The value to save for `setting set`: typed text parsed as the shell accepts it, or a value
/// chosen in the TUI, which must be legal.
pub fn setting_value(s: &Setting, input: &SettingInput) -> Result<Value, String> {
    match input {
        SettingInput::Text(text) => parse_value(s, text),
        SettingInput::Value(v) if settings::accepts(s, v) => Ok(*v),
        SettingInput::Value(_) => Err(rejected(s)),
    }
}

/// A saved setting's apply state.
pub fn apply_state(s: &Setting) -> String {
    match &s.status {
        None => "Default".into(),
        Some(Ok(SettingState::Pending)) => "pending".into(),
        Some(Ok(SettingState::Applied)) => "applied".into(),
        Some(Ok(SettingState::ChangedOnDevice)) => "changed on device".into(),
        Some(Ok(SettingState::Unknown)) => "unknown".into(),
        Some(Err(e)) if model::unsupported(e) && model::plain(e) => {
            "unsupported by the device now".into()
        }
        Some(Err(e)) => format!("failed: {}", hidpp_error_words(e)),
    }
}

/// Whether a setting's reading is current: its integration is up and it has been read.
pub fn fresh(d: &p::Device, s: &Setting) -> bool {
    s.value.is_some()
        && d.integrations
            .iter()
            .any(|i| i.kind == s.field.integration && model::up(i) == Up::Active)
}

fn freshness(d: &p::Device, s: &Setting) -> &'static str {
    match () {
        _ if s.value.is_none() => "unavailable",
        _ if !fresh(d, s) => "stale",
        _ => "fresh",
    }
}

/// A value with the field's unit, as the shell shows it.
pub fn value_text(leaf: Leaf, v: Option<&Value>) -> String {
    let mut text = safe(&maybe_value(leaf, v));
    if let (Some(Value::Integer(_)), u) = (v, unit(leaf))
        && !u.is_empty()
    {
        let _ = write!(text, " {u}");
    }
    text
}

/// One setting of `setting list DEV`.
pub fn setting_line(d: &p::Device, s: &Setting) -> String {
    let mut line = format!(
        "  {}  {}: last observed on device: {} ({})",
        s.field.path(),
        label(&s.field),
        value_text(s.field.leaf, s.value.as_ref()),
        freshness(d, s)
    );
    match &s.saved {
        Some(saved) => {
            let _ = write!(
                line,
                "; saved on adapter: {}, {}",
                value_string(s.field.leaf, saved),
                apply_state(s)
            );
        }
        None => line.push_str("; Default"),
    }
    line
}

/// Labels records that are not a current reading of the device, and saved values that are
/// not being applied.
fn cached_note(d: &p::Device) -> &'static str {
    if !model::connected(d) {
        " (disconnected)"
    } else if !model::hidpp_enabled(d) {
        " (HID++ off; last known values)"
    } else {
        ""
    }
}

/// `setting list DEV`, grouped by category.
pub fn setting_list(d: &p::Device, settings: &[Setting]) -> String {
    let mut b = format!(
        "Settings of {}: {}{}",
        name(Some(&d.name)),
        crate::ui::text::up_token(&model::hidpp_up(d)),
        cached_note(d)
    );
    let known = presented(settings);
    if known.is_empty() {
        b.push_str("\n  No settings discovered.");
    }
    for category in categories(settings) {
        let _ = write!(b, "\n{category}");
        for s in known
            .iter()
            .filter(|s| self::category(s.field.leaf) == category)
        {
            let _ = write!(b, "\n{}", setting_line(d, s));
        }
    }
    b
}

/// `setting get`, one field per line.
pub fn setting_detail(d: &p::Device, s: &Setting) -> String {
    let mut b = format!(
        "Setting {} of {}{}",
        s.field.path(),
        name(Some(&d.name)),
        cached_note(d)
    );
    let mut field = |key: &str, value: &str| {
        let _ = write!(b, "\n  {key}: {value}");
    };
    field("Label", &label(&s.field));
    field("Category", category(s.field.leaf));
    field("Values", &legal_text(s));
    field(
        "Last Observed on Device",
        &format!(
            "{} ({})",
            value_text(s.field.leaf, s.value.as_ref()),
            freshness(d, s)
        ),
    );
    match &s.saved {
        Some(saved) => {
            field(
                "Saved on Adapter",
                &format!("yes, {}", value_string(s.field.leaf, saved)),
            );
            field("State", &apply_state(s));
            if !model::hidpp_enabled(d) {
                field(
                    "Applied",
                    "no; HID++ is off, so the saved value is applied when it is turned on",
                );
            }
        }
        None => {
            field("Saved on Adapter", "no (Default)");
            field("State", &apply_state(s));
        }
    }
    b
}

/// `feature list DEV`.
pub fn feature_list(d: &p::Device, features: &[p::Feature]) -> String {
    let mut b = format!(
        "Features last discovered on {}{}",
        name(Some(&d.name)),
        cached_note(d)
    );
    if features.is_empty() {
        b.push_str("\n  None discovered.");
    } else {
        b.push_str("\n  INDEX  FEATURE  VERSION  HANDLER");
    }
    for f in features {
        let handler = if f.supported { "supported" } else { "none" };
        match &f.detail {
            Some(p::feature::Detail::Hidpp(h)) => {
                let _ = write!(
                    b,
                    "\n  {:5}  0x{:04X}   {:7}  {handler}",
                    h.index, h.id, h.version
                );
            }
            None => {
                let _ = write!(b, "\n  {:5}  {:6}   {:7}  {handler}", "?", "?", "?");
            }
        }
    }
    b
}

/// `setting list DEV` from the session's view.
pub fn subject_text(subject: &Subject, state: Option<&State>) -> String {
    let Some(st) = state else {
        return String::new();
    };
    let Some(d) = st.device(subject.id) else {
        return String::new();
    };
    setting_list(d, &st.settings_of(d.id))
}

/// The result of saving or forgetting settings.
pub fn saved_text(
    subject: &Subject,
    set: &[Field],
    forget: &[Field],
    settings: &[Setting],
    state: Option<&State>,
) -> String {
    let hidpp_on = state
        .and_then(|st| st.device(subject.id))
        .is_none_or(model::hidpp_enabled);
    let mut lines = Vec::new();
    for field in set {
        let Some(s) = settings.iter().find(|s| s.field == *field) else {
            continue;
        };
        let value = maybe_value(field.leaf, s.saved.as_ref());
        let path = field.path();
        lines.push(if hidpp_on {
            format!("Saved on adapter: {path} = {value} ({}).", apply_state(s))
        } else {
            format!("Saved on adapter: {path} = {value} (not applied: HID++ is off).")
        });
    }
    if !set.is_empty() {
        lines.push(if hidpp_on {
            "It is reapplied when the device reconnects with HID++ on, when HID++ is turned on, and after an adapter platform change.".into()
        } else {
            "The device keeps its current value. The saved value is applied when HID++ is turned on.".into()
        });
    }
    for field in forget {
        lines.push(format!(
            "Default: forgot the saved value of {}; the device was left unchanged.",
            field.path()
        ));
    }
    lines.join("\n")
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::settings::{
        self,
        tests::{object, ranged, wheel},
    };
    use cordial_protocol::settings_object::Group;

    /// The settings of one object.
    pub fn of(object: p::SettingsObject) -> Vec<Setting> {
        settings::settings(&[object])
    }

    fn one(object: p::SettingsObject) -> Setting {
        of(object).remove(0)
    }

    #[test]
    fn labels_categories_and_choice_words() {
        let field = |leaf, index| Field::hidpp(leaf, index);
        assert_eq!(label(&field(Leaf::Dpi, Some(0))), "Pointer Sensitivity");
        assert_eq!(label(&field(Leaf::Dpi, Some(1))), "Pointer Sensitivity 2");
        assert_eq!(label(&field(Leaf::HandsAway, None)), "Hands Away Timeout");
        assert_eq!(category(Leaf::AutoOff), "Power");
        assert_eq!(unit(Leaf::Dpi), "DPI");
        let mode = |m: p::WheelMode| choice_words(Leaf::WheelMode, m as i32);
        assert_eq!(mode(p::WheelMode::FreeSpin), "Free Spin");
        assert_eq!(mode(p::WheelMode::Stepped), "Stepped");
        assert_eq!(choice_words(Leaf::WheelMode, 40), "Unknown");
        assert_eq!(
            choice_words(
                Leaf::BacklightMode,
                p::BacklightMode::PermanentManual as i32
            ),
            "Permanent Manual"
        );
        assert_eq!(
            choice_words(Leaf::FunctionRow, p::FunctionRowMode::FunctionKeys as i32),
            "F1-F12"
        );
        assert_eq!(
            choice_words(Leaf::KeyboardPlatform, p::KeyboardPlatform::Mac as i32),
            "macOS"
        );
        assert_eq!(
            choice_words(
                Leaf::KeyboardPlatform,
                p::KeyboardPlatform::WindowsEmbedded as i32
            ),
            "Windows Embedded"
        );
        // Every settable leaf is labelled.
        for leaf in settings::SETTABLE {
            assert!(!info_for(leaf).label.is_empty() && !category(leaf).is_empty());
        }
    }

    #[test]
    fn values_parse_from_types_and_limits() {
        let power = |s| one(object(Group::Power(p::PowerSettings { auto_off: Some(s) })));
        let s = one(object(Group::Backlight(p::BacklightSettings {
            delay: Some(p::BacklightDelaySettings {
                external_power: Some(ranged(None, 5, 300, 5)),
                ..Default::default()
            }),
            ..Default::default()
        })));
        assert_eq!(parse_value(&s, "25"), Ok(Value::Integer(25)));
        assert_eq!(
            parse_value(&s, "26").unwrap_err(),
            "backlight.external_power takes an integer from 5 through 300 in steps of 5"
        );
        let a = power(ranged(None, 0, 15300, 60));
        assert_eq!(parse_value(&a, "0"), Ok(Value::Integer(0)));
        assert_eq!(
            parse_value(&a, "90").unwrap_err(),
            "power.auto_off takes an integer from 0 through 15300 in steps of 60"
        );
        let wheel = of(wheel(None, None));
        let m = &wheel[0];
        assert_eq!(
            parse_value(m, "Stepped"),
            Ok(Value::Choice(p::WheelMode::Stepped as i32))
        );
        assert_eq!(
            parse_value(m, "x").unwrap_err(),
            "wheel.mode takes one of free_spin, stepped"
        );
        let b = &wheel[1];
        assert_eq!(parse_value(b, "YES"), Ok(Value::Bool(true)));
        assert_eq!(parse_value(b, "off"), Ok(Value::Bool(false)));
        assert_eq!(
            parse_value(b, "2").unwrap_err(),
            "wheel.invert takes on or off"
        );
        assert!(setting_value(b, &SettingInput::Value(Value::Integer(1))).is_err());
        let any = power(p::IntegerSetting::default());
        assert_eq!(legal_text(&any), "an integer");
        assert!(settings::accepts(&any, &Value::Integer(i64::MAX)));
    }

    #[test]
    fn lines_show_paths_values_and_units() {
        let d = p::Device {
            name: "Mouse".into(),
            ..Default::default()
        };
        let mut s = one(settings::tests::sensor(
            1,
            ranged(Some(1600), 200, 4000, 50),
        ));
        s.saved = Some(Value::Integer(800));
        s.status = Some(Ok(SettingState::Applied));
        assert_eq!(
            setting_line(&d, &s),
            "  pointer_sensor.1.dpi  Pointer Sensitivity 2: last observed on device: 1600 DPI (stale); saved on adapter: 800, applied"
        );
    }
}
