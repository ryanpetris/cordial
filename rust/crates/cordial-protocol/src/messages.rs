use crate::{
    hidpp::Feature,
    identifiers::*,
    settings::{Setting, SettingValue},
};
#[cfg(feature = "schema")]
use alloc::string::ToString;
use alloc::{string::String, vec::Vec};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Request {
    pub v: u8,
    pub id: RequestId,
    #[serde(flatten)]
    pub command: Command,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "cmd", content = "args", rename_all = "snake_case")]
pub enum Command {
    #[serde(rename = "adapter.status")]
    Status(Empty),
    #[serde(rename = "adapter.capabilities")]
    Capabilities(Empty),
    #[serde(rename = "adapter.wait_ready")]
    Ready(Empty),
    #[serde(rename = "session.heartbeat")]
    Heartbeat(Empty),
    #[serde(rename = "adapter.bootloader.enter")]
    Bootloader(Empty),
    #[serde(rename = "storage.list")]
    StorageList(StoragePath),
    #[serde(rename = "storage.read")]
    StorageRead(StoragePath),
    #[serde(rename = "session.monitor.set")]
    Monitor(Enabled),
    #[serde(rename = "device.list")]
    Devices(DeviceFilter),
    #[serde(rename = "device.get")]
    Info(DeviceRef),
    #[serde(rename = "device.info")]
    DeviceInfo(DeviceRef),
    #[serde(rename = "device.info.refresh")]
    DeviceInfoRefresh(DeviceRef),
    #[serde(rename = "discovery.scan")]
    Scan(Scan),
    #[serde(rename = "pairing.start")]
    Pair(Pair),
    #[serde(rename = "pairing.reply")]
    PairReply(PairReply),
    #[serde(rename = "device.connect")]
    Connect(Connect),
    #[serde(rename = "device.disconnect")]
    Disconnect(DeviceRef),
    #[serde(rename = "device.unpair")]
    Unpair(DeviceRef),
    #[serde(rename = "device.hidpp.set")]
    Hidpp(DeviceEnabled),
    #[serde(rename = "device.enabled.set")]
    DeviceEnabled(DeviceEnabled),
    #[serde(rename = "device.trusted.set")]
    DeviceTrusted(DeviceTrusted),
    #[serde(rename = "device.blocked.set")]
    DeviceBlocked(DeviceBlocked),
    #[serde(rename = "adapter.platform.set")]
    Platform(Platform),
    #[serde(rename = "adapter.name.set")]
    Name(AdapterName),
    #[serde(rename = "hidpp.feature.list")]
    Features(DeviceRef),
    #[serde(rename = "hidpp.setting.list")]
    Settings(DeviceRef),
    #[serde(rename = "hidpp.setting.get")]
    SettingsGet(SettingRef),
    #[serde(rename = "hidpp.setting.set")]
    SettingsSet(SettingSet),
    #[serde(rename = "hidpp.setting.forget")]
    SettingsForget(SettingRef),
    #[serde(rename = "hidpp.setting.refresh")]
    SettingsRefresh(DeviceRef),
    #[serde(rename = "hidpp.setting.apply")]
    SettingsApply(DeviceRef),
    #[serde(rename = "request.cancel")]
    Cancel(RequestRef),
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct StoragePath {
    pub path: String,
}
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct Empty {}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct Enabled {
    pub enabled: bool,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct DeviceRef {
    pub device_id: DeviceId,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct RequestRef {
    pub request_id: RequestId,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct AdapterName {
    /// An explicit null clears the override; the field is required.
    #[serde(deserialize_with = "Option::deserialize")]
    pub name: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct Platform {
    pub platform: HostPlatform,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct DeviceEnabled {
    pub device_id: DeviceId,
    pub enabled: bool,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct DeviceTrusted {
    pub device_id: DeviceId,
    pub trusted: bool,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct DeviceBlocked {
    pub device_id: DeviceId,
    pub blocked: bool,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum Filter {
    #[default]
    Saved,
    Paired,
    Connected,
}
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct DeviceFilter {
    #[serde(default)]
    pub filter: Filter,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct Scan {
    #[serde(default)]
    pub transport: ScanTransport,
    #[serde(default = "scan_duration")]
    pub duration_ms: u32,
}
const fn scan_duration() -> u32 {
    10_000
}
const fn pair_timeout() -> u32 {
    120_000
}
const fn connect_timeout() -> u32 {
    30_000
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct Pair {
    pub candidate_id: CandidateId,
    #[serde(default = "pair_timeout")]
    pub timeout_ms: u32,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct Connect {
    pub device_id: DeviceId,
    #[serde(default = "connect_timeout")]
    pub timeout_ms: u32,
}
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum PairAction {
    Accept,
    Reject,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct PairReply {
    pub request_id: RequestId,
    #[cfg_attr(feature = "schema", schemars(regex(pattern = "^[!-~]{1,64}$")))]
    pub prompt_id: String,
    pub action: PairAction,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct SettingRef {
    pub device_id: DeviceId,
    #[cfg_attr(feature = "schema", schemars(regex(pattern = "^[a-z0-9._-]{1,64}$")))]
    pub key: String,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct SettingSet {
    pub device_id: DeviceId,
    #[cfg_attr(feature = "schema", schemars(regex(pattern = "^[a-z0-9._-]{1,64}$")))]
    pub key: String,
    pub value: SettingValue,
}

impl Command {
    pub fn valid_arguments(&self) -> bool {
        fn identifier(value: &str) -> bool {
            !value.is_empty()
                && value.len() <= 64
                && value.bytes().all(|b| (0x21..=0x7e).contains(&b))
        }
        fn key(value: &str) -> bool {
            !value.is_empty()
                && value.len() <= 64
                && value
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._-".contains(&b))
        }
        let valid_ids = match self {
            Self::StorageList(a) | Self::StorageRead(a) => {
                a.path.starts_with('/')
                    && a.path.len() <= 255
                    && a.path.is_ascii()
                    && !a.path.bytes().any(|b| b < 32 || b == 127)
                    && !a.path.split('/').any(|p| p == "." || p == "..")
            }
            Self::DeviceInfo(a)
            | Self::DeviceInfoRefresh(a)
            | Self::Info(a)
            | Self::Disconnect(a)
            | Self::Unpair(a)
            | Self::Features(a)
            | Self::Settings(a)
            | Self::SettingsRefresh(a)
            | Self::SettingsApply(a) => identifier(&a.device_id.0),
            Self::Connect(a) => identifier(&a.device_id.0),
            Self::Hidpp(a) | Self::DeviceEnabled(a) => identifier(&a.device_id.0),
            Self::DeviceTrusted(a) => identifier(&a.device_id.0),
            Self::DeviceBlocked(a) => identifier(&a.device_id.0),
            Self::Pair(a) => identifier(&a.candidate_id.0),
            Self::PairReply(a) => {
                identifier(&a.prompt_id) && a.value.as_ref().is_none_or(|v| v.len() <= 16)
            }
            Self::SettingsSet(a) => identifier(&a.device_id.0) && key(&a.key),
            Self::SettingsGet(a) | Self::SettingsForget(a) => {
                identifier(&a.device_id.0) && key(&a.key)
            }
            _ => true,
        };
        if !valid_ids {
            return false;
        }
        match self {
            Self::Scan(args) => {
                args.duration_ms == 0 || (1_000..=60_000).contains(&args.duration_ms)
            }
            Self::Pair(args) => (1_000..=180_000).contains(&args.timeout_ms),
            Self::Connect(args) => (1_000..=60_000).contains(&args.timeout_ms),
            Self::SettingsSet(args) => match &args.value {
                SettingValue::Null => false,
                SettingValue::Integer(n) => n.unsigned_abs() <= crate::MAX_REVISION,
                _ => true,
            },
            _ => true,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct WireError {
    pub code: crate::errors::ErrorCode,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<crate::payloads::ErrorDetails>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Message<R = serde_json::Value, D = serde_json::Value> {
    Response {
        v: u8,
        id: RequestId,
        ok: bool,
        done: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        result: Option<R>,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<WireError>,
    },
    Event {
        v: u8,
        event: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        request_id: Option<RequestId>,
        data: D,
    },
}

impl<R, D> Message<R, D> {
    pub fn success(id: RequestId, result: R, done: bool) -> Self {
        Self::Response {
            v: crate::PROTOCOL_VERSION,
            id,
            ok: true,
            done,
            result: Some(result),
            error: None,
        }
    }
    pub fn failure(id: RequestId, error: WireError) -> Self {
        Self::Response {
            v: crate::PROTOCOL_VERSION,
            id,
            ok: false,
            done: true,
            result: None,
            error: Some(error),
        }
    }
    pub fn event(event: String, request_id: Option<RequestId>, data: D) -> Self {
        Self::Event {
            v: crate::PROTOCOL_VERSION,
            event,
            request_id,
            data,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Device {
    pub device_id: DeviceId,
    pub pairing_state: PairingState,
    pub name: Option<String>,
    pub transport: Transport,
    pub roles: Vec<Role>,
    pub state: ConnectionState,
    pub security: Option<ConnectionSecurity>,
    /// Persisted Bluetooth enablement preference, independent of `hidpp_enabled`.
    pub enabled: bool,
    /// Runtime admission; `enabled_reason` explains why it is false.
    pub effective_enabled: bool,
    pub enabled_reason: Option<crate::errors::DisabledReason>,
    /// Derived from the current backend's transports; never persisted.
    pub transport_supported: bool,
    pub validation_error: Option<crate::errors::ValidationError>,
    pub trusted: bool,
    pub blocked: bool,
    pub reconnect: Reconnect,
    pub last_error: Option<WireError>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<crate::errors::WarningCode>,
    pub hidpp_enabled: bool,
    pub normalization_state: NormalizationState,
    pub normalization_error: Option<crate::errors::ErrorCode>,
    pub settings_state: SettingsState,
    pub settings_error: Option<crate::errors::ErrorCode>,
    pub settings_revision: u64,
}
/// Observed properties of the current Bluetooth link, never requested policy.
/// None means the backend cannot report that property. Key size is in bytes.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ConnectionSecurity {
    pub encrypted: Option<bool>,
    pub authenticated: Option<bool>,
    pub secure_connections: Option<bool>,
    pub key_size: Option<u8>,
    pub bonded: Option<bool>,
}
/// Discovery hint from advertised BLE Appearance or Classic Class of Device.
/// Unknown includes devices whose advertised metadata does not identify an input type.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum DeviceKind {
    Unknown,
    Keyboard,
    Mouse,
    KeyboardMouse,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Candidate {
    pub candidate_id: CandidateId,
    pub kind: DeviceKind,
    pub name: Option<String>,
    pub transport: Transport,
    pub rssi: Option<i16>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct SettingChunk {
    pub revision: u64,
    pub device_id: DeviceId,
    pub setting: Setting,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct FeatureChunk {
    pub revision: u64,
    pub device_id: DeviceId,
    pub feature: Feature,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum PromptMethod {
    ConfirmPasskey,
    EnterPasskey,
    EnterPin,
    #[serde(rename = "passkey")]
    DisplayPasskey,
    #[serde(rename = "pin")]
    DisplayPin,
}
impl PromptMethod {
    pub fn display(self) -> bool {
        matches!(self, Self::DisplayPasskey | Self::DisplayPin)
    }
    pub fn valid_reply(self, action: PairAction, value: Option<&str>) -> bool {
        if self.display() {
            return false;
        }
        match (action, self, value) {
            (PairAction::Accept, Self::EnterPasskey, Some(v)) => {
                v.len() == 6 && v.bytes().all(|b| b.is_ascii_digit())
            }
            (PairAction::Accept, Self::EnterPin, Some(v)) => {
                !v.is_empty() && v.len() <= 16 && v.bytes().all(|b| (0x20..=0x7e).contains(&b))
            }
            (PairAction::Accept, Self::EnterPasskey | Self::EnterPin, _) => false,
            (_, _, None) => true,
            _ => false,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Prompt {
    pub candidate_id: CandidateId,
    #[cfg_attr(feature = "schema", schemars(regex(pattern = "^[!-~]{1,64}$")))]
    pub prompt_id: String,
    pub method: PromptMethod,
    pub expires_in_ms: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum BuildProfile {
    Development,
    Production,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Limits {
    pub max_line_bytes: usize,
    pub max_pending_requests: usize,
    pub saved_devices: usize,
    pub active_connections: usize,
    pub scan_candidates: usize,
    pub hidpp_settings: usize,
    pub hidpp_saved_settings: usize,
    pub hidpp_sensors: usize,
    pub hidpp_firmware_entities: usize,
    pub hidpp_setting_choices: usize,
    pub hidpp_features: usize,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Counts {
    pub saved: usize,
    pub paired: usize,
    pub preferred_enabled: usize,
    pub enabled: usize,
    pub connected: usize,
}
/// One native constraint on ordinarily enabled bonds. `limit` excludes the
/// entry reserved for temporary pairing.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct EnabledCapacity {
    pub transports: Vec<Transport>,
    pub limit: usize,
    pub enabled: usize,
}
/// Advisory per-transport pairing admission. Estimates share resources and
/// must not be added together.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct PairingCapacity {
    pub transport: Transport,
    pub available: bool,
    pub reason: Option<crate::errors::PairUnavailable>,
    pub estimated_additional: usize,
}
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Capacity {
    pub enabled: Vec<EnabledCapacity>,
    pub pairing: Vec<PairingCapacity>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Heartbeat {
    pub interval_ms: u32,
    pub timeout_ms: u32,
    pub remaining_ms: u64,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct PendingRequest {
    pub id: RequestId,
    pub cmd: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device_id: Option<DeviceId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub candidate_id: Option<CandidateId>,
}
/// Last native authentication failure, retained in RAM for development diagnostics.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "backend", rename_all = "snake_case")]
pub enum AuthenticationFailure {
    Nimble {
        attempt: u32,
        stage: NimbleAuthenticationStage,
        status: i32,
        encrypted: bool,
        bonded: bool,
    },
}
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum NimbleAuthenticationStage {
    Initiate,
    Encryption,
    SecurityState,
    Prompt,
    Inject,
    Reply,
}
/// Development-only recent GATT writes for up to four tokens; no report payloads.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct GattWriteDiagnostic {
    pub token: u32,
    pub request: u32,
    pub handle: u16,
    pub response: bool,
    pub accepted: bool,
    pub queued_ms: u64,
    pub started_ms: Option<u64>,
    pub completed_ms: Option<u64>,
    pub status: Option<i32>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Status {
    pub protocol: u8,
    pub firmware_version: String,
    pub hardware_config: String,
    pub name: String,
    pub hardware_digest: String,
    pub radio_backend: String,
    pub adapter_id: String,
    pub build_profile: BuildProfile,
    pub boot_id: String,
    pub session_id: String,
    pub limits: Limits,
    pub counts: Counts,
    pub capacity: Capacity,
    pub revision: u64,
    pub host_platform: HostPlatform,
    pub monitor: bool,
    pub radio_ready: bool,
    pub storage_ready: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub authentication_failure: Option<AuthenticationFailure>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gatt_writes: Option<Vec<GattWriteDiagnostic>>,
    pub heartbeat: Heartbeat,
    pub pending: Vec<PendingRequest>,
}
// Command identifiers share request names; optional capabilities are separate.
macro_rules! command_ids {
    ($($variant:ident => $name:literal),* $(,)?) => {
        #[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
        pub enum CommandId { $(#[serde(rename = $name)] $variant,)* }
        impl CommandId {
            pub const fn as_str(self) -> &'static str {
                match self { $(Self::$variant => $name,)* }
            }
            /// Required session operations are available even with no optional capabilities.
            pub const fn session(self) -> bool {
                matches!(self, Self::Status | Self::Capabilities | Self::Ready | Self::Heartbeat)
            }
        }
        pub const COMMANDS: &[CommandId] = &[$(CommandId::$variant,)*];
        impl Command {
            pub fn id(&self) -> CommandId {
                match self { $(Self::$variant(_) => CommandId::$variant,)* }
            }
            pub fn name(&self) -> &'static str { self.id().as_str() }
        }
    };
}
command_ids! {
    Status => "adapter.status",
    Capabilities => "adapter.capabilities",
    Ready => "adapter.wait_ready",
    Heartbeat => "session.heartbeat",
    Bootloader => "adapter.bootloader.enter",
    StorageList => "storage.list",
    StorageRead => "storage.read",
    Monitor => "session.monitor.set",
    Devices => "device.list",
    Info => "device.get",
    DeviceInfo => "device.info",
    DeviceInfoRefresh => "device.info.refresh",
    Scan => "discovery.scan",
    Pair => "pairing.start",
    PairReply => "pairing.reply",
    Connect => "device.connect",
    Disconnect => "device.disconnect",
    Unpair => "device.unpair",
    Hidpp => "device.hidpp.set",
    DeviceEnabled => "device.enabled.set",
    DeviceTrusted => "device.trusted.set",
    DeviceBlocked => "device.blocked.set",
    Platform => "adapter.platform.set",
    Name => "adapter.name.set",
    Features => "hidpp.feature.list",
    Settings => "hidpp.setting.list",
    SettingsGet => "hidpp.setting.get",
    SettingsSet => "hidpp.setting.set",
    SettingsForget => "hidpp.setting.forget",
    SettingsRefresh => "hidpp.setting.refresh",
    SettingsApply => "hidpp.setting.apply",
    Cancel => "request.cancel",
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    Classic,
    Ble,
    Debug,
    StorageManagement,
}
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(transparent)]
#[cfg_attr(feature="schema", schemars(extend("uniqueItems" = true)))]
pub struct Capabilities(pub Vec<Capability>);
impl Capabilities {
    pub fn contains(&self, capability: Capability) -> bool {
        self.0.contains(&capability)
    }
    pub fn valid(&self) -> bool {
        self.0
            .iter()
            .enumerate()
            .all(|(i, c)| !self.0[..i].contains(c))
    }
    pub fn supports_command(&self, command: CommandId) -> bool {
        match command {
            CommandId::Bootloader => self.contains(Capability::Debug),
            CommandId::StorageList | CommandId::StorageRead => {
                self.contains(Capability::StorageManagement)
            }
            _ => true,
        }
    }
    pub fn supports_transport(&self, transport: Transport) -> bool {
        self.contains(match transport {
            Transport::Classic => Capability::Classic,
            Transport::Ble => Capability::Ble,
        })
    }
}
impl Status {
    pub fn pairing(&self, transport: Transport) -> Option<&PairingCapacity> {
        self.capacity
            .pairing
            .iter()
            .find(|p| p.transport == transport)
    }
    /// Remaining ordinary enabled capacity for a transport: the minimum over
    /// every constraint containing it, or None when no constraint applies.
    pub fn enabled_remaining(&self, transport: Transport) -> Option<usize> {
        self.capacity
            .enabled
            .iter()
            .filter(|c| c.transports.contains(&transport))
            .map(|c| c.limit.saturating_sub(c.enabled))
            .min()
    }
}
impl Capabilities {
    /// Resolve a scan to the independently advertised transports.
    pub fn scan_transport(&self, requested: ScanTransport) -> Option<ScanTransport> {
        if !self.supports_command(CommandId::Scan) {
            return None;
        }
        let classic = self.supports_transport(Transport::Classic);
        let ble = self.supports_transport(Transport::Ble);
        match requested {
            ScanTransport::Both => match (classic, ble) {
                (true, true) => Some(ScanTransport::Both),
                (true, false) => Some(ScanTransport::Classic),
                (false, true) => Some(ScanTransport::Ble),
                (false, false) => None,
            },
            ScanTransport::Classic if classic => Some(requested),
            ScanTransport::Ble if ble => Some(requested),
            _ => None,
        }
    }
    pub fn unsupported(&self, command: &Command) -> Option<&'static str> {
        if !self.supports_command(command.id()) {
            return Some("this adapter does not support that command");
        }
        if matches!(command, Command::Pair(_) | Command::Connect(_))
            && !self.supports_transport(Transport::Classic)
            && !self.supports_transport(Transport::Ble)
        {
            return Some("this adapter has no supported Bluetooth transports");
        }
        if let Command::Scan(args) = command
            && self.scan_transport(args.transport).is_none()
        {
            return Some(match args.transport {
                ScanTransport::Classic => "this adapter does not support Bluetooth Classic",
                ScanTransport::Ble => "this adapter does not support Bluetooth LE",
                ScanTransport::Both => "this adapter has no supported Bluetooth transports",
            });
        }
        None
    }
}
impl From<crate::errors::ErrorCode> for WireError {
    fn from(code: crate::errors::ErrorCode) -> Self {
        Self {
            code,
            details: None,
        }
    }
}
