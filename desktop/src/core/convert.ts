// Converts protocol messages into the app's plain state, and app values back
// into protocol values. Unknown enum values read as the enum's zero value,
// unknown oneof cases as unset, and settings of a type this app doesn't know
// are left out.
import { create } from "@bufbuild/protobuf";
import {
  CapacityReason,
  CodeKind,
  DeviceState,
  ErrorCode,
  InactiveReason,
  IntegrationKind,
  IntegrationState,
  Kind,
  Platform,
  ReportType,
  Role,
  SettingState,
  Transport,
  ValueSchema,
  WarningCode,
  type Candidate as WireCandidate,
  type Device,
  type DeviceWarning as WireWarning,
  type Info,
  type Pairing,
  type Setting as WireSetting,
  type Status,
  type Value,
} from "@cordial/protocol";
import type { CordialError } from "@cordial/client";
import type {
  AdapterStatus,
  Candidate,
  DeviceRecord,
  DeviceWarning,
  InfoEntry,
  Integration,
  PairingPrompt,
  Scalar,
  Setting,
  ValueType,
  WireError,
} from "../shared/state.ts";

/** The lower-case name of an enum value; a value this app doesn't know reads as zero. */
function name<E extends Record<string, string | number>>(e: E, value: number): Lowercase<Extract<keyof E, string>> {
  const key = (e as Record<number, string>)[value] ?? (e as Record<number, string>)[0]!;
  return key.toLowerCase() as Lowercase<Extract<keyof E, string>>;
}

/** The enum value of a lower-case name. */
export function wire<E extends Record<string, string | number>>(e: E, value: Lowercase<Extract<keyof E, string>>): E[keyof E] {
  return e[value.toUpperCase() as keyof E];
}

export const transport = (t: Transport) => (t === Transport.CLASSIC ? "classic" : t === Transport.BLE ? "ble" : null);
const optional = <T>(value: T | undefined): T | null => (value === undefined ? null : value);
/** Device values fit in a double; larger ones only lose precision. */
const number = (n: bigint) => Number(n);

export const errorCode = (code: ErrorCode) => name(ErrorCode, code);

export function wireError(error: CordialError): WireError {
  return {
    code: errorCode(error.code),
    reason: error.code === ErrorCode.NO_CAPACITY ? name(CapacityReason, error.reason) : null,
    outcomeUnknown: error.outcomeUnknown,
  };
}

function scalar(value: Value | undefined): { type: InfoEntry["type"]; value: Scalar } | null {
  const v = value?.value;
  switch (v?.case) {
    case "bool":
      return { type: "bool", value: v.value };
    case "integer":
      return { type: "integer", value: number(v.value) };
    case "text":
      return { type: "text", value: v.value };
    case "color":
      return { type: "color", value: v.value };
    default:
      return null;
  }
}

export function info(list: Info[]): InfoEntry[] {
  const seen = new Set<string>();
  return list.flatMap((i) => {
    const v = scalar(i.value);
    if (!v || seen.has(i.key)) return [];
    seen.add(i.key);
    return [{ key: i.key, ...v }];
  });
}

export function status(s: Status): AdapterStatus {
  return {
    id: s.id,
    name: s.name,
    platform: name(Platform, s.platform),
    ready: s.ready,
    transports: s.transports.flatMap((t) => {
      const kind = transport(t.transport);
      // Firmware that predates the setting doesn't report it, and uses every transport it supports.
      return kind ? [{ transport: kind, maxEnabled: optional(t.maxEnabled), enabled: t.enabled !== false, settable: t.enabled !== undefined }] : [];
    }),
    info: info(s.info),
  };
}

function hidpp(d: Device): Integration | null {
  const i = d.integrations.find((x) => x.kind === IntegrationKind.HIDPP);
  if (!i) return null;
  const version = i.detected?.version;
  const s = i.status;
  return {
    kind: i.kind,
    enabled: i.enabled,
    version: version ? { major: version.major, minor: version.minor } : null,
    // A status this app doesn't know reads as off.
    state: s.case === "error" ? null : name(IntegrationState, s.case === "state" ? s.value : IntegrationState.OFF),
    error: s.case === "error" ? errorCode(s.value) : null,
  };
}

export function device(d: Device): DeviceRecord {
  return {
    id: d.id,
    transport: transport(d.transport),
    name: d.name,
    kind: name(Kind, d.kind),
    state: name(DeviceState, d.state),
    enabled: d.enabled,
    trusted: d.trusted,
    blocked: d.blocked,
    paused: d.paused,
    inactive: d.inactive === undefined ? null : name(InactiveReason, d.inactive),
    error: d.error === undefined ? null : errorCode(d.error),
    security: d.security
      ? {
          encrypted: optional(d.security.encrypted),
          authenticated: optional(d.security.authenticated),
          secureConnections: optional(d.security.secureConnections),
          keySize: optional(d.security.keySize),
        }
      : null,
    hidpp: hidpp(d),
    info: info(d.info),
    roles: [...new Set(d.roles.filter((r) => r in Role && r !== Role.UNKNOWN).map((r) => name(Role, r)))] as DeviceRecord["roles"],
  };
}

export function warning(w: WireWarning): DeviceWarning {
  return {
    code: name(WarningCode, w.code),
    service: w.service,
    reportType: w.reportType === ReportType.UNKNOWN || !(w.reportType in ReportType) ? null : (name(ReportType, w.reportType) as DeviceWarning["reportType"]),
    reportId: optional(w.reportId),
    bitOffset: optional(w.bitOffset),
    usagePage: optional(w.usagePage),
    usage: optional(w.usage),
  };
}

export function setting(s: WireSetting): Setting | null {
  const status = s.status;
  const base = {
    integration: s.integration,
    key: s.key,
    state: status.case === "state" ? name(SettingState, status.value) : null,
    error: status.case === "error" ? errorCode(status.value) : null,
    choices: [] as Scalar[],
    min: null,
    max: null,
    step: null,
    maxBytes: null,
  };
  const t = s.type;
  const of = (type: ValueType, value: Scalar | undefined, saved: Scalar | undefined) => ({ ...base, type, value: optional(value), saved: optional(saved) });
  switch (t.case) {
    case "bool":
      return of("bool", t.value.value, t.value.saved);
    case "integer": {
      const v = t.value;
      const row: Setting = of("integer", v.value === undefined ? undefined : number(v.value), v.saved === undefined ? undefined : number(v.saved));
      if (v.limits.case === "range") {
        row.min = number(v.limits.value.min);
        row.max = number(v.limits.value.max);
        row.step = Math.max(1, number(v.limits.value.step));
      } else if (v.limits.case === "choices") row.choices = v.limits.value.values.map(number);
      return row;
    }
    case "enum":
      return { ...of("enum", t.value.value, t.value.saved), choices: [...t.value.choices] };
    case "text":
      return { ...of("text", t.value.value, t.value.saved), maxBytes: optional(t.value.maxBytes) };
    case "color":
      return of("color", t.value.value, t.value.saved);
    default:
      return null;
  }
}

export const settings = (list: WireSetting[]) => list.flatMap((s) => setting(s) ?? []);

export function candidate(c: WireCandidate): Candidate {
  return { id: c.id, transport: transport(c.transport), name: c.name, kind: name(Kind, c.kind), rssi: optional(c.rssi) };
}

/** A pairing step: a prompt for the user, the saved device, or the failure. */
export type PairingStep =
  | { kind: "progress" }
  | { kind: "prompt"; prompt: PairingPrompt }
  | { kind: "done"; device: string }
  | { kind: "failed"; code: ReturnType<typeof errorCode> };

export function pairingStep(p: Pairing): PairingStep {
  const step = p.step;
  switch (step.case) {
    case "enterCode":
      return { kind: "prompt", prompt: { kind: "enter", code: name(CodeKind, step.value.kind) } };
    case "confirmCode":
      return { kind: "prompt", prompt: { kind: "confirm", value: step.value.passkey } };
    case "showCode":
      return { kind: "prompt", prompt: { kind: "show", code: name(CodeKind, step.value.kind), value: step.value.value } };
    case "done":
      return { kind: "done", device: step.value.device };
    case "failed":
      return { kind: "failed", code: errorCode(step.value) };
    default:
      // Connecting, or a step this app doesn't know: still in progress.
      return { kind: "progress" };
  }
}

/** A setting value as the protocol carries it for a setting of `type`. */
export function value(type: ValueType, v: Scalar) {
  switch (type) {
    case "bool":
      return create(ValueSchema, { value: { case: "bool", value: v as boolean } });
    case "integer":
      return create(ValueSchema, { value: { case: "integer", value: BigInt(v as number) } });
    case "color":
      return create(ValueSchema, { value: { case: "color", value: v as number } });
    default:
      return create(ValueSchema, { value: { case: "text", value: String(v) } });
  }
}
