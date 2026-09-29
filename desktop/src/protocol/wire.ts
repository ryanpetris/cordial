// Generated from schema/wire.schema.json by scripts/generate-wire.mjs. Do not edit.
/* eslint-disable */

export type CordialSerialProtocol = Request | Response | Event;
export type Request =
  | AdapterStatusRequest
  | AdapterCapabilitiesRequest
  | AdapterWaitReadyRequest
  | SessionHeartbeatRequest
  | AdapterBootloaderEnterRequest
  | StorageListRequest
  | StorageReadRequest
  | SessionMonitorSetRequest
  | DeviceListRequest
  | DeviceGetRequest
  | DeviceInfoRequest
  | DeviceInfoRefreshRequest
  | DiscoveryScanRequest
  | PairingStartRequest
  | PairingReplyRequest
  | DeviceConnectRequest
  | DeviceDisconnectRequest
  | DeviceUnpairRequest
  | DeviceHidppSetRequest
  | DeviceEnabledSetRequest
  | DeviceTrustedSetRequest
  | DeviceBlockedSetRequest
  | AdapterPlatformSetRequest
  | AdapterNameSetRequest
  | HidppFeatureListRequest
  | HidppSettingListRequest
  | HidppSettingGetRequest
  | HidppSettingSetRequest
  | HidppSettingForgetRequest
  | HidppSettingRefreshRequest
  | HidppSettingApplyRequest
  | RequestCancelRequest;
export type RequestId = number;
export type DeviceId = string;
export type CandidateId = string;
export type PairAction = "accept" | "reject";
export type HostPlatform = "linux" | "windows" | "mac";
export type Response =
  | AdapterStatusResponse
  | AdapterCapabilitiesResponse
  | AdapterWaitReadyResponse
  | SessionHeartbeatResponse
  | AdapterBootloaderEnterResponse
  | StorageListResponse
  | StorageReadResponse
  | SessionMonitorSetResponse
  | DeviceListResponse
  | DeviceGetResponse
  | DeviceInfoResponse
  | DeviceInfoRefreshResponse
  | DiscoveryScanResponse
  | PairingStartResponse
  | PairingReplyResponse
  | DeviceConnectResponse
  | DeviceDisconnectResponse
  | DeviceUnpairResponse
  | DeviceHidppSetResponse
  | DeviceEnabledSetResponse
  | DeviceTrustedSetResponse
  | DeviceBlockedSetResponse
  | AdapterPlatformSetResponse
  | AdapterNameSetResponse
  | HidppFeatureListResponse
  | HidppSettingListResponse
  | HidppSettingGetResponse
  | HidppSettingSetResponse
  | HidppSettingForgetResponse
  | HidppSettingRefreshResponse
  | HidppSettingApplyResponse
  | RequestCancelResponse;
export type AdapterStatusResponse =
  | {
      done: true;
      id: RequestId;
      ok: true;
      result: Status;
      type: "response";
      v: 1;
    }
  | {
      done: true;
      error: AdapterStatusError;
      id: RequestId;
      ok: false;
      type: "response";
      v: 1;
    };
/**
 * Last native authentication failure, retained in RAM for development diagnostics.
 */
export type AuthenticationFailure = {
  attempt: number;
  backend: "nimble";
  bonded: boolean;
  encrypted: boolean;
  stage: NimbleAuthenticationStage;
  status: number;
};
export type NimbleAuthenticationStage = "initiate" | "encryption" | "security_state" | "prompt" | "inject" | "reply";
export type BuildProfile = "development" | "production";
export type Transport = "classic" | "ble";
/**
 * Why status reports Pair unavailable for a transport.
 */
export type PairUnavailable =
  | "storage_full"
  | "setup_capacity"
  | "connections_full"
  | "pairing_active"
  | "radio_unavailable"
  | "storage_unavailable";
export type AdapterStatusError = BareError | StorageMutationError;
/**
 * Machine codes only. Human explanations belong to the host application.
 */
export type ErrorCode =
  | "invalid_request"
  | "invalid_json"
  | "message_too_large"
  | "unsupported_version"
  | "unknown_command"
  | "invalid_args"
  | "busy"
  | "not_found"
  | "blocked"
  | "heartbeat_required"
  | "client_timeout"
  | "candidate_expired"
  | "disabled"
  | "pairing_required"
  | "capacity"
  | "unsupported_hid"
  | "unsupported_transport"
  | "authentication_failed"
  | "authentication_rejected"
  | "stale_prompt"
  | "connection_failed"
  | "radio_unavailable"
  | "input_overflow"
  | "storage_failed"
  | "storage_changed"
  | "storage_full"
  | "timeout"
  | "cancelled"
  | "not_pending"
  | "not_cancellable"
  | "session_fault"
  | "internal_error"
  | "not_connected"
  | "read_only"
  | "hidpp_disabled"
  | "settings_unavailable"
  | "unsupported_setting"
  | "settings_limit"
  | "settings_apply_failed"
  | "settings_refresh_failed"
  | "feature_set_unavailable"
  | "readback_mismatch"
  | "backlight_mode_selection_required"
  | "backlight_permanent_manual_required"
  | "native_routing_required"
  | "native_standard_resolution_required"
  | "hidpp_reports_unavailable"
  | "hidpp_protocol_unsupported"
  | "hidpp_reset_unavailable"
  | "hidpp_controls_unavailable"
  | "hidpp_timeout"
  | "hidpp_transport_error"
  | "hidpp_device_error"
  | "hidpp_invalid_response";
/**
 * `details.outcome` of a `storage_failed` error; absent means not saved.
 */
export type StorageOutcome = "not_saved" | "unknown";
export type AdapterCapabilitiesResponse =
  | {
      done: true;
      id: RequestId;
      ok: true;
      result: Capabilities;
      type: "response";
      v: 1;
    }
  | {
      done: true;
      error: AdapterCapabilitiesError;
      id: RequestId;
      ok: false;
      type: "response";
      v: 1;
    };
export type Capability = "classic" | "ble" | "debug" | "storage_management";
export type Capabilities = Capability[];
export type AdapterCapabilitiesError = BareError | StorageMutationError;
export type AdapterWaitReadyResponse =
  | {
      done: true;
      id: RequestId;
      ok: true;
      result: ReadyResult & {
        state?: "ready";
      };
      type: "response";
      v: 1;
    }
  | {
      done: true;
      error: AdapterWaitReadyError;
      id: RequestId;
      ok: false;
      type: "response";
      v: 1;
    }
  | {
      done: false;
      id: RequestId;
      ok: true;
      result: {
        state: "initializing";
      };
      type: "response";
      v: 1;
    };
export type ReadyResult =
  | {
      state: "initializing";
    }
  | {
      state: "ready";
      status: Status;
    };
export type AdapterWaitReadyError = BareError | StorageMutationError;
export type SessionHeartbeatResponse =
  | {
      done: true;
      id: RequestId;
      ok: true;
      result: HeartbeatResult;
      type: "response";
      v: 1;
    }
  | {
      done: true;
      error: SessionHeartbeatError;
      id: RequestId;
      ok: false;
      type: "response";
      v: 1;
    };
export type SessionHeartbeatError = BareError | StorageMutationError;
export type AdapterBootloaderEnterResponse =
  | {
      done: true;
      id: RequestId;
      ok: true;
      result: BootloaderResult;
      type: "response";
      v: 1;
    }
  | {
      done: true;
      error: AdapterBootloaderEnterError;
      id: RequestId;
      ok: false;
      type: "response";
      v: 1;
    };
export type BootloaderMode = "bootsel" | "download";
export type AdapterBootloaderEnterError = BareError | StorageMutationError;
export type StorageListResponse =
  | {
      done: true;
      id: RequestId;
      ok: true;
      result: StorageListEnd;
      type: "response";
      v: 1;
    }
  | {
      done: true;
      error: StorageListError;
      id: RequestId;
      ok: false;
      type: "response";
      v: 1;
    }
  | {
      done: false;
      id: RequestId;
      ok: true;
      result: FileEntry;
      type: "response";
      v: 1;
    };
export type StorageListError = BareError | StorageMutationError;
export type FileType = "file" | "directory";
export type StorageReadResponse =
  | {
      done: true;
      id: RequestId;
      ok: true;
      result: StorageReadEnd;
      type: "response";
      v: 1;
    }
  | {
      done: true;
      error: StorageReadError;
      id: RequestId;
      ok: false;
      type: "response";
      v: 1;
    }
  | {
      done: false;
      id: RequestId;
      ok: true;
      result: StorageChunk;
      type: "response";
      v: 1;
    };
export type StorageReadError = BareError | StorageMutationError;
export type SessionMonitorSetResponse =
  | {
      done: true;
      id: RequestId;
      ok: true;
      result: MonitorResult;
      type: "response";
      v: 1;
    }
  | {
      done: true;
      error: SessionMonitorSetError;
      id: RequestId;
      ok: false;
      type: "response";
      v: 1;
    };
export type SessionMonitorSetError = BareError | StorageMutationError;
export type DeviceListResponse =
  | {
      done: true;
      id: RequestId;
      ok: true;
      result: DeviceListEnd;
      type: "response";
      v: 1;
    }
  | {
      done: true;
      error: DeviceListError;
      id: RequestId;
      ok: false;
      type: "response";
      v: 1;
    }
  | {
      done: false;
      id: RequestId;
      ok: true;
      result: DeviceSnapshot;
      type: "response";
      v: 1;
    };
export type DeviceListError = BareError | StorageMutationError;
/**
 * Why a saved device is not effectively enabled, in precedence order.
 */
export type DisabledReason = "unsupported_transport" | "invalid" | "blocked" | "disabled" | "capacity";
export type ErrorDetails = MutationDetails | CapacityDetails | ExistingDeviceDetails | SettingsSummary;
/**
 * `details.reason` of a `capacity` error.
 */
export type CapacityReason = "enabled_full" | "storage_full" | "setup_capacity" | "connections_full";
export type NormalizationState =
  "off" | "pending" | "probing" | "resetting" | "configuring" | "active" | "unsupported" | "error";
export type PairingState = "paired" | "needs_pairing";
export type Reconnect = "auto" | "paused";
export type Role = "keyboard" | "mouse" | "consumer_control";
export type SettingsState = "off" | "pending" | "discovering" | "ready" | "applying" | "unsupported" | "error";
export type ConnectionState = "disconnected" | "connecting" | "connected" | "disconnecting";
/**
 * Structured stored-record validation problem for a saved device.
 */
export type ValidationError = "bond_missing" | "bond_corrupt" | "bond_mismatch" | "device_corrupt" | "read_failed";
export type WarningCode = "led_output_unavailable" | "unsupported_fields";
export type DeviceGetResponse =
  | {
      done: true;
      id: RequestId;
      ok: true;
      result: DeviceSnapshot;
      type: "response";
      v: 1;
    }
  | {
      done: true;
      error: DeviceGetError;
      id: RequestId;
      ok: false;
      type: "response";
      v: 1;
    };
export type DeviceGetError = BareError | StorageMutationError;
export type DeviceInfoResponse =
  | {
      done: true;
      id: RequestId;
      ok: true;
      result: DeviceInfo;
      type: "response";
      v: 1;
    }
  | {
      done: true;
      error: DeviceInfoError;
      id: RequestId;
      ok: false;
      type: "response";
      v: 1;
    };
export type InfoKey =
  | "name"
  | "kind"
  | "manufacturer"
  | "model"
  | "serial"
  | "firmware"
  | "hardware"
  | "software"
  | "vendor_id_namespace"
  | "vendor_id"
  | "product_id"
  | "product_version"
  | "battery_percent"
  | "battery_charging";
export type SettingValue = null | boolean | string | number;
export type DeviceInfoError = BareError | StorageMutationError;
export type DeviceInfoRefreshResponse =
  | {
      done: true;
      id: RequestId;
      ok: true;
      result: DeviceInfo;
      type: "response";
      v: 1;
    }
  | {
      done: true;
      error: DeviceInfoRefreshError;
      id: RequestId;
      ok: false;
      type: "response";
      v: 1;
    };
export type DeviceInfoRefreshError = BareError | StorageMutationError;
export type DiscoveryScanResponse =
  | {
      done: true;
      id: RequestId;
      ok: true;
      result: ScanEnd;
      type: "response";
      v: 1;
    }
  | {
      done: true;
      error: DiscoveryScanError;
      id: RequestId;
      ok: false;
      type: "response";
      v: 1;
    };
export type DiscoveryScanError = BareError | StorageMutationError;
export type PairingStartResponse =
  | {
      done: true;
      id: RequestId;
      ok: true;
      result: DeviceResult;
      type: "response";
      v: 1;
    }
  | {
      done: true;
      error: PairingStartError;
      id: RequestId;
      ok: false;
      type: "response";
      v: 1;
    };
export type PairingStartError = BareError | StorageMutationError | CapacityError | ExistingDeviceError;
export type PairingReplyResponse =
  | {
      done: true;
      id: RequestId;
      ok: true;
      result: PairReplyResult;
      type: "response";
      v: 1;
    }
  | {
      done: true;
      error: PairingReplyError;
      id: RequestId;
      ok: false;
      type: "response";
      v: 1;
    };
export type PairingReplyError = BareError | StorageMutationError;
export type DeviceConnectResponse =
  | {
      done: true;
      id: RequestId;
      ok: true;
      result: DeviceResult;
      type: "response";
      v: 1;
    }
  | {
      done: true;
      error: DeviceConnectError;
      id: RequestId;
      ok: false;
      type: "response";
      v: 1;
    };
export type DeviceConnectError = BareError | StorageMutationError | CapacityError;
export type DeviceDisconnectResponse =
  | {
      done: true;
      id: RequestId;
      ok: true;
      result: DeviceResult;
      type: "response";
      v: 1;
    }
  | {
      done: true;
      error: DeviceDisconnectError;
      id: RequestId;
      ok: false;
      type: "response";
      v: 1;
    };
export type DeviceDisconnectError = BareError | StorageMutationError;
export type DeviceUnpairResponse =
  | {
      done: true;
      id: RequestId;
      ok: true;
      result: DeviceRemoved;
      type: "response";
      v: 1;
    }
  | {
      done: true;
      error: DeviceUnpairError;
      id: RequestId;
      ok: false;
      type: "response";
      v: 1;
    };
export type DeviceUnpairError = BareError | StorageMutationError;
export type DeviceHidppSetResponse =
  | {
      done: true;
      id: RequestId;
      ok: true;
      result: DeviceResult;
      type: "response";
      v: 1;
    }
  | {
      done: true;
      error: DeviceHidppSetError;
      id: RequestId;
      ok: false;
      type: "response";
      v: 1;
    };
export type DeviceHidppSetError = BareError | StorageMutationError;
export type DeviceEnabledSetResponse =
  | {
      done: true;
      id: RequestId;
      ok: true;
      result: DeviceResult;
      type: "response";
      v: 1;
    }
  | {
      done: true;
      error: DeviceEnabledSetError;
      id: RequestId;
      ok: false;
      type: "response";
      v: 1;
    };
export type DeviceEnabledSetError = BareError | StorageMutationError | CapacityError;
export type DeviceTrustedSetResponse =
  | {
      done: true;
      id: RequestId;
      ok: true;
      result: DeviceResult;
      type: "response";
      v: 1;
    }
  | {
      done: true;
      error: DeviceTrustedSetError;
      id: RequestId;
      ok: false;
      type: "response";
      v: 1;
    };
export type DeviceTrustedSetError = BareError | StorageMutationError;
export type DeviceBlockedSetResponse =
  | {
      done: true;
      id: RequestId;
      ok: true;
      result: DeviceResult;
      type: "response";
      v: 1;
    }
  | {
      done: true;
      error: DeviceBlockedSetError;
      id: RequestId;
      ok: false;
      type: "response";
      v: 1;
    };
export type DeviceBlockedSetError = BareError | StorageMutationError | CapacityError;
export type AdapterPlatformSetResponse =
  | {
      done: true;
      id: RequestId;
      ok: true;
      result: AdapterSettings;
      type: "response";
      v: 1;
    }
  | {
      done: true;
      error: AdapterPlatformSetError;
      id: RequestId;
      ok: false;
      type: "response";
      v: 1;
    };
export type AdapterPlatformSetError = BareError | StorageMutationError;
export type AdapterNameSetResponse =
  | {
      done: true;
      id: RequestId;
      ok: true;
      result: AdapterSettings;
      type: "response";
      v: 1;
    }
  | {
      done: true;
      error: AdapterNameSetError;
      id: RequestId;
      ok: false;
      type: "response";
      v: 1;
    };
export type AdapterNameSetError = BareError | StorageMutationError;
export type HidppFeatureListResponse =
  | {
      done: true;
      id: RequestId;
      ok: true;
      result: SettingsListEnd;
      type: "response";
      v: 1;
    }
  | {
      done: true;
      error: HidppFeatureListError;
      id: RequestId;
      ok: false;
      type: "response";
      v: 1;
    }
  | {
      done: false;
      id: RequestId;
      ok: true;
      result: FeatureChunk;
      type: "response";
      v: 1;
    };
export type HidppFeatureListError = BareError | StorageMutationError;
/**
 * Stable protocol feature number, independent of each device's feature table.
 */
export type FeatureId = number;
/**
 * Index assigned by the connected peripheral, not a feature number.
 */
export type FeatureIndex = number;
export type HidppSettingListResponse =
  | {
      done: true;
      id: RequestId;
      ok: true;
      result: SettingsListEnd;
      type: "response";
      v: 1;
    }
  | {
      done: true;
      error: HidppSettingListError;
      id: RequestId;
      ok: false;
      type: "response";
      v: 1;
    }
  | {
      done: false;
      id: RequestId;
      ok: true;
      result: SettingChunk;
      type: "response";
      v: 1;
    };
export type HidppSettingListError = BareError | StorageMutationError;
/**
 * Explicit codes: persisted setting-record keys use them, so never renumber.
 */
export type SettingKey =
  | "fn.row_default"
  | "backlight.enabled"
  | "backlight.mode"
  | "backlight.level"
  | "backlight.delay.hands_out"
  | "backlight.delay.hands_in"
  | "backlight.delay.powered"
  | "pointer.dpi.0"
  | "pointer.dpi.1"
  | "wheel.mode"
  | "wheel.threshold"
  | "wheel.invert"
  | "thumbwheel.invert"
  | "backlight.power_on"
  | "backlight.crown"
  | "backlight.power_save"
  | "backlight.effect"
  | "backlight.current_level"
  | "backlight.status"
  | "wheel.info";
export type ObservationSource = "read" | "event";
export type SettingScope = "device" | "current_host";
export type SettingState =
  "unmanaged" | "pending" | "applying" | "applied" | "changed_on_device" | "unsupported" | "error" | "uncertain";
export type SettingType = "bool" | "integer" | "enum" | "text";
export type HidppSettingGetResponse =
  | {
      done: true;
      id: RequestId;
      ok: true;
      result: SettingChunk;
      type: "response";
      v: 1;
    }
  | {
      done: true;
      error: HidppSettingGetError;
      id: RequestId;
      ok: false;
      type: "response";
      v: 1;
    };
export type HidppSettingGetError = BareError | StorageMutationError;
export type HidppSettingSetResponse =
  | {
      done: true;
      id: RequestId;
      ok: true;
      result: SettingChunk;
      type: "response";
      v: 1;
    }
  | {
      done: true;
      error: HidppSettingSetError;
      id: RequestId;
      ok: false;
      type: "response";
      v: 1;
    };
export type HidppSettingSetError = BareError | StorageMutationError;
export type HidppSettingForgetResponse =
  | {
      done: true;
      id: RequestId;
      ok: true;
      result: SettingChunk;
      type: "response";
      v: 1;
    }
  | {
      done: true;
      error: HidppSettingForgetError;
      id: RequestId;
      ok: false;
      type: "response";
      v: 1;
    };
export type HidppSettingForgetError = BareError | StorageMutationError;
export type HidppSettingRefreshResponse =
  | {
      done: true;
      id: RequestId;
      ok: true;
      result: SettingsSummary;
      type: "response";
      v: 1;
    }
  | {
      done: true;
      error: HidppSettingRefreshError;
      id: RequestId;
      ok: false;
      type: "response";
      v: 1;
    }
  | {
      done: false;
      id: RequestId;
      ok: true;
      result: SettingOutcomeChunk;
      type: "response";
      v: 1;
    };
export type HidppSettingRefreshError = BareError | StorageMutationError | SettingsRefreshError;
export type SettingOutcome = "read" | "applied" | "unchanged" | "unsupported" | "failed" | "uncertain";
export type HidppSettingApplyResponse =
  | {
      done: true;
      id: RequestId;
      ok: true;
      result: SettingsSummary;
      type: "response";
      v: 1;
    }
  | {
      done: true;
      error: HidppSettingApplyError;
      id: RequestId;
      ok: false;
      type: "response";
      v: 1;
    }
  | {
      done: false;
      id: RequestId;
      ok: true;
      result: SettingOutcomeChunk;
      type: "response";
      v: 1;
    };
export type HidppSettingApplyError = BareError | StorageMutationError | SettingsApplyError;
export type RequestCancelResponse =
  | {
      done: true;
      id: RequestId;
      ok: true;
      result: CancelResult;
      type: "response";
      v: 1;
    }
  | {
      done: true;
      error: RequestCancelError;
      id: RequestId;
      ok: false;
      type: "response";
      v: 1;
    };
export type RequestCancelError = BareError | StorageMutationError;
export type Event =
  | AdapterChangedEvent
  | DeviceInfoChangedEvent
  | DeviceChangedEvent
  | DevicePairedEvent
  | DeviceConnectedEvent
  | DeviceDisconnectedEvent
  | DeviceUnpairedEvent
  | HidppSettingChangedEvent
  | DiscoveryResultEvent
  | PairingPromptEvent
  | PairingDisplayEvent
  | EventsLostEvent
  | ProtocolErrorEvent;
export type DisconnectReason = "requested" | "remote" | "link_loss" | "unknown";
/**
 * Discovery hint from advertised BLE Appearance or Classic Class of Device.
 * Unknown includes devices whose advertised metadata does not identify an input type.
 */
export type DeviceKind = "unknown" | "keyboard" | "mouse" | "keyboard_mouse";
export type PromptMethod = "confirm_passkey" | "enter_passkey" | "enter_pin" | "passkey" | "pin";

export interface AdapterStatusRequest {
  args: Empty;
  cmd: "adapter.status";
  id: RequestId;
  v: 1;
}
export interface Empty {}
export interface AdapterCapabilitiesRequest {
  args: Empty;
  cmd: "adapter.capabilities";
  id: RequestId;
  v: 1;
}
export interface AdapterWaitReadyRequest {
  args: Empty;
  cmd: "adapter.wait_ready";
  id: RequestId;
  v: 1;
}
export interface SessionHeartbeatRequest {
  args: Empty;
  cmd: "session.heartbeat";
  id: RequestId;
  v: 1;
}
export interface AdapterBootloaderEnterRequest {
  args: Empty;
  cmd: "adapter.bootloader.enter";
  id: RequestId;
  v: 1;
}
export interface StorageListRequest {
  args: StoragePath;
  cmd: "storage.list";
  id: RequestId;
  v: 1;
}
export interface StoragePath {
  path: string;
}
export interface StorageReadRequest {
  args: StoragePath;
  cmd: "storage.read";
  id: RequestId;
  v: 1;
}
export interface SessionMonitorSetRequest {
  args: Enabled;
  cmd: "session.monitor.set";
  id: RequestId;
  v: 1;
}
export interface Enabled {
  enabled: boolean;
}
export interface DeviceListRequest {
  args: DeviceFilter;
  cmd: "device.list";
  id: RequestId;
  v: 1;
}
export interface DeviceFilter {
  filter?: "saved" | "paired" | "connected";
}
export interface DeviceGetRequest {
  args: DeviceRef;
  cmd: "device.get";
  id: RequestId;
  v: 1;
}
export interface DeviceRef {
  device_id: DeviceId;
}
export interface DeviceInfoRequest {
  args: DeviceRef;
  cmd: "device.info";
  id: RequestId;
  v: 1;
}
export interface DeviceInfoRefreshRequest {
  args: DeviceRef;
  cmd: "device.info.refresh";
  id: RequestId;
  v: 1;
}
export interface DiscoveryScanRequest {
  args: Scan;
  cmd: "discovery.scan";
  id: RequestId;
  v: 1;
}
export interface Scan {
  duration_ms?: 0 | number;
  transport?: "both" | "classic" | "ble";
}
export interface PairingStartRequest {
  args: Pair;
  cmd: "pairing.start";
  id: RequestId;
  v: 1;
}
export interface Pair {
  candidate_id: CandidateId;
  timeout_ms?: number;
}
export interface PairingReplyRequest {
  args: PairReply;
  cmd: "pairing.reply";
  id: RequestId;
  v: 1;
}
export interface PairReply {
  action: PairAction;
  prompt_id: string;
  request_id: RequestId;
  value?: string | null;
}
export interface DeviceConnectRequest {
  args: Connect;
  cmd: "device.connect";
  id: RequestId;
  v: 1;
}
export interface Connect {
  device_id: DeviceId;
  timeout_ms?: number;
}
export interface DeviceDisconnectRequest {
  args: DeviceRef;
  cmd: "device.disconnect";
  id: RequestId;
  v: 1;
}
export interface DeviceUnpairRequest {
  args: DeviceRef;
  cmd: "device.unpair";
  id: RequestId;
  v: 1;
}
export interface DeviceHidppSetRequest {
  args: DeviceEnabled;
  cmd: "device.hidpp.set";
  id: RequestId;
  v: 1;
}
export interface DeviceEnabled {
  device_id: DeviceId;
  enabled: boolean;
}
export interface DeviceEnabledSetRequest {
  args: DeviceEnabled;
  cmd: "device.enabled.set";
  id: RequestId;
  v: 1;
}
export interface DeviceTrustedSetRequest {
  args: DeviceTrusted;
  cmd: "device.trusted.set";
  id: RequestId;
  v: 1;
}
export interface DeviceTrusted {
  device_id: DeviceId;
  trusted: boolean;
}
export interface DeviceBlockedSetRequest {
  args: DeviceBlocked;
  cmd: "device.blocked.set";
  id: RequestId;
  v: 1;
}
export interface DeviceBlocked {
  blocked: boolean;
  device_id: DeviceId;
}
export interface AdapterPlatformSetRequest {
  args: Platform;
  cmd: "adapter.platform.set";
  id: RequestId;
  v: 1;
}
export interface Platform {
  platform: HostPlatform;
}
export interface AdapterNameSetRequest {
  args: AdapterName;
  cmd: "adapter.name.set";
  id: RequestId;
  v: 1;
}
export interface AdapterName {
  /**
   * An explicit null clears the override; the field is required.
   */
  name: string | null;
}
export interface HidppFeatureListRequest {
  args: DeviceRef;
  cmd: "hidpp.feature.list";
  id: RequestId;
  v: 1;
}
export interface HidppSettingListRequest {
  args: DeviceRef;
  cmd: "hidpp.setting.list";
  id: RequestId;
  v: 1;
}
export interface HidppSettingGetRequest {
  args: SettingRef;
  cmd: "hidpp.setting.get";
  id: RequestId;
  v: 1;
}
export interface SettingRef {
  device_id: DeviceId;
  key: string;
}
export interface HidppSettingSetRequest {
  args: SettingSet;
  cmd: "hidpp.setting.set";
  id: RequestId;
  v: 1;
}
export interface SettingSet {
  device_id: DeviceId;
  key: string;
  value: boolean | string | number;
}
export interface HidppSettingForgetRequest {
  args: SettingRef;
  cmd: "hidpp.setting.forget";
  id: RequestId;
  v: 1;
}
export interface HidppSettingRefreshRequest {
  args: DeviceRef;
  cmd: "hidpp.setting.refresh";
  id: RequestId;
  v: 1;
}
export interface HidppSettingApplyRequest {
  args: DeviceRef;
  cmd: "hidpp.setting.apply";
  id: RequestId;
  v: 1;
}
export interface RequestCancelRequest {
  args: RequestRef;
  cmd: "request.cancel";
  id: RequestId;
  v: 1;
}
export interface RequestRef {
  request_id: RequestId;
}
export interface Status {
  adapter_id: string;
  authentication_failure?: AuthenticationFailure | null;
  boot_id: string;
  build_profile: BuildProfile;
  capacity: Capacity;
  counts: Counts;
  firmware_version: string;
  gatt_writes?: GattWriteDiagnostic[] | null;
  hardware_config: string;
  hardware_digest: string;
  heartbeat: Heartbeat;
  host_platform: HostPlatform;
  limits: Limits;
  monitor: boolean;
  name: string;
  pending: PendingRequest[];
  protocol: 1;
  radio_backend: string;
  radio_ready: boolean;
  revision: number;
  session_id: string;
  storage_ready: boolean;
}
export interface Capacity {
  enabled: EnabledCapacity[];
  pairing: PairingCapacity[];
}
/**
 * One native constraint on ordinarily enabled bonds. `limit` excludes the
 * entry reserved for temporary pairing.
 */
export interface EnabledCapacity {
  enabled: number;
  limit: number;
  transports: Transport[];
}
/**
 * Advisory per-transport pairing admission. Estimates share resources and
 * must not be added together.
 */
export interface PairingCapacity {
  available: boolean;
  estimated_additional: number;
  reason: PairUnavailable | null;
  transport: Transport;
}
export interface Counts {
  connected: number;
  enabled: number;
  paired: number;
  preferred_enabled: number;
  saved: number;
}
/**
 * Development-only recent GATT writes for up to four tokens; no report payloads.
 */
export interface GattWriteDiagnostic {
  accepted: boolean;
  completed_ms: number | null;
  handle: number;
  queued_ms: number;
  request: number;
  response: boolean;
  started_ms: number | null;
  status: number | null;
  token: number;
}
export interface Heartbeat {
  interval_ms: number;
  remaining_ms: number;
  timeout_ms: number;
}
export interface Limits {
  active_connections: number;
  hidpp_features: number;
  hidpp_firmware_entities: number;
  hidpp_saved_settings: number;
  hidpp_sensors: number;
  hidpp_setting_choices: number;
  hidpp_settings: number;
  max_line_bytes: number;
  max_pending_requests: number;
  saved_devices: number;
  scan_candidates: number;
}
export interface PendingRequest {
  candidate_id?: CandidateId | null;
  cmd: string;
  device_id?: DeviceId | null;
  id: RequestId;
}
export interface BareError {
  code: ErrorCode;
}
export interface StorageMutationError {
  code: "storage_failed" | "storage_full";
  details: MutationDetails;
}
export interface MutationDetails {
  outcome: StorageOutcome;
}
export interface HeartbeatResult {
  monitor: boolean;
  timeout_ms: number;
}
export interface BootloaderResult {
  mode: BootloaderMode;
  rebooting: boolean;
}
export interface StorageListEnd {
  count: number;
}
export interface FileEntry {
  name: string;
  size: number;
  type: FileType;
}
export interface StorageReadEnd {
  bytes: number;
}
export interface StorageChunk {
  data: string;
  offset: number;
}
export interface MonitorResult {
  enabled: boolean;
  revision: number;
}
export interface DeviceListEnd {
  count: number;
  revision: number;
}
export interface DeviceSnapshot {
  device: Device;
  revision: number;
}
export interface Device {
  blocked: boolean;
  device_id: DeviceId;
  /**
   * Runtime admission; `enabled_reason` explains why it is false.
   */
  effective_enabled: boolean;
  /**
   * Persisted Bluetooth enablement preference, independent of `hidpp_enabled`.
   */
  enabled: boolean;
  enabled_reason: DisabledReason | null;
  hidpp_enabled: boolean;
  last_error: WireError | null;
  name: string | null;
  normalization_error: ErrorCode | null;
  normalization_state: NormalizationState;
  pairing_state: PairingState;
  reconnect: Reconnect;
  roles: Role[];
  security: ConnectionSecurity | null;
  settings_error: ErrorCode | null;
  settings_revision: number;
  settings_state: SettingsState;
  state: ConnectionState;
  transport: Transport;
  /**
   * Derived from the current backend's transports; never persisted.
   */
  transport_supported: boolean;
  trusted: boolean;
  validation_error: ValidationError | null;
  warnings?: WarningCode[];
}
export interface WireError {
  code: ErrorCode;
  details?: ErrorDetails | null;
}
export interface CapacityDetails {
  reason: CapacityReason;
}
export interface ExistingDeviceDetails {
  device_id: DeviceId;
}
export interface SettingsSummary {
  applied: number;
  count: number;
  device_id: DeviceId;
  failed: number;
  read: number;
  revision: number;
  uncertain: number;
  unchanged: number;
  unsupported: number;
}
/**
 * Observed properties of the current Bluetooth link, never requested policy.
 * None means the backend cannot report that property. Key size is in bytes.
 */
export interface ConnectionSecurity {
  authenticated: boolean | null;
  bonded: boolean | null;
  encrypted: boolean | null;
  key_size: number | null;
  secure_connections: boolean | null;
}
/**
 * Full snapshot for responses; only changed fields for device.info.changed.
 */
export interface DeviceInfo {
  device_id: DeviceId;
  /**
   * @maxItems 28
   */
  fields: InfoField[];
  revision: number;
}
export interface InfoField {
  key: InfoKey;
  instance: number;
  value: SettingValue;
  available: boolean;
  fresh: boolean;
}
export interface ScanEnd {
  count: number;
  truncated: boolean;
}
export interface DeviceResult {
  device: Device;
}
export interface CapacityError {
  code: "capacity";
  details: CapacityDetails;
}
export interface ExistingDeviceError {
  code: ErrorCode;
  details: ExistingDeviceDetails;
}
export interface PairReplyResult {
  accepted: boolean;
}
export interface DeviceRemoved {
  device_id: DeviceId;
  removed: boolean;
}
export interface AdapterSettings {
  host_platform: HostPlatform;
  name: string;
  revision: number;
}
export interface SettingsListEnd {
  count: number;
  device_id: DeviceId;
  revision: number;
  settings_error: ErrorCode | null;
  settings_state: SettingsState;
}
export interface FeatureChunk {
  device_id: DeviceId;
  feature: Feature;
  revision: number;
}
export interface Feature {
  flags: number;
  id: FeatureId;
  index: FeatureIndex;
  /**
   * The firmware has a usable handler; not a device-provided flag.
   */
  supported: boolean;
  version: number;
}
export interface SettingChunk {
  device_id: DeviceId;
  revision: number;
  setting: Setting;
}
export interface Setting {
  choices: SettingValue[];
  desired: SettingValue;
  error: ErrorCode | null;
  feature: FeatureId;
  feature_version: number;
  fresh: boolean;
  key: SettingKey;
  managed: boolean;
  max: number | null;
  min: number | null;
  observation_source: ObservationSource | null;
  observed: SettingValue;
  observed_at_ms: number | null;
  scope: SettingScope;
  state: SettingState;
  step: number | null;
  type: SettingType;
  writable: boolean;
}
export interface SettingsRefreshError {
  code: "settings_refresh_failed" | "not_connected";
  details: SettingsSummary;
}
export interface SettingOutcomeChunk {
  device_id: DeviceId;
  outcome: SettingOutcome;
  revision: number;
  setting: Setting;
}
export interface SettingsApplyError {
  code: "settings_apply_failed" | "not_connected";
  details: SettingsSummary;
}
export interface CancelResult {
  request_id: RequestId;
  requested: boolean;
}
export interface AdapterChangedEvent {
  data: AdapterSettings;
  event: "adapter.changed";
  request_id?: RequestId;
  type: "event";
  v: 1;
}
export interface DeviceInfoChangedEvent {
  data: DeviceInfo;
  event: "device.info.changed";
  request_id?: RequestId;
  type: "event";
  v: 1;
}
export interface DeviceChangedEvent {
  data: DeviceSnapshot;
  event: "device.changed";
  request_id?: RequestId;
  type: "event";
  v: 1;
}
export interface DevicePairedEvent {
  data: DeviceSnapshot;
  event: "device.paired";
  request_id?: RequestId;
  type: "event";
  v: 1;
}
export interface DeviceConnectedEvent {
  data: DeviceSnapshot;
  event: "device.connected";
  request_id?: RequestId;
  type: "event";
  v: 1;
}
export interface DeviceDisconnectedEvent {
  data: DeviceDisconnected;
  event: "device.disconnected";
  request_id?: RequestId;
  type: "event";
  v: 1;
}
export interface DeviceDisconnected {
  device: Device;
  reason: DisconnectReason;
  revision: number;
}
export interface DeviceUnpairedEvent {
  data: DeviceUnpaired;
  event: "device.unpaired";
  request_id?: RequestId;
  type: "event";
  v: 1;
}
export interface DeviceUnpaired {
  device_id: DeviceId;
  revision: number;
}
export interface HidppSettingChangedEvent {
  data: SettingChunk;
  event: "hidpp.setting.changed";
  request_id?: RequestId;
  type: "event";
  v: 1;
}
export interface DiscoveryResultEvent {
  data: Candidate;
  event: "discovery.result";
  request_id: RequestId;
  type: "event";
  v: 1;
}
export interface Candidate {
  candidate_id: CandidateId;
  kind: DeviceKind;
  name: string | null;
  rssi: number | null;
  transport: Transport;
}
export interface PairingPromptEvent {
  data: Prompt;
  event: "pairing.prompt";
  request_id: RequestId;
  type: "event";
  v: 1;
}
export interface Prompt {
  candidate_id: CandidateId;
  expires_in_ms: number;
  method: PromptMethod;
  prompt_id: string;
  value?: string | null;
}
export interface PairingDisplayEvent {
  data: Prompt;
  event: "pairing.display";
  request_id: RequestId;
  type: "event";
  v: 1;
}
export interface EventsLostEvent {
  data: LostEvents;
  event: "events.lost";
  request_id?: RequestId;
  type: "event";
  v: 1;
}
export interface LostEvents {
  dropped: number;
  revision: number;
}
export interface ProtocolErrorEvent {
  data: ProtocolError;
  event: "protocol.error";
  request_id?: RequestId;
  type: "event";
  v: 1;
}
export interface ProtocolError {
  code: ErrorCode;
  supplied_id?: RequestId | null;
}

/** Every command's response union, by command name. */
export interface ResponseMap {
  "adapter.bootloader.enter": AdapterBootloaderEnterResponse;
  "adapter.capabilities": AdapterCapabilitiesResponse;
  "adapter.name.set": AdapterNameSetResponse;
  "adapter.platform.set": AdapterPlatformSetResponse;
  "adapter.status": AdapterStatusResponse;
  "adapter.wait_ready": AdapterWaitReadyResponse;
  "device.blocked.set": DeviceBlockedSetResponse;
  "device.connect": DeviceConnectResponse;
  "device.disconnect": DeviceDisconnectResponse;
  "device.enabled.set": DeviceEnabledSetResponse;
  "device.get": DeviceGetResponse;
  "device.hidpp.set": DeviceHidppSetResponse;
  "device.info": DeviceInfoResponse;
  "device.info.refresh": DeviceInfoRefreshResponse;
  "device.list": DeviceListResponse;
  "device.trusted.set": DeviceTrustedSetResponse;
  "device.unpair": DeviceUnpairResponse;
  "discovery.scan": DiscoveryScanResponse;
  "hidpp.feature.list": HidppFeatureListResponse;
  "hidpp.setting.apply": HidppSettingApplyResponse;
  "hidpp.setting.forget": HidppSettingForgetResponse;
  "hidpp.setting.get": HidppSettingGetResponse;
  "hidpp.setting.list": HidppSettingListResponse;
  "hidpp.setting.refresh": HidppSettingRefreshResponse;
  "hidpp.setting.set": HidppSettingSetResponse;
  "pairing.reply": PairingReplyResponse;
  "pairing.start": PairingStartResponse;
  "request.cancel": RequestCancelResponse;
  "session.heartbeat": SessionHeartbeatResponse;
  "session.monitor.set": SessionMonitorSetResponse;
  "storage.list": StorageListResponse;
  "storage.read": StorageReadResponse;
}
