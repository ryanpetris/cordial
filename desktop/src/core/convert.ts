// Converts protocol messages into the app's plain state, and app values back
// into protocol values. Unknown enum values read as the enum's zero value,
// except in errors, which keep them as numbers. Unknown oneof cases read as
// unset, and settings of a group or field this app doesn't know are left out.
import { create, type MessageInitShape } from "@bufbuild/protobuf";
import {
  BacklightEffect,
  BacklightMode,
  BacklightStatus,
  CodeKind,
  DeviceState,
  ErrorCode,
  ErrorOutcome,
  ErrorReason,
  ErrorSchema,
  FunctionRowMode,
  InactiveReason,
  IntegrationKind,
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
  type Candidate as WireCandidate,
  type Device,
  type DeviceInformation as WireDeviceInformation,
  type Error as ProtocolError,
  type Pairing,
  type RecordError,
  type Profile as WireProfile,
  type ResourceCapacity,
  type SettingsObject,
  type SettingsRef,
  type SettingsUpdate,
  type SettingsUpdateSchema,
  type Status,
} from "@cordial/protocol";
import type {
  AdapterStatus,
  Candidate,
  DeviceInformation,
  DeviceRecord,
  Integration,
  KindName,
  OutputModeName,
  PairingPrompt,
  Profile,
  Resource,
  ResourceTypeName,
  RoleName,
  Scalar,
  Setting,
  SettingField,
  SettingFields,
  SettingsChange,
  SettingsGroupName,
  ValueType,
  WireError,
} from "../shared/state.ts";

/** The lower-case name of an enum value; a value this app doesn't know reads as zero. */
export function name<E extends Record<string, string | number>>(e: E, value: number): Lowercase<Extract<keyof E, string>> {
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

/** An enum value by lower-case name, or its number when this app doesn't know it. */
const named = <E extends Record<string, string | number>>(e: E, value: number) => (value in e ? name(e, value) : value);

/** An error from a message or a CordialError, with every detail it carries. */
export function wireError(e: Pick<ProtocolError, "code" | "reason" | "outcome" | "related">): WireError {
  return { code: named(ErrorCode, e.code), reason: named(ErrorReason, e.reason), outcome: named(ErrorOutcome, e.outcome), related: e.related.map(wireError) };
}

/** A record's read failure; a missing error reads as an unknown one. */
export const recordError = (r: RecordError): WireError => wireError(r.error ?? create(ErrorSchema));

/** Resource entries of known types, the first of each type. */
export function resources(list: ResourceCapacity[]): Resource[] {
  const seen = new Set<number>();
  return list.flatMap((r) => {
    if (!(r.type in ResourceType) || r.type === ResourceType.UNKNOWN || seen.has(r.type)) return [];
    seen.add(r.type);
    return [{ type: name(ResourceType, r.type) as ResourceTypeName, count: optional(r.count), limit: optional(r.limit) }];
  });
}

/** Distinct complete errors in the adapter's order. */
export const errors = (list: ProtocolError[]): WireError[] => [...new Map(list.map(wireError).map((e) => [JSON.stringify(e), e])).values()];

export function status(s: Status): AdapterStatus {
  const p = s.profileSupport;
  return {
    id: s.id,
    name: s.name,
    platform: name(Platform, s.platform),
    ready: s.ready,
    errors: errors(s.errors),
    resources: resources(s.resources),
    transports: s.transports.flatMap((t) => {
      const kind = transport(t.transport);
      // Missing means enabled.
      return kind ? [{ transport: kind, enabled: t.enabled !== false, resources: resources(t.resources) }] : [];
    }),
    information: {
      firmwareVersion: optional(s.information?.firmwareVersion),
      boardName: optional(s.information?.boardName),
      developmentBuild: optional(s.information?.developmentBuild),
    },
    profileSupport: p ? { resources: resources(p.resources) } : null,
    // Without profile support there are no interfaces, whatever the message carries.
    interfaces: p
      ? s.configurationInterfaces.flatMap((i) =>
          i.interface ? [{ interface: i.interface, enabled: i.enabled, active: i.active, profile: i.profile, conflicts: [...i.conflicts] }] : [])
      : [],
    outputSupport: s.outputSupport.flatMap((o) => {
      const r = role(o.role);
      const modes = o.modes.flatMap((m) => {
        const n = outputMode(m.mode);
        return n ? [{ mode: n, resources: resources(m.resources) }] : [];
      });
      return r ? [{ role: r, modes }] : [];
    }),
  };
}

/** A known role or output mode, or null for one this app doesn't know. */
export const role = (r: Role): RoleName | null => (r in Role && r !== Role.UNKNOWN ? (name(Role, r) as RoleName) : null);
export const outputMode = (m: OutputMode): OutputModeName | null => (m in OutputMode ? name(OutputMode, m) : null);

/** Distinct known values of a repeated enum, by lower-case name. */
function known<E extends Record<string, string | number>, N extends string>(e: E, values: number[]): N[] {
  return [...new Set(values.filter((v) => v in e && v !== 0).map((v) => name(e, v)))] as unknown as N[];
}

const kinds = (list: Kind[]) => known<typeof Kind, KindName>(Kind, list);
const roles = (list: Role[]) => known<typeof Role, RoleName>(Role, list);

export const profile = (p: WireProfile): Profile => ({ id: p.id, name: p.name, roles: roles(p.roles) });

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
    error: s.case === "error" ? wireError(s.value) : null,
  };
}

export function device(d: Device): DeviceRecord {
  return {
    id: d.id,
    transport: transport(d.transport),
    name: d.name,
    kinds: kinds(d.kinds),
    state: name(DeviceState, d.state),
    enabled: d.enabled,
    trusted: d.trusted,
    blocked: d.blocked,
    paused: d.paused,
    inactive: d.inactive === undefined ? null : name(InactiveReason, d.inactive),
    errors: errors(d.errors),
    security: d.security
      ? {
          encrypted: optional(d.security.encrypted),
          authenticated: optional(d.security.authenticated),
          secureConnections: optional(d.security.secureConnections),
          keySize: optional(d.security.keySize),
        }
      : null,
    hidpp: hidpp(d),
    information: information(d.information),
    roles: roles(d.roles),
    profiles: d.profiles ? [...d.profiles.profiles] : null,
    outputPreferences: d.outputPreferences.flatMap((p) => {
      const r = role(p.role);
      return r ? [{ role: r, mode: outputMode(p.mode) }] : [];
    }),
    outputs: d.outputs.flatMap((o) => {
      const r = role(o.role);
      // An unset status, or one this app doesn't know, reads as unknown.
      const s = o.status;
      const state = s.case === "error" ? null : name(OutputState, s.case === "state" ? s.value : OutputState.UNKNOWN);
      return r ? [{ role: r, mode: outputMode(o.mode), state, error: s.case === "error" ? wireError(s.value) : null, slot: optional(o.slot) }] : [];
    }),
  };
}

/** A reported enum value by lower-case name, or null while unknown; a value this app doesn't know
 * reads as the enum's zero value. */
const optionalName = <E extends Record<string, string | number>>(e: E, v: number | undefined) => (v === undefined ? null : name(e, v));

/** The device's information, each field null while unknown. */
export function information(i: WireDeviceInformation | undefined): DeviceInformation {
  const id = i?.identity;
  const b = i?.backlight;
  const w = i?.wheel;
  // At most one sensor per index, in index order.
  const sensors = new Map((i?.pointerSensors ?? []).map((s) => [s.index, optional(s.dpi)]));
  return {
    identity: {
      manufacturer: optional(id?.manufacturer),
      model: optional(id?.model),
      serial: optional(id?.serial),
      firmwareVersion: optional(id?.firmwareVersion),
      bootloaderVersion: optional(id?.bootloaderVersion),
      hardwareRevision: optional(id?.hardwareRevision),
      softwareRevision: optional(id?.softwareRevision),
      vendor: { registry: optionalName(VendorRegistry, id?.vendor?.registry), id: optional(id?.vendor?.id) },
      product: { id: optional(id?.product?.id), version: optional(id?.product?.version) },
    },
    battery: { level: optional(i?.battery?.level), charging: optional(i?.battery?.charging) },
    keyboard: { platform: optionalName(KeyboardPlatform, i?.keyboard?.platform), functionRow: optionalName(FunctionRowMode, i?.keyboard?.functionRow) },
    power: { autoOff: optional(i?.power?.autoOff) },
    backlight: {
      currentBrightness: optional(b?.currentBrightness),
      status: optionalName(BacklightStatus, b?.status),
      enabled: optional(b?.enabled),
      mode: optionalName(BacklightMode, b?.mode),
      brightness: optional(b?.brightness),
      delay: { handsAway: optional(b?.delay?.handsAway), handsNearby: optional(b?.delay?.handsNearby), externalPower: optional(b?.delay?.externalPower) },
      startupEffect: optional(b?.startupEffect),
      dialEffect: optional(b?.dialEffect),
      powerSave: optional(b?.powerSave),
      effect: optionalName(BacklightEffect, b?.effect),
    },
    pointerSensors: [...sensors].sort(([a], [b]) => a - b).map(([index, dpi]) => ({ index, dpi })),
    wheel: {
      resolutionMultiplier: optional(w?.resolutionMultiplier),
      ratchetsPerRotation: optional(w?.ratchetsPerRotation),
      diameter: optional(w?.diameter),
      mode: optionalName(WheelMode, w?.mode),
      freeSpinThreshold: optional(w?.freeSpinThreshold),
      invert: optional(w?.invert),
    },
    thumbwheel: { invert: optional(i?.thumbwheel?.invert) },
  };
}

type Enum = Record<string, string | number>;
/** Where a setting field sits in its group's settings and update messages, and its value type. */
interface FieldSpec {
  type: ValueType;
  path: string[];
  enum?: Enum;
}
const bool = (...path: string[]): FieldSpec => ({ type: "bool", path });
const integer = (...path: string[]): FieldSpec => ({ type: "integer", path });
const enumOf = (e: Enum, ...path: string[]): FieldSpec => ({ type: "enum", path, enum: e });

/** Every setting field this app knows, by group, in the protocol's field order. */
export const FIELDS: { [G in SettingsGroupName]: Record<SettingFields[G], FieldSpec> } = {
  keyboard: { platform: enumOf(KeyboardPlatform, "platform"), function_row: enumOf(FunctionRowMode, "functionRow") },
  power: { auto_off: integer("autoOff") },
  backlight: {
    enabled: bool("enabled"),
    mode: enumOf(BacklightMode, "mode"),
    brightness: integer("brightness"),
    hands_away: integer("delay", "handsAway"),
    hands_nearby: integer("delay", "handsNearby"),
    external_power: integer("delay", "externalPower"),
    startup_effect: bool("startupEffect"),
    dial_effect: bool("dialEffect"),
    power_save: bool("powerSave"),
    effect: enumOf(BacklightEffect, "effect"),
  },
  pointer_sensor: { dpi: integer("dpi") },
  wheel: { mode: enumOf(WheelMode, "mode"), free_spin_threshold: integer("freeSpinThreshold"), invert: bool("invert") },
  thumbwheel: { invert: bool("invert") },
};

type GroupCase = NonNullable<SettingsObject["group"]["case"]>;
/** The oneof case of each group in settings objects and updates. */
export const CASES: Record<SettingsGroupName, GroupCase> = {
  keyboard: "keyboard",
  power: "power",
  backlight: "backlight",
  pointer_sensor: "pointerSensor",
  wheel: "wheel",
  thumbwheel: "thumbwheel",
};
const GROUPS = Object.fromEntries(Object.entries(CASES).map(([g, c]) => [c, g])) as Record<GroupCase, SettingsGroupName>;

const spec = (f: SettingField): FieldSpec => (FIELDS[f.group] as Record<string, FieldSpec>)[f.field]!;
const fieldsOf = (group: SettingsGroupName) => Object.entries(FIELDS[group]) as [SettingField["field"], FieldSpec][];
const at = (message: any, path: string[]): any => path.reduce((m, k) => m?.[k], message);

/** A setting value as the app keeps it: enum values by lower-case name. */
function scalar(s: FieldSpec, v: boolean | bigint | number | undefined): Scalar | null {
  if (v === undefined) return null;
  if (s.type === "integer") return number(v as bigint);
  return s.enum ? name(s.enum, v as number) : (v as boolean);
}

/** A setting value as the protocol carries it. */
function wireScalar(s: FieldSpec, v: Scalar): boolean | bigint | number {
  if (s.type === "integer") return BigInt(v as number);
  return s.enum ? (s.enum[(v as string).toUpperCase()] as number) : (v as boolean);
}

/** The settings of one object, one per field present; none for a group this app doesn't know. */
export function objectSettings(o: SettingsObject): Setting[] {
  const g = o.group;
  if (!g.case) return [];
  const group = GROUPS[g.case];
  const index = g.case === "pointerSensor" ? g.value.index : null;
  return fieldsOf(group).flatMap(([field, s]) => {
    const w = at(g.value, s.path);
    if (!w) return [];
    const status = w.status?.result;
    const row = {
      integration: o.integration,
      group,
      index,
      field,
      type: s.type,
      value: scalar(s, w.value),
      saved: scalar(s, w.saved),
      state: status?.case === "state" ? name(SettingState, status.value) : null,
      error: status?.case === "error" ? wireError(status.value) : null,
      choices: [] as Scalar[],
      min: null as number | null,
      max: null as number | null,
      step: null as number | null,
    } as Setting;
    if (s.enum) row.choices = known(s.enum, w.choices);
    else if (w.limits?.case === "range") {
      row.min = number(w.limits.value.min);
      row.max = number(w.limits.value.max);
      row.step = Math.max(1, number(w.limits.value.step));
    } else if (w.limits?.case === "choices") row.choices = [...new Set((w.limits.value.values as bigint[]).map(number))];
    return [row];
  });
}

export const settings = (list: SettingsObject[]) => list.flatMap(objectSettings);

/** The identity of the object holding a setting, as the protocol orders and names it. */
export const objectRef = (f: SettingField) => ({
  integration: f.integration,
  group: wire(SettingsGroup, f.group),
  index: f.group === "pointer_sensor" ? (f.index ?? 0) : undefined,
});

/** Whether a setting belongs to the object `ref` names; a group this app doesn't know names none. */
export function inObject(f: SettingField, ref: Pick<SettingsRef, "integration" | "group" | "index">): boolean {
  if (!(ref.group in SettingsGroup) || ref.group === SettingsGroup.UNKNOWN) return false;
  const group = name(SettingsGroup, ref.group);
  return f.integration === ref.integration && f.group === group && (group !== "pointer_sensor" || f.index === (ref.index ?? 0));
}

/** The listing order: by object, then field in protocol order. */
export function compareSettings(a: Setting, b: Setting): number {
  const x = objectRef(a);
  const y = objectRef(b);
  return x.integration - y.integration || x.group - y.group || (x.index ?? -1) - (y.index ?? -1)
    || Object.keys(FIELDS[a.group]).indexOf(a.field) - Object.keys(FIELDS[b.group]).indexOf(b.field);
}

/** The update that saves or forgets one field. */
export function settingsUpdate(change: SettingsChange): MessageInitShape<typeof SettingsUpdateSchema> {
  const f = change.setting;
  const s = spec(f);
  const value: Record<string, unknown> = f.group === "pointer_sensor" ? { index: f.index ?? 0 } : {};
  let node = value;
  for (const k of s.path.slice(0, -1)) node = (node[k] = {}) as Record<string, unknown>;
  node[s.path.at(-1)!] = { change: change.type === "set" ? { case: "value", value: wireScalar(s, change.value) } : { case: "forget", value: {} } };
  return { integration: f.integration, group: { case: CASES[f.group], value } } as MessageInitShape<typeof SettingsUpdateSchema>;
}

/** The changes an update carries, field by field; none for a group this app doesn't know. */
export function updateChanges(u: SettingsUpdate): SettingsChange[] {
  const g = u.group;
  if (!g.case) return [];
  const group = GROUPS[g.case];
  const index = g.case === "pointerSensor" ? g.value.index : null;
  return fieldsOf(group).flatMap(([field, s]): SettingsChange[] => {
    const c = at(g.value, s.path)?.change;
    const setting = { integration: u.integration, group, index, field } as SettingField;
    if (c?.case === "forget") return [{ type: "forget", setting }];
    if (c?.case === "value") return [{ type: "set", setting, value: scalar(s, c.value)! }];
    return [];
  });
}

export function candidate(c: WireCandidate): Candidate {
  return { id: c.id, transport: transport(c.transport), name: c.name, kinds: kinds(c.kinds), rssi: optional(c.rssi) };
}

/** A pairing step: a prompt for the user, the saved device, or the failure. */
export type PairingStep =
  | { kind: "progress" }
  | { kind: "prompt"; prompt: PairingPrompt }
  | { kind: "done"; device: number }
  | { kind: "failed"; error: WireError };

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
    case "error":
      return { kind: "failed", error: wireError(step.value) };
    default:
      // Connecting, or a step this app doesn't know: still in progress.
      return { kind: "progress" };
  }
}
