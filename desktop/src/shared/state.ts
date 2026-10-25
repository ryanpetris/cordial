// State the main process publishes to the window, and the actions the window
// asks it to perform. Plain data only: everything crosses the IPC boundary
// and the development server's JSON. Enum values are the protocol's names in
// lower case; a value this app doesn't know reads as the enum's zero value, except in errors.
import type {
  BacklightEffect,
  BacklightMode,
  BacklightStatus,
  CodeKind,
  DeviceState,
  ErrorCode,
  ErrorOutcome,
  ErrorReason,
  FunctionRowMode,
  InactiveReason,
  IntegrationState,
  KeyboardPlatform,
  Kind,
  OutputMode,
  OutputState,
  Platform,
  ResourceType,
  Role,
  SettingState,
  SettingsGroup,
  Transport,
  VendorRegistry,
  WheelMode,
} from "@cordial/protocol";

type Name<E> = Lowercase<Extract<keyof E, string>>;

export type Code = Name<typeof ErrorCode>;
export type HostPlatform = Name<typeof Platform>;
export type TransportName = Exclude<Name<typeof Transport>, "unspecified">;
/** What a device or candidate says it is; kinds this app doesn't know are skipped. */
export type KindName = Exclude<Name<typeof Kind>, "unknown">;
/** What the app shows a device or candidate as: one kind, or both a keyboard and a mouse. */
export type DeviceKind = "keyboard" | "mouse" | "keyboard_mouse" | "gamepad" | "other";
export type ConnectionState = Name<typeof DeviceState>;
export type Inactive = Name<typeof InactiveReason>;
export type RoleName = Exclude<Name<typeof Role>, "unknown">;
export type OutputModeName = Name<typeof OutputMode>;
export type OutputStateName = Name<typeof OutputState>;
export type IntegrationStateName = Name<typeof IntegrationState>;
export type SettingStateName = Name<typeof SettingState>;
export type ResourceTypeName = Exclude<Name<typeof ResourceType>, "unknown">;
export type CodeKindName = Name<typeof CodeKind>;
export type SettingsGroupName = Exclude<Name<typeof SettingsGroup>, "unknown">;
export type KeyboardPlatformName = Name<typeof KeyboardPlatform>;
export type FunctionRowModeName = Name<typeof FunctionRowMode>;
export type BacklightStatusName = Name<typeof BacklightStatus>;
export type BacklightModeName = Name<typeof BacklightMode>;
export type BacklightEffectName = Name<typeof BacklightEffect>;
export type WheelModeName = Name<typeof WheelMode>;
export type VendorRegistryName = Name<typeof VendorRegistry>;

/** A setting or reading value; enum values are the protocol's names in lower case. */
export type Scalar = boolean | number | string;
export type ValueType = "bool" | "integer" | "enum";

export type ErrorReasonName = Name<typeof ErrorReason>;
export type ErrorOutcomeName = Name<typeof ErrorOutcome>;

/** A protocol error with every detail the Dongle added to it. A code, reason or outcome this app
 * doesn't know keeps its number, so errors that differ only in those stay distinct. */
export interface WireError {
  code: Code | number;
  /** Further context; `unknown` when none was given. */
  reason: ErrorReasonName | number;
  /** What is known of the failed operation's effects or results; `unspecified` when nothing was
   * given, which implies neither success nor unchanged data. */
  outcome: ErrorOutcomeName | number;
  /** Further failures, in the adapter's order; they never replace this one. */
  related: WireError[];
}

/** Facts about the adapter itself; each is null while unknown. */
export interface AdapterInformation {
  firmwareVersion: string | null;
  boardName: string | null;
  developmentBuild: boolean | null;
  /** The validated format of the adapter's mounted storage. */
  storageFormat: number | null;
}

/** Facts from the device's current connection; each is null while unknown, including while
 * disconnected. Configurable behavior appears here only when the device doesn't take writes for it. */
export interface DeviceInformation {
  identity: {
    manufacturer: string | null;
    model: string | null;
    serial: string | null;
    firmwareVersion: string | null;
    bootloaderVersion: string | null;
    hardwareRevision: string | null;
    softwareRevision: string | null;
    vendor: { registry: VendorRegistryName | null; id: number | null };
    product: { id: number | null; version: number | null };
  };
  battery: { level: number | null; charging: boolean | null };
  keyboard: { platform: KeyboardPlatformName | null; functionRow: FunctionRowModeName | null };
  power: { autoOff: number | null };
  backlight: {
    currentBrightness: number | null;
    status: BacklightStatusName | null;
    enabled: boolean | null;
    mode: BacklightModeName | null;
    brightness: number | null;
    delay: { handsAway: number | null; handsNearby: number | null; externalPower: number | null };
    startupEffect: boolean | null;
    dialEffect: boolean | null;
    powerSave: boolean | null;
    effect: BacklightEffectName | null;
  };
  /** In ascending index order. */
  pointerSensors: { index: number; dpi: number | null }[];
  wheel: {
    resolutionMultiplier: number | null;
    ratchetsPerRotation: number | null;
    diameter: number | null;
    mode: WheelModeName | null;
    freeSpinThreshold: number | null;
    invert: boolean | null;
  };
  thumbwheel: { invert: boolean | null };
}

/** A resource count and its allowance, in units the record holding it defines. Either is null
 * while unknown, which is neither zero nor unlimited. */
export interface Resource {
  type: ResourceTypeName;
  count: number | null;
  limit: number | null;
}

export interface AdapterStatus {
  id: string;
  name: string;
  platform: HostPlatform;
  ready: boolean;
  /** The adapter's current problems. */
  errors: WireError[];
  /** USB usage: `interfaces` counts the exposed USB interfaces. */
  resources: Resource[];
  /** Transports the firmware supports and whether each is enabled, with their `devices` and
   * `connections` usage. */
  transports: { transport: TransportName; enabled: boolean; resources: Resource[] }[];
  information: AdapterInformation;
  /** Null on a board without profile support, which has no configuration interfaces either. */
  profileSupport: ProfileSupport | null;
  /** Every configuration interface the firmware supports, enabled or not. */
  interfaces: InterfaceState[];
  /** Output modes the firmware implements, by role; a mode not listed is unsupported. */
  outputSupport: { role: RoleName; modes: { mode: OutputModeName; resources: Resource[] }[] }[];
}

/** What the adapter supports for profiles: `memory` in bytes of loaded profiles, and `layers`, the
 * most profiles one device's layers can list. */
export interface ProfileSupport {
  resources: Resource[];
}

/** A configuration interface's saved preferences. */
export interface InterfaceState {
  /** The protocol's ConfigurationInterface number, passed back unchanged. */
  interface: number;
  /** The saved preference; it can be enabled without fitting in the USB allocation. */
  enabled: boolean;
  /** Whether it is currently exposed over USB. */
  active: boolean;
  /** The profile it edits; 0 when none is selected. */
  profile: number;
  /** Interfaces it can't be enabled alongside. */
  conflicts: number[];
}

/** A saved profile. */
export interface Profile {
  id: number;
  name: string;
  /** The kinds of input its rules change. */
  roles: RoleName[];
}

/** The shown page of the adapter's profiles. */
export interface ProfilePage {
  profiles: Profile[];
  /** Profiles on the page whose record the adapter couldn't read, with why. */
  unreadable: { id: number; error: WireError }[];
  /** Whether a previous or a following page exists. */
  previous: boolean;
  next: boolean;
  loading: boolean;
  /** Why the last read failed, in words. */
  error: string | null;
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
  error: WireError | null;
}

export interface DeviceRecord {
  id: number;
  transport: TransportName | null;
  /** The best known name: reported by the device, else seen at pairing. */
  name: string;
  /** What the device is; empty while unknown. */
  kinds: KindName[];
  state: ConnectionState;
  enabled: boolean;
  trusted: boolean;
  blocked: boolean;
  /** A Disconnect holds reconnection off. */
  paused: boolean;
  /** Null when the adapter uses the device. */
  inactive: Inactive | null;
  /** The last failed connection attempt, and why a connected device's profiles aren't loaded. */
  errors: WireError[];
  security: Security | null;
  hidpp: Integration | null;
  information: DeviceInformation;
  roles: RoleName[];
  /** The device's layers: profile IDs in the order they apply. Null without profile support. */
  profiles: number[] | null;
  /** Saved output choices; a missing gamepad choice means Disabled. A mode this app doesn't know
   * is null. */
  outputPreferences: { role: RoleName; mode: OutputModeName | null }[];
  /** Output state, independent of the Bluetooth connection; `state` is null when setting the
   * output up failed with `error`. `slot` is set only while active. */
  outputs: { role: RoleName; mode: OutputModeName | null; state: OutputStateName | null; error: WireError | null; slot: number | null }[];
}

/** The protocol's field names of each settings group, with the backlight delay's fields at the
 * group's level. */
export interface SettingFields {
  keyboard: "platform" | "function_row";
  power: "auto_off";
  backlight: "enabled" | "mode" | "brightness" | "hands_away" | "hands_nearby" | "external_power" | "startup_effect" | "dial_effect" | "power_save" | "effect";
  pointer_sensor: "dpi";
  wheel: "mode" | "free_spin_threshold" | "invert";
  thumbwheel: "invert";
}

/** One field of a settings object: the object's integration, group and component index, and the
 * field in it. */
export type SettingField = {
  [G in SettingsGroupName]: {
    /** The protocol's IntegrationKind number, passed back unchanged. */
    integration: number;
    group: G;
    /** The component index, for pointer sensors; null in other groups. */
    index: number | null;
    field: SettingFields[G];
  };
}[SettingsGroupName];

/** A value the adapter can change on the device and save. */
export type Setting = SettingField & {
  type: ValueType;
  /** The last value read from the device; current only while its integration is active. An enum
   * reading the app can't interpret is `unknown`. */
  value: Scalar | null;
  /** The value saved on the adapter; null when the adapter leaves the setting alone. */
  saved: Scalar | null;
  /** How applying the saved value went; null without a saved value or after a failure. */
  state: SettingStateName | null;
  /** The last apply's failure. */
  error: WireError | null;
  /** Accepted enum or integer values; empty when not a fixed list. An enum setting without choices
   * takes no value. */
  choices: Scalar[];
  min: number | null;
  max: number | null;
  step: number | null;
};

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
  | { type: "set"; setting: SettingField; value: Scalar }
  | { type: "forget"; setting: SettingField };

export interface SettingsSaveItem {
  change: SettingsChange;
  /** `unknown` when the adapter couldn't confirm the outcome or the session ended before it answered. */
  status: "saving" | "saved" | "not_saved" | "unknown";
  error: string | null;
}

/** Outcomes belong only to the changes in this submission. */
export interface SettingsSave {
  running: boolean;
  items: SettingsSaveItem[];
}

/** Device settings to change; each omitted field stays as it is. */
export interface DeviceChanges {
  enabled?: boolean;
  trusted?: boolean;
  blocked?: boolean;
  /** Whether HID++ is enabled. */
  hidpp?: boolean;
  /** The device's layers, in the order they apply. */
  profiles?: number[];
  gamepadOutput?: OutputModeName;
}

/** Adapter settings to change; each omitted field stays as it is. */
export interface AdapterChanges {
  platform?: HostPlatform;
  /** Whether each listed transport is enabled. */
  transports?: Partial<Record<TransportName, boolean>>;
  /** Configuration interface preferences by the protocol's ConfigurationInterface number; a
   * profile of 0 clears it. */
  interfaces?: Record<string, InterfaceChange>;
}

export interface InterfaceChange {
  enabled?: boolean;
  profile?: number;
}

export interface AdapterEntry {
  id: string;
  /** Name reported by the adapter. */
  name: string;
  /** Reconnecting retains the session during automatic transport recovery. */
  connection: "connected" | "connecting" | "reconnecting" | "disconnected";
  /** Why the last Connect failed. */
  connectError: string | null;
  readiness: "waiting" | "ready";
  status: AdapterStatus | null;
  /** Whether the session is still reading its devices for the first time. */
  listing: boolean;
  /** Why the latest device listing on the connected, ready adapter failed, until another
   * listing starts. */
  listError: string | null;
  /** Problems worth a banner and the tray's attention badge. */
  attention: string[];
  /** The Profiles list's page; null without profile support. */
  profilePage: ProfilePage | null;
  /** The profile picker's page, paged separately so paging it leaves the list where it is; null
   * without profile support or before the picker first opens. */
  pickerPage: ProfilePage | null;
  /** Saved devices whose record the latest listing couldn't read and that this session knows
   * nothing else of, with why, in ID order. */
  unreadableDevices: { id: number; error: WireError }[];
  /** The profiles this session has seen, by ID, for naming layers and interface profiles. */
  profileNames: Record<string, Profile>;
  /** The profile listing has succeeded on the current ready transport. */
  profilesAvailable: boolean;
  /** Why this app can't use the adapter's saved data yet, and any migration of it; null when it can. */
  migration: AdapterMigration | null;
}

/** An adapter whose saved data this app can't use yet. `required`: the Cordial CLI must migrate
 * it; `message` says what stops that, if anything. `starting` through `failed`: the CLI's
 * migration, during which every other adapter is closed. `unsupported`: the adapter's firmware operates on a storage format this app doesn't. */
export interface AdapterMigration {
  phase: "required" | "starting" | "migrating" | "waiting" | "cancelling" | "failed" | "unsupported";
  /** What stops the adapter from being used, in words. */
  message: string | null;
  /** Bytes of the current step done and in total; null while unknown. */
  progress: { completed: number; total: number } | null;
  /** Migrate starts a migration with the CLI. */
  canStart: boolean;
  /** Cancel stops the migration, or ends a failed one, and opens the other adapters again. */
  canCancel: boolean;
}

export interface DeviceEntry {
  /** `adapterId/deviceId`; device IDs are only unique per adapter. */
  key: string;
  adapterId: string;
  device: DeviceRecord;
  /** Whether the current adapter transport has confirmed the device listing. */
  available: boolean;
  /** The device's name cleaned for display. */
  name: string;
  kind: DeviceKind;
  /** Last known readings with their freshness; null when neither is known. */
  battery: Battery | null;
  /** Commands in flight for this device. */
  pending: string[];
  /** Null until the list has been read. */
  settings: Setting[] | null;
  /** The settings list has succeeded on the current ready transport. */
  settingsAvailable: boolean;
  /** Why the last read of the settings list failed, in words. */
  settingsError: string | null;
  /** Why the adapter's last device listing couldn't read this device's record. `device` then holds
   * what was known before, which is only last known until the next device event. */
  readError: WireError | null;
  /** The current or most recent settings submission in this adapter session. */
  settingsSave: SettingsSave | null;
}

export interface Candidate {
  id: number;
  transport: TransportName | null;
  name: string;
  /** What the candidate says it is; empty while unknown. */
  kinds: KindName[];
  rssi: number | null;
}

export interface ScanState {
  adapterId: string;
  running: boolean;
  candidates: Candidate[];
  error: string | null;
  /** The adapter's error behind `error`, with every detail. */
  failure: WireError | null;
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
  candidateId: number;
  name: string;
  phase: "pairing" | "connecting" | "connected" | "saved" | "failed" | "cancelled";
  prompt: PairingPrompt | null;
  deviceKey: string | null;
  /** Explanation for the saved-but-not-connected and failed phases. */
  message: string | null;
  /** The adapter's error behind a failed or cancelled pairing, with every detail. */
  failure: WireError | null;
}

/** Profile problems worth a notification: the adapter's profile memory nearly full, or a connected
 * device whose profiles aren't loaded. */
export type ProfileAlert =
  | { kind: "memory"; adapterId: string; name: string }
  | { kind: "device"; key: string; name: string; error: WireError };

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
  /** Counts the controller's publications, so an action's result can name the state that
   * includes its effect. */
  revision: number;
}

export type Action =
  /** `device.refresh` reads the connected device again; `device.reload` reads its settings list from the adapter again. */
  | { type: "device.connect" | "device.disconnect" | "device.unpair" | "device.refresh" | "device.reload"; key: string }
  /** Changes the given device settings in one request; omitted ones stay as they are. */
  | ({ type: "device.update"; key: string } & DeviceChanges)
  | { type: "settings.save"; key: string; changes: SettingsChange[] }
  | { type: "adapter.name"; adapterId: string; name: string | null }
  /** Changes the given adapter settings in one request; omitted ones stay as they are. */
  | ({ type: "adapter.settings"; adapterId: string } & AdapterChanges)
  /** Creates an empty profile. */
  | { type: "profile.create"; adapterId: string; name: string }
  | { type: "profile.copy"; adapterId: string; profile: number; name: string }
  | { type: "profile.delete"; adapterId: string; profile: number }
  /** Shows the first, next or previous page of profiles. */
  | { type: "profiles.page"; adapterId: string; page: "first" | "next" | "previous"; picker?: boolean }
  /** `adapter.reload` reads the shown profile page again; `devices.reload` lists the devices again. */
  | { type: "adapter.connect" | "adapter.disconnect" | "adapter.menu" | "adapter.reload" | "devices.reload"; adapterId: string }
  /** Resets the adapter, which starts again from its saved data. */
  | { type: "adapter.restart"; adapterId: string }
  | { type: "adapters.refresh" }
  /** Opens the window's app menu at a point in CSS pixels from the window's top left. */
  | { type: "app.menu"; x: number; y: number }
  | { type: "scan.start"; adapterId: string }
  | { type: "scan.stop" }
  | { type: "pair.start"; adapterId: string; candidateId: number }
  | { type: "pair.reply"; accept: boolean; value?: string }
  | { type: "pair.cancel" | "pair.dismiss" }
  | { type: "preferences"; preferences: Partial<Preferences> }
  /** Migrates the adapter's saved data with the Cordial CLI. */
  | { type: "migration.start"; adapterId: string }
  | { type: "migration.cancel" };

export type ActionResult = ({ ok: true } | { ok: false; message: string; inline?: boolean }) & {
  settingsSave?: SettingsSave;
  /** The published state that includes the action's effect; the window waits for it before it
   * finishes the action, so controls never show the state from before the action. */
  revision?: number;
};

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
