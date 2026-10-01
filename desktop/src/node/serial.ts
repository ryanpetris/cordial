// USB serial ports through the Node serial library, for Electron and the
// development server.
//
// DTR follows the port's lifetime: opening the tty raises DTR, starting a new
// control session, and closing it lowers DTR (HUPCL), ending it. The kernel
// closes a crashed process's descriptor, so every open is a fresh low-to-high
// transition. Opening also discards input received so far, so the stream
// starts at a line boundary and no earlier session's fragment remains. The
// serial library can't set DTR alone: it always issues a break request as
// well, which the adapter's CDC interface doesn't advertise or support.
import { SerialPort } from "serialport";
import type { HostPlatform } from "../protocol/types.ts";
import { USB_PRODUCT_ID, USB_VENDOR_ID, type PortInfo, type Transport } from "../core/transport.ts";

const VENDOR_ID = USB_VENDOR_ID.toString(16);
const PRODUCT_ID = USB_PRODUCT_ID.toString(16);

/** Cordial USB serial ports; opening them is left to the handshake. */
export async function candidatePorts(): Promise<PortInfo[]> {
  const ports = await SerialPort.list();
  const found = ports
    .filter((p) => p.vendorId?.toLowerCase() === VENDOR_ID
      && p.productId?.toLowerCase() === PRODUCT_ID
      && p.manufacturer === "Cordial")
    .map((p) => ({ path: p.path, serial: p.serialNumber ?? "" }));
  // macOS lists both callout and dial-in nodes for one USB serial device.
  const callouts = new Set(found.flatMap((p) => (p.path.startsWith("/dev/cu.") ? [p.path.slice(8)] : [])));
  return found
    .filter((p) => !(p.path.startsWith("/dev/tty.") && callouts.has(p.path.slice(9))))
    .sort((a, b) => a.path.localeCompare(b.path));
}

class SerialTransport implements Transport {
  #closing = false;
  #closeListeners: ((error: Error | null) => void)[] = [];
  readonly #port: SerialPort;

  constructor(port: SerialPort) {
    this.#port = port;
    port.on("close", (error: Error | null) => this.#closed(error ?? null));
    port.on("error", (error: Error) => this.#closed(error));
  }

  #closed(error: Error | null) {
    if (this.#closing) return;
    this.#closing = true;
    for (const listener of this.#closeListeners.splice(0)) listener(error);
  }

  onData(listener: (chunk: Uint8Array) => void) {
    this.#port.on("data", listener);
  }
  onClose(listener: (error: Error | null) => void) {
    this.#closeListeners.push(listener);
  }
  write(text: string) {
    return new Promise<void>((resolve, reject) => {
      this.#port.write(text, "utf8", (error) => {
        if (error) reject(error);
        else this.#port.drain((drained) => (drained ? reject(drained) : resolve()));
      });
    });
  }
  close() {
    this.#closing = true;
    this.#closeListeners = [];
    return new Promise<void>((resolve) => {
      if (!this.#port.isOpen) return resolve();
      this.#port.close(() => resolve());
    });
  }
}

/** Opens a port at the conventional 115200 8-N-1 settings, raising DTR. */
export function openSerial(path: string): Promise<Transport> {
  return new Promise((resolve, reject) => {
    const port = new SerialPort({ path, baudRate: 115200, hupcl: true, autoOpen: false });
    port.open((error) => (error ? reject(error) : resolve(new SerialTransport(port))));
  });
}

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
