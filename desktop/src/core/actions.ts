// Run-time validation of actions arriving over IPC from the window.
import type { Action } from "../shared/state.ts";

type Kind =
  | "string"
  | "boolean"
  | "number"
  | "id"
  | "value"
  | "string?"
  | "boolean?"
  | "nullable"
  | "changes"
  | "transports?"
  | "interfaces?"
  | "ids?"
  | "page";
const key = { key: "string" } as const;
const FIELDS: Record<Action["type"], Record<string, Kind>> = {
  "device.connect": key,
  "device.disconnect": key,
  "device.unpair": key,
  "device.refresh": key,
  "device.reload": key,
  "device.update": {
    key: "string",
    enabled: "boolean?",
    trusted: "boolean?",
    blocked: "boolean?",
    hidpp: "boolean?",
    profiles: "ids?",
  },
  "settings.save": { key: "string", changes: "changes" },
  "adapter.name": { adapterId: "string", name: "nullable" },
  "adapter.settings": {
    adapterId: "string",
    platform: "string?",
    transports: "transports?",
    interfaces: "interfaces?",
  },
  "profile.create": { adapterId: "string", name: "string" },
  "profile.copy": { adapterId: "string", profile: "id", name: "string" },
  "profile.delete": { adapterId: "string", profile: "id" },
  "profiles.page": { adapterId: "string", page: "page", picker: "boolean?" },
  "adapter.reload": { adapterId: "string" },
  "adapter.connect": { adapterId: "string" },
  "adapter.disconnect": { adapterId: "string" },
  "adapter.menu": { adapterId: "string" },
  "adapters.refresh": {},
  "app.menu": { x: "number", y: "number" },
  "scan.start": { adapterId: "string" },
  "scan.stop": {},
  "pair.start": { adapterId: "string", candidateId: "id" },
  "pair.reply": { accept: "boolean", value: "string?" },
  "pair.cancel": {},
  "pair.dismiss": {},
  preferences: { preferences: "string" },
};

const PREFERENCES: Record<string, "boolean" | "number"> = {
  startAtLogin: "boolean",
  alwaysShowTray: "boolean",
  notifyLowBattery: "boolean",
  lowBatteryPercent: "number",
  notifyConnections: "boolean",
};

/** A protocol ID: a uint32, where 0 means none. */
const id = (value: unknown) => typeof value === "number" && Number.isInteger(value) && value >= 0 && value <= 0xffffffff;
const record = (value: unknown): value is Record<string, unknown> => !!value && typeof value === "object" && !Array.isArray(value);

function fits(value: unknown, kind: Kind): boolean {
  switch (kind) {
    case "string":
      return typeof value === "string" && value.length <= 256;
    case "boolean":
      return typeof value === "boolean";
    case "number":
      return typeof value === "number" && Number.isFinite(value);
    case "id":
      return id(value);
    case "value":
      return typeof value === "boolean" || typeof value === "string" || Number.isSafeInteger(value);
    case "boolean?":
      return value === undefined || typeof value === "boolean";
    case "string?":
      return value === undefined || (typeof value === "string" && value.length <= 256);
    case "nullable":
      return value === null || (typeof value === "string" && value.length <= 256);
    case "changes":
      return Array.isArray(value) && value.length > 0 && value.every((change: unknown) => {
        if (!record(change)) return false;
        const fields = change.type === "set" ? ["type", "setting", "value"] : ["type", "setting"];
        return (change.type === "set" || change.type === "forget")
          && Object.keys(change).every((k) => fields.includes(k))
          && fits(change.setting, "string") && (change.type === "forget" || fits(change.value, "value"));
      });
    case "page":
      return value === "first" || value === "next" || value === "previous";
    case "ids?":
      return value === undefined || (Array.isArray(value) && value.length <= 64 && value.every(id));
    case "transports?":
      return value === undefined || (record(value) && Object.entries(value).every(([k, v]) => fits(k, "string") && typeof v === "boolean"));
    case "interfaces?":
      return value === undefined || (record(value) && Object.entries(value).every(([k, v]) =>
        /^[1-9][0-9]{0,9}$/.test(k) && record(v)
        && Object.keys(v).every((f) => f === "enabled" || f === "profile")
        && fits(v.enabled, "boolean?") && (v.profile === undefined || id(v.profile))));
  }
}

export function isAction(value: unknown): value is Action {
  if (!value || typeof value !== "object") return false;
  const a = value as Record<string, unknown>;
  const fields = typeof a.type === "string" ? FIELDS[a.type as Action["type"]] : undefined;
  if (!fields || Object.keys(a).some((k) => k !== "type" && !(k in fields))) return false;
  if (a.type === "preferences") {
    const p = a.preferences;
    return (
      !!p &&
      typeof p === "object" &&
      Object.entries(p).every(([k, v]) => PREFERENCES[k] !== undefined && typeof v === PREFERENCES[k] && (typeof v !== "number" || Number.isFinite(v)))
    );
  }
  return Object.entries(fields).every(([k, kind]) => fits(a[k], kind));
}
