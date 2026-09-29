use serde::{Deserialize, Serialize};

/// Machine codes only. Human explanations belong to the host application.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    InvalidRequest,
    InvalidJson,
    MessageTooLarge,
    UnsupportedVersion,
    UnknownCommand,
    InvalidArgs,
    Busy,
    NotFound,
    Blocked,
    HeartbeatRequired,
    ClientTimeout,
    CandidateExpired,
    Disabled,
    PairingRequired,
    Capacity,
    UnsupportedHid,
    UnsupportedTransport,
    AuthenticationFailed,
    AuthenticationRejected,
    StalePrompt,
    ConnectionFailed,
    RadioUnavailable,
    InputOverflow,
    StorageFailed,
    StorageChanged,
    StorageFull,
    Timeout,
    Cancelled,
    NotPending,
    NotCancellable,
    SessionFault,
    InternalError,
    NotConnected,
    ReadOnly,
    HidppDisabled,
    SettingsUnavailable,
    UnsupportedSetting,
    SettingsLimit,
    SettingsApplyFailed,
    SettingsRefreshFailed,
    FeatureSetUnavailable,
    ReadbackMismatch,
    BacklightModeSelectionRequired,
    BacklightPermanentManualRequired,
    NativeRoutingRequired,
    NativeStandardResolutionRequired,
    HidppReportsUnavailable,
    HidppProtocolUnsupported,
    HidppResetUnavailable,
    HidppControlsUnavailable,
    HidppTimeout,
    HidppTransportError,
    HidppDeviceError,
    HidppInvalidResponse,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum WarningCode {
    LedOutputUnavailable,
    UnsupportedFields,
}

/// Why a saved device is not effectively enabled, in precedence order.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum DisabledReason {
    UnsupportedTransport,
    Invalid,
    Blocked,
    Disabled,
    Capacity,
}

/// Structured stored-record validation problem for a saved device.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum ValidationError {
    BondMissing,
    BondCorrupt,
    BondMismatch,
    DeviceCorrupt,
    ReadFailed,
}
impl ValidationError {
    /// Bond problems are repaired by pairing again; others are storage problems.
    pub const fn needs_pairing(self) -> bool {
        matches!(
            self,
            Self::BondMissing | Self::BondCorrupt | Self::BondMismatch
        )
    }
}

/// Why status reports Pair unavailable for a transport.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum PairUnavailable {
    StorageFull,
    SetupCapacity,
    ConnectionsFull,
    PairingActive,
    RadioUnavailable,
    StorageUnavailable,
}

/// `details.reason` of a `capacity` error.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum CapacityReason {
    EnabledFull,
    StorageFull,
    SetupCapacity,
    ConnectionsFull,
}

/// `details.outcome` of a `storage_failed` error; absent means not saved.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum StorageOutcome {
    NotSaved,
    Unknown,
}
