use crate::hidpp::{FeatureId, FeatureRevision};
use alloc::{string::String, vec::Vec};
use serde::{Deserialize, Serialize};

/// Explicit codes: persisted setting-record keys use them, so never renumber.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[repr(u8)]
pub enum SettingKey {
    #[serde(rename = "fn.row_default")]
    FnRowDefault = 7,
    #[serde(rename = "backlight.enabled")]
    BacklightEnabled = 8,
    #[serde(rename = "backlight.mode")]
    BacklightMode = 9,
    #[serde(rename = "backlight.level")]
    BacklightLevel = 10,
    #[serde(rename = "backlight.delay.hands_out")]
    BacklightDelayHandsOut = 11,
    #[serde(rename = "backlight.delay.hands_in")]
    BacklightDelayHandsIn = 12,
    #[serde(rename = "backlight.delay.powered")]
    BacklightDelayPowered = 13,
    #[serde(rename = "pointer.dpi.0")]
    PointerDpi0 = 14,
    #[serde(rename = "pointer.dpi.1")]
    PointerDpi1 = 15,
    #[serde(rename = "wheel.mode")]
    WheelMode = 16,
    #[serde(rename = "wheel.threshold")]
    WheelThreshold = 17,
    #[serde(rename = "wheel.invert")]
    WheelInvert = 18,
    #[serde(rename = "thumbwheel.invert")]
    ThumbwheelInvert = 19,
    #[serde(rename = "backlight.power_on")]
    BacklightPowerOn = 22,
    #[serde(rename = "backlight.crown")]
    BacklightCrown = 23,
    #[serde(rename = "backlight.power_save")]
    BacklightPowerSave = 24,
    #[serde(rename = "backlight.effect")]
    BacklightEffect = 25,
    #[serde(rename = "backlight.current_level")]
    BacklightCurrentLevel = 26,
    #[serde(rename = "backlight.status")]
    BacklightStatus = 27,
    #[serde(rename = "wheel.info")]
    WheelInfo = 28,
}

impl SettingKey {
    pub const ALL: [Self; 20] = [
        Self::FnRowDefault,
        Self::BacklightEnabled,
        Self::BacklightMode,
        Self::BacklightLevel,
        Self::BacklightDelayHandsOut,
        Self::BacklightDelayHandsIn,
        Self::BacklightDelayPowered,
        Self::PointerDpi0,
        Self::PointerDpi1,
        Self::WheelMode,
        Self::WheelThreshold,
        Self::WheelInvert,
        Self::ThumbwheelInvert,
        Self::BacklightPowerOn,
        Self::BacklightCrown,
        Self::BacklightPowerSave,
        Self::BacklightEffect,
        Self::BacklightCurrentLevel,
        Self::BacklightStatus,
        Self::WheelInfo,
    ];
    pub fn from_name(value: &str) -> Option<Self> {
        Self::deserialize(serde::de::value::StrDeserializer::<serde::de::value::Error>::new(value))
            .ok()
    }
    pub fn kind(self) -> SettingType {
        use SettingKey::*;
        match self {
            WheelInfo => SettingType::Text,
            FnRowDefault | BacklightMode | BacklightEffect | BacklightStatus | WheelMode => {
                SettingType::Enum
            }
            BacklightEnabled | BacklightPowerOn | BacklightCrown | BacklightPowerSave
            | WheelInvert | ThumbwheelInvert => SettingType::Bool,
            _ => SettingType::Integer,
        }
    }
    pub fn writable_feature(self, feature: FeatureId, revision: FeatureRevision) -> bool {
        use SettingKey::*;
        match self {
            FnRowDefault => feature == FeatureId::FN_INVERSION,
            BacklightEnabled | BacklightPowerOn | BacklightCrown => feature == FeatureId::BACKLIGHT,
            BacklightPowerSave => feature == FeatureId::BACKLIGHT && revision.0 >= 1,
            BacklightEffect => feature == FeatureId::BACKLIGHT && revision.0 >= 2,
            BacklightMode
            | BacklightLevel
            | BacklightDelayHandsOut
            | BacklightDelayHandsIn
            | BacklightDelayPowered => feature == FeatureId::BACKLIGHT && revision.0 >= 3,
            PointerDpi0 | PointerDpi1 => feature == FeatureId::ADJUSTABLE_DPI,
            WheelMode | WheelThreshold => feature == FeatureId::SMART_SHIFT,
            WheelInvert => feature == FeatureId::HIRES_WHEEL,
            ThumbwheelInvert => feature == FeatureId::THUMBWHEEL,
            _ => false,
        }
    }
    pub fn enum_values(self) -> &'static [&'static str] {
        use SettingKey::*;
        match self {
            FnRowDefault => &["function_keys", "special_actions"],
            BacklightMode => &["none", "automatic", "temporary_manual", "permanent_manual"],
            BacklightEffect => &[
                "static",
                "none",
                "breathing",
                "contrast",
                "reaction",
                "random",
                "waves",
            ],
            BacklightStatus => &[
                "disabled",
                "battery",
                "automatic",
                "saturated",
                "manual",
                "permanent_manual",
            ],
            WheelMode => &["freespin", "ratchet"],
            _ => &[],
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(untagged)]
pub enum SettingValue {
    #[default]
    Null,
    Bool(bool),
    Integer(i64),
    Text(String),
}
// Deserialize the scalar directly; an untagged enum buffers arrays and objects
// before rejecting them, needlessly allocating for invalid setting values.
impl<'de> Deserialize<'de> for SettingValue {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Scalar;
        impl<'de> serde::de::Visitor<'de> for Scalar {
            type Value = SettingValue;
            fn expecting(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
                f.write_str("a setting scalar")
            }
            fn visit_unit<E: serde::de::Error>(self) -> Result<Self::Value, E> {
                Ok(SettingValue::Null)
            }
            fn visit_bool<E: serde::de::Error>(self, value: bool) -> Result<Self::Value, E> {
                Ok(SettingValue::Bool(value))
            }
            fn visit_i64<E: serde::de::Error>(self, value: i64) -> Result<Self::Value, E> {
                if value.unsigned_abs() > crate::MAX_REVISION {
                    return Err(E::custom("invalid_setting_value"));
                }
                Ok(SettingValue::Integer(value))
            }
            fn visit_u64<E: serde::de::Error>(self, value: u64) -> Result<Self::Value, E> {
                if value > crate::MAX_REVISION {
                    return Err(E::custom("invalid_setting_value"));
                }
                Ok(SettingValue::Integer(value as i64))
            }
            fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<Self::Value, E> {
                Ok(SettingValue::Text(value.into()))
            }
            fn visit_string<E: serde::de::Error>(self, value: String) -> Result<Self::Value, E> {
                Ok(SettingValue::Text(value))
            }
        }
        deserializer.deserialize_any(Scalar)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum SettingType {
    Bool,
    Integer,
    Enum,
    Text,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum SettingScope {
    Device,
    CurrentHost,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum SettingState {
    #[default]
    Unmanaged,
    Pending,
    Applying,
    Applied,
    ChangedOnDevice,
    Unsupported,
    Error,
    Uncertain,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum ObservationSource {
    Read,
    Event,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum SettingOutcome {
    Read,
    Applied,
    Unchanged,
    Unsupported,
    Failed,
    Uncertain,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Setting {
    pub key: SettingKey,
    #[serde(rename = "type")]
    pub kind: SettingType,
    pub writable: bool,
    pub feature: FeatureId,
    pub feature_version: FeatureRevision,
    pub scope: SettingScope,
    pub choices: Vec<SettingValue>,
    pub min: Option<i64>,
    pub max: Option<i64>,
    pub step: Option<i64>,
    pub managed: bool,
    pub desired: SettingValue,
    pub observed: SettingValue,
    pub fresh: bool,
    pub observed_at_ms: Option<u64>,
    pub observation_source: Option<ObservationSource>,
    pub state: SettingState,
    pub error: Option<crate::errors::ErrorCode>,
}

impl Setting {
    pub fn accepts(&self, value: &SettingValue) -> bool {
        if !self.writable {
            return false;
        }
        let typed = matches!(
            (self.kind, value),
            (SettingType::Bool, SettingValue::Bool(_))
                | (SettingType::Integer, SettingValue::Integer(_))
                | (SettingType::Enum, SettingValue::Text(_))
        );
        if !typed {
            return false;
        }
        if matches!(value, SettingValue::Integer(n) if n.unsigned_abs() > crate::MAX_REVISION) {
            return false;
        }
        if !self.choices.is_empty() {
            return self.choices.contains(value);
        }
        match value {
            SettingValue::Bool(_) => true,
            SettingValue::Integer(n) => {
                let (Some(min), Some(max), Some(step)) = (self.min, self.max, self.step) else {
                    return false;
                };
                step > 0
                    && min <= *n
                    && *n <= max
                    && n.unsigned_abs() <= crate::MAX_REVISION
                    && (i128::from(*n) - i128::from(min)) % i128::from(step) == 0
            }
            _ => false,
        }
    }
    /// None means no current comparison is available.
    pub fn differs_from_saved(&self) -> Option<bool> {
        (self.managed
            && self.fresh
            && self.observed != SettingValue::Null
            && self.desired != SettingValue::Null)
            .then(|| self.observed != self.desired)
    }
}
