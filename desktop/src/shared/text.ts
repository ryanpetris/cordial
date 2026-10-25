// The host's words for adapter codes and records. The adapter sends machine
// codes and typed fields only. Records keyed by the protocol's enums and
// setting fields are exhaustive, so a protocol change fails type checking until
// its wording is added here.
import type {
  AdapterEntry,
  AdapterStatus,
  Battery,
  Code,
  DeviceKind,
  DeviceInformation,
  DeviceEntry,
  DeviceRecord,
  HostPlatform,
  Inactive,
  IntegrationStateName,
  Integration,
  KindName,
  OutputModeName,
  OutputStateName,
  ProfileAlert,
  Resource,
  RoleName,
  Scalar,
  Security,
  SettingField,
  SettingFields,
  SettingStateName,
  SettingsGroupName,
  TransportName,
  WireError,
  ErrorOutcomeName,
  ErrorReasonName,
} from "./state.ts";

/** A clause as a complete sentence: capitalized, with a final period. */
const sentence = (s: string) => `${s.charAt(0).toUpperCase()}${s.slice(1)}.`;

/** A message as a reason inserted into other text, such as "Failed: {reason}", without its final
 * period. */
export const reasonText = (s: string) => (s.endsWith(".") ? s.slice(0, -1) : s);

const ERRORS: Record<Code, string> = {
  unknown: "the adapter hit an unexpected failure",
  bad_request: "the adapter couldn't understand the request from Cordial",
  unknown_command: "this adapter's firmware doesn't support that action. Update the firmware to use it",
  bad_args: "the adapter couldn't accept those values",
  too_long: "the request was too large for the adapter",
  not_ready: "the adapter's Bluetooth or storage isn't ready. Try again in a moment",
  not_found: "the adapter couldn't find this device, setting or profile",
  not_connected: "the device isn't connected. Connect it first",
  busy: "the adapter is busy. Try again when the current operation finishes",
  disabled: "this device is turned off in Cordial. Turn on “Use This Device” first",
  blocked: "connections to this device are blocked. Unblock it before connecting",
  unsupported: "the adapter or device doesn't support this action",
  no_capacity: "the adapter has no room for that right now",
  no_prompt: "that pairing prompt is no longer waiting for an answer",
  storage_failed: "the adapter couldn't save the change",
  internal: "the adapter hit an unexpected failure",
  in_use: "this profile is still in use. Remove it from every device and interface before deleting it",
  candidate_expired: "this device is no longer available. Search again",
  auth_failed: "Bluetooth authentication with this device failed",
  rejected: "the pairing was rejected on this computer or on the device",
  timeout: "this device didn't respond in time",
  cancelled: "the operation was cancelled",
  connection_failed: "the adapter couldn't connect to this device",
  unsupported_hid: "the adapter can't use this device's input format",
  protocol_unsupported: "this device doesn't support this feature",
  feature_unavailable: "this device doesn't have a feature this action needs",
  transport_error: "the adapter couldn't exchange messages with this device",
  device_error: "this device reported an error",
  invalid_response: "the adapter couldn't understand this device's reply",
  readback_mismatch: "the device reported a different value from the one requested",
  storage_full: "the adapter's storage is full",
  profile_memory_low: "the adapter is almost out of profile memory",
  usb_required_capacity_exceeded: "the adapter doesn't have enough USB capacity for every required interface",
  usb_reserved_capacity_exceeded: "the adapter doesn't have enough USB capacity for every reserved interface",
  profile_memory_exceeded: "the adapter doesn't have room for this device's profiles",
  profile_load_failed: "the adapter couldn't read one of this device's profiles",
};

/** Wording for a code whose reason changes what failed. */
const SPECIFIC: Partial<Record<`${Code}:${ErrorReasonName}`, string>> = {
  "no_capacity:enabled_devices": "every enabled-device place is in use. Turn off another device first",
  "no_capacity:storage": "the adapter's storage is full. Remove an unused device or forget a saved setting, then try again",
  "no_capacity:connections": "every connection is in use. Disconnect a device first",
  "no_capacity:profile_memory": "the adapter doesn't have enough profile memory for this change",
  "no_capacity:scan_candidates": "the adapter had no room to list every device it found",
  "no_capacity:output_backend": "the adapter doesn't have enough USB capacity for this output",
  "not_ready:radio": "the adapter's Bluetooth radio isn't ready. Try again in a moment",
  "not_ready:storage": "the adapter's storage isn't ready. Try again in a moment",
  "unsupported:output_backend": "the adapter doesn't support this output",
  "unsupported:input_layout": "the adapter can't use this device's input layout for this output",
  "internal:scan_candidates": "the adapter couldn't keep track of every device it found",
};

/** What a reason concerns, for a code whose wording doesn't already say. */
const CONTEXT: Record<ErrorReasonName, string | null> = {
  unknown: null,
  enabled_devices: "enabled devices",
  connections: "the adapter's connections",
  scan_candidates: "the devices found while searching",
  storage: "the adapter's storage",
  profile_memory: "profile memory",
  radio: "the adapter's Bluetooth radio",
  output_backend: "the adapter's USB output",
  input_layout: "this device's input layout",
};

/** The reason each code's wording already names. */
const IMPLIED: Partial<Record<Code, ErrorReasonName>> = {
  storage_failed: "storage",
  storage_full: "storage",
  profile_memory_low: "profile_memory",
  profile_memory_exceeded: "profile_memory",
  usb_required_capacity_exceeded: "output_backend",
  usb_reserved_capacity_exceeded: "output_backend",
};

/** A failed read of the adapter's saved data, where the save wording doesn't apply. */
export const READ_FAILED = "The adapter couldn't read its saved data. Try again.";

/** What an error's outcome is about: a change saved on the adapter, a change applied to the
 * device, a read, a pairing's saved device, the cleanup after a failed pairing, or another
 * operation, as for related failures with no more specific scope. */
export type Scope = "save" | "apply" | "read" | "pair" | "cleanup" | "other";

const OUTCOMES: Record<Scope, Partial<Record<ErrorOutcomeName, string>>> = {
  save: {
    unknown: "The adapter couldn't confirm whether the change was saved. Check before trying again.",
    partial: "The adapter confirmed saving only part of the change.",
    complete: "The adapter saved the change despite this.",
  },
  apply: {
    unknown: "The adapter couldn't confirm whether the change took effect.",
    partial: "Only part of the change is confirmed to have taken effect.",
    complete: "The change took effect despite this.",
  },
  read: {
    unknown: "The adapter couldn't confirm whether the results are complete.",
    partial: "The adapter returned only some of the results.",
    complete: "The results are complete despite this.",
  },
  pair: {
    unknown: "The adapter couldn't confirm whether it saved the device. Check its saved devices before pairing again.",
    partial: "The adapter confirmed saving only part of the pairing.",
    complete: "The adapter saved the device despite this.",
  },
  cleanup: {
    unknown: "The adapter couldn't confirm whether it finished cleaning up.",
    partial: "The adapter finished only part of the cleanup.",
    complete: "The adapter finished cleaning up despite this.",
  },
  other: {
    unknown: "Its outcome is unknown.",
    partial: "It partly completed.",
    complete: "Its intended effects completed.",
  },
};

/** The scope of the related failures of an error in `scope`. */
const RELATED: Record<Scope, Scope> = { save: "other", apply: "other", read: "read", pair: "cleanup", cleanup: "other", other: "other" };

/** A local error's text as a complete sentence. */
export const asSentence = (s: string) => {
  const text = s.trim();
  if (!text) return text;
  const capital = text.charAt(0).toUpperCase() + text.slice(1);
  return /[.!?]$/.test(capital) ? capital : `${capital}.`;
};

/** Whether an outcome is evidence: unknown, partial or complete. A value this app doesn't know is
 * none. */
const evidence = (e: WireError) => e.outcome === "unknown" || e.outcome === "partial" || e.outcome === "complete";

/** Whether an error reports nothing beyond its code and reason, so a short wording of it loses
 * nothing. */
export const plainError = (e: WireError) => !evidence(e) && !e.related.length;

/** What failed, as a clause, with what its reason concerns. */
function failureClause(e: WireError, scope: Scope): string {
  if (typeof e.code === "number") return ERRORS.unknown;
  const reason = typeof e.reason === "number" ? "unknown" : e.reason;
  const specific = SPECIFIC[`${e.code}:${reason}`];
  if (specific) return specific;
  let clause = ERRORS[e.code];
  if (e.code === "storage_failed") {
    // Only an outcome of no effect confirms that nothing was saved.
    clause = scope === "read" ? reasonText(READ_FAILED)
      : scope === "save" && e.outcome === "no_effect" ? clause
      : scope === "pair" && e.outcome === "no_effect" ? "the adapter couldn't save the device"
      : "the adapter couldn't read or write its saved data";
  }
  const context = IMPLIED[e.code] === reason ? null : CONTEXT[reason];
  return context ? `${clause}. This involves ${context}` : clause;
}

/** An adapter error as sentences for the window or a notification: what failed, what is known of
 * its effects, then each related failure with its own outcome. */
export function errorText(e: WireError, scope: Scope = "save", also = false): string {
  const clause = failureClause(e, scope);
  const outcome = typeof e.outcome === "number" ? undefined : OUTCOMES[scope][e.outcome];
  return [also ? `Also, ${clause}.` : sentence(clause), ...(outcome ? [outcome] : []), ...e.related.map((r) => errorText(r, RELATED[scope], true))].join(" ");
}

/** Whether an error is a plain refusal the short "Unsupported" wording covers. */
export const unsupportedError = (e: WireError | null) => e?.code === "unsupported" && plainError(e);
/** Whether an integration's error plainly says the device can't use it at all. */
export const integrationUnsupported = (e: WireError | null) => !!e && (e.code === "unsupported" || e.code === "protocol_unsupported") && plainError(e);

export const hasError = (list: WireError[] | undefined, code: Code) => !!list?.some((e) => e.code === code);

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

/** Whether HID++ is up on the device, in a few words. */
export const INTEGRATION_STATES: Record<IntegrationStateName, string> = {
  off: "Off",
  disconnected: "Waiting to Connect",
  starting: "Setting Up",
  active: "Active",
};

/** A status, or a failure as "Unsupported" or "Failed: {reason}". */
export const stateText = (state: string | null, error: WireError | null, unsupported = unsupportedError(error)) =>
  state ?? (unsupported ? "Unsupported" : `Failed: ${reasonText(error ? errorText(error, "apply") : codeText("unknown"))}`);

export const integrationText = (i: Integration) => stateText(i.state && INTEGRATION_STATES[i.state], i.error, integrationUnsupported(i.error));

/** The detected HID++ version, as "4.5". */
export const versionText = (i: Integration) => (i.version ? `${i.version.major}.${i.version.minor}` : "Unknown");

/** A saved setting's state as its marker names it; `unmanaged` is a setting with no saved value. */
export const SETTING_STATES: Record<SettingStateName | "unmanaged" | "unsupported" | "error", string> = {
  unmanaged: "Not Saved",
  unknown: "Saved",
  pending: "Saved",
  applied: "Saved",
  changed_on_device: "Changed on Device",
  unsupported: "Can't Apply Now",
  error: "Failed",
};

export const OUTPUT_MODES: Record<OutputModeName, string> = { disabled: "Disabled", hid: "HID", xinput: "XInput" };
export const OUTPUT_STATES: Record<OutputStateName, string> = {
  unknown: "Unknown",
  disabled: "Disabled",
  disconnected: "Waiting to Connect",
  pending: "Setting Up",
  active: "Active",
};

export const PLATFORMS: Record<HostPlatform, string> = { linux: "Linux", windows: "Windows", mac: "macOS" };
export const TRANSPORTS: Record<TransportName, string> = { ble: "Bluetooth LE", classic: "Bluetooth Classic" };
export const ROLES: Record<RoleName, string> = { keyboard: "Keyboard", mouse: "Mouse", consumer_control: "Media Keys", system_control: "System Keys", gamepad: "Gamepad" };

const hex = (n: number) => `0x${n.toString(16).toUpperCase().padStart(4, "0")}`;
const known = (facts: [string, string | null][]) => facts.filter((f): f is [string, string] => f[1] !== null);

/** The device's identity facts the Information card shows, in display order; unknown ones are left
 * out. */
export function identityFacts(i: DeviceInformation): [string, string][] {
  const id = i.identity;
  return known([
    ["Manufacturer", id.manufacturer],
    ["Model", id.model],
    ["Serial Number", id.serial],
    ["Firmware", id.firmwareVersion],
    ["Hardware", id.hardwareRevision],
    ["Software", id.softwareRevision],
  ]);
}

const REGISTRIES = { unknown: "Unknown", usb: "USB", bluetooth: "Bluetooth" } as const;

/** The device's identifiers Diagnostics shows, in display order; unknown ones are left out. */
export function identifierFacts(i: DeviceInformation): [string, string][] {
  const id = i.identity;
  return known([
    ["Vendor ID Namespace", id.vendor.registry && REGISTRIES[id.vendor.registry]],
    ["Vendor ID", id.vendor.id === null ? null : hex(id.vendor.id)],
    ["Product ID", id.product.id === null ? null : hex(id.product.id)],
    ["Product Version", id.product.version === null ? null : hex(id.product.version)],
    ["Bootloader", id.bootloaderVersion],
  ]);
}

const KINDS: Record<DeviceKind, string> = {
  keyboard: "Keyboard",
  mouse: "Mouse",
  keyboard_mouse: "Keyboard and Mouse",
  gamepad: "Gamepad",
  other: "Other",
};

export const kindText = (kind: DeviceKind) => KINDS[kind];

/** What the app shows a device or candidate as, from what it says it is, else from the input its
 * HID descriptor produces. */
export function displayKind(kinds: KindName[], roles: RoleName[] = []): DeviceKind {
  const keyboard = kinds.includes("keyboard") || (!kinds.length && roles.includes("keyboard"));
  const mouse = kinds.includes("mouse") || (!kinds.length && roles.includes("mouse"));
  const gamepad = kinds.includes("gamepad") || (!kinds.length && roles.includes("gamepad"));
  return keyboard && mouse ? "keyboard_mouse" : keyboard ? "keyboard" : mouse ? "mouse" : gamepad ? "gamepad" : "other";
}

/** Device errors that say why a connected device's profiles aren't loaded. */
const PROFILE_ERRORS: (Code | number)[] = ["profile_memory_exceeded", "profile_load_failed"];

/** Why a connected device's profiles aren't loaded, if they aren't. */
export const profileError = (d: DeviceRecord): WireError | null =>
  d.state === "connected" ? (d.errors.find((e) => PROFILE_ERRORS.includes(e.code)) ?? null) : null;

/** The device's last failed connection attempt. */
export const connectionError = (d: DeviceRecord): WireError | null => d.errors.find((e) => !PROFILE_ERRORS.includes(e.code)) ?? null;

/** Why a connected device's profiles aren't loaded. The short wording of a lack of room is for a
 * plain error whose reason, if any, is profile memory. */
export function profileErrorText(e: WireError): string {
  const room = e.code === "profile_memory_exceeded" && plainError(e) && (e.reason === "unknown" || e.reason === "profile_memory");
  return room ? "The adapter doesn't have room for them. Disconnect another device or give this one fewer profiles." : errorText(e, "apply");
}

/** A profile alert's notification; a memory alert has no body. */
export function profileAlertText(alert: ProfileAlert): { title: string; body: string | null } {
  return alert.kind === "memory"
    ? { title: `${alert.name} is almost out of profile memory.`, body: null }
    : { title: `${alert.name} connected without its profiles.`, body: profileErrorText(alert.error) };
}

/** How full the adapter's profile memory is, as a whole percentage; null while either value is
 * unknown or without a budget. */
export function memoryPercent(s: AdapterStatus | null): number | null {
  const m = s?.profileSupport?.resources.find((r) => r.type === "memory");
  return m?.count != null && m.limit ? Math.round((m.count / m.limit) * 100) : null;
}

/** A resource's usage in a few words: "3 of 7", or what is known of it. */
export function capacityText(r: Resource): string {
  if (r.count !== null && r.limit !== null) return `${r.count} of ${r.limit}`;
  if (r.count !== null) return String(r.count);
  return r.limit !== null ? `Up to ${r.limit}` : "Unknown";
}

/** Status labels for the adapter's USB capacity errors, in the order shown. */
export const USB_CAPACITY: [Code, string][] = [
  ["usb_required_capacity_exceeded", "Required Capacity Exceeded"],
  ["usb_reserved_capacity_exceeded", "Reserved Capacity Exceeded"],
];

/** Why a nearby device can't be added: the adapter has no room to save it. */
export const STORAGE_FULL = "Storage Full";

export const storageFull = (s: AdapterStatus | null) => hasError(s?.errors, "storage_full");

export interface SettingInfo {
  label: string;
  category: string;
  unit?: string;
  choices?: Record<string, string>;
}

/** A setting field or a reading shown with the settings, by group and protocol field name. */
export type FieldName =
  | { [G in SettingsGroupName]: `${G}.${SettingFields[G]}` }[SettingsGroupName]
  | "backlight.current_brightness"
  | "backlight.status";

/** A field's name as the settings tables list it. */
export const fieldName = (f: Pick<SettingField, "group" | "field">) => `${f.group}.${f.field}` as FieldName;

/** Settings and setting-like readings in display order; categories follow first use. */
const SETTINGS: Record<FieldName, SettingInfo> = {
  "keyboard.function_row": {
    label: "Function Row",
    category: "Keyboard",
    choices: { function_keys: "F1-F12", special_actions: "Shortcuts" },
  },
  "keyboard.platform": {
    label: "Keyboard Platform",
    category: "Keyboard",
    choices: {
      windows: "Windows",
      windows_embedded: "Windows Embedded",
      linux: "Linux",
      chrome_os: "ChromeOS",
      android: "Android",
      mac: "macOS",
      ios: "iOS",
      webos: "webOS",
      tizen: "Tizen",
    },
  },
  "backlight.enabled": { label: "Backlight", category: "Backlight" },
  "backlight.mode": { label: "Backlight Mode", category: "Backlight" },
  "backlight.brightness": { label: "Brightness", category: "Backlight" },
  "backlight.current_brightness": { label: "Current Brightness", category: "Backlight" },
  "backlight.status": {
    label: "Backlight Status",
    category: "Backlight",
    choices: { battery: "Off (Battery)", saturated: "Automatic (Saturated)" },
  },
  "backlight.effect": { label: "Backlight Effect", category: "Backlight" },
  "backlight.hands_away": { label: "Hands Away Timeout", category: "Backlight", unit: "s" },
  "backlight.hands_nearby": { label: "Hands Nearby Timeout", category: "Backlight", unit: "s" },
  "backlight.external_power": { label: "External Power Timeout", category: "Backlight", unit: "s" },
  "backlight.startup_effect": { label: "Startup Effect", category: "Backlight" },
  "backlight.dial_effect": { label: "Dial Lighting Effect", category: "Backlight" },
  "backlight.power_save": { label: "Backlight Power Saving", category: "Backlight" },
  "pointer_sensor.dpi": { label: "Pointer Sensitivity", category: "Pointer", unit: "DPI" },
  "wheel.mode": { label: "Wheel Mode", category: "Wheel", choices: { free_spin: "Free Spin", stepped: "Stepped" } },
  "wheel.free_spin_threshold": { label: "Automatic Free Spin Threshold", category: "Wheel" },
  "wheel.invert": { label: "Reverse Vertical Scrolling", category: "Wheel" },
  "thumbwheel.invert": { label: "Reverse Horizontal Scrolling", category: "Wheel" },
  "power.auto_off": { label: "Automatic Power-Off", category: "Power", unit: "s" },
};

/** A field with the component it belongs to: the pointer sensor's index, else null. */
export type FieldRef = { name: FieldName; index: number | null };

/** How the settings form shows a field; a second pointer sensor is numbered. */
export function settingInfo({ name, index }: FieldRef): SettingInfo {
  const info = SETTINGS[name];
  return index ? { ...info, label: `${info.label} ${index + 1}` } : info;
}

/** Whether a displayed value reads "Never", which takes no unit. */
export const neverValue = (name: FieldName, v: Scalar | null) => name === "power.auto_off" && v === 0;

/** The unit beside a displayed value; none when the value reads "Never". */
export const displayUnit = (name: FieldName, v: Scalar | null) => (neverValue(name, v) ? undefined : SETTINGS[name].unit);

const ORDER = Object.keys(SETTINGS);
/** Display order; a second sensor follows the first. */
export const settingOrder = ({ name, index }: FieldRef) => ORDER.indexOf(name) + (index ?? 0) / 100;

/** Capitalizes each word: "permanent manual" becomes "Permanent Manual". */
const titleCase = (s: string) => s.replace(/(^|\s)(\p{Ll})/gu, (_, space: string, c: string) => space + c.toUpperCase());

/** An enum value in words: the table's wording, else the protocol name in title case. */
export function choiceText(name: FieldName, token: string): string {
  return SETTINGS[name].choices?.[token] ?? titleCase(token.replace(/[_-]/g, " "));
}

/** A reading the device reports for a field, shown with the settings. */
export type Reading = FieldRef & { value: Scalar };

/** The device's setting-like readings, each one it reports. */
export function readings(i: DeviceInformation): Reading[] {
  const b = i.backlight;
  const list: [FieldName, Scalar | null][] = [
    ["keyboard.platform", i.keyboard.platform],
    ["keyboard.function_row", i.keyboard.functionRow],
    ["power.auto_off", i.power.autoOff],
    ["backlight.current_brightness", b.currentBrightness],
    ["backlight.status", b.status],
    ["backlight.enabled", b.enabled],
    ["backlight.mode", b.mode],
    ["backlight.brightness", b.brightness],
    ["backlight.hands_away", b.delay.handsAway],
    ["backlight.hands_nearby", b.delay.handsNearby],
    ["backlight.external_power", b.delay.externalPower],
    ["backlight.startup_effect", b.startupEffect],
    ["backlight.dial_effect", b.dialEffect],
    ["backlight.power_save", b.powerSave],
    ["backlight.effect", b.effect],
    ["wheel.mode", i.wheel.mode],
    ["wheel.free_spin_threshold", i.wheel.freeSpinThreshold],
    ["wheel.invert", i.wheel.invert],
    ["thumbwheel.invert", i.thumbwheel.invert],
  ];
  return [
    ...list.flatMap(([name, value]) => (value === null ? [] : [{ name, index: null, value }])),
    ...i.pointerSensors.flatMap((p) => (p.dpi === null ? [] : [{ name: "pointer_sensor.dpi" as const, index: p.index, value: p.dpi }])),
  ];
}

/** The wheel's capability readings as labelled figures. */
export function wheelFigures(i: DeviceInformation): [string, string][] {
  const w = i.wheel;
  return known([
    ["Resolution Multiplier", w.resolutionMultiplier === null ? null : String(w.resolutionMultiplier)],
    ["Ratchets per Rotation", w.ratchetsPerRotation === null ? null : String(w.ratchetsPerRotation)],
    ["Wheel Diameter", w.diameter === null ? null : `${w.diameter} mm`],
  ]);
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

/** Whether the device is connected as far as the adapter last confirmed; a record the latest
 * listing couldn't read, or one its adapter hasn't listed again since reconnecting, confirms
 * nothing. */
export const connected = (e: Pick<DeviceEntry, "device" | "readError" | "available">) =>
  e.available && !e.readError && e.device.state === "connected";

/** The device's state in a few words, as lists show it. */
export function deviceStatus({ device: d, readError, available }: Pick<DeviceEntry, "device" | "readError" | "available">): string {
  if (!available) return "Disconnected";
  if (readError) return "Couldn't Read";
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

/** Adapter errors shown in their own places, which say nothing of readiness. */
const ADVISORY: (Code | number)[] = ["storage_full", "profile_memory_low", "usb_required_capacity_exceeded", "usb_reserved_capacity_exceeded"];

/** The adapter's radio, storage and other faults, which a starting adapter doesn't report. */
export const adapterFaults = (s: AdapterStatus | null) => s?.errors.filter((e) => !ADVISORY.includes(e.code)) ?? [];

/** The adapter's state in a few words, and whether it needs attention. */
export function adapterStatus(a: AdapterEntry): { text: string; warn: boolean } {
  if (a.connection === "disconnected" || a.connection === "reconnecting") return { text: "Disconnected", warn: false };
  if (a.connection === "connecting") return { text: "Connecting…", warn: false };
  if (a.readiness === "waiting") return adapterFaults(a.status).length || a.listError ? { text: "Needs Attention", warn: true } : { text: "Starting…", warn: false };
  return a.attention.length ? { text: "Needs Attention", warn: true } : { text: "Ready", warn: false };
}

/** Removes control and format characters (including bidirectional
 * overrides) from untrusted text before display. */
export const clean = (s: string) =>
  s
    .replace(/[\p{Cc}\u2028\u2029]/gu, " ")
    .replace(/\p{Cf}/gu, "")
    .trim();
