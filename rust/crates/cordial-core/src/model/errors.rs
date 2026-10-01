use serde::{Deserialize, Serialize};

/// Machine codes only. Human explanations belong to the host application.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    UnknownCommand,
    InvalidArgs,
    Busy,
    NotFound,
    Blocked,
    CandidateExpired,
    Disabled,
    Capacity,
    UnsupportedHid,
    HidReportTooLarge,
    UnsupportedTransport,
    AuthenticationFailed,
    AuthenticationRejected,
    StalePrompt,
    ConnectionFailed,
    RadioUnavailable,
    InputOverflow,
    StorageFailed,
    StorageFull,
    Timeout,
    Cancelled,
    NotPending,
    InternalError,
    NotConnected,
    ReadOnly,
    HidppDisabled,
    SettingsUnavailable,
    UnsupportedSetting,
    SettingsLimit,
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
#[serde(rename_all = "snake_case")]
pub enum WarningCode {
    NumericSelectorUnsupported,
    PointerSelectorUnsupported,
    BufferedInputUnsupported,
    IndicatorStateUnknown,
    IndicatorReadFailed,
    IndicatorWriteFailed,
    IndicatorReadUnsupported,
    IndicatorWriteUnsupported,
    IndicatorReportTooLarge,
    BufferedIndicatorUnsupported,
    IndicatorArrayFull,
    IndicatorRelativeSelectorUnsupported,
    IndicatorRangeUnsupported,
    IndicatorModeUnsupported,
    IndicatorNonlinearUnsupported,
    IndicatorScaleUnsupported,
}

/// The report types defined by USB HID.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HidReportType {
    Input,
    Output,
    Feature,
}

/// Identifies a HID field or report responsible for a limitation or failed update.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DeviceWarning {
    pub code: WarningCode,
    pub service: u16,
    pub report_id: Option<u8>,
    #[serde(default)]
    pub report_type: Option<HidReportType>,
    pub bit_offset: Option<u16>,
    pub usage_page: Option<u16>,
    pub usage: Option<u16>,
}
