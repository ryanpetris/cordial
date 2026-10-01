//! Source-neutral, volatile device observations. Null explicitly clears a field.
use crate::model::{identifiers::DeviceId, settings::SettingValue};
use alloc::vec::Vec;
use serde::{Deserialize, Serialize};

pub const MAX_FIRMWARE: u8 = 2;
pub const MAX_TEXT: usize = 64;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InfoKey {
    Name,
    Kind,
    Manufacturer,
    Model,
    Serial,
    Firmware,
    Hardware,
    Software,
    VendorIdNamespace,
    VendorId,
    ProductId,
    ProductVersion,
    BatteryPercent,
    BatteryCharging,
}
impl InfoKey {
    pub const ALL: [Self; 14] = [
        Self::Name,
        Self::Kind,
        Self::Manufacturer,
        Self::Model,
        Self::Serial,
        Self::Firmware,
        Self::Hardware,
        Self::Software,
        Self::VendorIdNamespace,
        Self::VendorId,
        Self::ProductId,
        Self::ProductVersion,
        Self::BatteryPercent,
        Self::BatteryCharging,
    ];
    pub const fn instances(self) -> u8 {
        match self {
            Self::Firmware => MAX_FIRMWARE,
            _ => 1,
        }
    }
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InfoField {
    pub key: InfoKey,
    pub instance: u8,
    pub value: SettingValue,
    pub available: bool,
    pub fresh: bool,
}
impl InfoField {
    pub fn unknown(key: InfoKey, instance: u8) -> Self {
        Self {
            key,
            instance,
            value: SettingValue::Null,
            available: false,
            fresh: false,
        }
    }
    pub fn valid(&self) -> bool {
        use InfoKey::*;
        use SettingValue as V;
        if self.instance >= self.key.instances() {
            return false;
        }
        if !self.available {
            return self.value == V::Null && !self.fresh;
        }
        match (&self.key, &self.value) {
            (BatteryPercent, V::Integer(n)) => (0..=100).contains(n),
            (VendorId | ProductId | ProductVersion, V::Integer(n)) => (0..=65535).contains(n),
            (VendorIdNamespace, V::Text(s)) => ["usb", "bluetooth"].contains(&s.as_str()),
            (BatteryCharging, V::Bool(_)) => true,
            (Kind, V::Text(s)) => {
                ["keyboard", "mouse", "keyboard_mouse", "other"].contains(&s.as_str())
            }
            (Name | Manufacturer | Model | Serial | Firmware | Hardware | Software, V::Text(s)) => {
                !s.is_empty() && s.len() <= MAX_TEXT && !s.chars().any(char::is_control)
            }
            _ => false,
        }
    }
}
/// Full snapshot for responses; only changed fields for device.info.changed.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeviceInfo {
    pub revision: u64,
    pub device_id: DeviceId,
    pub fields: Vec<InfoField>,
}
