//! Converts the firmware's model into serial API messages. Internal error codes map onto the wire
//! codes that describe them to a user; raw device data never leaves this module undecoded.
use alloc::{
    string::{String, ToString},
    vec::Vec,
};

use cordial_protocol::{self as p, keys};

use crate::{
    compact::{Observed, Record, scalar},
    devices::Device,
    manager::Manager,
    model::{
        errors::{DeviceWarning, ErrorCode as E, HidReportType, WarningCode},
        hidpp::ProtocolState,
        identifiers::{ConnectionState, HostPlatform, SettingsState, Transport},
        info::{InfoField, InfoKey},
        link::{ConnectionSecurity, DeviceKind},
        settings::{SettingKey, SettingState, SettingType, SettingValue},
    },
};

/// The wire code for an internal error.
pub fn error_code(code: E) -> p::ErrorCode {
    use p::ErrorCode as W;
    match code {
        E::UnknownCommand => W::UnknownCommand,
        E::InvalidArgs => W::BadArgs,
        E::Busy => W::Busy,
        E::NotFound | E::NotPending | E::ReadOnly => W::NotFound,
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

/// The wire error for an internal error. `reason` says what ran out for a capacity error.
pub fn error(code: E, reason: Option<p::CapacityReason>, outcome_unknown: bool) -> p::Error {
    let wire = error_code(code);
    p::Error {
        code: wire as i32,
        reason: if wire == p::ErrorCode::NoCapacity {
            reason.unwrap_or(match code {
                E::StorageFull | E::SettingsLimit => p::CapacityReason::Storage,
                _ => p::CapacityReason::Unknown,
            }) as i32
        } else {
            0
        },
        outcome_unknown: wire == p::ErrorCode::StorageFailed && outcome_unknown,
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

pub fn kind(kind: DeviceKind) -> p::Kind {
    match kind {
        DeviceKind::Unknown => p::Kind::Unknown,
        DeviceKind::Keyboard => p::Kind::Keyboard,
        DeviceKind::Mouse => p::Kind::Mouse,
        DeviceKind::KeyboardMouse => p::Kind::KeyboardMouse,
    }
}

fn info_kind(value: &str) -> p::Kind {
    match value {
        "keyboard" => p::Kind::Keyboard,
        "mouse" => p::Kind::Mouse,
        "keyboard_mouse" => p::Kind::KeyboardMouse,
        "other" => p::Kind::Other,
        _ => p::Kind::Unknown,
    }
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

/// The wire key of a setting record. Repeated parts carry their index in the key.
pub fn setting_key(key: SettingKey) -> String {
    use SettingKey::*;
    match key {
        FnRowDefault => keys::KEYBOARD_FN_ROW.into(),
        PointerDpi0 => keys::indexed(keys::POINTER_SENSOR_N_DPI, 0),
        PointerDpi1 => keys::indexed(keys::POINTER_SENSOR_N_DPI, 1),
        BacklightEnabled => keys::BACKLIGHT_ENABLED.into(),
        BacklightMode => keys::BACKLIGHT_MODE.into(),
        BacklightLevel => keys::BACKLIGHT_LEVEL.into(),
        BacklightDelayHandsOut => keys::BACKLIGHT_DELAY_HANDS_OUT.into(),
        BacklightDelayHandsIn => keys::BACKLIGHT_DELAY_HANDS_IN.into(),
        BacklightDelayPowered => keys::BACKLIGHT_DELAY_POWERED.into(),
        BacklightPowerOn => keys::BACKLIGHT_POWER_ON.into(),
        BacklightCrown => keys::BACKLIGHT_CROWN.into(),
        BacklightPowerSave => keys::BACKLIGHT_POWER_SAVE.into(),
        BacklightEffect => keys::BACKLIGHT_EFFECT.into(),
        BacklightCurrentLevel => keys::BACKLIGHT_CURRENT_LEVEL.into(),
        BacklightStatus => keys::BACKLIGHT_STATUS.into(),
        WheelMode => keys::WHEEL_MODE.into(),
        WheelThreshold => keys::WHEEL_THRESHOLD.into(),
        WheelInvert => keys::WHEEL_INVERT.into(),
        ThumbwheelInvert => keys::THUMBWHEEL_INVERT.into(),
        // Decoded into its own information keys; never sent under this name.
        WheelInfo => String::new(),
    }
}

/// The setting record for a wire key, if the firmware has one.
pub fn parse_setting_key(key: &str) -> Option<SettingKey> {
    SettingKey::ALL
        .into_iter()
        .filter(|k| *k != SettingKey::WheelInfo)
        .find(|k| setting_key(*k) == key)
}

fn value(value: SettingValue) -> Option<p::Value> {
    use p::value::Value as V;
    Some(p::Value {
        value: Some(match value {
            SettingValue::Null => return None,
            SettingValue::Bool(b) => V::Bool(b),
            SettingValue::Integer(n) => V::Integer(n),
            SettingValue::Text(t) => V::Text(t),
        }),
    })
}

/// The internal value of a wire value, for a setting of type `kind`.
pub fn setting_value(kind: SettingType, value: &p::Value) -> Option<SettingValue> {
    use p::value::Value as V;
    match (kind, value.value.as_ref()?) {
        (SettingType::Bool, V::Bool(b)) => Some(SettingValue::Bool(*b)),
        (SettingType::Integer, V::Integer(n)) => Some(SettingValue::Integer(*n)),
        (SettingType::Enum | SettingType::Text, V::Text(t)) => Some(SettingValue::Text(t.clone())),
        _ => None,
    }
}

fn info(key: impl Into<String>, value: SettingValue) -> Option<p::Info> {
    Some(p::Info {
        key: key.into(),
        value: self::value(value),
    })
    .filter(|i| i.value.is_some())
}

/// A device's information list: identity and battery from any source, the values its
/// integrations only report, and the read-only settings of this device.
fn device_info(fields: &[InfoField], records: &[Record]) -> Vec<p::Info> {
    let mut list = Vec::new();
    for field in fields.iter().filter(|f| f.available) {
        let key = match (field.key, field.instance) {
            (InfoKey::Manufacturer, _) => keys::DEVICE_MANUFACTURER,
            (InfoKey::Model, _) => keys::DEVICE_MODEL,
            (InfoKey::Serial, _) => keys::DEVICE_SERIAL,
            (InfoKey::Firmware, 0) => keys::FIRMWARE_VERSION,
            (InfoKey::Firmware, 1) => keys::BOOTLOADER_VERSION,
            (InfoKey::Hardware, _) => keys::HARDWARE_REVISION,
            (InfoKey::Software, _) => keys::SOFTWARE_REVISION,
            (InfoKey::VendorIdNamespace, _) => keys::VENDOR_REGISTRY,
            (InfoKey::VendorId, _) => keys::VENDOR_ID,
            (InfoKey::ProductId, _) => keys::PRODUCT_ID,
            (InfoKey::ProductVersion, _) => keys::PRODUCT_VERSION,
            (InfoKey::BatteryPercent, _) => keys::BATTERY_LEVEL,
            (InfoKey::BatteryCharging, _) => keys::BATTERY_CHARGING,
            // Name and kind are fields of the device record.
            (InfoKey::Name | InfoKey::Kind | InfoKey::Firmware, _) => continue,
        };
        list.extend(info(key, field.value.clone()));
    }
    for record in records.iter().filter(|r| !r.writable) {
        if record.metadata.key == SettingKey::WheelInfo {
            list.extend(wheel_info(record));
        } else if let Some(value) = observed(record) {
            list.extend(info(setting_key(record.metadata.key), value));
        }
    }
    list
}

fn observed(record: &Record) -> Option<SettingValue> {
    Some(record.observed.wire(record.metadata.key)).filter(|v| *v != SettingValue::Null)
}

/// Decodes the HiResWheel capability response: resolution multiplier, capability flags, and from
/// revision 1 ratchets per rotation and wheel diameter in millimetres.
fn wheel_info(record: &Record) -> Vec<p::Info> {
    let Observed::Text(hex) = &record.observed else {
        return Vec::new();
    };
    let bytes: Vec<u8> = hex
        .as_bytes()
        .chunks(2)
        .filter_map(|pair| u8::from_str_radix(core::str::from_utf8(pair).ok()?, 16).ok())
        .collect();
    if bytes.len() * 2 != hex.len() || !matches!(bytes.len(), 2 | 4) {
        return Vec::new();
    }
    let mut list = Vec::new();
    list.extend(info(
        keys::WHEEL_RESOLUTION_MULTIPLIER,
        SettingValue::Integer(bytes[0].into()),
    ));
    if bytes.len() == 4 {
        list.extend(info(
            keys::WHEEL_RATCHETS_PER_ROTATION,
            SettingValue::Integer(bytes[2].into()),
        ));
        list.extend(info(
            keys::WHEEL_DIAMETER,
            SettingValue::Integer(bytes[3].into()),
        ));
    }
    list
}

fn hidpp(manager: &Manager, slot: usize, d: &Device) -> Option<p::Integration> {
    let runtime = manager
        .connections
        .iter()
        .flatten()
        .find(|c| c.device == Some(slot) && !c.closing)
        .and_then(|c| c.runtime.as_deref())
        .filter(|_| d.state == ConnectionState::Connected);
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
    if !d.policy.hidpp_enabled && detected.is_none() {
        return None;
    }
    use p::integration::Status;
    let status = if !d.policy.hidpp_enabled {
        Status::State(p::IntegrationState::Off as i32)
    } else if let Some(runtime) = runtime {
        match protocol {
            ProtocolState::Unavailable => Status::State(p::IntegrationState::Unsupported as i32),
            ProtocolState::Error { code } => Status::Error(error_code(code) as i32),
            ProtocolState::Unknown | ProtocolState::Probing => {
                Status::State(p::IntegrationState::Starting as i32)
            }
            ProtocolState::Detected { .. } => match runtime.settings.state {
                SettingsState::Ready | SettingsState::Applying | SettingsState::Unsupported => {
                    Status::State(p::IntegrationState::Active as i32)
                }
                SettingsState::Error => Status::Error(error_code(
                    runtime.settings.error.unwrap_or(E::InternalError),
                ) as i32),
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
        enabled: d.policy.hidpp_enabled,
        detected,
        status: Some(status),
    })
}

/// The device record of saved device `slot`.
pub fn device(manager: &Manager, slot: usize) -> Option<p::Device> {
    let d = manager.devices.get(slot)?.as_ref()?;
    let fields = d.catalog.info.snapshot();
    let text = |key| {
        fields
            .iter()
            .find(|f| f.key == key && f.available)
            .and_then(|f| match &f.value {
                SettingValue::Text(t) => Some(t.clone()),
                _ => None,
            })
    };
    let inactive = if !d.transport_supported {
        Some(p::InactiveReason::UnsupportedTransport)
    } else if d.transport_disabled {
        Some(p::InactiveReason::TransportDisabled)
    } else if d.policy.blocked {
        Some(p::InactiveReason::Blocked)
    } else if !d.policy.enabled {
        Some(p::InactiveReason::Disabled)
    } else if !d.effective_enabled {
        Some(p::InactiveReason::Capacity)
    } else {
        None
    };
    let connection = manager
        .connections
        .iter()
        .flatten()
        .find(|c| c.device == Some(slot) && !c.closing);
    Some(p::Device {
        id: d.policy.device_id().0,
        transport: transport(d.policy.peer.transport) as i32,
        name: text(InfoKey::Name).unwrap_or_else(|| d.policy.name.to_string()),
        kind: text(InfoKey::Kind).map_or(p::Kind::Unknown, |k| info_kind(&k)) as i32,
        state: state(d.state) as i32,
        enabled: d.policy.enabled,
        trusted: d.policy.trusted,
        blocked: d.policy.blocked,
        paused: d.paused,
        inactive: inactive.map(|r| r as i32),
        error: d.error.map(|e| error_code(e) as i32),
        security: connection
            .filter(|_| d.state == ConnectionState::Connected)
            .and_then(|c| c.security)
            .map(security),
        integrations: hidpp(manager, slot, d).into_iter().collect(),
        info: device_info(&fields, d.catalog.records()),
        roles: [
            p::Role::Keyboard,
            p::Role::Mouse,
            p::Role::ConsumerControl,
            p::Role::SystemControl,
        ]
        .into_iter()
        .enumerate()
        .filter(|(i, _)| d.roles & (1 << i) != 0)
        .map(|(_, r)| r as i32)
        .collect(),
    })
}

/// A setting record as a wire setting. `None` for read-only records, which are information.
pub fn setting(record: &Record) -> Option<p::Setting> {
    if !record.writable {
        return None;
    }
    let key = record.metadata.key;
    let saved = record.preference.as_ref().map(|p| scalar(key, p.value));
    let observed = observed(record);
    let metadata = record.metadata.wire();
    use p::setting::{Status, Type};
    let status = record.preference.as_ref().map(|_| match record.state {
        SettingState::Unmanaged | SettingState::Pending | SettingState::Applying => {
            Status::State(p::SettingState::Pending as i32)
        }
        SettingState::Applied => Status::State(p::SettingState::Applied as i32),
        SettingState::ChangedOnDevice => Status::State(p::SettingState::ChangedOnDevice as i32),
        SettingState::Unsupported => Status::State(p::SettingState::Unsupported as i32),
        SettingState::Uncertain => Status::Error(p::ErrorCode::Timeout as i32),
        SettingState::Error => {
            Status::Error(error_code(record.error.unwrap_or(E::InternalError)) as i32)
        }
    });
    let integer = |v: Option<SettingValue>| match v {
        Some(SettingValue::Integer(n)) => Some(n),
        _ => None,
    };
    let text = |v: Option<SettingValue>| match v {
        Some(SettingValue::Text(t)) => Some(t),
        _ => None,
    };
    let kind = match metadata.kind {
        SettingType::Bool => Type::Bool(p::BoolSetting {
            value: match observed {
                Some(SettingValue::Bool(b)) => Some(b),
                _ => None,
            },
            saved: match saved {
                Some(SettingValue::Bool(b)) => Some(b),
                _ => None,
            },
        }),
        SettingType::Integer => Type::Integer(p::IntegerSetting {
            value: integer(observed),
            saved: integer(saved),
            limits: if !metadata.choices.is_empty() {
                Some(p::integer_setting::Limits::Choices(p::IntegerChoices {
                    values: metadata
                        .choices
                        .iter()
                        .filter_map(|c| match c {
                            SettingValue::Integer(n) => Some(*n),
                            _ => None,
                        })
                        .collect(),
                }))
            } else if let (Some(min), Some(max)) = (metadata.min, metadata.max) {
                Some(p::integer_setting::Limits::Range(p::IntegerRange {
                    min,
                    max,
                    step: metadata.step.map_or(0, |s| s.max(0) as u64),
                }))
            } else {
                None
            },
        }),
        SettingType::Enum => Type::Enum(p::EnumSetting {
            value: text(observed),
            saved: text(saved),
            choices: metadata
                .choices
                .iter()
                .filter_map(|c| match c {
                    SettingValue::Text(t) => Some(t.clone()),
                    _ => None,
                })
                .collect(),
        }),
        SettingType::Text => Type::Text(p::TextSetting {
            value: text(observed),
            saved: text(saved),
            max_bytes: Some(crate::settings::MAX_TEXT_BYTES as u32),
        }),
    };
    Some(p::Setting {
        integration: p::IntegrationKind::Hidpp as i32,
        key: setting_key(key),
        status,
        r#type: Some(kind),
    })
}

/// Every setting of saved device `slot`.
pub fn settings(manager: &Manager, slot: usize) -> Option<p::DeviceSettings> {
    let d = manager.devices.get(slot)?.as_ref()?;
    Some(p::DeviceSettings {
        device: d.policy.device_id().0,
        settings: d.catalog.records().iter().filter_map(setting).collect(),
    })
}

fn warning_code(code: WarningCode) -> p::WarningCode {
    use WarningCode as C;
    use p::WarningCode as W;
    match code {
        C::NumericSelectorUnsupported => W::NumericSelectorUnsupported,
        C::PointerSelectorUnsupported => W::PointerSelectorUnsupported,
        C::BufferedInputUnsupported => W::BufferedInputUnsupported,
        C::IndicatorReadUnsupported => W::IndicatorReadUnsupported,
        C::IndicatorWriteUnsupported => W::IndicatorWriteUnsupported,
        C::IndicatorReportTooLarge => W::IndicatorReportTooLarge,
        C::BufferedIndicatorUnsupported => W::BufferedIndicatorUnsupported,
        C::IndicatorArrayFull => W::IndicatorArrayFull,
        C::IndicatorRelativeSelectorUnsupported => W::IndicatorRelativeSelectorUnsupported,
        C::IndicatorRangeUnsupported => W::IndicatorRangeUnsupported,
        C::IndicatorModeUnsupported => W::IndicatorModeUnsupported,
        C::IndicatorNonlinearUnsupported => W::IndicatorNonlinearUnsupported,
        C::IndicatorScaleUnsupported => W::IndicatorScaleUnsupported,
        C::IndicatorStateUnknown => W::IndicatorStateUnknown,
        C::IndicatorReadFailed => W::IndicatorReadFailed,
        C::IndicatorWriteFailed => W::IndicatorWriteFailed,
    }
}

fn warning(w: &DeviceWarning) -> p::DeviceWarning {
    p::DeviceWarning {
        code: warning_code(w.code) as i32,
        service: w.service.into(),
        report_type: match w.report_type {
            None => p::ReportType::Unknown,
            Some(HidReportType::Input) => p::ReportType::Input,
            Some(HidReportType::Output) => p::ReportType::Output,
            Some(HidReportType::Feature) => p::ReportType::Feature,
        } as i32,
        report_id: w.report_id.map(u32::from),
        bit_offset: w.bit_offset.map(u32::from),
        usage_page: w.usage_page.map(u32::from),
        usage: w.usage.map(u32::from),
    }
}

/// The warnings of saved device `slot`.
pub fn warnings(manager: &Manager, slot: usize) -> Option<p::DeviceWarnings> {
    let d = manager.devices.get(slot)?.as_ref()?;
    Some(p::DeviceWarnings {
        device: d.policy.device_id().0,
        warnings: d.warnings.iter().map(warning).collect(),
    })
}
