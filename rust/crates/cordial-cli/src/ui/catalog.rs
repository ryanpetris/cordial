//! Presentation of information and settings keys. The adapter sends keys from the shared
//! catalog with typed values only; names, grouping, choice wording and value parsing belong to
//! the host. Keys this build doesn't know are never shown.
use crate::{
    controller::{SettingInput, State, Subject},
    model::{self, Type, Up},
    ui::text::{hidpp_words, name, safe},
};
use cordial_protocol::{self as p, SettingState, keys, value::Value};
use std::fmt::Write;

pub struct Info {
    /// The key, or its template with `{n}`.
    pub key: &'static str,
    pub label: &'static str,
    /// The settings page's group; information keys have none.
    pub category: &'static str,
    /// The unit integer values are in, or "".
    pub unit: &'static str,
    /// Wording of enum values that plain capitalization would garble.
    choices: &'static [(&'static str, &'static str)],
}

const fn info(key: &'static str, label: &'static str, category: &'static str) -> Info {
    Info {
        key,
        label,
        category,
        unit: "",
        choices: &[],
    }
}

/// Known keys in display order; categories appear in the order of their first key.
static CATALOG: &[Info] = &[
    info(keys::BATTERY_LEVEL, "Battery", ""),
    info(keys::BATTERY_CHARGING, "Charging", ""),
    info(keys::DEVICE_MANUFACTURER, "Manufacturer", ""),
    info(keys::DEVICE_MODEL, "Model", ""),
    info(keys::DEVICE_SERIAL, "Serial Number", ""),
    info(keys::FIRMWARE_VERSION, "Firmware", ""),
    info(keys::BOOTLOADER_VERSION, "Bootloader", ""),
    info(keys::HARDWARE_REVISION, "Hardware", ""),
    info(keys::SOFTWARE_REVISION, "Software", ""),
    info(keys::VENDOR_ID, "Vendor ID", ""),
    Info {
        choices: &[("usb", "USB"), ("bluetooth", "Bluetooth")],
        ..info(keys::VENDOR_REGISTRY, "Vendor ID Namespace", "")
    },
    info(keys::PRODUCT_ID, "Product ID", ""),
    info(keys::PRODUCT_VERSION, "Product Version", ""),
    Info {
        choices: &[
            ("function_keys", "F1-F12"),
            ("special_actions", "Shortcuts"),
        ],
        ..info(keys::KEYBOARD_FN_ROW, "Function Row", "Keyboard")
    },
    Info {
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
        ..info(keys::KEYBOARD_PLATFORM, "Keyboard Platform", "Keyboard")
    },
    info(keys::BACKLIGHT_ENABLED, "Backlight", "Backlight"),
    info(keys::BACKLIGHT_MODE, "Backlight Mode", "Backlight"),
    info(keys::BACKLIGHT_LEVEL, "Manual Backlight Level", "Backlight"),
    info(
        keys::BACKLIGHT_CURRENT_LEVEL,
        "Current Backlight Level",
        "Backlight",
    ),
    Info {
        choices: &[
            ("battery", "Off (Battery)"),
            ("saturated", "Automatic (Saturated)"),
        ],
        ..info(keys::BACKLIGHT_STATUS, "Backlight Status", "Backlight")
    },
    info(keys::BACKLIGHT_EFFECT, "Backlight Effect", "Backlight"),
    Info {
        unit: "s",
        ..info(
            keys::BACKLIGHT_DELAY_HANDS_OUT,
            "Timeout With Hands Away",
            "Backlight",
        )
    },
    Info {
        unit: "s",
        ..info(
            keys::BACKLIGHT_DELAY_HANDS_IN,
            "Timeout With Hands Nearby",
            "Backlight",
        )
    },
    Info {
        unit: "s",
        ..info(
            keys::BACKLIGHT_DELAY_POWERED,
            "Timeout While Plugged In",
            "Backlight",
        )
    },
    info(
        keys::BACKLIGHT_POWER_ON,
        "Backlight at Power-On",
        "Backlight",
    ),
    info(keys::BACKLIGHT_CROWN, "Crown Backlight", "Backlight"),
    info(
        keys::BACKLIGHT_POWER_SAVE,
        "Backlight Power Saving",
        "Backlight",
    ),
    Info {
        unit: "DPI",
        ..info(keys::POINTER_SENSOR_N_DPI, "Pointer Speed", "Pointer")
    },
    info(keys::WHEEL_MODE, "Wheel Mode", "Wheel"),
    info(keys::WHEEL_THRESHOLD, "SmartShift", "Wheel"),
    info(keys::WHEEL_INVERT, "Reverse Vertical Scrolling", "Wheel"),
    info(
        keys::WHEEL_RESOLUTION_MULTIPLIER,
        "Resolution Multiplier",
        "Wheel",
    ),
    info(
        keys::WHEEL_RATCHETS_PER_ROTATION,
        "Ratchets per Rotation",
        "Wheel",
    ),
    Info {
        unit: "mm",
        ..info(keys::WHEEL_DIAMETER, "Wheel Diameter", "Wheel")
    },
    info(
        keys::THUMBWHEEL_INVERT,
        "Reverse Horizontal Scrolling",
        "Wheel",
    ),
    Info {
        unit: "s",
        ..info(keys::POWER_AUTO_OFF, "Automatic Power-Off", "Power")
    },
];

/// A key's presentation, its display position and index; `None` for keys this build doesn't
/// present.
pub fn info_for(key: &str) -> Option<(&'static Info, usize, Option<u32>)> {
    let (entry, index) = keys::lookup(key)?;
    let i = CATALOG.iter().position(|i| i.key == entry.key)?;
    Some((&CATALOG[i], i, index))
}

/// A key's label. Repeated parts after the first are numbered, so the second sensor reads
/// "Pointer Speed 2".
pub fn label(key: &str) -> String {
    match info_for(key) {
        Some((info, _, Some(n))) if n > 0 => format!("{} {}", info.label, n + 1),
        Some((info, ..)) => info.label.into(),
        None => key.into(),
    }
}

pub fn category(key: &str) -> &'static str {
    info_for(key).map_or("", |(i, ..)| i.category)
}

pub fn unit(key: &str) -> &'static str {
    info_for(key).map_or("", |(i, ..)| i.unit)
}

fn order(key: &str) -> (usize, u32) {
    info_for(key).map_or((usize::MAX, 0), |(_, i, n)| (i, n.unwrap_or(0)))
}

/// Settings this build presents, in display order.
pub fn presented(settings: &[p::Setting]) -> Vec<&p::Setting> {
    let mut known: Vec<&p::Setting> = settings
        .iter()
        .filter(|s| info_for(&s.key).is_some() && model::kind(s).is_some())
        .collect();
    known.sort_by_key(|s| order(&s.key));
    known
}

/// Categories of the settings in display order.
pub fn categories(settings: &[p::Setting]) -> Vec<&'static str> {
    let mut out = Vec::new();
    for s in presented(settings) {
        let c = category(&s.key);
        if !out.contains(&c) {
            out.push(c);
        }
    }
    out
}

/// Information entries this build presents, in display order.
pub fn presented_info(info: &[p::Info]) -> Vec<&p::Info> {
    let mut known: Vec<&p::Info> = info
        .iter()
        .filter(|i| {
            info_for(&i.key).is_some() && i.value.as_ref().is_some_and(|v| v.value.is_some())
        })
        .collect();
    known.sort_by_key(|i| order(&i.key));
    known
}

/// Words an enum value: the catalog's wording, else the value in title case with spaces for
/// separators.
pub fn choice_words(key: &str, token: &str) -> String {
    if let Some((_, words)) =
        info_for(key).and_then(|(i, ..)| i.choices.iter().find(|(t, _)| *t == token))
    {
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

/// A value as the shell accepts it: on/off, the integer, the text, or a color as #rrggbb.
pub fn value_string(v: &Value) -> String {
    match v {
        Value::Bool(b) => if *b { "on" } else { "off" }.into(),
        Value::Integer(n) => n.to_string(),
        Value::Text(s) => s.clone(),
        Value::Color(c) => format!("#{c:06x}"),
    }
}

/// An optional value as the shell shows it.
pub fn maybe_value(v: Option<&Value>) -> String {
    v.map_or_else(|| "unavailable".into(), value_string)
}

/// A setting's legal values.
pub fn legal_text(s: &p::Setting) -> String {
    let choices = model::choices(s);
    match model::kind(s) {
        Some(Type::Bool) => "on or off".into(),
        _ if !choices.is_empty() => {
            let names: Vec<String> = choices.iter().map(|c| safe(&value_string(c))).collect();
            format!("one of {}", names.join(", "))
        }
        Some(Type::Integer) => {
            let mut text = String::from("an integer");
            if let Some((min, max, step)) = model::range(s) {
                let _ = write!(text, " from {min} through {max}");
                if step > 1 {
                    let _ = write!(text, " in steps of {step}");
                }
            }
            text
        }
        Some(Type::Color) => "a color as #rrggbb".into(),
        Some(Type::Text) => match model::max_bytes(s) {
            Some(n) => format!("text of at most {n} bytes"),
            None => "text".into(),
        },
        Some(Type::Enum) => "one of no values".into(),
        None => "no values".into(),
    }
}

fn parse_color(text: &str) -> Option<u32> {
    let hex = text.strip_prefix('#').unwrap_or(text);
    (hex.len() == 6 && hex.bytes().all(|b| b.is_ascii_hexdigit()))
        .then(|| u32::from_str_radix(hex, 16).ok())
        .flatten()
}

/// Why a value for `s` was refused: the values it takes, or that it has none.
fn rejected(s: &p::Setting) -> String {
    let none = match model::kind(s) {
        Some(Type::Enum) => model::choices(s).is_empty(),
        None => true,
        _ => false,
    };
    if none {
        format!("{} has no values to choose from", s.key)
    } else {
        format!("{} takes {}", s.key, legal_text(s))
    }
}

/// Types a command-line value from a setting's type and limits.
fn parse_value(s: &p::Setting, text: &str) -> Result<Value, String> {
    let key = &s.key;
    let invalid = || rejected(s);
    let v = match model::kind(s) {
        Some(Type::Bool) => match text.to_lowercase().as_str() {
            "on" | "true" | "yes" => Value::Bool(true),
            "off" | "false" | "no" => Value::Bool(false),
            _ => return Err(format!("{key} takes on or off")),
        },
        Some(Type::Integer) => Value::Integer(text.parse().map_err(|_| invalid())?),
        Some(Type::Enum) => model::choices(s)
            .into_iter()
            .find(|c| matches!(c, Value::Text(t) if t.eq_ignore_ascii_case(text)))
            .ok_or_else(invalid)?,
        Some(Type::Text) => {
            if let Some(n) = model::max_bytes(s).filter(|n| text.len() > *n as usize) {
                return Err(format!("{key} takes at most {n} bytes"));
            }
            Value::Text(text.into())
        }
        Some(Type::Color) => Value::Color(parse_color(text).ok_or_else(invalid)?),
        None => return Err(invalid()),
    };
    if !model::accepts(s, &v) {
        return Err(invalid());
    }
    Ok(v)
}

/// The value to save for `setting set`: typed text parsed as the shell accepts it, or a value
/// chosen in the TUI, which must be legal.
pub fn setting_value(s: &p::Setting, input: &SettingInput) -> Result<Value, String> {
    match input {
        SettingInput::Text(text) => parse_value(s, text),
        SettingInput::Value(v) if model::accepts(s, v) => Ok(v.clone()),
        SettingInput::Value(_) => Err(rejected(s)),
    }
}

/// A saved setting's apply state.
pub fn apply_state(s: &p::Setting) -> String {
    match model::applied(s) {
        None => "Default".into(),
        Some(Ok(SettingState::Pending)) => "pending".into(),
        Some(Ok(SettingState::Applied)) => "applied".into(),
        Some(Ok(SettingState::ChangedOnDevice)) => "changed on device".into(),
        Some(Ok(SettingState::Unsupported)) => "unsupported by the device now".into(),
        Some(Err(code)) => format!("failed: {}", hidpp_words(code)),
    }
}

/// Whether a setting's reading is current: its integration is up and it has been read.
pub fn fresh(d: &p::Device, s: &p::Setting) -> bool {
    model::current(s).is_some()
        && d.integrations
            .iter()
            .any(|i| i.kind == s.integration && model::up(i) == Up::Active)
}

fn freshness(d: &p::Device, s: &p::Setting) -> &'static str {
    match () {
        _ if model::current(s).is_none() => "unavailable",
        _ if !fresh(d, s) => "stale",
        _ => "fresh",
    }
}

/// A value with the key's choice wording or unit, as the shell shows it.
pub fn value_text(key: &str, v: Option<&Value>) -> String {
    safe(&maybe_value(v)).to_string() + &unit_suffix(key, v)
}

fn unit_suffix(key: &str, v: Option<&Value>) -> String {
    match (v, unit(key)) {
        (Some(Value::Integer(_)), u) if !u.is_empty() => format!(" {u}"),
        _ => String::new(),
    }
}

/// One setting of `setting list DEV`.
pub fn setting_line(d: &p::Device, s: &p::Setting) -> String {
    let key = safe(&s.key);
    let label = safe(&label(&s.key));
    let mut line = format!(
        "  {key}  {label}: last observed on device: {} ({})",
        value_text(&s.key, model::current(s).as_ref()),
        freshness(d, s)
    );
    match model::saved(s) {
        Some(saved) => {
            let _ = write!(
                line,
                "; saved on adapter: {}, {}",
                safe(&value_string(&saved)),
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
        " (disconnected; last known values)"
    } else if !model::hidpp_enabled(d) {
        " (HID++ off; last known values)"
    } else {
        ""
    }
}

/// `setting list DEV`, grouped by category.
pub fn setting_list(d: &p::Device, settings: &[p::Setting]) -> String {
    let mut b = format!(
        "Settings of {}: {}{}",
        name(Some(&d.name)),
        crate::ui::text::up_token(model::hidpp_up(d)),
        cached_note(d)
    );
    let known = presented(settings);
    if known.is_empty() {
        b.push_str("\n  No settings discovered.");
    }
    for category in categories(settings) {
        let _ = write!(b, "\n{category}");
        for s in known.iter().filter(|s| self::category(&s.key) == category) {
            let _ = write!(b, "\n{}", setting_line(d, s));
        }
    }
    b
}

/// `setting get`, one field per line.
pub fn setting_detail(d: &p::Device, s: &p::Setting) -> String {
    let mut b = format!(
        "Setting {} of {}{}",
        safe(&s.key),
        name(Some(&d.name)),
        cached_note(d)
    );
    let mut field = |key: &str, value: &str| {
        let _ = write!(b, "\n  {key}: {value}");
    };
    field("Label", &label(&s.key));
    field("Category", category(&s.key));
    field("Values", &legal_text(s));
    field(
        "Last Observed on Device",
        &format!(
            "{} ({})",
            value_text(&s.key, model::current(s).as_ref()),
            freshness(d, s)
        ),
    );
    match model::saved(s) {
        Some(saved) => {
            field(
                "Saved on Adapter",
                &format!("yes, {}", safe(&value_string(&saved))),
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
    let Some(d) = st.device(&subject.id) else {
        return String::new();
    };
    setting_list(d, st.settings_of(&d.id))
}

/// The result of saving or forgetting settings.
pub fn saved_text(
    subject: &Subject,
    set: &[String],
    forget: &[String],
    settings: &[p::Setting],
    state: Option<&State>,
) -> String {
    let hidpp_on = state
        .and_then(|st| st.device(&subject.id))
        .is_none_or(model::hidpp_enabled);
    let mut lines = Vec::new();
    for key in set {
        let Some(s) = settings.iter().find(|s| s.key == *key) else {
            continue;
        };
        let value = safe(&maybe_value(model::saved(s).as_ref()));
        let key = safe(key);
        lines.push(if hidpp_on {
            format!("Saved on adapter: {key} = {value} ({}).", apply_state(s))
        } else {
            format!("Saved on adapter: {key} = {value} (not applied: HID++ is off).")
        });
    }
    if !set.is_empty() {
        lines.push(if hidpp_on {
            "It is reapplied when the device reconnects with HID++ on, when HID++ is turned on, and after an adapter platform change.".into()
        } else {
            "The device keeps its current value. The saved value is applied when HID++ is turned on.".into()
        });
    }
    for key in forget {
        lines.push(format!(
            "Default: forgot the saved value of {}; the device was left unchanged.",
            safe(key)
        ));
    }
    lines.join("\n")
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use cordial_protocol::{IntegrationKind, setting};

    pub fn setting(key: &str, kind: setting::Type) -> p::Setting {
        p::Setting {
            integration: IntegrationKind::Hidpp as i32,
            key: key.into(),
            status: None,
            r#type: Some(kind),
        }
    }

    pub fn integer(key: &str, min: i64, max: i64, step: u64) -> p::Setting {
        setting(
            key,
            setting::Type::Integer(p::IntegerSetting {
                value: None,
                saved: None,
                limits: Some(p::integer_setting::Limits::Range(p::IntegerRange {
                    min,
                    max,
                    step,
                })),
            }),
        )
    }

    pub fn choice(key: &str, choices: &[&str]) -> p::Setting {
        setting(
            key,
            setting::Type::Enum(p::EnumSetting {
                value: None,
                saved: None,
                choices: choices.iter().map(|c| (*c).into()).collect(),
            }),
        )
    }

    pub fn boolean(key: &str) -> p::Setting {
        setting(key, setting::Type::Bool(p::BoolSetting::default()))
    }

    #[test]
    fn catalog_covers_every_device_key_once() {
        for key in keys::KEYS.iter().filter(|k| k.device || k.setting) {
            assert_eq!(
                CATALOG.iter().filter(|i| i.key == key.key).count(),
                1,
                "{}",
                key.key
            );
        }
        assert_eq!(choice_words(keys::WHEEL_MODE, "freespin"), "Freespin");
        assert_eq!(
            choice_words(keys::BACKLIGHT_MODE, "permanent_manual"),
            "Permanent Manual"
        );
        assert_eq!(
            choice_words(keys::KEYBOARD_FN_ROW, "function_keys"),
            "F1-F12"
        );
        assert_eq!(label(keys::KEYBOARD_PLATFORM), "Keyboard Platform");
        assert_eq!(choice_words(keys::KEYBOARD_PLATFORM, "mac"), "macOS");
        assert_eq!(
            choice_words(keys::KEYBOARD_PLATFORM, "windows_embedded"),
            "Windows Embedded"
        );
        assert_eq!(category(keys::POWER_AUTO_OFF), "Power");
        assert_eq!(label("pointer.sensor.0.dpi"), "Pointer Speed");
        assert_eq!(label("pointer.sensor.1.dpi"), "Pointer Speed 2");
        assert!(info_for("pointer.sensor.x.dpi").is_none());
    }

    #[test]
    fn values_parse_from_types_and_limits() {
        let s = integer(keys::BACKLIGHT_DELAY_POWERED, 5, 300, 5);
        assert_eq!(parse_value(&s, "25"), Ok(Value::Integer(25)));
        assert_eq!(
            parse_value(&s, "26").unwrap_err(),
            "backlight.delay.powered takes an integer from 5 through 300 in steps of 5"
        );
        let a = integer(keys::POWER_AUTO_OFF, 0, 15300, 60);
        assert_eq!(parse_value(&a, "0"), Ok(Value::Integer(0)));
        assert_eq!(
            parse_value(&a, "90").unwrap_err(),
            "power.auto_off takes an integer from 0 through 15300 in steps of 60"
        );
        let m = choice(keys::WHEEL_MODE, &["freespin", "ratchet"]);
        assert_eq!(
            parse_value(&m, "Ratchet"),
            Ok(Value::Text("ratchet".into()))
        );
        assert_eq!(
            parse_value(&m, "x").unwrap_err(),
            "wheel.mode takes one of freespin, ratchet"
        );
        let b = boolean(keys::WHEEL_INVERT);
        assert_eq!(parse_value(&b, "YES"), Ok(Value::Bool(true)));
        assert_eq!(
            parse_value(&b, "2").unwrap_err(),
            "wheel.invert takes on or off"
        );
        assert!(setting_value(&b, &SettingInput::Value(Value::Integer(1))).is_err());
        let c = setting(
            "light.color",
            setting::Type::Color(p::ColorSetting::default()),
        );
        assert_eq!(parse_value(&c, "#00FF7f"), Ok(Value::Color(0x00ff7f)));
        assert!(parse_value(&c, "12345").is_err());
    }
}
