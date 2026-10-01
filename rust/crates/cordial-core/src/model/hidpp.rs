use serde::{Deserialize, Serialize};

/// Protocol negotiation on the current connection, independent of feature readiness.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum ProtocolState {
    #[default]
    Unknown,
    Probing,
    Detected {
        major: u8,
        minor: u8,
    },
    Unavailable,
    Error {
        code: crate::model::errors::ErrorCode,
    },
}
impl ProtocolState {
    pub const fn major(self) -> u8 {
        match self {
            Self::Detected { major, .. } => major,
            _ => 0,
        }
    }
    pub fn valid(self) -> bool {
        use crate::model::errors::ErrorCode as C;
        match self {
            Self::Detected { major, .. } => major != 0,
            Self::Error { code } => matches!(
                code,
                C::HidppTimeout
                    | C::HidppTransportError
                    | C::HidppDeviceError
                    | C::HidppInvalidResponse
            ),
            _ => true,
        }
    }
}

/// Stable protocol feature number, independent of each device's feature table.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct FeatureId(pub u16);

/// Index assigned by the connected peripheral, not a feature number.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct FeatureIndex(pub u8);

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct FeatureRevision(pub u8);

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct FeatureFlags(pub u8);
impl FeatureFlags {
    pub const fn device_hidden(self) -> bool {
        self.0 & 0x40 != 0
    }
    pub const fn engineering(self) -> bool {
        self.0 & 0x20 != 0
    }
    pub const fn obsolete(self) -> bool {
        self.0 & 0x80 != 0
    }
    pub const fn public(self) -> bool {
        self.0 & 0x60 == 0
    }
}

impl FeatureId {
    pub const ROOT: Self = Self(0x0000);
    pub const FEATURE_SET: Self = Self(0x0001);
    pub const DEVICE_INFORMATION: Self = Self(0x0003);
    pub const DEVICE_NAME: Self = Self(0x0005);
    pub const CONFIG_CHANGE: Self = Self(0x0020);
    pub const BATTERY: Self = Self(0x1000);
    pub const BATTERY_VOLTAGE: Self = Self(0x1001);
    pub const UNIFIED_BATTERY: Self = Self(0x1004);
    pub const ADC_MEASUREMENT: Self = Self(0x1f20);
    pub const SOLAR: Self = Self(0x4301);
    pub const BACKLIGHT: Self = Self(0x1982);
    pub const REPROG_CONTROLS: Self = Self(0x1b04);
    pub const SMART_SHIFT: Self = Self(0x2110);
    pub const HIRES_WHEEL: Self = Self(0x2121);
    pub const THUMBWHEEL: Self = Self(0x2150);
    pub const ADJUSTABLE_DPI: Self = Self(0x2201);
    pub const FN_INVERSION: Self = Self(0x40a2);
    pub const FN_INVERSION_MULTI_HOST: Self = Self(0x40a3);
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Feature {
    pub index: FeatureIndex,
    pub id: FeatureId,
    pub version: FeatureRevision,
    pub flags: FeatureFlags,
    /// The firmware has a usable handler; not a device-provided flag.
    pub supported: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ControlId(pub u16);
impl ControlId {
    pub const BRIGHTNESS_DOWN: Self = Self(0x00c7);
    pub const BRIGHTNESS_UP: Self = Self(0x00c8);
    pub const PREVIOUS_TRACK: Self = Self(0x00e4);
    pub const PLAY_PAUSE: Self = Self(0x00e5);
    pub const NEXT_TRACK: Self = Self(0x00e6);
    pub const MUTE: Self = Self(0x00e7);
    pub const VOLUME_DOWN: Self = Self(0x00e8);
    pub const VOLUME_UP: Self = Self(0x00e9);
}
