// State the main process publishes to the window, and the actions the window
// asks it to perform. Plain data only: everything crosses the IPC boundary.
import type {
  Candidate,
  Capability,
  Device,
  ErrorCode,
  HostPlatform,
  InfoField,
  Prompt,
  Setting,
  SettingKey,
  SettingsState,
  Status,
} from "../protocol/types.ts";

export interface Battery {
  /** Current charge; null while unknown. */
  percent: number | null;
  /** Null while unknown; unknown never means "not charging". */
  charging: boolean | null;
}

export interface AdapterEntry {
  id: string;
  /** Name reported by the adapter. */
  name: string;
  connection: "connected" | "connecting" | "disconnected";
  /** Why the last Connect failed. */
  connectError: string | null;
  readiness: "waiting" | "ready" | "failed";
  status: Status | null;
  capabilities: Capability[];
  platform: HostPlatform | null;
  /** Problems worth the tray's attention badge. */
  attention: string[];
}

export interface SettingsEntry {
  settings: Setting[];
  state: SettingsState | null;
  error: ErrorCode | null;
  current: boolean;
  /** Why the last read of the list failed, in words. */
  loadError: string | null;
}

export interface DeviceEntry {
  /** `adapterId/deviceId`; device IDs are only unique per adapter. */
  key: string;
  adapterId: string;
  device: Device;
  /** Reported name, else the saved pairing name. */
  name: string;
  kind: "keyboard" | "mouse" | "keyboard_mouse" | "other";
  /** Current readings only; null when neither is known. */
  battery: Battery | null;
  /** Null until the first information snapshot. */
  info: InfoField[] | null;
  /** Why the last information read failed, in words. */
  infoError: string | null;
  /** Present only while the window shows this device. */
  settings: SettingsEntry | null;
}

export interface ScanState {
  adapterId: string;
  running: boolean;
  candidates: Candidate[];
  error: string | null;
}

export interface PairingPrompt {
  /** A prompt needs an answer; a display only shows a code to type. */
  kind: "prompt" | "display";
  prompt: Prompt;
  /** Wall-clock expiry in milliseconds. */
  expiresAt: number;
}

export interface PairingState {
  adapterId: string;
  candidateId: string;
  name: string;
  phase: "pairing" | "connecting" | "connected" | "saved" | "failed";
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
  | { type: "device.connect" | "device.disconnect" | "device.unpair" | "device.info.refresh"; key: string }
  | { type: "device.enabled" | "device.trusted" | "device.blocked" | "device.hidpp"; key: string; value: boolean }
  | { type: "setting.set"; key: string; setting: SettingKey; value: boolean | number | string }
  | { type: "setting.forget"; key: string; setting: SettingKey }
  | { type: "settings.refresh" | "settings.apply"; key: string }
  | { type: "settings.watch"; key: string | null }
  | { type: "adapter.name"; adapterId: string; name: string | null }
  | { type: "adapter.platform"; adapterId: string; platform: HostPlatform }
  | { type: "adapter.connect" | "adapter.disconnect" | "adapter.menu"; adapterId: string }
  | { type: "adapters.refresh" }
  | { type: "scan.start"; adapterId: string }
  | { type: "scan.stop" }
  | { type: "pair.start"; adapterId: string; candidateId: string }
  | { type: "pair.reply"; accept: boolean; value?: string }
  | { type: "pair.cancel" | "pair.dismiss" }
  | { type: "preferences"; preferences: Partial<Preferences> };

export type ActionResult = { ok: true } | { ok: false; message: string };

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
