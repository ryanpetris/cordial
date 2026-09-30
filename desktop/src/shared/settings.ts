import type { DeviceEntry } from "./state.ts";

/** Settings edits and live jobs share the device's HID++ work queue. */
export function settingsBusy(entry: Pick<DeviceEntry, "device" | "pending">): boolean {
  return entry.pending.some((p) => ["hidpp.setting.set", "hidpp.setting.forget", "hidpp.setting.refresh", "hidpp.setting.apply", "device.hidpp.set"].includes(p.command))
    || ["discovering", "applying"].includes(entry.device.settings_state)
    || ["probing", "configuring", "resetting"].includes(entry.device.normalization_state);
}

export function settingsLive(entry: Pick<DeviceEntry, "device">): boolean {
  return entry.device.state === "connected";
}

export function hidppBusy(entry: Pick<DeviceEntry, "pending">): boolean {
  return entry.pending.some((p) => p.command === "device.hidpp.set");
}
