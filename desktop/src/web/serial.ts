// USB serial ports through the browser's Web Serial API (Chromium only).
//
// Web Serial reads neither the USB serial number nor the manufacturer, so
// ports are filtered by vendor and product ID and the handshake identifies the
// adapter. It changes DTR without a break request, so opening cycles DTR low
// then high, as the CLI does: a fresh control session starts even if the port
// was left open with DTR high. Input from an earlier session is ignored by the
// session until its first valid response. Closing lowers DTR.
import { USB_PRODUCT_ID, USB_VENDOR_ID, type PortInfo, type Transport } from "../core/transport.ts";

export const PORT_FILTERS = [{ usbVendorId: USB_VENDOR_ID, usbProductId: USB_PRODUCT_ID }];
/** How long DTR stays low before rising, as in the CLI. */
const DTR_LOW_MS = 60;

const ids = new WeakMap<SerialPort, string>();
let listed = new Map<string, SerialPort>();
let nextId = 1;

/** The ports the user has granted that carry Cordial's USB ID. */
export async function listPorts(serial: Serial): Promise<PortInfo[]> {
  const ports = (await serial.getPorts()).filter((p) => {
    const info = p.getInfo();
    return info.usbVendorId === USB_VENDOR_ID && info.usbProductId === USB_PRODUCT_ID;
  });
  listed = new Map();
  return ports.map((port) => {
    let path = ids.get(port);
    if (!path) ids.set(port, (path = `webserial:${nextId++}`));
    listed.set(path, port);
    return { path, serial: null };
  });
}

class WebSerialTransport implements Transport {
  #closing = false;
  #dataListeners: ((chunk: Uint8Array) => void)[] = [];
  #closeListeners: ((error: Error | null) => void)[] = [];
  readonly #port: SerialPort;
  #reader: ReadableStreamDefaultReader<Uint8Array> | null = null;
  readonly #writer: WritableStreamDefaultWriter<Uint8Array>;
  readonly #reading: Promise<void>;
  readonly #encoder = new TextEncoder();
  #shutdown: Promise<void> | null = null;

  constructor(port: SerialPort) {
    this.#port = port;
    this.#writer = port.writable!.getWriter();
    this.#reading = this.#read();
  }

  async #read() {
    let error: Error | null = null;
    // A read fails when the device is lost, and on recoverable errors such as
    // a buffer overrun, after which the port offers a new stream.
    while (this.#port.readable && !this.#closing) {
      const reader = (this.#reader = this.#port.readable.getReader());
      try {
        for (;;) {
          const { value, done } = await reader.read();
          if (done) break;
          for (const listener of this.#dataListeners) listener(value);
        }
        error = null;
        break;
      } catch (e) {
        error = e as Error;
      } finally {
        reader.releaseLock();
      }
    }
    this.#closed(error);
  }

  /** The port ended by itself: unplugged or failed. */
  #closed(error: Error | null) {
    if (this.#closing) return;
    this.#closing = true;
    for (const listener of this.#closeListeners.splice(0)) listener(error);
    void this.#release();
  }

  #release() {
    this.#shutdown ??= (async () => {
      await this.#reader?.cancel().catch(() => {});
      await this.#reading;
      this.#writer.releaseLock();
      await this.#port.setSignals({ dataTerminalReady: false }).catch(() => {});
      await this.#port.close().catch(() => {});
    })();
    return this.#shutdown;
  }

  onData(listener: (chunk: Uint8Array) => void) {
    this.#dataListeners.push(listener);
  }
  onClose(listener: (error: Error | null) => void) {
    this.#closeListeners.push(listener);
  }
  write(text: string) {
    return this.#writer.write(this.#encoder.encode(text));
  }
  close() {
    this.#closing = true;
    this.#closeListeners = [];
    return this.#release();
  }
}

/** Opens a listed port and starts a control session by raising DTR. */
export async function openWebSerial(path: string): Promise<Transport> {
  const port = listed.get(path);
  if (!port) throw new Error("the port is no longer available");
  await port.open({ baudRate: 115200 });
  try {
    await port.setSignals({ dataTerminalReady: false });
    await new Promise((resolve) => setTimeout(resolve, DTR_LOW_MS));
    await port.setSignals({ dataTerminalReady: true });
  } catch (error) {
    await port.close().catch(() => {});
    throw error;
  }
  return new WebSerialTransport(port);
}
