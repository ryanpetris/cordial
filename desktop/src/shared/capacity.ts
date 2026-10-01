import type { AdapterStatus, DeviceRecord } from "./state.ts";

/**
 * Whether enabling `device` would exceed its transport's limit, from what the
 * adapter last reported. False while the limit is unknown: the adapter decides.
 */
export function enabledFull(status: AdapterStatus | null, devices: DeviceRecord[], device: DeviceRecord): boolean {
  const limit = status?.transports.find((t) => t.transport === device.transport)?.maxEnabled;
  if (limit == null) return false;
  return devices.filter((d) => d.id !== device.id && d.transport === device.transport && d.inactive === null).length >= limit;
}
