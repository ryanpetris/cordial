import { describe, expect, it, vi } from "vitest";
import { listPorts, openWebSerial } from "../src/web.ts";

/** A Web Serial port whose streams and modem lines are recorded. */
class MockPort extends EventTarget {
  readable: ReadableStream<Uint8Array> | null = null;
  writable: WritableStream<Uint8Array> | null = null;
  /** open, dtr:<level> and close, in order. */
  events: string[] = [];
  written = "";
  #input!: ReadableStreamDefaultController<Uint8Array>;

  constructor(readonly info: SerialPortInfo = { usbVendorId: 0x1209, usbProductId: 0xc0d1 }) {
    super();
  }
  #stream() {
    this.readable = new ReadableStream({ start: (c) => void (this.#input = c) });
  }
  getInfo() {
    return this.info;
  }
  async open() {
    this.events.push("open");
    this.#stream();
    this.writable = new WritableStream({ write: (chunk) => void (this.written += new TextDecoder().decode(chunk)) });
  }
  async setSignals(signals: SerialOutputSignals) {
    this.events.push(`dtr:${signals.dataTerminalReady}`);
  }
  async close() {
    if (this.readable?.locked || this.writable?.locked) throw new Error("streams are locked");
    this.events.push("close");
  }
  send(text: string) {
    this.#input.enqueue(new TextEncoder().encode(text));
  }
  unplug() {
    this.readable = null;
    this.#input.error(new Error("device lost"));
  }
  /** A recoverable error: the stream fails and the port offers a new one. */
  overrun() {
    const input = this.#input;
    this.#stream();
    input.error(new Error("buffer overrun"));
  }
}

const serialOf = (...ports: MockPort[]) => ({ getPorts: async () => ports }) as unknown as Serial;

async function opened(port: MockPort) {
  const [info] = await listPorts(serialOf(port));
  return openWebSerial(info!.path);
}

describe("Web Serial stream", () => {
  it("lists granted Cordial ports with stable paths and no serial number", async () => {
    const a = new MockPort();
    const b = new MockPort();
    const other = new MockPort({ usbVendorId: 0x1234, usbProductId: 0xc0d1 });
    const first = await listPorts(serialOf(a, other, b));
    expect(first.map((p) => p.serial)).toEqual([null, null]);
    expect(new Set(first.map((p) => p.path)).size).toBe(2);
    expect(await listPorts(serialOf(b, a))).toEqual([first[1], first[0]]);
  });

  it("cycles DTR on open, carries data both ways and lowers DTR before closing", async () => {
    const port = new MockPort();
    const transport = await opened(port);
    expect(port.events).toEqual(["open", "dtr:false", "dtr:true"]);
    const received: string[] = [];
    transport.onData((chunk) => received.push(new TextDecoder().decode(chunk)));
    const closed = vi.fn();
    transport.onClose(closed);
    port.send("{}\n");
    await vi.waitFor(() => expect(received).toEqual(["{}\n"]));
    await transport.write(new TextEncoder().encode("\x02\x01\x00"));
    expect(port.written).toBe("\x02\x01\x00");
    await transport.close();
    expect(port.events.slice(3)).toEqual(["dtr:false", "close"]);
    expect(closed).not.toHaveBeenCalled();
  });

  it("reports an unplugged port once and releases it", async () => {
    const port = new MockPort();
    const transport = await opened(port);
    const closed = vi.fn();
    transport.onClose(closed);
    port.unplug();
    await vi.waitFor(() => expect(port.events.at(-1)).toBe("close"));
    expect(closed).toHaveBeenCalledTimes(1);
    expect(closed.mock.calls[0]![0]).toBeInstanceOf(Error);
    await transport.close();
    expect(port.events.filter((e) => e === "close")).toHaveLength(1);
  });

  it("keeps reading after a recoverable error", async () => {
    const port = new MockPort();
    const transport = await opened(port);
    const received: string[] = [];
    transport.onData((chunk) => received.push(new TextDecoder().decode(chunk)));
    const closed = vi.fn();
    transport.onClose(closed);
    port.overrun();
    await new Promise((r) => setTimeout(r, 10));
    port.send("{}\n");
    await vi.waitFor(() => expect(received).toEqual(["{}\n"]));
    expect(closed).not.toHaveBeenCalled();
    await transport.close();
    expect(port.events.slice(3)).toEqual(["dtr:false", "close"]);
  });

  it("refuses a port that is no longer listed", async () => {
    await listPorts(serialOf());
    await expect(openWebSerial("webserial:missing")).rejects.toThrow("no longer available");
  });
});
