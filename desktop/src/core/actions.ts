// Run-time validation of actions arriving over IPC from the window.
import type { Action } from "../shared/state.ts";

type Kind = "string" | "boolean" | "number" | "value" | "string?" | "nullable" | "changes";
const key = { key: "string" } as const;
const FIELDS: Record<Action["type"], Record<string, Kind>> = {
  "device.connect": key,
  "device.disconnect": key,
  "device.unpair": key,
  "device.refresh": key,
  "device.reload": key,
  "device.enabled": { key: "string", value: "boolean" },
  "device.trusted": { key: "string", value: "boolean" },
  "device.blocked": { key: "string", value: "boolean" },
  "device.hidpp": { key: "string", value: "boolean" },
  "settings.save": { key: "string", changes: "changes" },
  "adapter.name": { adapterId: "string", name: "nullable" },
  "adapter.platform": { adapterId: "string", platform: "string" },
  "adapter.connect": { adapterId: "string" },
  "adapter.disconnect": { adapterId: "string" },
  "adapter.menu": { adapterId: "string" },
  "adapters.refresh": {},
  "app.menu": { x: "number", y: "number" },
  "scan.start": { adapterId: "string" },
  "scan.stop": {},
  "pair.start": { adapterId: "string", candidateId: "string" },
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

function fits(value: unknown, kind: Kind): boolean {
  switch (kind) {
    case "string":
      return typeof value === "string" && value.length <= 256;
    case "boolean":
      return typeof value === "boolean";
    case "number":
      return typeof value === "number" && Number.isFinite(value);
    case "value":
      return typeof value === "boolean" || typeof value === "string" || Number.isSafeInteger(value);
    case "string?":
      return value === undefined || (typeof value === "string" && value.length <= 256);
    case "nullable":
      return value === null || (typeof value === "string" && value.length <= 256);
    case "changes":
      return Array.isArray(value) && value.length > 0 && value.every((change: unknown) => {
        if (!change || typeof change !== "object") return false;
        const c = change as Record<string, unknown>;
        const fields = c.type === "set" ? ["type", "setting", "value"] : ["type", "setting"];
        return (c.type === "set" || c.type === "forget")
          && Object.keys(c).every((k) => fields.includes(k))
          && fits(c.setting, "string") && (c.type === "forget" || fits(c.value, "value"));
      });
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
