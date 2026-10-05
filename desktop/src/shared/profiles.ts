// Profiles, device layers and configuration interfaces: their names, and the checks the app
// makes before asking the adapter to change them.
import { adapterName } from "./adapter-name.ts";
import type { AdapterChanges, AdapterEntry, AdapterStatus, DeviceEntry, InterfaceState } from "./state.ts";

/** Canonical profile name, or null when invalid. Profile names follow the adapter name's rules. */
export const profileName = adapterName;

/** The name a copy of `name` starts with, kept within the adapter's limit. */
export function copyName(name: string): string {
  const suffix = " Copy";
  let base = name;
  while (new TextEncoder().encode(base + suffix).length > 64) base = [...base].slice(0, -1).join("");
  return profileName(base + suffix) ?? suffix.trim();
}

/** Configuration interfaces this app names, by the protocol's ConfigurationInterface number. */
const INTERFACES: Record<number, string> = { 1: "VIA", 2: "Vial" };

/** A configuration interface's name; null for one this app doesn't know. */
export const interfaceName = (i: number): string | null => INTERFACES[i] ?? null;

/** A configuration interface's name for messages, numbering one this app doesn't know. */
const interfaceText = (i: number) => interfaceName(i) ?? `Unknown ${i}`;

/** A profile's name, or its ID while the adapter hasn't named it. */
export const profileText = (adapter: AdapterEntry, id: number) => adapter.profileNames[id]?.name ?? `Profile ${id}`;

/** The adapter's configuration interfaces with `changes` in place of the saved values. */
export function stagedInterfaces(status: AdapterStatus, changes: AdapterChanges): InterfaceState[] {
  return status.interfaces.map((i) => ({ ...i, ...changes.interfaces?.[i.interface] }));
}

/** Whether `i` can be turned on: it has a profile and no interface it conflicts with is on. */
export const canEnable = (interfaces: InterfaceState[], i: InterfaceState) =>
  i.profile !== 0 && !conflicting(interfaces, i);

/** An enabled interface that `i` can't be enabled alongside. */
const conflicting = (interfaces: InterfaceState[], i: InterfaceState) =>
  interfaces.find((x) => x.interface !== i.interface && x.enabled && (i.conflicts.includes(x.interface) || x.conflicts.includes(i.interface)));

/** Why the adapter would refuse these interface preferences, or null when it would take them. */
export function interfaceProblem(interfaces: InterfaceState[]): string | null {
  for (const i of interfaces.filter((x) => x.enabled)) {
    const name = interfaceText(i.interface);
    if (!i.profile) return `Choose a profile for ${name} before turning it on.`;
    const other = conflicting(interfaces, i);
    if (other) return `${name} and ${interfaceText(other.interface)} can't both be on. Turn one off first.`;
  }
  return null;
}

/** Whether saving `changes` reconnects the adapter's USB: an interface turned on or off, or an
 * enabled interface given another profile. */
export function reconnectsUsb(status: AdapterStatus, changes: AdapterChanges): boolean {
  const after = stagedInterfaces(status, changes);
  return status.interfaces.some((before, n) => {
    const i = after[n]!;
    return i.enabled !== before.enabled || (i.enabled && i.profile !== before.profile);
  });
}

/** Why the adapter would refuse to delete `profile`, or null when nothing uses it. */
export function profileInUse(status: AdapterStatus | null, devices: DeviceEntry[], profile: number): string | null {
  const selected = status?.interfaces.find((i) => i.profile === profile);
  if (selected) {
    const name = interfaceText(selected.interface);
    return `${name} is using this profile. Pick a different profile for ${name} first.`;
  }
  const user = devices.find((d) => d.device.profiles?.includes(profile));
  if (user) return `${user.name} is using this profile. Remove it from ${user.name}'s profiles first.`;
  return null;
}
