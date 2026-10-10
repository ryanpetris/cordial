// What the tray shows for a state, independent of Electron.
import type { AppState, DeviceEntry } from "../shared/state.ts";
import { batteryText, deviceStatus } from "../shared/text.ts";
import { batteryLevel, isLow } from "../core/battery.ts";

export type TrayBase = "idle" | "connected";
export type TrayBadge = "none" | "low" | "attention";

export interface TrayDevice {
  key: string;
  label: string;
  connected: boolean;
  /** Connect is offered only where it can work. */
  canConnect: boolean;
  low: boolean;
}

export interface TrayModel {
  visible: boolean;
  base: TrayBase;
  badge: TrayBadge;
  tooltip: string;
  groups: { title: string | null; devices: TrayDevice[] }[];
  /** What the menu says in place of devices when it has none. */
  empty: string | null;
  attention: string[];
}

function trayDevice(d: DeviceEntry, threshold: number, ready: boolean): TrayDevice {
  const level = batteryLevel(d.battery, threshold);
  const battery = batteryText(d.battery);
  const parts = [deviceStatus(d.device)];
  if (battery) parts.push(level === "low" || level === "critical" ? `${battery} (low)` : battery);
  return {
    key: d.key,
    label: `${d.name} — ${parts.join(" · ")}`,
    connected: d.device.state === "connected",
    canConnect: ready && d.device.state === "disconnected" && d.device.inactive === null,
    low: isLow(d.battery, threshold),
  };
}

export function trayModel(state: AppState): TrayModel {
  const threshold = state.preferences.lowBatteryPercent;
  const adapters = state.adapters.filter((a) => a.connection === "connected");
  const ids = new Set(adapters.map((a) => a.id));
  const devices = state.devices.filter((d) => ids.has(d.adapterId));
  const ready = new Set(adapters.filter((a) => a.readiness === "ready").map((a) => a.id));
  const tray = (d: DeviceEntry) => trayDevice(d, threshold, ready.has(d.adapterId));
  const connected = devices.filter((d) => d.device.state === "connected");
  const low = connected.filter((d) => isLow(d.battery, threshold));
  const attention = adapters.flatMap((a) => a.attention.map((reason) => `${a.name}: ${reason}`));
  const groups =
    adapters.length > 1
      ? adapters.map((a) => ({
          title: a.name,
          devices: devices.filter((d) => d.adapterId === a.id).map(tray),
        }))
      : [{ title: null, devices: devices.map(tray) }];
  const tooltip = [
    connected.length === 0
      ? "No devices connected"
      : `${connected.length} ${connected.length === 1 ? "device" : "devices"} connected`,
    ...low.map((d) => {
      const where = adapters.length > 1 ? ` (${adapters.find((a) => a.id === d.adapterId)?.name})` : "";
      return `${d.name}${where}: battery ${batteryText(d.battery)}`;
    }),
    ...attention,
  ].join("\n");
  return {
    visible: adapters.length > 0 || state.preferences.alwaysShowTray,
    base: connected.length ? "connected" : "idle",
    badge: low.length ? "low" : attention.length ? "attention" : "none",
    tooltip: `Cordial — ${tooltip}`,
    groups,
    empty: devices.length ? null : adapters.length ? "No Saved Devices" : "No Adapter Connected",
    attention,
  };
}
