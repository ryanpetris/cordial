//! Converts the firmware's model into serial API messages. Internal error codes map onto the wire
//! codes that describe them to a user; raw device data never leaves this module undecoded.
use alloc::{
    string::{String, ToString},
    vec::Vec,
};

use cordial_protocol as p;

mod settings;
pub use settings::*;

use crate::{
    compact::{Observed, Record},
    devices::{Device, Policy},
    manager::{Connection, Manager},
    model::{
        errors::ErrorCode as E,
        hidpp::ProtocolState,
        identifiers::{ConnectionState, HostPlatform, SettingsState, Transport},
        info::{InfoField, InfoKey},
        link::{ConnectionSecurity, DeviceKind},
        settings::{SettingKey, SettingValue},
    },
};

/// The wire code for an internal error.
pub fn error_code(code: E) -> p::ErrorCode {
    use p::ErrorCode as W;
    match code {
        E::UnknownCommand => W::UnknownCommand,
        E::InvalidArgs => W::BadArgs,
        E::Busy => W::Busy,
        E::NotFound | E::NotPending => W::NotFound,
        E::ReadOnly => W::Unsupported,
        E::Blocked => W::Blocked,
        E::Disabled => W::Disabled,
        E::CandidateExpired => W::CandidateExpired,
        E::Capacity | E::StorageFull | E::SettingsLimit => W::NoCapacity,
        E::UnsupportedHid => W::UnsupportedHid,
        E::UnsupportedTransport
        | E::UnsupportedSetting
        | E::SettingsUnavailable
        | E::HidppDisabled
        | E::BacklightModeSelectionRequired
        | E::BacklightPermanentManualRequired
        | E::NativeRoutingRequired
        | E::NativeStandardResolutionRequired => W::Unsupported,
        E::AuthenticationFailed => W::AuthFailed,
        E::AuthenticationRejected => W::Rejected,
        E::StalePrompt => W::NoPrompt,
        E::ConnectionFailed => W::ConnectionFailed,
        E::RadioUnavailable => W::NotReady,
        E::StorageFailed => W::StorageFailed,
        E::Timeout | E::HidppTimeout => W::Timeout,
        E::Cancelled => W::Cancelled,
        E::NotConnected => W::NotConnected,
        E::InUse => W::InUse,
        E::FeatureSetUnavailable | E::HidppResetUnavailable | E::HidppControlsUnavailable => {
            W::FeatureUnavailable
        }
        E::HidppReportsUnavailable | E::HidppProtocolUnsupported => W::ProtocolUnsupported,
        E::HidReportTooLarge | E::HidppTransportError => W::TransportError,
        E::HidppDeviceError => W::DeviceError,
        E::HidppInvalidResponse => W::InvalidResponse,
        E::ReadbackMismatch => W::ReadbackMismatch,
        E::InputOverflow | E::InternalError => W::Internal,
    }
}

/// The wire error for an internal error, with context and evidence retained for every code.
pub fn error(code: E, reason: Option<p::ErrorReason>, outcome_unknown: bool) -> p::Error {
    p::Error {
        code: error_code(code) as i32,
        reason: reason.unwrap_or(match code {
            E::StorageFull | E::SettingsLimit | E::StorageFailed => p::ErrorReason::Storage,
            _ => p::ErrorReason::Unknown,
        }) as i32,
        outcome: if outcome_unknown {
            p::ErrorOutcome::Unknown
        } else {
            p::ErrorOutcome::Unspecified
        } as i32,
        related: Vec::new(),
    }
}

pub fn transport(transport: Transport) -> p::Transport {
    match transport {
        Transport::Classic => p::Transport::Classic,
        Transport::Ble => p::Transport::Ble,
    }
}

pub fn platform(platform: HostPlatform) -> p::Platform {
    match platform {
        HostPlatform::Linux => p::Platform::Linux,
        HostPlatform::Windows => p::Platform::Windows,
        HostPlatform::Mac => p::Platform::Mac,
    }
}

pub fn host_platform(platform: p::Platform) -> HostPlatform {
    match platform {
        p::Platform::Linux => HostPlatform::Linux,
        p::Platform::Windows => HostPlatform::Windows,
        p::Platform::Mac => HostPlatform::Mac,
    }
}

/// What a device that reports itself as `kind` is.
pub fn kinds(kind: DeviceKind) -> Vec<i32> {
    match kind {
        DeviceKind::Unknown => Vec::new(),
        DeviceKind::Keyboard => alloc::vec![p::Kind::Keyboard as i32],
        DeviceKind::Mouse => alloc::vec![p::Kind::Mouse as i32],
        DeviceKind::KeyboardMouse => alloc::vec![p::Kind::Keyboard as i32, p::Kind::Mouse as i32],
        DeviceKind::Gamepad => alloc::vec![p::Kind::Gamepad as i32],
    }
}

fn info_kinds(value: &str) -> Vec<i32> {
    match value {
        "keyboard" => kinds(DeviceKind::Keyboard),
        "mouse" => kinds(DeviceKind::Mouse),
        "gamepad" => kinds(DeviceKind::Gamepad),
        "keyboard_mouse" => kinds(DeviceKind::KeyboardMouse),
        "other" => alloc::vec![p::Kind::Other as i32],
        _ => Vec::new(),
    }
}

/// Device kinds identified by HID descriptor roles.
fn role_kinds(roles: u8) -> Vec<i32> {
    let mut list = Vec::new();
    if roles & crate::hid::KEYBOARD != 0 {
        list.push(p::Kind::Keyboard as i32);
    }
    if roles & crate::hid::MOUSE != 0 {
        list.push(p::Kind::Mouse as i32);
    }
    if roles & crate::outputs::GAMEPAD != 0 {
        list.push(p::Kind::Gamepad as i32);
    }
    if list.is_empty() && roles != 0 {
        list.push(p::Kind::Other as i32);
    }
    list
}

/// The wire roles of `hid` role bits.
pub fn roles(roles: u8) -> Vec<i32> {
    [
        (crate::hid::KEYBOARD, p::Role::Keyboard),
        (crate::hid::MOUSE, p::Role::Mouse),
        (crate::hid::CONSUMER, p::Role::ConsumerControl),
        (crate::hid::SYSTEM, p::Role::SystemControl),
        (crate::outputs::GAMEPAD, p::Role::Gamepad),
    ]
    .into_iter()
    .filter(|(bit, _)| roles & bit != 0)
    .map(|(_, role)| role as i32)
    .collect()
}

fn state(state: ConnectionState) -> p::DeviceState {
    match state {
        ConnectionState::Disconnected => p::DeviceState::Disconnected,
        ConnectionState::Connecting => p::DeviceState::Connecting,
        ConnectionState::Connected => p::DeviceState::Connected,
        ConnectionState::Disconnecting => p::DeviceState::Disconnecting,
    }
}

fn security(security: ConnectionSecurity) -> p::Security {
    p::Security {
        encrypted: security.encrypted,
        authenticated: security.authenticated,
        secure_connections: security.secure_connections,
        key_size: security.key_size.map(u32::from),
    }
}

fn text(value: &SettingValue) -> Option<String> {
    match value {
        SettingValue::Text(v) => Some(v.clone()),
        _ => None,
    }
}
fn integer(value: &SettingValue) -> Option<u32> {
    match value {
        SettingValue::Integer(v) => (*v).try_into().ok(),
        _ => None,
    }
}
fn boolean(value: &SettingValue) -> Option<bool> {
    match value {
        SettingValue::Bool(v) => Some(*v),
        _ => None,
    }
}

/// Current connection facts, including behavior the device only permits reading.
fn device_info(fields: &[InfoField], records: &[Record]) -> p::DeviceInformation {
    let mut info = p::DeviceInformation::default();
    let mut identity = p::DeviceIdentity::default();
    let mut battery = p::BatteryInformation::default();
    for field in fields.iter().filter(|f| f.available) {
        let value = &field.value;
        match (field.key, field.instance) {
            (InfoKey::Manufacturer, _) => identity.manufacturer = text(value),
            (InfoKey::Model, _) => identity.model = text(value),
            (InfoKey::Serial, _) => identity.serial = text(value),
            (InfoKey::Firmware, 0) => identity.firmware_version = text(value),
            (InfoKey::Firmware, 1) => identity.bootloader_version = text(value),
            (InfoKey::Hardware, _) => identity.hardware_revision = text(value),
            (InfoKey::Software, _) => identity.software_revision = text(value),
            (InfoKey::VendorIdNamespace, _) => {
                if let Some(value) = text(value) {
                    identity.vendor.get_or_insert_default().registry = Some(match value.as_str() {
                        "usb" => p::VendorRegistry::Usb,
                        "bluetooth" => p::VendorRegistry::Bluetooth,
                        _ => p::VendorRegistry::Unknown,
                    }
                        as i32);
                }
            }
            (InfoKey::VendorId, _) => {
                if let Some(v) = integer(value) {
                    identity.vendor.get_or_insert_default().id = Some(v);
                }
            }
            (InfoKey::ProductId, _) => {
                if let Some(v) = integer(value) {
                    identity.product.get_or_insert_default().id = Some(v);
                }
            }
            (InfoKey::ProductVersion, _) => {
                if let Some(v) = integer(value) {
                    identity.product.get_or_insert_default().version = Some(v);
                }
            }
            (InfoKey::BatteryPercent, _) => battery.level = integer(value),
            (InfoKey::BatteryCharging, _) => battery.charging = boolean(value),
            _ => {}
        }
    }
    if identity != p::DeviceIdentity::default() {
        info.identity = Some(identity);
    }
    if battery != p::BatteryInformation::default() {
        info.battery = Some(battery);
    }
    for record in records.iter().filter(|r| !r.writable && r.available) {
        let key = record.metadata.key;
        let value = record.observed.wire(key);
        if matches!(record.observed, Observed::Missing) {
            continue;
        }
        use SettingKey::*;
        match key {
            KeyboardPlatform => {
                info.keyboard.get_or_insert_default().platform = enum_observed(record)
            }
            FnRowDefault => {
                info.keyboard.get_or_insert_default().function_row = enum_observed(record)
            }
            PowerAutoOff => info.power.get_or_insert_default().auto_off = integer(&value),
            BacklightCurrentLevel => {
                info.backlight.get_or_insert_default().current_brightness = integer(&value)
            }
            BacklightStatus => {
                info.backlight.get_or_insert_default().status = enum_observed(record)
            }
            BacklightEnabled => info.backlight.get_or_insert_default().enabled = boolean(&value),
            BacklightMode => info.backlight.get_or_insert_default().mode = enum_observed(record),
            BacklightLevel => info.backlight.get_or_insert_default().brightness = integer(&value),
            BacklightDelayHandsOut => {
                info.backlight
                    .get_or_insert_default()
                    .delay
                    .get_or_insert_default()
                    .hands_away = integer(&value)
            }
            BacklightDelayHandsIn => {
                info.backlight
                    .get_or_insert_default()
                    .delay
                    .get_or_insert_default()
                    .hands_nearby = integer(&value)
            }
            BacklightDelayPowered => {
                info.backlight
                    .get_or_insert_default()
                    .delay
                    .get_or_insert_default()
                    .external_power = integer(&value)
            }
            BacklightPowerOn => {
                info.backlight.get_or_insert_default().startup_effect = boolean(&value)
            }
            BacklightCrown => info.backlight.get_or_insert_default().dial_effect = boolean(&value),
            BacklightPowerSave => {
                info.backlight.get_or_insert_default().power_save = boolean(&value)
            }
            BacklightEffect => {
                info.backlight.get_or_insert_default().effect = enum_observed(record)
            }
            PointerDpi0 | PointerDpi1 => info.pointer_sensors.push(p::PointerSensorInformation {
                index: u32::from(key == PointerDpi1),
                dpi: integer(&value),
            }),
            WheelMode => info.wheel.get_or_insert_default().mode = enum_observed(record),
            WheelThreshold => {
                info.wheel.get_or_insert_default().free_spin_threshold = integer(&value)
            }
            WheelInvert => info.wheel.get_or_insert_default().invert = boolean(&value),
            ThumbwheelInvert => info.thumbwheel.get_or_insert_default().invert = boolean(&value),
            WheelInfo => wheel_info(record, &mut info),
        }
    }
    info.pointer_sensors.sort_by_key(|s| s.index);
    info
}

/// Decodes the HiResWheel capability response, including revision-one geometry.
fn wheel_info(record: &Record, info: &mut p::DeviceInformation) {
    let Observed::Text(hex) = &record.observed else {
        return;
    };
    let bytes: Vec<u8> = hex
        .as_bytes()
        .chunks(2)
        .filter_map(|pair| u8::from_str_radix(core::str::from_utf8(pair).ok()?, 16).ok())
        .collect();
    if bytes.len() * 2 != hex.len() || !matches!(bytes.len(), 2 | 4) {
        return;
    }
    let wheel = info.wheel.get_or_insert_default();
    wheel.resolution_multiplier = Some(bytes[0].into());
    if bytes.len() == 4 {
        wheel.ratchets_per_rotation = Some(bytes[2].into());
        wheel.diameter = Some(bytes[3].into());
    }
}

fn hidpp(
    policy: &Policy,
    d: Option<&Device>,
    connection: Option<&Connection>,
) -> Option<p::Integration> {
    let enabled = policy.hidpp_enabled();
    let runtime = connection
        .and_then(|c| c.runtime.as_deref())
        .filter(|_| d.is_some_and(|d| d.state == ConnectionState::Connected));
    let protocol = runtime.map_or(ProtocolState::Unknown, |r| r.client.protocol);
    let detected = match protocol {
        ProtocolState::Detected { major, minor } => Some(p::IntegrationDetection {
            version: Some(p::Version {
                major: major.into(),
                minor: minor.into(),
            }),
        }),
        _ => None,
    };
    let saved = policy
        .integrations
        .iter()
        .any(|i| i.kind == crate::devices::IntegrationKind::Hidpp);
    if !saved && detected.is_none() {
        return None;
    }
    use p::integration::Status;
    let status = if !enabled {
        Status::State(p::IntegrationState::Off as i32)
    } else if let Some(runtime) = runtime {
        match protocol {
            ProtocolState::Unavailable => {
                Status::Error(error(E::HidppProtocolUnsupported, None, false))
            }
            ProtocolState::Error { code } => Status::Error(error(code, None, false)),
            ProtocolState::Unknown | ProtocolState::Probing => {
                Status::State(p::IntegrationState::Starting as i32)
            }
            ProtocolState::Detected { .. } => match runtime.settings.state {
                SettingsState::Ready | SettingsState::Applying | SettingsState::Unsupported => {
                    Status::State(p::IntegrationState::Active as i32)
                }
                SettingsState::Error => Status::Error(error(
                    runtime.settings.error.unwrap_or(E::InternalError),
                    None,
                    false,
                )),
                SettingsState::Off | SettingsState::Pending | SettingsState::Discovering => {
                    Status::State(p::IntegrationState::Starting as i32)
                }
            },
        }
    } else {
        Status::State(p::IntegrationState::Disconnected as i32)
    };
    Some(p::Integration {
        kind: p::IntegrationKind::Hidpp as i32,
        enabled,
        detected,
        status: Some(status),
    })
}

/// The device record of saved device `policy`. `slot` is its resident entry, if it has one; a
/// connected device's own policy takes the place of `policy` once read.
pub fn device(manager: &Manager, policy: &Policy, slot: Option<usize>) -> p::Device {
    let d = slot.and_then(|slot| manager.devices.get(slot)?.as_ref());
    let connection = slot.and_then(|slot| {
        manager
            .connections
            .iter()
            .flatten()
            .find(|c| c.device == Some(slot) && !c.closing)
    });
    let live = d.and_then(|d| d.live.as_deref());
    let policy = live.and_then(|l| l.policy.as_ref()).unwrap_or(policy);
    let fields = live.map(|l| l.catalog.info.snapshot()).unwrap_or_default();
    let text = |key| {
        fields
            .iter()
            .find(|f| f.key == key && f.available)
            .and_then(|f| match &f.value {
                SettingValue::Text(t) => Some(t.clone()),
                _ => None,
            })
    };
    let transport_disabled = !manager
        .preference
        .transports
        .contains(policy.peer.transport);
    let inactive = if !manager.supports(policy.peer.transport) {
        Some(p::InactiveReason::UnsupportedTransport)
    } else if transport_disabled {
        Some(p::InactiveReason::TransportDisabled)
    } else if policy.blocked {
        Some(p::InactiveReason::Blocked)
    } else if !policy.enabled {
        Some(p::InactiveReason::Disabled)
    } else if d.is_none_or(|d| d.retiring) {
        Some(p::InactiveReason::Capacity)
    } else {
        None
    };
    let role_bits = live.map_or(policy.roles.0, |l| l.roles);
    let kinds = if role_bits != 0 {
        role_kinds(role_bits)
    } else {
        text(InfoKey::Kind)
            .map(|k| info_kinds(&k))
            .unwrap_or_default()
    };
    p::Device {
        id: policy.id as u32,
        transport: transport(policy.peer.transport) as i32,
        name: text(InfoKey::Name).unwrap_or_else(|| policy.name.to_string()),
        kinds,
        state: state(d.map_or(ConnectionState::Disconnected, |d| d.state)) as i32,
        enabled: policy.enabled,
        trusted: policy.trusted,
        blocked: policy.blocked,
        paused: d.is_some_and(|d| d.paused),
        inactive: inactive.map(|r| r as i32),
        errors: {
            let mut errors: Vec<p::Error> = d.and_then(|d| d.error.clone()).into_iter().collect();
            if let Some(error) = live.and_then(|l| l.profile_error) {
                errors.push(p::Error {
                    code: match error {
                        E::Capacity => p::ErrorCode::ProfileMemoryExceeded,
                        _ => p::ErrorCode::ProfileLoadFailed,
                    } as i32,
                    reason: if error == E::Capacity {
                        p::ErrorReason::ProfileMemory
                    } else {
                        p::ErrorReason::Storage
                    } as i32,
                    ..Default::default()
                });
            }
            errors.sort_unstable();
            errors.dedup();
            errors
        },
        security: connection
            .filter(|_| d.is_some_and(|d| d.state == ConnectionState::Connected))
            .and_then(|c| c.security)
            .map(security),
        integrations: hidpp(policy, d, connection).into_iter().collect(),
        information: live
            .filter(|_| d.is_some_and(|d| d.state == ConnectionState::Connected))
            .map(|l| device_info(&fields, l.catalog.records())),
        roles: roles(role_bits),
        output_preferences: policy
            .output_preferences
            .iter()
            .copied()
            .map(crate::outputs::Preference::wire)
            .collect(),
        outputs: if role_bits & crate::outputs::GAMEPAD != 0
            || !policy.output_preferences.is_empty()
        {
            alloc::vec![manager.gamepad_status(policy, slot)]
        } else {
            Vec::new()
        },
        profiles: manager.profiles_supported().then(|| p::ProfileLayers {
            profiles: policy.profiles.iter().map(|&id| id as u32).collect(),
        }),
    }
}

/// A resource whose count and allowance can each be unknown.
pub fn resource(
    kind: p::ResourceType,
    count: Option<u32>,
    limit: Option<u32>,
) -> p::ResourceCapacity {
    p::ResourceCapacity {
        r#type: kind as i32,
        count,
        limit,
    }
}
