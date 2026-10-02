// The host's words for adapter codes and records. The adapter sends machine
// codes and catalog keys only. Records keyed by the protocol's enums are
// exhaustive, so a protocol change fails type checking until its wording is
// added here; keys missing from the catalog tables here are not shown.
import type {
  AdapterEntry,
  AdapterStatus,
  Battery,
  Code,
  DeviceEntry,
  DeviceRecord,
  DeviceWarning,
  HostPlatform,
  InfoEntry,
  Inactive,
  IntegrationStateName,
  Integration,
  ReportTypeName,
  RoleName,
  Scalar,
  Security,
  SettingStateName,
  TransportName,
  WarningName,
  WireError,
} from "./state.ts";

const sentence = (s: string) => s.charAt(0).toUpperCase() + s.slice(1);

const ERRORS: Record<Code, string> = {
  unknown: "the adapter hit an unexpected failure",
  bad_request: "the adapter rejected a malformed request",
  unknown_command: "this adapter's firmware doesn't support that command",
  bad_args: "the adapter rejected the command's arguments",
  too_long: "the request was too large for the adapter",
  not_ready: "the adapter's Bluetooth or storage isn't ready. Try again in a moment",
  not_found: "the adapter couldn't find this device or setting",
  not_connected: "the device isn't connected. Connect it first",
  busy: "the adapter is busy. Try again when the current operation finishes",
  disabled: "the device is turned off in Cordial; turn on “Use This Device” first",
  blocked: "connections to this device are blocked. Unblock it before connecting",
  unsupported: "the adapter or device doesn't support this action",
  no_capacity: "the adapter has no room for that right now",
  no_prompt: "that pairing prompt is no longer waiting for an answer",
  storage_failed: "the adapter couldn't save the change. Your saved data hasn't changed",
  internal: "the adapter hit an unexpected failure",
  candidate_expired: "this device is no longer available. Search again",
  auth_failed: "Bluetooth authentication failed",
  rejected: "authentication was rejected by you or the device",
  timeout: "the operation timed out",
  cancelled: "the operation was cancelled",
  connection_failed: "the Bluetooth link or HID setup failed",
  unsupported_hid: "the device's HID format isn't supported",
  protocol_unsupported: "not supported",
  feature_unavailable: "the device doesn't provide a feature this action needs",
  transport_error: "couldn't send",
  device_error: "device error",
  invalid_response: "unexpected reply",
  readback_mismatch: "the device reported a different value from the one requested",
};

const CAPACITY: Record<NonNullable<WireError["reason"]>, string> = {
  unknown: ERRORS.no_capacity,
  enabled: "every enabled-device place is in use; turn off another device first",
  storage: "the adapter's storage is full. Remove an unused device or forget a saved setting, then try again",
  connections: "every connection is in use; disconnect a device first",
};

/** An adapter error as a sentence for the window or a notification. */
export function errorText(error: WireError): string {
  if (error.code === "no_capacity") return sentence(CAPACITY[error.reason ?? "unknown"]);
  if (error.code === "storage_failed" && error.outcomeUnknown)
    return "The adapter couldn't confirm whether the change was saved. Check before trying again.";
  return sentence(ERRORS[error.code]);
}

export const codeText = (code: Code) => sentence(ERRORS[code]);

/** Why work on a transport is refused, and its saved devices are unused, while the adapter has it
 * disabled. */
export const transportDisabledText = (t: TransportName) => `${TRANSPORTS[t]} is disabled. Enable it in the adapter settings.`;

/** Why a device the adapter doesn't use is inactive. */
export const INACTIVE: Record<Inactive, string> = {
  unknown: "The adapter isn't using this device.",
  unsupported_transport: "This adapter doesn't support the device's Bluetooth type.",
  transport_disabled: "The adapter isn't using this device.",
  blocked: "Connections to this device are blocked.",
  disabled: "It is turned off.",
  capacity: "The adapter can't enable another device. Turn off another device first.",
};

/** Why `d` is inactive, naming its Bluetooth type where that is the reason. */
export function inactiveText(d: DeviceRecord): string {
  if (d.inactive === "unsupported_transport" && d.transport) return `This adapter doesn't support ${TRANSPORTS[d.transport]}.`;
  if (d.inactive === "transport_disabled" && d.transport) return transportDisabledText(d.transport);
  return INACTIVE[d.inactive ?? "unknown"];
}

export const WARNINGS: Record<WarningName, string> = {
  unknown: "The adapter can't use part of this device.",
  numeric_selector_unsupported: "The adapter can't derive a value from this numeric selector field.",
  pointer_selector_unsupported: "The adapter can't forward pointer coordinates from a selector field.",
  buffered_input_unsupported: "The adapter can't interpret this input field's custom byte format.",
  indicator_report_too_large: "The indicator report is larger than the Bluetooth connection allows.",
  indicator_read_unsupported: "The device doesn't support reading this indicator report.",
  indicator_write_unsupported: "The device doesn't support writing this indicator report.",
  buffered_indicator_unsupported: "The adapter can't interpret this indicator field's custom byte format.",
  indicator_state_unknown: "The device doesn't report the indicator state needed for this update.",
  indicator_read_failed: "The adapter couldn't read the indicator report.",
  indicator_write_failed: "The adapter couldn't update the indicator lights.",
  indicator_array_full: "The indicator report has no room for all active lights.",
  indicator_relative_selector_unsupported: "The adapter can't forward this relative indicator selector.",
  indicator_mode_unsupported: "The indicator doesn't provide a usable on/off control.",
  indicator_nonlinear_unsupported: "The adapter can't convert this indicator's nonlinear values.",
  indicator_scale_unsupported: "The indicator's units don't provide a usable value conversion.",
  indicator_range_unsupported: "The indicator's value range can't represent both states.",
};

const REPORT_TYPES: Record<ReportTypeName, string> = {
  input: "Input Report",
  output: "Output Report",
  feature: "Feature Report",
};

const hex = (n: number) => n.toString(16).toUpperCase().padStart(4, "0");

export const WARNINGS_READ_FAILED = "The adapter couldn't read the device's warnings.";

const INPUT_WARNINGS: WarningName[] = ["numeric_selector_unsupported", "pointer_selector_unsupported", "buffered_input_unsupported"];

/** A device warning as an Information fact: what it concerns, the problem,
 * and the HID service, report and field it was found in. */
export function warningFact(w: DeviceWarning): { label: string; text: string; context: string } {
  const context = [`Service ${w.service}`];
  const kind = w.reportType ? REPORT_TYPES[w.reportType] : null;
  if (kind) context.push(w.reportId !== null ? `${kind} ${w.reportId}` : kind);
  else if (w.reportId !== null) context.push(`Report ${w.reportId}`);
  if (w.bitOffset !== null) context.push(`Bit ${w.bitOffset}`);
  if (w.usagePage !== null) context.push(`Usage ${hex(w.usagePage)}${w.usage !== null ? `:${hex(w.usage)}` : ""}`);
  return {
    label: INPUT_WARNINGS.includes(w.code) ? "Input Field" : w.code === "unknown" ? "Device Warning" : "Lock Indicators",
    text: WARNINGS[w.code],
    context: context.join(", "),
  };
}

/** Whether HID++ is up on the device, in a few words. */
export const INTEGRATION_STATES: Record<IntegrationStateName, string> = {
  off: "Off",
  disconnected: "Waiting to Connect",
  starting: "Setting Up",
  active: "Active",
  unsupported: "Unsupported",
};

export function integrationText(i: Integration): string {
  return i.state ? INTEGRATION_STATES[i.state] : `Failed: ${codeText(i.error ?? "unknown")}`;
}

/** The detected HID++ version, as "4.5". */
export const versionText = (i: Integration) => (i.version ? `${i.version.major}.${i.version.minor}` : "Unknown");

/** A saved setting's state as its marker names it; `unmanaged` is a setting with no saved value. */
export const SETTING_STATES: Record<SettingStateName | "unmanaged" | "error", string> = {
  unmanaged: "Not Saved",
  pending: "Saved",
  applied: "Saved",
  changed_on_device: "Changed on Device",
  unsupported: "Can't Apply Now",
  error: "Failed",
};

export const PLATFORMS: Record<HostPlatform, string> = { linux: "Linux", windows: "Windows", mac: "macOS" };
export const TRANSPORTS: Record<TransportName, string> = { ble: "Bluetooth LE", classic: "Bluetooth Classic" };
export const ROLES: Record<RoleName, string> = { keyboard: "Keyboard", mouse: "Mouse", consumer_control: "Media Keys", system_control: "System Keys" };

/** Labels of the information keys the Information card shows, in display order. */
export const INFO_LABELS: Record<string, string> = {
  "device.manufacturer": "Manufacturer",
  "device.model": "Model",
  "device.serial": "Serial Number",
  "firmware.version": "Firmware",
  "bootloader.version": "Bootloader",
  "hardware.revision": "Hardware",
  "software.revision": "Software",
  "vendor.registry": "Vendor ID Namespace",
  "vendor.id": "Vendor ID",
  "product.id": "Product ID",
  "product.version": "Product Version",
};

const KINDS: Record<DeviceEntry["kind"], string> = {
  keyboard: "Keyboard",
  mouse: "Mouse",
  keyboard_mouse: "Keyboard and Mouse",
  other: "Other",
};

export const kindText = (kind: DeviceEntry["kind"]) => KINDS[kind];

export function infoValue(f: InfoEntry): string {
  const v = f.value;
  switch (f.key) {
    case "vendor.id":
    case "product.id":
    case "product.version":
      return typeof v === "number" ? `0x${v.toString(16).toUpperCase().padStart(4, "0")}` : String(v);
    case "vendor.registry":
      return v === "usb" ? "USB" : "Bluetooth";
    default:
      if (f.type === "bool") return v ? "Yes" : "No";
      if (f.type === "color") return `#${(v as number).toString(16).padStart(6, "0").toUpperCase()}`;
      return String(v);
  }
}

/** The text an information entry or the adapter status carries for `key`, if any. */
export const infoOf = (list: InfoEntry[], key: string) => list.find((f) => f.key === key)?.value;

/** Why a nearby device can't be added: the adapter has no room to save it. */
export const STORAGE_FULL = "Storage Full";

export const storageFull = (s: AdapterStatus | null) => infoOf(s?.info ?? [], "storage.full") === true;

export interface SettingInfo {
  label: string;
  category: string;
  unit?: string;
  choices?: Record<string, string>;
}

/** Recognized settings and setting-like readings in display order; categories follow first use. */
const SETTINGS: Record<string, SettingInfo> = {
  "keyboard.fn_row": {
    label: "Function Row",
    category: "Keyboard",
    choices: { function_keys: "F1-F12", special_actions: "Shortcuts" },
  },
  "backlight.enabled": { label: "Backlight", category: "Backlight" },
  "backlight.mode": { label: "Backlight Mode", category: "Backlight" },
  "backlight.level": { label: "Manual Backlight Level", category: "Backlight" },
  "backlight.current_level": { label: "Current Backlight Level", category: "Backlight" },
  "backlight.status": {
    label: "Backlight Status",
    category: "Backlight",
    choices: { battery: "Off (Battery)", saturated: "Automatic (Saturated)" },
  },
  "backlight.effect": { label: "Backlight Effect", category: "Backlight" },
  "backlight.delay.hands_out": { label: "Timeout With Hands Away", category: "Backlight", unit: "s" },
  "backlight.delay.hands_in": { label: "Timeout With Hands Nearby", category: "Backlight", unit: "s" },
  "backlight.delay.powered": { label: "Timeout While Plugged In", category: "Backlight", unit: "s" },
  "backlight.power_on": { label: "Backlight at Power-On", category: "Backlight" },
  "backlight.crown": { label: "Crown Backlight", category: "Backlight" },
  "backlight.power_save": { label: "Backlight Power Saving", category: "Backlight" },
  "pointer.sensor.{n}.dpi": { label: "Pointer Speed", category: "Pointer", unit: "DPI" },
  "wheel.mode": { label: "Wheel Mode", category: "Wheel" },
  "wheel.threshold": { label: "SmartShift", category: "Wheel" },
  "wheel.invert": { label: "Reverse Vertical Scrolling", category: "Wheel" },
  "thumbwheel.invert": { label: "Reverse Horizontal Scrolling", category: "Wheel" },
};

const SENSOR = /^pointer\.sensor\.([0-9]+)\.dpi$/;
const template = (key: string) => (SENSOR.test(key) ? "pointer.sensor.{n}.dpi" : key);

/** How the settings form shows `key`; null for keys this app doesn't know. */
export function settingInfo(key: string): SettingInfo | null {
  const info = SETTINGS[template(key)];
  if (!info) return null;
  const sensor = Number(SENSOR.exec(key)?.[1] ?? 0);
  return sensor === 0 ? info : { ...info, label: `Pointer Speed ${sensor + 1}` };
}

const ORDER = Object.keys(SETTINGS);
/** Display order; a second sensor follows the first. */
export const settingOrder = (key: string) => ORDER.indexOf(template(key)) + Number(SENSOR.exec(key)?.[1] ?? 0) / 100;

/** Capitalizes each word: "permanent manual" becomes "Permanent Manual". */
const titleCase = (s: string) => s.replace(/(^|\s)(\p{Ll})/gu, (_, space: string, c: string) => space + c.toUpperCase());

/** An enum token in words: the catalog's wording, else the token in title case. */
export function choiceText(key: string, token: string): string {
  return settingInfo(key)?.choices?.[token] ?? titleCase(token.replace(/[_-]/g, " "));
}

/** The wheel's capability readings as labelled figures. */
export function wheelFigures(info: InfoEntry[]): [string, string][] {
  const figures: [string, string, (v: Scalar) => string][] = [
    ["wheel.resolution_multiplier", "Resolution Multiplier", String],
    ["wheel.ratchets_per_rotation", "Ratchets per Rotation", String],
    ["wheel.diameter", "Wheel Diameter", (v) => `${String(v)} mm`],
  ];
  return figures.flatMap(([key, label, text]) => {
    const v = infoOf(info, key);
    return v === undefined ? [] : [[label, text(v)] as [string, string]];
  });
}

export function securityFacts(s: Security): [string, string][] {
  const flag = (v: boolean | null) => (v == null ? "Not Reported" : v ? "Yes" : "No");
  return [
    ["Encrypted", flag(s.encrypted)],
    ["Authenticated Pairing", flag(s.authenticated)],
    ["Secure Connections", flag(s.secureConnections)],
    ["Encryption Key", s.keySize == null ? "Not Reported" : `${s.keySize * 8}-bit`],
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

/** The device's state in a few words, as lists show it. */
export function deviceStatus(d: DeviceRecord): string {
  if (d.blocked) return "Blocked";
  switch (d.state) {
    case "connected":
      return "Connected";
    case "connecting":
      return "Connecting…";
    case "disconnecting":
      return "Disconnecting…";
    default:
      return d.inactive === null ? "Disconnected" : d.enabled ? "Inactive" : "Disabled";
  }
}

/** The adapter's state in a few words, and whether it needs attention. */
export function adapterStatus(a: AdapterEntry): { text: string; warn: boolean } {
  if (a.connection === "disconnected") return { text: "Disconnected", warn: false };
  if (a.connection === "connecting") return { text: "Connecting…", warn: false };
  if (a.readiness === "waiting") return { text: "Starting…", warn: false };
  return a.attention.length ? { text: "Needs Attention", warn: true } : { text: "Ready", warn: false };
}

/** Removes control and format characters (including bidirectional
 * overrides) from untrusted text before display. */
export const clean = (s: string) =>
  s
    .replace(/[\p{Cc}\u2028\u2029]/gu, " ")
    .replace(/\p{Cf}/gu, "")
    .trim();
