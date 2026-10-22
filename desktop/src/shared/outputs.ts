import type { AdapterStatus, DeviceRecord, OutputModeName } from "./state.ts";

/** The device's saved Gamepad Output mode: Disabled when none is saved, null when the adapter
 * saved a mode this app doesn't know. */
export function gamepadOutput(d: DeviceRecord): OutputModeName | null {
  const p = d.outputPreferences.find((x) => x.role === "gamepad");
  return p ? p.mode : "disabled";
}

/** The Gamepad Output modes the adapter takes: Disabled always, the others when it lists them. */
export function gamepadModes(status: AdapterStatus | null): OutputModeName[] {
  const listed = status?.outputSupport.find((s) => s.role === "gamepad")?.modes.map((m) => m.mode) ?? [];
  return ["disabled", ...(["hid", "xinput"] as const).filter((m) => listed.includes(m))];
}

/** Whether the device offers Gamepad Output: a gamepad on an adapter with a gamepad mode, or any
 * device with a saved choice other than Disabled, which can always be turned off. */
export function offersGamepadOutput(d: DeviceRecord, status: AdapterStatus | null): boolean {
  const gamepad = d.kinds.includes("gamepad") || d.roles.includes("gamepad");
  return gamepadOutput(d) !== "disabled" || (gamepad && gamepadModes(status).length > 1);
}

/** Whether saving `mode` as `device`'s Gamepad Output changes the size of a pool the adapter
 * offers, which reconnects its USB. A pool holds the devices choosing its mode, up to the mode's
 * advertised capacity. `devices` are the adapter's paired devices, connected or not. */
export function reconnectsGamepadUsb(devices: DeviceRecord[], device: DeviceRecord, mode: OutputModeName, status: AdapterStatus | null): boolean {
  const offered = status?.outputSupport.find((s) => s.role === "gamepad")?.modes.filter((m) => m.mode === "hid" || m.mode === "xinput") ?? [];
  const pools = (modes: (OutputModeName | null)[]) => offered.map((m) => Math.min(m.maxSlots, modes.filter((x) => x === m.mode).length)).join();
  const others = devices.filter((d) => d.id !== device.id).map(gamepadOutput);
  return pools([...others, gamepadOutput(device)]) !== pools([...others, mode]);
}
