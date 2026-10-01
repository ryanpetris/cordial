// Host support for the Node hosts, Electron and the development server: USB
// hotplug events and the host platform. Ports and streams come from
// @cordial/client/node.
import type { HostPlatform } from "../shared/state.ts";

/**
 * Calls `changed` whenever a USB device is attached or detached.
 * Returns cleanup that owns the watcher, or null when events are unavailable.
 */
export async function watchHotplug(changed: () => void, log: (message: string) => void): Promise<(() => Promise<void>) | null> {
  let emitter: import("usb/index.js").Emitter | undefined;
  const stop = async () => {
    const current = emitter;
    emitter = undefined;
    if (current) await Promise.allSettled([current.removeAttach(), current.removeDetach()]);
  };
  try {
    const { Emitter } = (await import("usb/index.js")) as typeof import("usb/index.js");
    emitter = new Emitter();
    await emitter.addAttach(changed);
    await emitter.addDetach(changed);
    return stop;
  } catch (error) {
    await stop().catch(() => {});
    log(`USB hotplug events unavailable; use the Adapters refresh button: ${(error as Error).message}`);
    return null;
  }
}

export function hostPlatform(): HostPlatform {
  return process.platform === "win32" ? "windows" : process.platform === "darwin" ? "mac" : "linux";
}
