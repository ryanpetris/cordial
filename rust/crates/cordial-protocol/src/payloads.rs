//! Shared payloads for responses, streamed results, notifications and errors.
use crate::{
    errors::ErrorCode,
    identifiers::*,
    messages::{Device, Status},
    settings::{Setting, SettingOutcome},
};
use alloc::string::String;
use serde::{Deserialize, Serialize};

macro_rules! payload {
    ($name:ident { $($field:ident : $ty:ty),* $(,)? }) => {
        #[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
        #[cfg_attr(feature="schema", derive(schemars::JsonSchema))]
        #[serde(deny_unknown_fields)]
        pub struct $name { $(pub $field: $ty,)* }
    };
}
payload!(HeartbeatResult {
    timeout_ms: u32,
    monitor: bool
});
payload!(MonitorResult {
    enabled: bool,
    revision: u64
});
payload!(LostEvents {
    dropped: u64,
    revision: u64
});
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct ProtocolError {
    pub code: ErrorCode,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supplied_id: Option<RequestId>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum ReadyResult<T = Status> {
    Initializing,
    Ready { status: T },
}
payload!(BootloaderResult {
    rebooting: bool,
    mode: BootloaderMode
});
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum BootloaderMode {
    Bootsel,
    Download,
}
payload!(AdapterSettings {
    revision: u64,
    name: String,
    host_platform: HostPlatform
});
payload!(DeviceResult { device: Device });
payload!(DeviceSnapshot {
    revision: u64,
    device: Device
});
payload!(DeviceRemoved {
    device_id: DeviceId,
    removed: bool
});
payload!(DeviceUnpaired {
    revision: u64,
    device_id: DeviceId
});
payload!(DeviceDisconnected {
    revision: u64,
    device: Device,
    reason: DisconnectReason
});
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum DisconnectReason {
    Requested,
    Remote,
    LinkLoss,
    Unknown,
}
payload!(CancelResult {
    request_id: RequestId,
    requested: bool
});
payload!(PairReplyResult { accepted: bool });
payload!(DeviceListEnd {
    count: usize,
    revision: u64
});
payload!(ScanEnd {
    count: usize,
    truncated: bool
});
payload!(SettingsListEnd { revision: u64, device_id: DeviceId, count: usize, settings_state: SettingsState, settings_error: Option<ErrorCode> });
payload!(SettingOutcomeChunk {
    revision: u64,
    device_id: DeviceId,
    setting: Setting,
    outcome: SettingOutcome
});
payload!(SettingsSummary {
    revision: u64,
    device_id: DeviceId,
    count: usize,
    read: usize,
    applied: usize,
    unchanged: usize,
    unsupported: usize,
    failed: usize,
    uncertain: usize
});
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum FileType {
    File,
    Directory,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct FileEntry {
    pub name: String,
    #[serde(rename = "type")]
    pub kind: FileType,
    pub size: usize,
}
payload!(StorageListEnd { count: usize });
payload!(StorageChunk {
    offset: u32,
    data: String
});
payload!(StorageReadEnd { bytes: u32 });
pub use crate::errors::{CapacityReason, StorageOutcome};
payload!(MutationDetails {
    outcome: StorageOutcome
});
payload!(CapacityDetails {
    reason: CapacityReason
});
payload!(ExistingDeviceDetails {
    device_id: DeviceId
});
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(untagged)]
pub enum ErrorDetails {
    Mutation(MutationDetails),
    Capacity(CapacityDetails),
    ExistingDevice(ExistingDeviceDetails),
    Settings(SettingsSummary),
}
