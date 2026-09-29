use alloc::string::String;
#[cfg(feature = "schema")]
use alloc::string::ToString;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(try_from = "u32", into = "u32")]
pub struct RequestId(u32);
impl RequestId {
    pub const fn get(self) -> u32 {
        self.0
    }
}
impl TryFrom<u32> for RequestId {
    type Error = &'static str;
    fn try_from(value: u32) -> Result<Self, Self::Error> {
        if (1..=crate::MAX_REQUEST_ID).contains(&value) {
            Ok(Self(value))
        } else {
            Err("invalid_request_id")
        }
    }
}
impl From<RequestId> for u32 {
    fn from(value: RequestId) -> Self {
        value.0
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(transparent)]
pub struct DeviceId(
    #[cfg_attr(feature = "schema", schemars(regex(pattern = "^[!-~]{1,64}$")))] pub String,
);

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(transparent)]
pub struct CandidateId(
    #[cfg_attr(feature = "schema", schemars(regex(pattern = "^[!-~]{1,64}$")))] pub String,
);

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum HostPlatform {
    #[default]
    Linux,
    Windows,
    Mac,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum Transport {
    Classic,
    Ble,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum ScanTransport {
    #[default]
    Both,
    Classic,
    Ble,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Keyboard,
    Mouse,
    ConsumerControl,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum ConnectionState {
    #[default]
    Disconnected,
    Connecting,
    Connected,
    Disconnecting,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum PairingState {
    Paired,
    NeedsPairing,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum Reconnect {
    #[default]
    Auto,
    Paused,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum SettingsState {
    #[default]
    Off,
    Pending,
    Discovering,
    Ready,
    Applying,
    Unsupported,
    Error,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum NormalizationState {
    #[default]
    Off,
    Pending,
    Probing,
    Resetting,
    Configuring,
    Active,
    Unsupported,
    Error,
}

#[cfg(feature = "schema")]
impl schemars::JsonSchema for RequestId {
    fn schema_name() -> alloc::borrow::Cow<'static, str> {
        "RequestId".into()
    }
    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({"type":"integer","minimum":1,"maximum":crate::MAX_REQUEST_ID})
    }
}
