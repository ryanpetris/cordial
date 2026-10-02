// State the main process publishes to the window, and the actions the window
// asks it to perform. Plain data only: everything crosses the IPC boundary
// and the development server's JSON. Enum values are the protocol's names in
// lower case; a value this app doesn't know reads as the enum's zero value.
import type {
  CodeKind,
  DeviceState,
  ErrorCode,
  InactiveReason,
  IntegrationState,
  Kind,
  Platform,
  ReportType,
  Role,
  SettingState,
  Transport,
  WarningCode,
} from "@cordial/protocol";

type Name<E> = Lowercase<Extract<keyof E, string>>;

export type Code = Name<typeof ErrorCode>;
export type HostPlatform = Name<typeof Platform>;
export type TransportName = Exclude<Name<typeof Transport>, "unspecified">;
export type CandidateKind = Name<typeof Kind>;
export type DeviceKind = Exclude<CandidateKind, "unknown">;
export type ConnectionState = Name<typeof DeviceState>;
export type Inactive = Name<typeof InactiveReason>;
export type RoleName = Exclude<Name<typeof Role>, "unknown">;
export type IntegrationStateName = Name<typeof IntegrationState>;
export type SettingStateName = Name<typeof SettingState>;
export type WarningName = Name<typeof WarningCode>;
export type ReportTypeName = Exclude<Name<typeof ReportType>, "unknown">;
export type CodeKindName = Name<typeof CodeKind>;

export type Scalar = boolean | number | string;
export type ValueType = "bool" | "integer" | "enum" | "text" | "color";

/** A protocol error, with what the Dongle added to it. */
export interface WireError {
  code: Code;
  /** What ran out, for `no_capacity`. */
  reason: "unknown" | "enabled" | "storage" | "connections" | null;
  /** For `storage_failed`: the save may or may not have happened. */
  outcomeUnknown: boolean;
}

/** One read-only fact. Keys come from the key catalog; colors are 0xRRGGBB. */
export interface InfoEntry {
  key: string;
  type: "bool" | "integer" | "text" | "color";
  value: Scalar;
}

export interface AdapterStatus {
  id: string;
  name: string;
  platform: HostPlatform;
  ready: boolean;
  /** Transports the firmware supports and whether each is enabled; `maxEnabled` is null while
   * unknown. `settable` is false on firmware that predates the setting: it uses every transport
   * it supports and can't change that. */
  transports: { transport: TransportName; maxEnabled: number | null; enabled: boolean; settable: boolean }[];
  info: InfoEntry[];
}

export interface Security {
  encrypted: boolean | null;
  authenticated: boolean | null;
  secureConnections: boolean | null;
  /** In bytes. */
  keySize: number | null;
}

/** HID++ on the device: the saved switch, the detected version, and whether it is up. */
export interface Integration {
  /** The protocol's IntegrationKind number, passed back unchanged. */
  kind: number;
  enabled: boolean;
  version: { major: number; minor: number } | null;
  /** Null when starting failed with `error`. */
  state: IntegrationStateName | null;
  error: Code | null;
}

export interface DeviceRecord {
  id: string;
  transport: TransportName | null;
  /** The best known name: reported by the device, else seen at pairing. */
  name: string;
  kind: CandidateKind;
  state: ConnectionState;
  enabled: boolean;
  trusted: boolean;
  blocked: boolean;
  /** A Disconnect holds reconnection off. */
  paused: boolean;
  /** Null when the adapter uses the device. */
  inactive: Inactive | null;
  /** The last failed connection attempt. */
  error: Code | null;
  security: Security | null;
  hidpp: Integration | null;
  info: InfoEntry[];
  roles: RoleName[];
}

export interface DeviceWarning {
  code: WarningName;
  service: number;
  reportType: ReportTypeName | null;
  reportId: number | null;
  bitOffset: number | null;
  usagePage: number | null;
  usage: number | null;
}

/** A value the adapter can change on the device and save. */
export interface Setting {
  /** The protocol's IntegrationKind number, passed back unchanged. */
  integration: number;
  key: string;
  type: ValueType;
  /** The last value read from the device; current only while its integration is active. */
  value: Scalar | null;
  /** The value saved on the adapter; null when the adapter leaves the setting alone. */
  saved: Scalar | null;
  /** How applying the saved value went; null without a saved value or after a failure. */
  state: SettingStateName | null;
  /** The last apply's failure. */
  error: Code | null;
  /** Accepted enum or integer values; empty when not a fixed list. */
  choices: Scalar[];
  min: number | null;
  max: number | null;
  step: number | null;
  /** The longest text the device takes, in UTF-8 bytes. */
  maxBytes: number | null;
}

export interface Battery {
  /** Current charge; null while unknown. */
  percent: number | null;
  /** Null while unknown; unknown never means "not charging". */
  charging: boolean | null;
  percentFresh: boolean;
  chargingFresh: boolean;
}

/** A value or policy change staged for the device's settings form. */
export type SettingsChange =
  | { type: "set"; setting: string; value: Scalar }
  | { type: "forget"; setting: string };

export interface SettingsSaveItem {
  change: SettingsChange;
  status: "pending" | "saving" | "saved" | "not_saved" | "not_sent";
  error: string | null;
}

/** Outcomes belong only to the changes in this submission. */
export interface SettingsSave {
  running: boolean;
  items: SettingsSaveItem[];
}

export interface AdapterEntry {
  id: string;
  /** Name reported by the adapter. */
  name: string;
  connection: "connected" | "connecting" | "disconnected";
  /** Why the last Connect failed. */
  connectError: string | null;
  readiness: "waiting" | "ready";
  status: AdapterStatus | null;
  /** Problems worth the tray's attention badge. */
  attention: string[];
}

export interface DeviceEntry {
  /** `adapterId/deviceId`; device IDs are only unique per adapter. */
  key: string;
  adapterId: string;
  device: DeviceRecord;
  /** The device's name cleaned for display. */
  name: string;
  kind: DeviceKind;
  /** Last known readings with their freshness; null when neither is known. */
  battery: Battery | null;
  /** Commands in flight for this device. */
  pending: string[];
  /** Null until the list has been read. */
  warnings: DeviceWarning[] | null;
  /** Why the last read of the warning list failed, in words. */
  warningsError: string | null;
  /** Null until the list has been read. */
  settings: Setting[] | null;
  /** Why the last read of the settings list failed, in words. */
  settingsError: string | null;
  /** The current or most recent settings submission in this adapter session. */
  settingsSave: SettingsSave | null;
}

export interface Candidate {
  id: string;
  transport: TransportName | null;
  name: string;
  kind: CandidateKind;
  rssi: number | null;
}

export interface ScanState {
  adapterId: string;
  running: boolean;
  candidates: Candidate[];
  error: string | null;
}

/** What the pairing asks of the user. */
export type PairingPrompt =
  /** Type a code shown by the device, then Pair or Reject. */
  | { kind: "enter"; code: CodeKindName }
  /** Check the device shows `value`, then accept or reject. */
  | { kind: "confirm"; value: string }
  /** Type `value` on the device; nothing to answer. */
  | { kind: "show"; code: CodeKindName; value: string };

export interface PairingState {
  adapterId: string;
  candidateId: string;
  name: string;
  phase: "pairing" | "connecting" | "connected" | "saved" | "failed" | "cancelled";
  prompt: PairingPrompt | null;
  deviceKey: string | null;
  /** Explanation for the saved-but-not-connected and failed phases. */
  message: string | null;
}

export interface Preferences {
  startAtLogin: boolean;
  alwaysShowTray: boolean;
  notifyLowBattery: boolean;
  lowBatteryPercent: number;
  notifyConnections: boolean;
}

export const DEFAULT_PREFERENCES: Preferences = {
  startAtLogin: false,
  alwaysShowTray: false,
  notifyLowBattery: true,
  lowBatteryPercent: 20,
  notifyConnections: false,
};

/** Saved preferences over the defaults; keys the app doesn't define are dropped. */
export function preferencesFrom(saved: unknown): Preferences {
  if (!saved || typeof saved !== "object") return { ...DEFAULT_PREFERENCES };
  const known = Object.keys(DEFAULT_PREFERENCES).filter((k) => k in saved);
  return { ...DEFAULT_PREFERENCES, ...(Object.fromEntries(known.map((k) => [k, (saved as Record<string, unknown>)[k]])) as Partial<Preferences>) };
}

export interface AppState {
  adapters: AdapterEntry[];
  devices: DeviceEntry[];
  scan: ScanState | null;
  pairing: PairingState | null;
  preferences: Preferences;
  /** The platform this app runs on, for the adapter platform hint. */
  hostPlatform: HostPlatform;
}

export type Action =
  /** `device.refresh` reads the connected device again; `device.reload` reads its warning and settings lists from the adapter again. */
  | { type: "device.connect" | "device.disconnect" | "device.unpair" | "device.refresh" | "device.reload"; key: string }
  | { type: "device.enabled" | "device.trusted" | "device.blocked" | "device.hidpp"; key: string; value: boolean }
  | { type: "settings.save"; key: string; changes: SettingsChange[] }
  | { type: "adapter.name"; adapterId: string; name: string | null }
  | { type: "adapter.platform"; adapterId: string; platform: HostPlatform }
  | { type: "adapter.transport"; adapterId: string; transport: TransportName; enabled: boolean }
  | { type: "adapter.connect" | "adapter.disconnect" | "adapter.menu"; adapterId: string }
  | { type: "adapters.refresh" }
  /** Opens the window's app menu at a point in CSS pixels from the window's top left. */
  | { type: "app.menu"; x: number; y: number }
  | { type: "scan.start"; adapterId: string }
  | { type: "scan.stop" }
  | { type: "pair.start"; adapterId: string; candidateId: string }
  | { type: "pair.reply"; accept: boolean; value?: string }
  | { type: "pair.cancel" | "pair.dismiss" }
  | { type: "preferences"; preferences: Partial<Preferences> };

export type ActionResult = ({ ok: true } | { ok: false; message: string; inline?: boolean }) & { settingsSave?: SettingsSave };

/** Where the main process asks the window to go. */
export type Navigation =
  | { page: "device"; key: string }
  | { page: "adapter"; id: string; rename?: boolean }
  | { page: "add-device" }
  | { page: "preferences" }
  /** The window was hidden; its dialogs close. */
  | { page: "hidden" };

/** What the host running the controller provides beyond the state. */
export interface HostFeatures {
  /** Tray, login start, native menus and window shortcuts (Electron). */
  desktop: boolean;
  /** Asks the user to grant access to another adapter's port (Web Serial). */
  choosePort?: () => Promise<void>;
}

/** The API each host exposes to the window as `window.cordial`. */
export interface DesktopApi {
  host: HostFeatures;
  state(): Promise<AppState>;
  onState(listener: (state: AppState) => void): () => void;
  onNavigate(listener: (to: Navigation) => void): () => void;
  act(action: Action): Promise<ActionResult>;
}
