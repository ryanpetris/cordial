// The host's words for adapter codes and records. The adapter sends machine
// codes only. Records keyed by the generated enums are exhaustive, so a
// schema change fails type checking until its wording is added here.
import type {
  CapacityReason,
  ConnectionSecurity,
  DisabledReason,
  ErrorCode,
  HostPlatform,
  InfoField,
  InfoKey,
  NormalizationState,
  PairUnavailable,
  Role,
  SettingKey,
  SettingState,
  SettingValue,
  Transport,
  ValidationError,
  WarningCode,
  WireError,
} from "../protocol/types.ts";
import type { AdapterEntry, Battery, DeviceEntry, SettingsResult } from "./state.ts";

const sentence = (s: string) => s.charAt(0).toUpperCase() + s.slice(1);

const ERRORS: Record<ErrorCode, string> = {
  invalid_request: "the adapter rejected a malformed request",
  invalid_json: "the adapter couldn't parse a request",
  message_too_large: "a request exceeded the adapter's line limit",
  unsupported_version: "the adapter doesn't support this protocol version",
  unknown_command: "this adapter's firmware doesn't support that command",
  invalid_args: "the adapter rejected the command's arguments",
  busy: "the adapter is busy with a conflicting operation; try again when it finishes",
  not_found: "the adapter has no saved device with that ID",
  blocked: "the device is blocked; unblock it before connecting",
  heartbeat_required: "the adapter stopped hearing from this app; try again",
  client_timeout: "stopped because the adapter stopped hearing from this app",
  candidate_expired: "that nearby device is no longer available; search again",
  disabled: "the device is turned off in Cordial; turn on “Use This Device” first",
  pairing_required: "the device needs pairing again; put it in pairing mode and add it again",
  capacity: "the adapter has no room for that right now",
  unsupported_hid: "the device's HID format isn't supported",
  unsupported_transport: "this adapter doesn't support the device's Bluetooth type",
  authentication_failed: "Bluetooth authentication failed",
  authentication_rejected: "authentication was rejected by you or the device",
  stale_prompt: "that pairing prompt is no longer waiting for an answer",
  connection_failed: "the Bluetooth link or HID setup failed",
  radio_unavailable: "the adapter's Bluetooth controller isn't ready",
  input_overflow: "input was dropped because the computer didn't read it in time",
  storage_failed: "the adapter couldn't save the change; what it had saved is unchanged",
  storage_changed: "the adapter's saved data changed during the operation; try again",
  storage_full:
    "the adapter's storage is full, so the change wasn't saved; remove unused devices or set saved settings back to default",
  timeout: "the operation took too long",
  cancelled: "the operation was cancelled",
  not_pending: "that request has already finished",
  not_cancellable: "that operation can't be cancelled",
  session_fault: "the adapter stopped this session because its output stalled",
  internal_error: "the adapter hit an unexpected failure",
  not_connected: "the device isn't connected; connect it first",
  read_only: "that setting is read-only information from the device",
  hidpp_disabled: "Logitech features are off for this device; turn them on to apply saved settings",
  settings_unavailable: "the adapter hasn't read this device's settings yet; it reads them when the device connects",
  unsupported_setting: "the device doesn't support that setting or value now",
  settings_limit: "the adapter can't save more settings for this device; set another one back to default first",
  settings_apply_failed: "some saved values couldn't be applied or confirmed",
  settings_refresh_failed: "some settings couldn't be read",
  feature_set_unavailable: "the device's feature list couldn't be read",
  readback_mismatch: "the device reported a different value after the change",
  backlight_mode_selection_required: "the backlight is in temporary manual mode; choose a backlight mode first",
  backlight_permanent_manual_required: "the level only applies in permanent manual backlight mode",
  native_routing_required: "the thumbwheel isn't in its native mode",
  native_standard_resolution_required: "the wheel isn't in its native standard-resolution mode",
  hidpp_reports_unavailable: "not supported",
  hidpp_protocol_unsupported: "not supported",
  hidpp_reset_unavailable: "special keys unavailable",
  hidpp_controls_unavailable: "special keys unavailable",
  hidpp_timeout: "no response",
  hidpp_transport_error: "couldn't send",
  hidpp_device_error: "device error",
  hidpp_invalid_response: "unexpected reply",
};

const CAPACITY: Record<CapacityReason, string> = {
  enabled_full: "every enabled-device place is in use; turn off another device first",
  storage_full: "the adapter's storage has no room for another device; remove unused devices or saved settings",
  setup_capacity: "the adapter's Bluetooth stack has no pairing place available",
  connections_full: "every connection is in use; disconnect a device first",
};

/** An adapter error as a sentence for the window or a notification. */
export function errorText(error: WireError): string {
  if (error.code === "capacity" && error.details && "reason" in error.details)
    return sentence(CAPACITY[error.details.reason as CapacityReason] ?? ERRORS.capacity);
  if (error.code === "storage_failed" && error.details && "outcome" in error.details)
    if (error.details.outcome === "unknown")
      return "The adapter may or may not have saved the change; check the device and try again.";
  return sentence(ERRORS[error.code]);
}

export const codeText = (code: ErrorCode) => sentence(ERRORS[code]);

export const PAIR_UNAVAILABLE: Record<PairUnavailable, string> = {
  storage_full: "Adapter storage is full — forget an unused device to add more.",
  setup_capacity: "The adapter's Bluetooth stack has no pairing place available.",
  connections_full: "Every connection is in use — disconnect a device first.",
  pairing_active: "Another pairing is in progress.",
  radio_unavailable: "Bluetooth on the adapter isn't ready.",
  storage_unavailable: "Adapter storage isn't ready.",
};

export const DISABLED: Record<DisabledReason, string> = {
  unsupported_transport: "This adapter doesn't support its Bluetooth type.",
  invalid: "Its saved record is invalid.",
  blocked: "It is blocked.",
  disabled: "It is turned off.",
  capacity: "Every enabled-device place is in use; turn off another device to make room.",
};

export const VALIDATION: Record<ValidationError, string> = {
  bond_missing: "Its saved pairing is missing.",
  bond_corrupt: "Its saved pairing is damaged.",
  bond_mismatch: "Its saved pairing belongs to a different device.",
  device_corrupt: "Its saved device record is damaged.",
  read_failed: "The adapter couldn't read its saved record.",
};

export const WARNINGS: Record<WarningCode, string> = {
  unsupported_fields: "Some input fields are unsupported.",
  led_output_unavailable: "Lock indicator lights are unsupported.",
};

export const NORMALIZATION: Record<NormalizationState, string> = {
  off: "Off",
  pending: "Waiting for the device to connect",
  probing: "Checking the device",
  resetting: "Resetting",
  configuring: "Setting up",
  active: "Active",
  unsupported: "Not supported by this device",
  error: "Failed",
};

export const SETTING_STATES: Record<SettingState, string> = {
  unmanaged: "Default",
  pending: "Waiting to apply",
  applying: "Applying…",
  applied: "Applied",
  changed_on_device: "Changed on device",
  unsupported: "Unsupported now",
  error: "Failed",
  uncertain: "Unconfirmed",
};

export const PLATFORMS: Record<HostPlatform, string> = { linux: "Linux", windows: "Windows", mac: "Mac" };
export const TRANSPORTS: Record<Transport, string> = { ble: "Bluetooth LE", classic: "Bluetooth Classic" };
export const ROLES: Record<Role, string> = { keyboard: "Keyboard", mouse: "Mouse", consumer_control: "Media keys" };

export const INFO_LABELS: Record<InfoKey, string> = {
  name: "Reported Name",
  kind: "Device Type",
  manufacturer: "Manufacturer",
  model: "Model",
  serial: "Serial Number",
  firmware: "Firmware",
  hardware: "Hardware",
  software: "Software",
  vendor_id_namespace: "Vendor ID Namespace",
  vendor_id: "Vendor ID",
  product_id: "Product ID",
  product_version: "Product Version",
  battery_percent: "Battery",
  battery_charging: "Charging",
};

const KINDS: Record<DeviceEntry["kind"], string> = {
  keyboard: "Keyboard",
  mouse: "Mouse",
  keyboard_mouse: "Keyboard and mouse",
  other: "Other",
};

export function infoLabel(f: InfoField): string {
  if (f.key === "firmware") return f.instance === 0 ? "Firmware" : "Bootloader";
  return f.instance === 0 ? INFO_LABELS[f.key] : `${INFO_LABELS[f.key]} ${f.instance + 1}`;
}

export function infoValue(f: InfoField): string | null {
  if (!f.available) return null;
  const v = f.value;
  switch (f.key) {
    case "kind":
      return KINDS[v as DeviceEntry["kind"]] ?? "Other";
    case "vendor_id":
    case "product_id":
    case "product_version":
      return typeof v === "number" ? `0x${v.toString(16).toUpperCase().padStart(4, "0")}` : null;
    case "vendor_id_namespace":
      return v === "usb" ? "USB" : "Bluetooth";
    case "battery_percent":
      return `${String(v)}%`;
    case "battery_charging":
      return v ? "Yes" : "No";
    default:
      return String(v);
  }
}

export interface SettingInfo {
  label: string;
  category: string;
  note?: string;
  unit?: string;
  choices?: Record<string, string>;
}

/** Recognized settings in display order; categories follow first use. */
export const SETTINGS: Record<SettingKey, SettingInfo> = {
  "fn.row_default": {
    label: "Function Row Default",
    category: "Keyboard",
    choices: { function_keys: "Function keys (F1–F12)", special_actions: "Special actions" },
  },
  "backlight.enabled": { label: "Backlight", category: "Backlight" },
  "backlight.mode": { label: "Backlight Mode", category: "Backlight" },
  "backlight.level": { label: "Manual Backlight Level", category: "Backlight" },
  "backlight.current_level": { label: "Current Backlight Level", category: "Backlight" },
  "backlight.status": {
    label: "Backlight Status",
    category: "Backlight",
    choices: { battery: "Off (battery)", saturated: "Automatic (saturated)" },
  },
  "backlight.effect": { label: "Backlight Effect", category: "Backlight" },
  "backlight.delay.hands_out": { label: "Timeout With Hands Away", category: "Backlight", unit: "s" },
  "backlight.delay.hands_in": { label: "Timeout With Hands Nearby", category: "Backlight", unit: "s" },
  "backlight.delay.powered": { label: "Timeout While Charging", category: "Backlight", unit: "s" },
  "backlight.power_on": { label: "Backlight at Power-On", category: "Backlight" },
  "backlight.crown": { label: "Crown Backlight", category: "Backlight" },
  "backlight.power_save": { label: "Backlight Power Saving", category: "Backlight" },
  "pointer.dpi.0": { label: "Pointer Speed", category: "Pointer", unit: "DPI" },
  "pointer.dpi.1": { label: "Second Sensor Speed", category: "Pointer", unit: "DPI" },
  "wheel.mode": { label: "Wheel Mode", category: "Wheel" },
  "wheel.threshold": {
    label: "SmartShift Threshold",
    category: "Wheel",
    note: "255 turns automatic switching to ratchet mode off.",
  },
  "wheel.invert": { label: "Reverse Vertical Scrolling", category: "Wheel" },
  "wheel.info": { label: "Wheel Capabilities", category: "Wheel" },
  "thumbwheel.invert": { label: "Reverse Horizontal Scrolling", category: "Wheel" },
};

const ORDER = Object.keys(SETTINGS) as SettingKey[];
export const settingOrder = (key: SettingKey) => ORDER.indexOf(key);

/** An enum token in words: the catalog's wording, else the capitalized token. */
export function choiceText(key: SettingKey, token: string): string {
  return SETTINGS[key].choices?.[token] ?? sentence(token.replace(/[_-]/g, " "));
}

/** Decodes the lowercase-hex `wheel.info` readout into labelled facts; null when undocumented. */
export function wheelInfo(value: SettingValue, featureVersion: number): [string, string][] | null {
  if (typeof value !== "string" || !/^([0-9a-f]{2})+$/.test(value)) return null;
  const b = value.match(/../g)!.map((h) => parseInt(h, 16));
  if (b.length !== (featureVersion === 0 ? 2 : 4)) return null;
  const names: [number, string][] = [
    [2, "ratchet switch"],
    [3, "inversion"],
  ];
  if (featureVersion >= 1) names.push([4, "statistics"]);
  const caps = [];
  for (let bit = 0; bit < 8; bit++)
    if (b[1]! & (1 << bit)) caps.push(names.find(([n]) => n === bit)?.[1] ?? `bit ${bit}`);
  const facts: [string, string][] = [
    ["Resolution Multiplier", String(b[0])],
    ["Capabilities", caps.length ? sentence(caps.join(", ")) : "None"],
  ];
  if (b.length === 4) facts.push(["Ratchets per Rotation", String(b[2])], ["Wheel Diameter", `${b[3]} mm`]);
  return facts;
}

export function securityFacts(s: ConnectionSecurity): [string, string][] {
  const flag = (v: boolean | null | undefined) => (v == null ? "Not reported" : v ? "Yes" : "No");
  return [
    ["Encrypted", flag(s.encrypted)],
    ["Authenticated Pairing", flag(s.authenticated)],
    ["Secure Connections", flag(s.secure_connections)],
    ["Encryption Key", s.key_size == null ? "Not reported" : `${s.key_size * 8}-bit`],
    ["Saved Pairing", flag(s.bonded)],
  ];
}

/** "45%", "45% · Charging" or "Charging"; null when nothing is known. */
export function batteryText(b: Battery | null): string | null {
  if (!b) return null;
  const parts = [];
  if (b.percent != null) parts.push(`${b.percent}%`);
  if (b.charging) parts.push("Charging");
  return parts.length ? parts.join(" · ") : null;
}

/** Whether the shown battery reading is only last known. */
export const batteryStale = (b: Battery) => (b.percent != null && !b.percentFresh) || (b.charging != null && !b.chargingFresh);

/** A Read Again or Apply outcome: the counts that occurred, and always the main one. */
export function settingsResultText(r: SettingsResult): string {
  const c = r.counts;
  if (!c) return r.error ?? "";
  const apply = r.kind === "apply";
  const counts: [string, number][] = [
    ["read", c.read],
    ["applied", c.applied],
    ["unchanged", c.unchanged],
    ["unsupported", c.unsupported],
    ["failed", c.failed],
    ["uncertain", c.uncertain],
  ];
  const summary = sentence(
    counts
      .filter(([o, n]) => n > 0 || (o === "applied" && apply) || (o === "read" && !apply))
      .map(([o, n]) => `${n} ${o}`)
      .join(", "),
  );
  return r.error ? `${r.error} (${summary})` : summary;
}

/** The device's state in a few words, as lists show it. */
export function deviceStatus(d: DeviceEntry["device"]): string {
  if (d.pairing_state === "needs_pairing") return "Needs pairing";
  if (d.blocked) return "Blocked";
  switch (d.state) {
    case "connected":
      return "Connected";
    case "connecting":
      return "Connecting…";
    case "disconnecting":
      return "Disconnecting…";
    default:
      return d.effective_enabled ? "Not connected" : d.enabled ? "Inactive" : "Turned off";
  }
}

/** The adapter's state in a few words, and whether it needs attention. */
export function adapterStatus(a: AdapterEntry): { text: string; warn: boolean } {
  if (a.connection === "disconnected") return { text: "Disconnected", warn: false };
  if (a.connection === "connecting") return { text: "Connecting…", warn: false };
  if (a.readiness === "failed") return { text: "Not ready", warn: true };
  if (a.readiness === "waiting") return { text: "Starting…", warn: false };
  return a.attention.length ? { text: "Needs attention", warn: true } : { text: "Ready", warn: false };
}

/** Removes control and format characters (including bidirectional
 * overrides) from untrusted text before display. */
export const clean = (s: string) =>
  s
    .replace(/[\p{Cc}\u2028\u2029]/gu, " ")
    .replace(/\p{Cf}/gu, "")
    .trim();
