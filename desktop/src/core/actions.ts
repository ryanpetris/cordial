// Run-time validation of actions arriving over IPC from the window.
import type { Action } from "../shared/state.ts";

type Kind = "string" | "boolean" | "value" | "string?" | "nullable";
const key = { key: "string" } as const;
const FIELDS: Record<Action["type"], Record<string, Kind>> = {
  "device.connect": key,
  "device.disconnect": key,
  "device.unpair": key,
  "device.info.refresh": key,
  "device.enabled": { key: "string", value: "boolean" },
  "device.trusted": { key: "string", value: "boolean" },
  "device.blocked": { key: "string", value: "boolean" },
  "device.hidpp": { key: "string", value: "boolean" },
  "setting.set": { key: "string", setting: "string", value: "value" },
  "setting.forget": { key: "string", setting: "string" },
  "settings.refresh": key,
  "settings.apply": key,
  "settings.watch": { key: "nullable" },
  "adapter.name": { adapterId: "string", name: "nullable" },
  "adapter.platform": { adapterId: "string", platform: "string" },
  "adapter.connect": { adapterId: "string" },
  "adapter.disconnect": { adapterId: "string" },
  "adapter.menu": { adapterId: "string" },
  "adapters.refresh": {},
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
    case "value":
      return typeof value === "boolean" || typeof value === "string" || Number.isSafeInteger(value);
    case "string?":
      return value === undefined || (typeof value === "string" && value.length <= 256);
    case "nullable":
      return value === null || (typeof value === "string" && value.length <= 256);
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
