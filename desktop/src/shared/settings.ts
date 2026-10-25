import type { DeviceEntry, SettingField } from "./state.ts";
import { connected } from "./text.ts";

/** A settings submission or HID++ switch is in flight for the device. */
export function settingsBusy(entry: Pick<DeviceEntry, "pending" | "settingsSave">): boolean {
  return !!entry.settingsSave?.running || entry.pending.some((p) => p === "settings" || p === "hidpp");
}

/** Setting values are current readings only while HID++ is up on a connected device. */
export function settingsCurrent(entry: Pick<DeviceEntry, "device" | "readError">): boolean {
  return connected(entry) && entry.device.hidpp?.state === "active";
}

export function hidppBusy(entry: Pick<DeviceEntry, "pending">): boolean {
  return entry.pending.includes("hidpp");
}

/** A setting field's identity alone, without its values. */
export const fieldOf = (s: SettingField): SettingField => ({ integration: s.integration, group: s.group, index: s.index, field: s.field } as SettingField);

/** A string naming one setting field, for maps and element IDs. */
export const settingId = (s: SettingField) => `${s.integration}-${s.group}-${s.index ?? ""}-${s.field}`;

export const sameField = (a: SettingField, b: SettingField) => settingId(a) === settingId(b);
