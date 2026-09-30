// Low-battery rules shared by the tray, notifications and window: a current
// percentage at or below the threshold while not charging. Unknown never
// counts as low.
import type { Battery } from "./state.ts";

export const CRITICAL_PERCENT = 5;
export type Level = "ok" | "low" | "critical";

/** The battery's level, or null when the readings don't decide it. */
export function batteryLevel(b: Battery | null, threshold: number): Level | null {
  if (!b) return null;
  if (b.chargingFresh && b.charging === true) return "ok";
  if (!b.percentFresh || b.percent == null) return null;
  if (b.percent <= CRITICAL_PERCENT) return "critical";
  return b.percent <= threshold ? "low" : "ok";
}

export const isLow = (b: Battery | null, threshold: number) => {
  const level = batteryLevel(b, threshold);
  return level === "low" || level === "critical";
};
