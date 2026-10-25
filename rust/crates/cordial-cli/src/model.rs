//! Readers for the protocol's records: enum fields as their known values, resources and errors,
//! integration status and the pairing step.
use cordial_protocol::{
    self as p, CodeKind, DeviceState, ErrorCode, InactiveReason, IntegrationKind, IntegrationState,
    Kind, OutputMode, Platform, ResourceType, Role, Transport, integration, pairing,
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
    "KEYBOARD_PLATFORM_",
    "FUNCTION_ROW_MODE_",
    "BACKLIGHT_STATUS_",
    "BACKLIGHT_MODE_",
    "BACKLIGHT_EFFECT_",
    "WHEEL_MODE_",
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

/// Device errors that say why a connected device's profiles aren't loaded.
const PROFILE_ERRORS: [ErrorCode; 2] = [
    ErrorCode::ProfileMemoryExceeded,
    ErrorCode::ProfileLoadFailed,
];

/// The last failed connection attempt.
pub fn last_error(d: &p::Device) -> Option<ErrorCode> {
    d.errors
        .iter()
        .map(|n| code(*n))
        .find(|c| !PROFILE_ERRORS.contains(c))
}

/// Why a connected device's profiles aren't loaded, while they aren't.
pub fn profile_error(d: &p::Device) -> Option<ErrorCode> {
    d.errors
        .iter()
        .map(|n| code(*n))
        .find(|c| PROFILE_ERRORS.contains(c))
}

/// Whether the adapter reports a problem.
pub fn has_error(st: &p::Status, error: ErrorCode) -> bool {
    st.errors.contains(&(error as i32))
}

/// The entry of a resource type in a list of resources.
pub fn resource(list: &[p::ResourceCapacity], kind: ResourceType) -> Option<&p::ResourceCapacity> {
    list.iter().find(|r| r.r#type == kind as i32)
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

/// Pairing is refused while the Dongle's storage is full.
pub fn storage_full(st: &p::Status) -> bool {
    has_error(st, ErrorCode::StorageFull)
}

/// Development firmware, which answers the file, feature and bootloader commands.
pub fn development(st: &p::Status) -> bool {
    st.information
        .as_ref()
        .is_some_and(|i| i.development_build == Some(true))
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

/// A transport's saved enabled devices and how many it allows, each when the Dongle reports it.
pub fn enabled_devices(st: &p::Status, t: Transport) -> Option<&p::ResourceCapacity> {
    let support = st.transports.iter().find(|s| s.transport == t as i32)?;
    resource(&support.resources, ResourceType::Devices)
}

pub fn platform(st: &p::Status) -> Platform {
    st.platform()
}

/// The gamepad output modes the adapter supports besides Disabled, with their resources.
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
