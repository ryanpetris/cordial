//! Device settings as the CLI presents them. The adapter groups settings into typed objects; each
//! field of an object becomes one [`Setting`] with its identity, last reading, saved preference,
//! apply status, and limits. Readings of the same behavior from Device.information use the same
//! leaves.
use crate::model;
use cordial_client::order;
use cordial_protocol::{
    self as p, IntegrationKind, SettingState, SettingsGroup as G, integer_setting, setting_status,
    settings_object::Group, settings_update::Group as U,
};

/// A field this build knows, in display order.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum Leaf {
    FunctionRow,
    KeyboardPlatform,
    BacklightEnabled,
    BacklightMode,
    Brightness,
    CurrentBrightness,
    BacklightStatus,
    BacklightEffect,
    HandsAway,
    HandsNearby,
    ExternalPower,
    StartupEffect,
    DialEffect,
    PowerSave,
    Dpi,
    WheelMode,
    FreeSpinThreshold,
    WheelInvert,
    ResolutionMultiplier,
    RatchetsPerRotation,
    Diameter,
    ThumbwheelInvert,
    AutoOff,
}

/// Every leaf a SetSettings change can name, in display order.
pub const SETTABLE: [Leaf; 18] = [
    Leaf::FunctionRow,
    Leaf::KeyboardPlatform,
    Leaf::BacklightEnabled,
    Leaf::BacklightMode,
    Leaf::Brightness,
    Leaf::BacklightEffect,
    Leaf::HandsAway,
    Leaf::HandsNearby,
    Leaf::ExternalPower,
    Leaf::StartupEffect,
    Leaf::DialEffect,
    Leaf::PowerSave,
    Leaf::Dpi,
    Leaf::WheelMode,
    Leaf::FreeSpinThreshold,
    Leaf::WheelInvert,
    Leaf::ThumbwheelInvert,
    Leaf::AutoOff,
];

impl Leaf {
    pub fn group(self) -> G {
        use Leaf::*;
        match self {
            FunctionRow | KeyboardPlatform => G::Keyboard,
            AutoOff => G::Power,
            Dpi => G::PointerSensor,
            WheelMode | FreeSpinThreshold | WheelInvert | ResolutionMultiplier
            | RatchetsPerRotation | Diameter => G::Wheel,
            ThumbwheelInvert => G::Thumbwheel,
            _ => G::Backlight,
        }
    }

    /// The leaf's protocol field name within its group.
    pub fn word(self) -> &'static str {
        use Leaf::*;
        match self {
            FunctionRow => "function_row",
            KeyboardPlatform => "platform",
            BacklightEnabled => "enabled",
            BacklightMode | WheelMode => "mode",
            Brightness => "brightness",
            CurrentBrightness => "current_brightness",
            BacklightStatus => "status",
            BacklightEffect => "effect",
            HandsAway => "hands_away",
            HandsNearby => "hands_nearby",
            ExternalPower => "external_power",
            StartupEffect => "startup_effect",
            DialEffect => "dial_effect",
            PowerSave => "power_save",
            Dpi => "dpi",
            FreeSpinThreshold => "free_spin_threshold",
            WheelInvert | ThumbwheelInvert => "invert",
            ResolutionMultiplier => "resolution_multiplier",
            RatchetsPerRotation => "ratchets_per_rotation",
            Diameter => "diameter",
            AutoOff => "auto_off",
        }
    }

    /// The protocol name of an enum value of this leaf in lower case, or `unknown` for a value
    /// this build can't interpret. Leaves without an enum type read `unknown` too.
    pub fn choice_token(self, n: i32) -> String {
        use Leaf::*;
        let name = match self {
            KeyboardPlatform => p::KeyboardPlatform::try_from(n)
                .ok()
                .map(|e| e.as_str_name()),
            FunctionRow => p::FunctionRowMode::try_from(n)
                .ok()
                .map(|e| e.as_str_name()),
            BacklightMode => p::BacklightMode::try_from(n).ok().map(|e| e.as_str_name()),
            BacklightStatus => p::BacklightStatus::try_from(n)
                .ok()
                .map(|e| e.as_str_name()),
            BacklightEffect => p::BacklightEffect::try_from(n)
                .ok()
                .map(|e| e.as_str_name()),
            WheelMode => p::WheelMode::try_from(n).ok().map(|e| e.as_str_name()),
            _ => None,
        };
        name.map_or_else(|| "unknown".into(), model::token)
    }
}

fn group_word(group: G) -> &'static str {
    match group {
        G::Keyboard => "keyboard",
        G::Power => "power",
        G::Backlight => "backlight",
        G::PointerSensor => "pointer_sensor",
        G::Wheel => "wheel",
        G::Thumbwheel => "thumbwheel",
        G::Unknown => "unknown",
    }
}

/// One field of a settings object: its integration, leaf, and component index for groups that
/// have one. Ordered for display.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Field {
    pub leaf: Leaf,
    pub index: Option<u32>,
    pub integration: i32,
}

impl Field {
    /// A HID++ field.
    pub fn hidpp(leaf: Leaf, index: Option<u32>) -> Self {
        Self {
            leaf,
            index,
            integration: IntegrationKind::Hidpp as i32,
        }
    }

    /// How commands name the field: its group, the sensor index for pointer sensors, and its
    /// leaf, such as `wheel.mode` or `pointer_sensor.1.dpi`.
    pub fn path(&self) -> String {
        let group = group_word(self.leaf.group());
        match self.index {
            Some(n) => format!("{group}.{n}.{}", self.leaf.word()),
            None => format!("{group}.{}", self.leaf.word()),
        }
    }

    /// The HID++ field a command's path names, if it names a field this build can save.
    pub fn parse(path: &str) -> Option<Self> {
        let parts: Vec<&str> = path.split('.').collect();
        let (group, index, word) = match parts[..] {
            [group, word] => (group, None, word),
            [group, n, word] if !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()) => {
                (group, Some(n.parse().ok()?), word)
            }
            _ => return None,
        };
        let leaf = SETTABLE
            .into_iter()
            .find(|l| group_word(l.group()) == group && l.word() == word)?;
        (index.is_some() == (leaf.group() == G::PointerSensor)).then(|| Self::hidpp(leaf, index))
    }

    /// The object the field belongs to.
    pub fn object(&self) -> p::SettingsRef {
        p::SettingsRef {
            integration: self.integration,
            group: self.leaf.group() as i32,
            index: self.index,
        }
    }
}

/// A setting's value: on or off, an integer, or an enum's wire number.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Value {
    Bool(bool),
    Integer(i64),
    Choice(i32),
}

/// The values a setting accepts.
#[derive(Clone, Debug, PartialEq)]
pub enum Limits {
    Bool,
    /// A range, a list, or neither for any integer.
    Integer(Option<integer_setting::Limits>),
    /// The writable enum values, none when nothing can be written.
    Choice(Vec<i32>),
}

/// One field of a settings object.
#[derive(Clone, Debug, PartialEq)]
pub struct Setting {
    pub field: Field,
    /// The last reading from the device.
    pub value: Option<Value>,
    /// The preference saved on the adapter.
    pub saved: Option<Value>,
    /// How applying the saved preference went, while there is one.
    pub status: Option<Result<SettingState, p::Error>>,
    pub limits: Limits,
}

fn applied(
    saved: bool,
    status: Option<&p::SettingStatus>,
) -> Option<Result<SettingState, p::Error>> {
    saved.then(|| match status.and_then(|s| s.result.clone()) {
        Some(setting_status::Result::Error(e)) => Err(e),
        Some(setting_status::Result::State(n)) => Ok(SettingState::try_from(n).unwrap_or_default()),
        None => Ok(SettingState::Unknown),
    })
}

fn bool_setting(field: Field, s: &p::BoolSetting) -> Setting {
    Setting {
        field,
        value: s.value.map(Value::Bool),
        saved: s.saved.map(Value::Bool),
        status: applied(s.saved.is_some(), s.status.as_ref()),
        limits: Limits::Bool,
    }
}

fn integer_setting(field: Field, s: &p::IntegerSetting) -> Setting {
    Setting {
        field,
        value: s.value.map(Value::Integer),
        saved: s.saved.map(Value::Integer),
        status: applied(s.saved.is_some(), s.status.as_ref()),
        limits: Limits::Integer(s.limits.clone()),
    }
}

/// An enum setting wrapper as a [`Setting`]; every wrapper has the same fields.
macro_rules! choice_setting {
    ($field:expr, $s:expr) => {{
        let s = $s;
        Setting {
            field: $field,
            value: s.value.map(Value::Choice),
            saved: s.saved.map(Value::Choice),
            status: applied(s.saved.is_some(), s.status.as_ref()),
            limits: Limits::Choice(s.choices.clone()),
        }
    }};
}

/// The fields of settings objects, object by object in the order given.
pub fn settings(objects: &[p::SettingsObject]) -> Vec<Setting> {
    let mut out = Vec::new();
    for o in objects {
        let field = |leaf| Field {
            leaf,
            index: None,
            integration: o.integration,
        };
        let boolean =
            |leaf, s: &Option<p::BoolSetting>| s.as_ref().map(|s| bool_setting(field(leaf), s));
        let integer = |leaf, s: &Option<p::IntegerSetting>| {
            s.as_ref().map(|s| integer_setting(field(leaf), s))
        };
        let fields = match &o.group {
            Some(Group::Keyboard(k)) => vec![
                k.function_row
                    .as_ref()
                    .map(|s| choice_setting!(field(Leaf::FunctionRow), s)),
                k.platform
                    .as_ref()
                    .map(|s| choice_setting!(field(Leaf::KeyboardPlatform), s)),
            ],
            Some(Group::Power(power)) => vec![integer(Leaf::AutoOff, &power.auto_off)],
            Some(Group::Backlight(l)) => {
                let delay = l.delay.clone().unwrap_or_default();
                vec![
                    boolean(Leaf::BacklightEnabled, &l.enabled),
                    l.mode
                        .as_ref()
                        .map(|s| choice_setting!(field(Leaf::BacklightMode), s)),
                    integer(Leaf::Brightness, &l.brightness),
                    l.effect
                        .as_ref()
                        .map(|s| choice_setting!(field(Leaf::BacklightEffect), s)),
                    integer(Leaf::HandsAway, &delay.hands_away),
                    integer(Leaf::HandsNearby, &delay.hands_nearby),
                    integer(Leaf::ExternalPower, &delay.external_power),
                    boolean(Leaf::StartupEffect, &l.startup_effect),
                    boolean(Leaf::DialEffect, &l.dial_effect),
                    boolean(Leaf::PowerSave, &l.power_save),
                ]
            }
            Some(Group::PointerSensor(sensor)) => vec![sensor.dpi.as_ref().map(|s| {
                integer_setting(
                    Field {
                        index: Some(sensor.index),
                        ..field(Leaf::Dpi)
                    },
                    s,
                )
            })],
            Some(Group::Wheel(w)) => vec![
                w.mode
                    .as_ref()
                    .map(|s| choice_setting!(field(Leaf::WheelMode), s)),
                integer(Leaf::FreeSpinThreshold, &w.free_spin_threshold),
                boolean(Leaf::WheelInvert, &w.invert),
            ],
            Some(Group::Thumbwheel(t)) => vec![boolean(Leaf::ThumbwheelInvert, &t.invert)],
            None => Vec::new(),
        };
        out.extend(fields.into_iter().flatten());
    }
    out
}

/// A change to one field: a value to save, or `None` to forget the saved preference.
pub type Change = Option<Value>;

/// A SettingsUpdate that makes one change. A value of the wrong type for the field is sent as
/// its type's zero; callers check values with [`accepts`] first.
pub fn update(field: Field, change: Change) -> p::SettingsUpdate {
    use p::{
        BacklightDelaySettingsUpdate as Delay, BacklightSettingsUpdate as Backlight,
        KeyboardSettingsUpdate as Keyboard, WheelSettingsUpdate as Wheel,
    };
    let forget = p::SettingForget {};
    let boolean = Some(p::BoolSettingUpdate {
        change: Some(match change {
            Some(v) => p::bool_setting_update::Change::Value(v == Value::Bool(true)),
            None => p::bool_setting_update::Change::Forget(forget),
        }),
    });
    let integer = Some(p::IntegerSettingUpdate {
        change: Some(match change {
            Some(Value::Integer(n)) => p::integer_setting_update::Change::Value(n),
            Some(_) => p::integer_setting_update::Change::Value(0),
            None => p::integer_setting_update::Change::Forget(forget),
        }),
    });
    let choice = match change {
        Some(Value::Choice(n)) => Some(n),
        Some(_) => Some(0),
        None => None,
    };
    macro_rules! choice {
        ($module:ident, $update:ident) => {
            Some(p::$update {
                change: Some(match choice {
                    Some(n) => p::$module::Change::Value(n),
                    None => p::$module::Change::Forget(forget),
                }),
            })
        };
    }
    let group = match field.leaf {
        Leaf::KeyboardPlatform => U::Keyboard(Keyboard {
            platform: choice!(
                keyboard_platform_setting_update,
                KeyboardPlatformSettingUpdate
            ),
            ..Default::default()
        }),
        Leaf::FunctionRow => U::Keyboard(Keyboard {
            function_row: choice!(
                function_row_mode_setting_update,
                FunctionRowModeSettingUpdate
            ),
            ..Default::default()
        }),
        Leaf::AutoOff => U::Power(p::PowerSettingsUpdate { auto_off: integer }),
        Leaf::BacklightEnabled => U::Backlight(Backlight {
            enabled: boolean,
            ..Default::default()
        }),
        Leaf::BacklightMode => U::Backlight(Backlight {
            mode: choice!(backlight_mode_setting_update, BacklightModeSettingUpdate),
            ..Default::default()
        }),
        Leaf::Brightness => U::Backlight(Backlight {
            brightness: integer,
            ..Default::default()
        }),
        Leaf::HandsAway => U::Backlight(Backlight {
            delay: Some(Delay {
                hands_away: integer,
                ..Default::default()
            }),
            ..Default::default()
        }),
        Leaf::HandsNearby => U::Backlight(Backlight {
            delay: Some(Delay {
                hands_nearby: integer,
                ..Default::default()
            }),
            ..Default::default()
        }),
        Leaf::ExternalPower => U::Backlight(Backlight {
            delay: Some(Delay {
                external_power: integer,
                ..Default::default()
            }),
            ..Default::default()
        }),
        Leaf::StartupEffect => U::Backlight(Backlight {
            startup_effect: boolean,
            ..Default::default()
        }),
        Leaf::DialEffect => U::Backlight(Backlight {
            dial_effect: boolean,
            ..Default::default()
        }),
        Leaf::PowerSave => U::Backlight(Backlight {
            power_save: boolean,
            ..Default::default()
        }),
        Leaf::BacklightEffect => U::Backlight(Backlight {
            effect: choice!(
                backlight_effect_setting_update,
                BacklightEffectSettingUpdate
            ),
            ..Default::default()
        }),
        Leaf::Dpi => U::PointerSensor(p::PointerSensorSettingsUpdate {
            index: field.index.unwrap_or(0),
            dpi: integer,
        }),
        Leaf::WheelMode => U::Wheel(Wheel {
            mode: choice!(wheel_mode_setting_update, WheelModeSettingUpdate),
            ..Default::default()
        }),
        Leaf::FreeSpinThreshold => U::Wheel(Wheel {
            free_spin_threshold: integer,
            ..Default::default()
        }),
        Leaf::WheelInvert => U::Wheel(Wheel {
            invert: boolean,
            ..Default::default()
        }),
        Leaf::ThumbwheelInvert => U::Thumbwheel(p::ThumbwheelSettingsUpdate { invert: boolean }),
        // Readings, which no change names.
        Leaf::CurrentBrightness
        | Leaf::BacklightStatus
        | Leaf::ResolutionMultiplier
        | Leaf::RatchetsPerRotation
        | Leaf::Diameter => U::Backlight(Backlight::default()),
    };
    p::SettingsUpdate {
        integration: field.integration,
        group: Some(group),
    }
}

/// Takes a saved change into a wrapper's saved preference: a saved value waits to be applied,
/// and a forgotten one has no status.
fn take<T>(saved: &mut Option<T>, status: &mut Option<p::SettingStatus>, change: Option<T>) {
    *status = change.is_some().then_some(p::SettingStatus {
        result: Some(setting_status::Result::State(SettingState::Pending as i32)),
    });
    *saved = change;
}

/// The change a field update makes: `None` for no change, `Some(None)` to forget.
macro_rules! change_of {
    ($update:expr, $module:ident) => {
        $update
            .as_ref()
            .and_then(|u| u.change.as_ref())
            .map(|c| match c {
                p::$module::Change::Value(v) => Some(*v),
                p::$module::Change::Forget(_) => None,
            })
    };
}

/// Applies a change to a wrapper the object holds; a wrapper it doesn't hold is left to the
/// settings_changed event that follows.
macro_rules! save {
    ($wrapper:expr, $change:expr) => {
        if let (Some(w), Some(c)) = ($wrapper.as_mut(), $change) {
            take(&mut w.saved, &mut w.status, c);
        }
    };
}

/// Takes the preferences an accepted SetSettings update saved and forgot into the objects held.
pub fn take_update(objects: &mut [p::SettingsObject], update: &p::SettingsUpdate) {
    let Some(o) = objects.iter_mut().find(|o| {
        let r = order::settings_ref(o);
        r.integration == update.integration
            && match (&update.group, &o.group) {
                (Some(U::PointerSensor(u)), Some(Group::PointerSensor(s))) => u.index == s.index,
                (Some(u), Some(_)) => r.group == update_group(u) as i32,
                _ => false,
            }
    }) else {
        return;
    };
    match (&update.group, &mut o.group) {
        (Some(U::Keyboard(u)), Some(Group::Keyboard(k))) => {
            save!(
                k.platform,
                change_of!(u.platform, keyboard_platform_setting_update)
            );
            save!(
                k.function_row,
                change_of!(u.function_row, function_row_mode_setting_update)
            );
        }
        (Some(U::Power(u)), Some(Group::Power(power))) => {
            save!(
                power.auto_off,
                change_of!(u.auto_off, integer_setting_update)
            );
        }
        (Some(U::Backlight(u)), Some(Group::Backlight(l))) => {
            save!(l.enabled, change_of!(u.enabled, bool_setting_update));
            save!(l.mode, change_of!(u.mode, backlight_mode_setting_update));
            save!(
                l.brightness,
                change_of!(u.brightness, integer_setting_update)
            );
            if let (Some(d), Some(du)) = (l.delay.as_mut(), u.delay.as_ref()) {
                save!(
                    d.hands_away,
                    change_of!(du.hands_away, integer_setting_update)
                );
                save!(
                    d.hands_nearby,
                    change_of!(du.hands_nearby, integer_setting_update)
                );
                save!(
                    d.external_power,
                    change_of!(du.external_power, integer_setting_update)
                );
            }
            save!(
                l.startup_effect,
                change_of!(u.startup_effect, bool_setting_update)
            );
            save!(
                l.dial_effect,
                change_of!(u.dial_effect, bool_setting_update)
            );
            save!(l.power_save, change_of!(u.power_save, bool_setting_update));
            save!(
                l.effect,
                change_of!(u.effect, backlight_effect_setting_update)
            );
        }
        (Some(U::PointerSensor(u)), Some(Group::PointerSensor(s))) => {
            save!(s.dpi, change_of!(u.dpi, integer_setting_update));
        }
        (Some(U::Wheel(u)), Some(Group::Wheel(w))) => {
            save!(w.mode, change_of!(u.mode, wheel_mode_setting_update));
            save!(
                w.free_spin_threshold,
                change_of!(u.free_spin_threshold, integer_setting_update)
            );
            save!(w.invert, change_of!(u.invert, bool_setting_update));
        }
        (Some(U::Thumbwheel(u)), Some(Group::Thumbwheel(t))) => {
            save!(t.invert, change_of!(u.invert, bool_setting_update));
        }
        _ => {}
    }
}

fn update_group(u: &U) -> G {
    match u {
        U::Keyboard(_) => G::Keyboard,
        U::Power(_) => G::Power,
        U::Backlight(_) => G::Backlight,
        U::PointerSensor(_) => G::PointerSensor,
        U::Wheel(_) => G::Wheel,
        U::Thumbwheel(_) => G::Thumbwheel,
    }
}

/// Behavior the device reports without accepting writes for it, with each reading's leaf and
/// sensor index, in display order.
pub fn readings(info: &p::DeviceInformation) -> Vec<(Field, Value)> {
    let mut out = Vec::new();
    let mut put = |leaf, index, v: Option<Value>| {
        if let Some(v) = v {
            out.push((
                Field {
                    leaf,
                    index,
                    integration: IntegrationKind::Unspecified as i32,
                },
                v,
            ));
        }
    };
    let integer = |n: Option<u32>| n.map(|n| Value::Integer(n.into()));
    if let Some(k) = &info.keyboard {
        put(Leaf::FunctionRow, None, k.function_row.map(Value::Choice));
        put(Leaf::KeyboardPlatform, None, k.platform.map(Value::Choice));
    }
    if let Some(l) = &info.backlight {
        put(Leaf::BacklightEnabled, None, l.enabled.map(Value::Bool));
        put(Leaf::BacklightMode, None, l.mode.map(Value::Choice));
        put(Leaf::Brightness, None, integer(l.brightness));
        put(Leaf::CurrentBrightness, None, integer(l.current_brightness));
        put(Leaf::BacklightStatus, None, l.status.map(Value::Choice));
        put(Leaf::BacklightEffect, None, l.effect.map(Value::Choice));
        let delay = l.delay.as_ref();
        put(
            Leaf::HandsAway,
            None,
            integer(delay.and_then(|d| d.hands_away)),
        );
        put(
            Leaf::HandsNearby,
            None,
            integer(delay.and_then(|d| d.hands_nearby)),
        );
        put(
            Leaf::ExternalPower,
            None,
            integer(delay.and_then(|d| d.external_power)),
        );
        put(Leaf::StartupEffect, None, l.startup_effect.map(Value::Bool));
        put(Leaf::DialEffect, None, l.dial_effect.map(Value::Bool));
        put(Leaf::PowerSave, None, l.power_save.map(Value::Bool));
    }
    for s in &info.pointer_sensors {
        put(Leaf::Dpi, Some(s.index), integer(s.dpi));
    }
    if let Some(w) = &info.wheel {
        put(Leaf::WheelMode, None, w.mode.map(Value::Choice));
        put(
            Leaf::FreeSpinThreshold,
            None,
            integer(w.free_spin_threshold),
        );
        put(Leaf::WheelInvert, None, w.invert.map(Value::Bool));
        put(
            Leaf::ResolutionMultiplier,
            None,
            integer(w.resolution_multiplier),
        );
        put(
            Leaf::RatchetsPerRotation,
            None,
            integer(w.ratchets_per_rotation),
        );
        put(Leaf::Diameter, None, integer(w.diameter));
    }
    if let Some(t) = &info.thumbwheel {
        put(Leaf::ThumbwheelInvert, None, t.invert.map(Value::Bool));
    }
    if let Some(power) = &info.power {
        put(Leaf::AutoOff, None, integer(power.auto_off));
    }
    out
}

/// A setting's value type.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Type {
    Bool,
    Integer,
    Choice,
}

pub fn kind(s: &Setting) -> Type {
    match s.limits {
        Limits::Bool => Type::Bool,
        Limits::Integer(_) => Type::Integer,
        Limits::Choice(_) => Type::Choice,
    }
}

/// An integer setting's range: minimum, maximum, and step.
pub fn range(s: &Setting) -> Option<(i64, i64, i64)> {
    match &s.limits {
        Limits::Integer(Some(integer_setting::Limits::Range(r))) => {
            Some((r.min, r.max, r.step.clamp(1, i64::MAX as u64) as i64))
        }
        _ => None,
    }
}

/// How many steps of `step` span `min` through `max`; negative when `min` is above `max`.
pub fn step_count(min: i64, max: i64, step: i64) -> i128 {
    (i128::from(max) - i128::from(min)) / i128::from(step.max(1))
}

/// The values an integer or enum setting lists. An enum lists only the known members it names,
/// each once.
pub fn choices(s: &Setting) -> Vec<Value> {
    match &s.limits {
        Limits::Integer(Some(integer_setting::Limits::Choices(c))) => {
            c.values.iter().map(|n| Value::Integer(*n)).collect()
        }
        Limits::Choice(list) => {
            let mut out = Vec::new();
            for n in list {
                let v = Value::Choice(*n);
                if *n != 0 && s.field.leaf.choice_token(*n) != "unknown" && !out.contains(&v) {
                    out.push(v);
                }
            }
            out
        }
        _ => Vec::new(),
    }
}

/// Whether the setting's type and limits accept `v`.
pub fn accepts(s: &Setting, v: &Value) -> bool {
    match (&s.limits, v) {
        (Limits::Bool, Value::Bool(_)) => true,
        (Limits::Integer(_), Value::Integer(n)) => {
            if let Some((min, max, step)) = range(s) {
                return (min..=max).contains(n)
                    && (i128::from(*n) - i128::from(min)) % i128::from(step) == 0;
            }
            let choices = choices(s);
            choices.is_empty() || choices.contains(v)
        }
        (Limits::Choice(_), Value::Choice(_)) => choices(s).contains(v),
        _ => false,
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use p::{ErrorCode, settings_object::Group};

    pub fn object(group: Group) -> p::SettingsObject {
        p::SettingsObject {
            integration: IntegrationKind::Hidpp as i32,
            group: Some(group),
        }
    }

    pub fn ranged(value: Option<i64>, min: i64, max: i64, step: u64) -> p::IntegerSetting {
        p::IntegerSetting {
            value,
            limits: Some(integer_setting::Limits::Range(p::IntegerRange {
                min,
                max,
                step,
            })),
            ..Default::default()
        }
    }

    pub fn sensor(index: u32, dpi: p::IntegerSetting) -> p::SettingsObject {
        object(Group::PointerSensor(p::PointerSensorSettings {
            index,
            dpi: Some(dpi),
        }))
    }

    pub fn wheel(mode: Option<i32>, invert: Option<bool>) -> p::SettingsObject {
        object(Group::Wheel(p::WheelSettings {
            mode: Some(p::WheelModeSetting {
                value: mode,
                choices: vec![p::WheelMode::FreeSpin as i32, p::WheelMode::Stepped as i32],
                ..Default::default()
            }),
            free_spin_threshold: None,
            invert: Some(p::BoolSetting {
                value: invert,
                ..Default::default()
            }),
        }))
    }

    #[test]
    fn paths_name_fields_and_parse_back() {
        let sensor = Field::hidpp(Leaf::Dpi, Some(0));
        assert_eq!(sensor.path(), "pointer_sensor.0.dpi");
        assert_eq!(Field::parse("pointer_sensor.0.dpi"), Some(sensor));
        assert_eq!(Field::parse("pointer_sensor.dpi"), None);
        assert_eq!(Field::parse("wheel.0.mode"), None);
        for leaf in SETTABLE {
            let index = (leaf == Leaf::Dpi).then_some(1);
            let field = Field::hidpp(leaf, index);
            assert_eq!(Field::parse(&field.path()), Some(field), "{}", field.path());
        }
        assert_eq!(
            Field::parse("backlight.hands_away"),
            Some(Field::hidpp(Leaf::HandsAway, None))
        );
        // Readings can't be saved.
        assert_eq!(Field::parse("wheel.diameter"), None);
        assert_eq!(Field::parse("backlight.current_brightness"), None);
    }

    #[test]
    fn objects_flatten_into_fields_with_their_identity() {
        let backlight = object(Group::Backlight(p::BacklightSettings {
            enabled: Some(p::BoolSetting {
                value: Some(false),
                saved: Some(false),
                status: Some(p::SettingStatus {
                    result: Some(setting_status::Result::Error(p::Error {
                        code: ErrorCode::Timeout as i32,
                        ..Default::default()
                    })),
                }),
            }),
            delay: Some(p::BacklightDelaySettings {
                external_power: Some(ranged(Some(0), 0, 600, 5)),
                ..Default::default()
            }),
            ..Default::default()
        }));
        let all = settings(&[backlight, sensor(0, ranged(Some(800), 200, 4000, 50))]);
        let fields: Vec<Field> = all.iter().map(|s| s.field).collect();
        assert_eq!(
            fields,
            [
                Field::hidpp(Leaf::BacklightEnabled, None),
                Field::hidpp(Leaf::ExternalPower, None),
                Field::hidpp(Leaf::Dpi, Some(0)),
            ]
        );
        assert_eq!(all[0].value, Some(Value::Bool(false)));
        assert_eq!(
            all[0].status,
            Some(Err(p::Error {
                code: ErrorCode::Timeout as i32,
                ..Default::default()
            }))
        );
        assert_eq!(all[1].value, Some(Value::Integer(0)));
        assert_eq!(all[1].status, None);
    }

    #[test]
    fn updates_keep_false_zero_and_the_sensor_index() {
        let off = update(
            Field::hidpp(Leaf::WheelInvert, None),
            Some(Value::Bool(false)),
        );
        let Some(U::Wheel(w)) = off.group else {
            panic!("{off:?}")
        };
        assert_eq!(
            w.invert.unwrap().change,
            Some(p::bool_setting_update::Change::Value(false))
        );
        assert!(w.mode.is_none() && w.free_spin_threshold.is_none());
        let never = update(Field::hidpp(Leaf::AutoOff, None), Some(Value::Integer(0)));
        assert_eq!(
            never.group,
            Some(U::Power(p::PowerSettingsUpdate {
                auto_off: Some(p::IntegerSettingUpdate {
                    change: Some(p::integer_setting_update::Change::Value(0)),
                }),
            }))
        );
        let first = update(Field::hidpp(Leaf::Dpi, Some(0)), None);
        let Some(U::PointerSensor(s)) = first.group else {
            panic!("{first:?}")
        };
        assert_eq!(s.index, 0);
        assert_eq!(
            s.dpi.unwrap().change,
            Some(p::integer_setting_update::Change::Forget(
                p::SettingForget {}
            ))
        );
        let away = update(
            Field::hidpp(Leaf::HandsAway, None),
            Some(Value::Integer(30)),
        );
        let Some(U::Backlight(l)) = away.group else {
            panic!("{away:?}")
        };
        let delay = l.delay.unwrap();
        assert!(delay.hands_nearby.is_none() && delay.external_power.is_none());
        assert_eq!(
            delay.hands_away.unwrap().change,
            Some(p::integer_setting_update::Change::Value(30))
        );
    }

    #[test]
    fn accepted_updates_save_and_forget_one_field() {
        let mut objects = vec![
            sensor(0, ranged(Some(800), 200, 4000, 50)),
            sensor(1, ranged(Some(1600), 200, 4000, 50)),
            wheel(Some(p::WheelMode::Stepped as i32), Some(true)),
        ];
        take_update(
            &mut objects,
            &update(Field::hidpp(Leaf::Dpi, Some(1)), Some(Value::Integer(400))),
        );
        take_update(
            &mut objects,
            &update(
                Field::hidpp(Leaf::WheelInvert, None),
                Some(Value::Bool(false)),
            ),
        );
        let all = settings(&objects);
        assert_eq!(all[0].saved, None);
        assert_eq!(all[1].saved, Some(Value::Integer(400)));
        assert_eq!(all[1].status, Some(Ok(SettingState::Pending)));
        let invert = all
            .iter()
            .find(|s| s.field.leaf == Leaf::WheelInvert)
            .unwrap();
        assert_eq!(invert.saved, Some(Value::Bool(false)));
        take_update(
            &mut objects,
            &update(Field::hidpp(Leaf::Dpi, Some(1)), None),
        );
        let all = settings(&objects);
        assert_eq!((all[1].saved, all[1].status.clone()), (None, None));
        // The other fields of the object are untouched.
        assert_eq!(all[2].field.leaf, Leaf::WheelMode);
        assert_eq!(
            all[2].value,
            Some(Value::Choice(p::WheelMode::Stepped as i32))
        );
    }

    #[test]
    fn enum_choices_skip_unknown_values() {
        let mut objects = vec![wheel(Some(9), None)];
        if let Some(Group::Wheel(w)) = &mut objects[0].group {
            w.mode.as_mut().unwrap().choices.extend([0, 9, 1]);
        }
        let mode = settings(&objects).remove(0);
        assert_eq!(
            choices(&mode),
            [
                Value::Choice(p::WheelMode::FreeSpin as i32),
                Value::Choice(p::WheelMode::Stepped as i32)
            ]
        );
        assert_eq!(mode.field.leaf.choice_token(9), "unknown");
        assert_eq!(mode.field.leaf.choice_token(1), "free_spin");
        assert!(!accepts(&mode, &Value::Choice(0)));
        assert!(accepts(&mode, &Value::Choice(2)));
    }
}
