import type { AdapterStatus, DeviceChanges, DeviceRecord } from "./state.ts";

const sameLayers = (a: number[], b: number[]) => a.length === b.length && a.every((x, i) => x === b[i]);

/** Whether `changes` change anything the adapter holds for `device`. */
function changesAnything(device: DeviceRecord, changes: DeviceChanges): boolean {
  return (changes.enabled !== undefined && changes.enabled !== device.enabled)
    || (changes.trusted !== undefined && changes.trusted !== device.trusted)
    || (changes.blocked !== undefined && changes.blocked !== device.blocked)
    || (changes.hidpp !== undefined && changes.hidpp !== (device.hidpp?.enabled ?? false))
    || (changes.profiles !== undefined && !sameLayers(changes.profiles, device.profiles ?? []));
}

/**
 * Whether the adapter would refuse `changes` to `device` because every enabled-device place for
 * its transport is in use, from what the adapter last reported. The adapter checks for a place
 * when it saves a change that leaves a device it isn't using enabled and not blocked on a
 * supported, enabled transport. False while the limit is unknown: the adapter decides.
 */
export function enabledFull(status: AdapterStatus | null, devices: DeviceRecord[], device: DeviceRecord, changes: DeviceChanges): boolean {
  const enabled = changes.enabled ?? device.enabled;
  const blocked = changes.blocked ?? device.blocked;
  if (!enabled || blocked || device.inactive === null) return false;
  if (device.inactive === "unsupported_transport" || device.inactive === "transport_disabled") return false;
  if (!changesAnything(device, changes)) return false;
  const limit = status?.transports.find((t) => t.transport === device.transport)?.maxEnabled;
  if (limit == null) return false;
  return devices.filter((d) => d.id !== device.id && d.transport === device.transport && d.inactive === null).length >= limit;
}
