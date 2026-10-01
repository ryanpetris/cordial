import type { DeviceEntry } from "./state.ts";

/** A settings submission or HID++ switch is in flight for the device. */
export function settingsBusy(entry: Pick<DeviceEntry, "pending" | "settingsSave">): boolean {
  return !!entry.settingsSave?.running || entry.pending.some((p) => p === "settings" || p === "hidpp");
}

/** Setting values are current readings only while HID++ is up on a connected device. */
export function settingsCurrent(entry: Pick<DeviceEntry, "device">): boolean {
  return entry.device.state === "connected" && entry.device.hidpp?.state === "active";
}

export function hidppBusy(entry: Pick<DeviceEntry, "pending">): boolean {
  return entry.pending.includes("hidpp");
}
