//! Readers for the protocol's records: enum fields as their known values, keyed information,
//! integration status, the pairing step and setting types.
use cordial_protocol::{
    self as p, CodeKind, DeviceState, ErrorCode, InactiveReason, IntegrationKind, IntegrationState,
    Kind, OutputMode, Platform, Role, SettingState, Transport, integer_setting, integration, keys,
    pairing, setting, setting_change, value::Value,
};

/// An enum value's name as the CLI spells it: lowercase, without the type prefix.
pub fn token(name: &str) -> String {
    let rest = PREFIXES
        .iter()
        .find_map(|prefix| name.strip_prefix(prefix))
        .unwrap_or(name);
    rest.to_lowercase()
}

const PREFIXES: &[&str] = &[
    "ERROR_CODE_",
    "CAPACITY_REASON_",
    "DEVICE_STATE_",
    "INACTIVE_REASON_",
    "INTEGRATION_STATE_",
    "INTEGRATION_KIND_",
    "SETTING_STATE_",
    "WARNING_CODE_",
    "REPORT_TYPE_",
    "TRANSPORT_",
    "PLATFORM_",
    "KIND_",
    "ROLE_",
    "CODE_KIND_",
    "CONFIGURATION_INTERFACE_",
    "OUTPUT_MODE_",
    "OUTPUT_STATE_",
];

pub fn code_token(code: ErrorCode) -> String {
    token(code.as_str_name())
}

/// An error code from a wire number; numbers this build doesn't know read as unknown.
pub fn code(n: i32) -> ErrorCode {
    ErrorCode::try_from(n).unwrap_or(ErrorCode::Unknown)
}

pub fn transport(n: i32) -> Option<Transport> {
    Transport::try_from(n)
        .ok()
        .filter(|t| *t != Transport::Unspecified)
}

pub fn connected(d: &p::Device) -> bool {
    d.state() == DeviceState::Connected
}

/// Why the Dongle doesn't use a saved device, or `None` when it does.
pub fn inactive(d: &p::Device) -> Option<InactiveReason> {
    d.inactive
        .map(|n| InactiveReason::try_from(n).unwrap_or(InactiveReason::Unknown))
}

/// The last failed connection attempt.
pub fn last_error(d: &p::Device) -> Option<ErrorCode> {
    d.error.map(code)
}

/// Why a connected device's profiles aren't loaded, while they aren't.
pub fn profile_error(d: &p::Device) -> Option<ErrorCode> {
    d.profile_error.map(code)
}

/// The roles this build knows, in the order given.
pub fn known_roles(roles: &[i32]) -> Vec<Role> {
    roles
        .iter()
        .filter_map(|n| Role::try_from(*n).ok())
        .filter(|r| *r != Role::Unknown)
        .collect()
}

/// The device's roles this build knows, in the device's order.
pub fn roles(d: &p::Device) -> Vec<Role> {
    known_roles(&d.roles)
}

/// The kinds this build knows, in the order given.
pub fn known_kinds(kinds: &[i32]) -> Vec<Kind> {
    kinds
        .iter()
        .filter_map(|n| Kind::try_from(*n).ok())
        .filter(|k| *k != Kind::Unknown)
        .collect()
}

pub fn hidpp(d: &p::Device) -> Option<&p::Integration> {
    d.integrations
        .iter()
        .find(|i| i.kind == IntegrationKind::Hidpp as i32)
}

/// Whether the HID++ preference is on.
pub fn hidpp_enabled(d: &p::Device) -> bool {
    hidpp(d).is_some_and(|i| i.enabled)
}

/// Whether an integration is up, or why it failed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Up {
    Off,
    Disconnected,
    Starting,
    Active,
    Unsupported,
    Error(ErrorCode),
}

pub fn up(i: &p::Integration) -> Up {
    match i.status {
        Some(integration::Status::Error(n)) => Up::Error(code(n)),
        Some(integration::Status::State(n)) => match IntegrationState::try_from(n) {
            Ok(IntegrationState::Disconnected) => Up::Disconnected,
            Ok(IntegrationState::Starting) => Up::Starting,
            Ok(IntegrationState::Active) => Up::Active,
            Ok(IntegrationState::Unsupported) => Up::Unsupported,
            _ => Up::Off,
        },
        None => Up::Off,
    }
}

/// HID++'s status on a device; off when the device has no HID++ record.
pub fn hidpp_up(d: &p::Device) -> Up {
    hidpp(d).map_or(Up::Off, up)
}

/// The HID++ version the device reported on its current link.
pub fn hidpp_version(d: &p::Device) -> Option<(u32, u32)> {
    let v = hidpp(d)?.detected.as_ref()?.version.as_ref()?;
    Some((v.major, v.minor))
}

/// The value of an information key.
pub fn info<'a>(list: &'a [p::Info], key: &str) -> Option<&'a Value> {
    list.iter()
        .find(|i| i.key == key)?
        .value
        .as_ref()?
        .value
        .as_ref()
}

pub fn info_bool(list: &[p::Info], key: &str) -> Option<bool> {
    match info(list, key)? {
        Value::Bool(b) => Some(*b),
        _ => None,
    }
}

pub fn info_integer(list: &[p::Info], key: &str) -> Option<i64> {
    match info(list, key)? {
        Value::Integer(n) => Some(*n),
        _ => None,
    }
}

pub fn info_text<'a>(list: &'a [p::Info], key: &str) -> Option<&'a str> {
    match info(list, key)? {
        Value::Text(t) => Some(t),
        _ => None,
    }
}

/// Pairing is refused while the Dongle's storage is full.
pub fn storage_full(st: &p::Status) -> bool {
    info_bool(&st.info, keys::STORAGE_FULL) == Some(true)
}

/// Development firmware, which answers the file, feature and bootloader commands.
pub fn development(st: &p::Status) -> bool {
    info_bool(&st.info, keys::BUILD_DEVELOPMENT) == Some(true)
}

/// The transports this firmware supports, enabled or not, in its order.
pub fn transports(st: &p::Status) -> Vec<Transport> {
    st.transports
        .iter()
        .filter_map(|t| transport(t.transport))
        .collect()
}

pub fn supports(st: &p::Status, t: Transport) -> bool {
    transports(st).contains(&t)
}

/// Whether a supported transport is enabled; `None` when the firmware doesn't support it. A
/// missing `enabled` means enabled.
pub fn transport_enabled(st: &p::Status, t: Transport) -> Option<bool> {
    st.transports
        .iter()
        .find(|s| s.transport == t as i32)
        .map(|s| s.enabled != Some(false))
}

/// The supported transports that are enabled, in the firmware's order.
pub fn enabled_transports(st: &p::Status) -> Vec<Transport> {
    transports(st)
        .into_iter()
        .filter(|t| transport_enabled(st, *t) == Some(true))
        .collect()
}

/// Whether the firmware supports the transport and has it disabled.
pub fn transport_disabled(st: &p::Status, t: Transport) -> bool {
    transport_enabled(st, t) == Some(false)
}

/// How many devices of a transport can be enabled at once, when the Dongle reports it.
pub fn max_enabled(st: &p::Status, t: Transport) -> Option<u32> {
    st.transports
        .iter()
        .find(|s| s.transport == t as i32)?
        .max_enabled
}

pub fn platform(st: &p::Status) -> Platform {
    st.platform()
}

/// The gamepad output modes the adapter supports besides Disabled, with their slots.
pub fn gamepad_support(st: &p::Status) -> Vec<&p::OutputModeSupport> {
    st.output_support
        .iter()
        .filter(|s| s.role == Role::Gamepad as i32)
        .flat_map(|s| &s.modes)
        .filter(|m| matches!(OutputMode::try_from(m.mode), Ok(m) if m != OutputMode::Disabled))
        .collect()
}

/// The gamepad output modes the adapter supports besides Disabled.
pub fn gamepad_modes(st: &p::Status) -> Vec<OutputMode> {
    gamepad_support(st).into_iter().map(|m| m.mode()).collect()
}

/// The device's saved gamepad output mode: Disabled without a saved choice, and `None` for a
/// mode this client doesn't know.
pub fn gamepad_mode(d: &p::Device) -> Option<OutputMode> {
    d.output_preferences
        .iter()
        .rfind(|o| o.role == Role::Gamepad as i32)
        .map_or(Some(OutputMode::Disabled), |o| {
            OutputMode::try_from(o.mode).ok()
        })
}

/// Whether the device has a saved gamepad output choice other than Disabled, including one this
/// client doesn't know.
pub fn gamepad_saved(d: &p::Device) -> bool {
    gamepad_mode(d) != Some(OutputMode::Disabled)
}

/// The device's gamepad output state, when it reports one.
pub fn gamepad_output(d: &p::Device) -> Option<&p::OutputStatus> {
    d.outputs.iter().find(|o| o.role == Role::Gamepad as i32)
}

/// Whether the device has a gamepad output to choose: it is a gamepad, or has a saved gamepad
/// output choice.
pub fn gamepad(d: &p::Device) -> bool {
    d.roles.contains(&(Role::Gamepad as i32))
        || d.kinds.contains(&(Kind::Gamepad as i32))
        || gamepad_saved(d)
}

/// The gamepad output modes the device can be given: Disabled, those the adapter supports, and
/// its saved one even when the adapter no longer supports it.
pub fn gamepad_choices(st: &p::Status, d: &p::Device) -> Vec<OutputMode> {
    let saved = gamepad_mode(d);
    let supported = gamepad_modes(st);
    [OutputMode::Disabled, OutputMode::Hid, OutputMode::Xinput]
        .into_iter()
        .filter(|m| *m == OutputMode::Disabled || Some(*m) == saved || supported.contains(m))
        .collect()
}

/// Whether giving device `id` gamepad output `mode` changes a gamepad pool the adapter offers,
/// which reconnects USB. Each pool holds the saved devices selecting its mode, up to the mode's
/// advertised capacity, whether or not they are connected, enabled or usable. Records the adapter couldn't read
/// can only fill the pools further, so leaving them out never misses a change.
pub fn gamepad_pools_change(
    st: &p::Status,
    devices: &[p::Device],
    id: u32,
    mode: OutputMode,
) -> bool {
    let pool = |of: &p::OutputModeSupport, changed: bool| {
        devices
            .iter()
            .filter(|d| {
                let selected = if changed && d.id == id {
                    Some(mode)
                } else {
                    gamepad_mode(d)
                };
                selected == Some(of.mode())
            })
            .count()
            .min(of.max_slots as usize)
    };
    gamepad_support(st)
        .into_iter()
        .any(|of| pool(of, false) != pool(of, true))
}

/// What a pairing asks of the user.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Prompt {
    /// Type a code on this computer.
    EnterCode(CodeKind),
    /// Compare a passkey with the one the device shows.
    ConfirmCode(String),
    /// Type the value on the device.
    ShowCode(CodeKind, String),
}

pub fn prompt(p: &p::Pairing) -> Option<Prompt> {
    Some(match p.step.as_ref()? {
        pairing::Step::EnterCode(e) => Prompt::EnterCode(e.kind()),
        pairing::Step::ConfirmCode(c) => Prompt::ConfirmCode(c.passkey.clone()),
        pairing::Step::ShowCode(s) => Prompt::ShowCode(s.kind(), s.value.clone()),
        _ => return None,
    })
}

/// Whether a pairing is still running: its step is neither done nor failed.
pub fn pairing_running(p: &p::Pairing) -> bool {
    !matches!(
        p.step,
        Some(pairing::Step::Done(_) | pairing::Step::Failed(_))
    )
}

/// Whether the pairing waits for an answer from this computer.
pub fn answerable(p: &p::Pairing) -> bool {
    matches!(
        p.step,
        Some(pairing::Step::EnterCode(_) | pairing::Step::ConfirmCode(_))
    )
}

/// A setting's value type.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Type {
    Bool,
    Integer,
    Enum,
    Text,
    Color,
}

pub fn kind(s: &p::Setting) -> Option<Type> {
    Some(match s.r#type.as_ref()? {
        setting::Type::Bool(_) => Type::Bool,
        setting::Type::Integer(_) => Type::Integer,
        setting::Type::Enum(_) => Type::Enum,
        setting::Type::Text(_) => Type::Text,
        setting::Type::Color(_) => Type::Color,
    })
}

/// The last value read from the device.
pub fn current(s: &p::Setting) -> Option<Value> {
    Some(match s.r#type.as_ref()? {
        setting::Type::Bool(t) => Value::Bool(t.value?),
        setting::Type::Integer(t) => Value::Integer(t.value?),
        setting::Type::Enum(t) => Value::Text(t.value.clone()?),
        setting::Type::Text(t) => Value::Text(t.value.clone()?),
        setting::Type::Color(t) => Value::Color(t.value?),
    })
}

/// The value saved on the Dongle, or `None` when the Dongle leaves the setting alone.
pub fn saved(s: &p::Setting) -> Option<Value> {
    Some(match s.r#type.as_ref()? {
        setting::Type::Bool(t) => Value::Bool(t.saved?),
        setting::Type::Integer(t) => Value::Integer(t.saved?),
        setting::Type::Enum(t) => Value::Text(t.saved.clone()?),
        setting::Type::Text(t) => Value::Text(t.saved.clone()?),
        setting::Type::Color(t) => Value::Color(t.saved?),
    })
}

/// How applying a saved value went: `None` without a saved value.
pub fn applied(s: &p::Setting) -> Option<Result<SettingState, ErrorCode>> {
    Some(match s.status? {
        setting::Status::State(n) => Ok(SettingState::try_from(n).unwrap_or_default()),
        setting::Status::Error(n) => Err(code(n)),
    })
}

/// An integer setting's range: minimum, maximum and step.
pub fn range(s: &p::Setting) -> Option<(i64, i64, i64)> {
    match s.r#type.as_ref()? {
        setting::Type::Integer(p::IntegerSetting {
            limits: Some(integer_setting::Limits::Range(r)),
            ..
        }) => Some((r.min, r.max, (r.step.max(1)).min(i64::MAX as u64) as i64)),
        _ => None,
    }
}

/// How many steps of `step` span `min` through `max`; negative when `min` is above `max`.
pub fn step_count(min: i64, max: i64, step: i64) -> i128 {
    (i128::from(max) - i128::from(min)) / i128::from(step.max(1))
}

/// The values an integer or enum setting accepts, when it lists them.
pub fn choices(s: &p::Setting) -> Vec<Value> {
    match s.r#type.as_ref() {
        Some(setting::Type::Integer(p::IntegerSetting {
            limits: Some(integer_setting::Limits::Choices(c)),
            ..
        })) => c.values.iter().map(|n| Value::Integer(*n)).collect(),
        Some(setting::Type::Enum(e)) => e.choices.iter().cloned().map(Value::Text).collect(),
        _ => Vec::new(),
    }
}

/// The longest text value the device accepts, in UTF-8 bytes.
pub fn max_bytes(s: &p::Setting) -> Option<u32> {
    match s.r#type.as_ref()? {
        setting::Type::Text(t) => t.max_bytes,
        _ => None,
    }
}

/// Whether the setting's type and limits accept `v`.
pub fn accepts(s: &p::Setting, v: &Value) -> bool {
    match (kind(s), v) {
        (Some(Type::Bool), Value::Bool(_)) => true,
        (Some(Type::Integer), Value::Integer(n)) => {
            if let Some((min, max, step)) = range(s) {
                return (min..=max).contains(n)
                    && (i128::from(*n) - i128::from(min)) % i128::from(step) == 0;
            }
            let choices = choices(s);
            choices.is_empty() || choices.contains(v)
        }
        (Some(Type::Enum), Value::Text(_)) => choices(s).contains(v),
        (Some(Type::Text), Value::Text(t)) => max_bytes(s).is_none_or(|m| t.len() <= m as usize),
        (Some(Type::Color), Value::Color(c)) => *c <= 0xff_ffff,
        _ => false,
    }
}

/// The catalog entry of a key, when this build knows it.
pub fn key(key: &str) -> Option<(&'static keys::Key, Option<u32>)> {
    keys::lookup(key)
}

/// A change that saves `v` as the setting's value.
pub fn save_change(s: &p::Setting, v: Value) -> p::SettingChange {
    p::SettingChange {
        integration: s.integration,
        key: s.key.clone(),
        change: Some(setting_change::Change::Value(wire_value(v))),
    }
}

/// A change that forgets the setting's saved value.
pub fn forget_change(s: &p::Setting) -> p::SettingChange {
    p::SettingChange {
        integration: s.integration,
        key: s.key.clone(),
        change: Some(setting_change::Change::Forget(p::SettingForget {})),
    }
}

/// Wraps a value for the wire.
pub fn wire_value(v: Value) -> p::Value {
    p::Value { value: Some(v) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pad(id: u32, mode: i32) -> p::Device {
        p::Device {
            id,
            output_preferences: vec![p::OutputPreference {
                role: Role::Gamepad as i32,
                mode,
            }],
            ..Default::default()
        }
    }

    fn offering(modes: &[OutputMode]) -> p::Status {
        p::Status {
            output_support: vec![p::OutputSupport {
                role: Role::Gamepad as i32,
                modes: modes
                    .iter()
                    .map(|&mode| p::OutputModeSupport {
                        mode: mode as i32,
                        max_slots: 4,
                        slots: Some(1),
                    })
                    .collect(),
            }],
            ..Default::default()
        }
    }

    #[test]
    fn gamepad_pools_count_every_saved_choice_up_to_four() {
        use OutputMode::{Disabled, Hid, Xinput};
        let st = offering(&[Hid, Xinput]);
        let mut devices: Vec<_> = (1..=4).map(|id| pad(id, Hid as i32)).collect();
        // Disabled, blocked, nonresident and transport-disabled devices still count.
        devices[1].enabled = false;
        devices[2].blocked = true;
        devices[2].inactive = Some(InactiveReason::Blocked as i32);
        devices[3].inactive = Some(InactiveReason::Capacity as i32);
        assert!(gamepad_pools_change(&st, &devices, 1, Disabled));
        assert!(!gamepad_pools_change(&st, &devices, 1, Hid));
        let mut off = pad(5, Hid as i32);
        off.inactive = Some(InactiveReason::TransportDisabled as i32);
        devices.push(off);
        // Five select HID: the pool stays at four without one of them.
        assert!(!gamepad_pools_change(&st, &devices, 1, Disabled));
        assert!(gamepad_pools_change(&st, &devices, 1, Xinput));
        devices.extend((6..=9).map(|id| pad(id, Xinput as i32)));
        assert!(gamepad_pools_change(&st, &devices, 6, Hid));
        devices.push(pad(10, Xinput as i32));
        assert!(!gamepad_pools_change(&st, &devices, 1, Disabled));
        // Both pools stay full.
        assert!(!gamepad_pools_change(&st, &devices, 1, Xinput));
        assert!(!gamepad_pools_change(&st, &devices, 6, Hid));
        // A choice this client doesn't know is in neither pool.
        let mut few = vec![pad(1, 7), pad(2, Hid as i32)];
        assert!(!gamepad_pools_change(&st, &few, 1, Disabled));
        assert!(gamepad_pools_change(&st, &few, 1, Hid));
        // Without a saved choice, a device is Disabled.
        few[0].output_preferences.clear();
        assert!(!gamepad_pools_change(&st, &few, 1, Disabled));
        assert!(gamepad_pools_change(&st, &few, 1, Xinput));
    }

    #[test]
    fn gamepad_pools_the_adapter_does_not_offer_never_change() {
        use OutputMode::{Disabled, Hid, Xinput};
        let devices = vec![pad(1, Hid as i32), pad(2, Xinput as i32)];
        // Clearing a stale choice on an adapter without gamepad output.
        let none = p::Status::default();
        assert!(!gamepad_pools_change(&none, &devices, 1, Disabled));
        assert!(!gamepad_pools_change(&none, &devices, 2, Disabled));
        let hid = offering(&[Hid]);
        assert!(!gamepad_pools_change(&hid, &devices, 2, Disabled));
        assert!(gamepad_pools_change(&hid, &devices, 1, Disabled));
        assert!(gamepad_pools_change(&hid, &devices, 2, Hid));
    }
}
