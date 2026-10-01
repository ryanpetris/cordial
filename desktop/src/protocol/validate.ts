// Validates messages against the checked-in wire schema. Responses are
// checked against the definition of the command that started them.
import type { CommandName, Device, DeviceInfo, Limits, Setting, SettingKey, SettingType } from "./types.ts";
import { validator } from "./validators.ts";

function check(definition: string, message: unknown): string | null {
  const validate = validator(definition);
  if (validate(message)) return null;
  const first = validate.errors?.[0];
  return first ? `${first.instancePath || "/"} ${first.message ?? "is invalid"}` : "invalid";
}

const KINDS: Record<SettingKey, SettingType> = {
  "fn.row_default": "enum", "backlight.enabled": "bool", "backlight.mode": "enum",
  "backlight.level": "integer", "backlight.delay.hands_out": "integer",
  "backlight.delay.hands_in": "integer", "backlight.delay.powered": "integer",
  "pointer.dpi.0": "integer", "pointer.dpi.1": "integer", "wheel.mode": "enum",
  "wheel.threshold": "integer", "wheel.invert": "bool", "thumbwheel.invert": "bool",
  "backlight.power_on": "bool", "backlight.crown": "bool", "backlight.power_save": "bool",
  "backlight.effect": "enum", "backlight.current_level": "integer", "backlight.status": "enum", "wheel.info": "text",
};
const bytes = (s: string) => new TextEncoder().encode(s).length;

function settingProblem(s: Setting, limits?: Limits): string | null {
  const typed = (v: unknown) => s.type === "bool" ? typeof v === "boolean"
    : s.type === "integer" ? Number.isSafeInteger(v) : typeof v === "string" && bytes(v) <= 128;
  if (s.type !== KINDS[s.key] || s.choices.length > (limits?.hidpp_setting_choices ?? 65536)
    || new Set(s.choices).size !== s.choices.length || s.choices.some((v) => !typed(v))
    || (!s.writable && (s.managed || s.desired !== null || s.state !== "unmanaged"))
    || (!s.managed && s.desired !== null) || (s.managed && (s.desired === null || s.state === "unmanaged"))
    || (s.desired !== null && !typed(s.desired)) || (s.observed !== null && !typed(s.observed))) return "invalid setting record";
  if (s.type === "integer") {
    if ((s.min !== null && s.max !== null && s.min > s.max) || (s.step !== null && s.step < 1)) return "invalid setting range";
  } else if (s.min !== null || s.max !== null || s.step !== null
    || (s.type !== "enum" && s.choices.length > 0) || (s.type === "enum" && s.writable && !s.choices.length)) return "invalid setting choices";
  return null;
}

function deviceProblem(d: Device): string | null {
  const protocol = d.hidpp_protocol;
  if (protocol?.state === "error" && !["hidpp_timeout", "hidpp_transport_error", "hidpp_device_error", "hidpp_invalid_response"].includes(protocol.code)) {
    return "invalid HID++ protocol state";
  }
  const paired = d.pairing_state === "paired";
  const needsPairing = d.validation_error === "bond_missing" || d.validation_error === "bond_corrupt" || d.validation_error === "bond_mismatch";
  const reason = d.enabled_reason;
  const holds = reason === null ? d.enabled && d.transport_supported && !d.blocked && d.validation_error === null && paired
    : reason === "unsupported_transport" ? !d.transport_supported
    : reason === "invalid" ? d.validation_error !== null || !paired
    : reason === "blocked" ? d.blocked : reason === "disabled" ? !d.enabled : d.enabled;
  return d.effective_enabled !== (reason === null) || !holds || needsPairing === paired
    || (d.state === "connected" && !d.effective_enabled) || (d.name !== null && bytes(d.name) > 128)
    ? "invalid device enablement" : null;
}

/** Checks semantic invariants shared with the Rust client after schema validation. */
function payloadProblem(value: unknown, limits?: Limits): string | null {
  if (typeof value === "number") return Number.isSafeInteger(value) ? null : "integer exceeds the safe range";
  if (!value || typeof value !== "object") return null;
  if (Array.isArray(value)) {
    for (const item of value) { const p = payloadProblem(item, limits); if (p) return p; }
    return null;
  }
  const r = value as Record<string, unknown>;
  if (r.device_id != null && (typeof r.device_id !== "string" || !/^[!-~]{1,64}$/.test(r.device_id))) return "invalid device ID";
  if ("settings_revision" in r) { const p = deviceProblem(value as Device); if (p) return p; }
  if ("observed_at_ms" in r) { const p = settingProblem(value as Setting, limits); if (p) return p; }
  if ("fields" in r) {
    const info = value as DeviceInfo;
    if (new Set(info.fields.map((f) => `${f.key}/${f.instance}`)).size !== info.fields.length) return "duplicate information field";
    if (info.fields.some((f) => typeof f.value === "string" && (bytes(f.value) > 64 || /[\u0000-\u001f\u007f-\u009f]/u.test(f.value)))) return "invalid information text";
  }
  for (const item of Object.values(r)) { const p = payloadProblem(item, limits); if (p) return p; }
  return null;
}

/** Null when `message` is a valid response to `command`, else the problem. */
export function responseProblem(command: CommandName, message: unknown, limits?: Limits) {
  const problem = check(`${command}.response`, message);
  if (problem || command === "adapter.protocol") return problem;
  const m = message as { result?: unknown; error?: unknown };
  return payloadProblem(m.result ?? m.error, limits);
}
/** Null when `message` is a valid event, else the problem. */
export function eventProblem(message: unknown, limits?: Limits) {
  const problem = check("Event", message);
  if (problem) return problem;
  const m = message as { event: string; data: { revision?: number } };
  if (m.event !== "events.lost" && m.data.revision === 0) return "invalid event revision";
  return payloadProblem(m.data, limits);
}
/** Null when `message` is a valid request, else the problem. */
export const requestProblem = (command: CommandName, message: unknown) =>
  check(`${command}.request`, message);
