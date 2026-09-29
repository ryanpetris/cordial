// Low-battery notifications: each device is notified once on entering low
// and once on entering critical; the alert resets only after a reading above
// the threshold or while charging.
import type { DeviceEntry } from "../shared/state.ts";
import { batteryLevel, type Level } from "../shared/battery.ts";

export { CRITICAL_PERCENT, batteryLevel, isLow } from "../shared/battery.ts";

const RANK: Record<Level, number> = { ok: 0, low: 1, critical: 2 };

export interface Alert {
  key: string;
  name: string;
  level: "low" | "critical";
  percent: number;
}

export class BatteryAlerts {
  readonly #levels = new Map<string, Level>();

  /** Returns the alerts to show for these devices' current readings. */
  update(devices: DeviceEntry[], threshold: number): Alert[] {
    const alerts: Alert[] = [];
    for (const d of devices) {
      const level = batteryLevel(d.battery, threshold);
      if (level === null) continue;
      const old = this.#levels.get(d.key) ?? "ok";
      if (RANK[level] > RANK[old] && level !== "ok")
        alerts.push({ key: d.key, name: d.name, level, percent: d.battery!.percent! });
      // Recovering from critical to low keeps the low alert without a repeat.
      if (level === "ok" || RANK[level] > RANK[old]) this.#levels.set(d.key, level);
    }
    // Levels outlive absences such as an adapter reconnecting; device IDs
    // are never reused, so a forgotten device's entry is simply unused.
    return alerts;
  }
}
