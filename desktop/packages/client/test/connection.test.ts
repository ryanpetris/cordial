import { create, fromBinary, toBinary, type MessageInitShape } from "@bufbuild/protobuf";
import { ErrorCode, FrameDecoder, MessageSchema, RequestSchema, ResourceType, SettingsGroup, Transport, UsageRange_Kind, encodeFrame, type Request } from "@cordial/protocol";
import { describe, expect, it, vi } from "vitest";
import { Connection, ConnectionClosedError, CordialError, UnexpectedResponseError, type ByteStream } from "../src/index.ts";

/** A stream whose far end the test plays: it records requests and sends messages. */
class Peer implements ByteStream {
  requests: Request[] = [];
  written: Uint8Array[] = [];
  closed = false;
  #data: ((chunk: Uint8Array) => void)[] = [];
  #close: ((error: Error | null) => void)[] = [];
  readonly #decoder = new FrameDecoder();
  onRequest: (request: Request) => void = () => {};

  onData(listener: (chunk: Uint8Array) => void) {
    this.#data.push(listener);
  }
  onClose(listener: (error: Error | null) => void) {
    this.#close.push(listener);
  }
  async write(bytes: Uint8Array) {
    this.written.push(bytes);
    for (const frame of this.#decoder.push(bytes)) {
      if (!frame.ok) throw new Error(frame.error);
      const request = fromBinary(RequestSchema, frame.bytes);
      this.requests.push(request);
      this.onRequest(request);
    }
  }
  async close() {
    this.closed = true;
  }
  raw(bytes: Uint8Array) {
    for (const listener of this.#data) listener(bytes);
  }
  frame(message: MessageInitShape<typeof MessageSchema>) {
    return encodeFrame(toBinary(MessageSchema, create(MessageSchema, message)));
  }
  send(...messages: MessageInitShape<typeof MessageSchema>[]) {
    const frames = messages.map((m) => [...this.frame(m)]);
    this.raw(new Uint8Array(frames.flat()));
  }
  unplug() {
    for (const listener of this.#close) listener(new Error("device lost"));
  }
}

const status = { kind: { case: "response", value: { result: { case: "status", value: { id: "A1", name: "Desk" } } } } } as const;
const ok = { kind: { case: "response", value: {} } } as const;
const removed = (id: number) => ({ kind: { case: "event", value: { kind: { case: "deviceRemoved", value: { id } } } } }) as const;

async function open(options = {}) {
  const peer = new Peer();
  peer.onRequest = () => queueMicrotask(() => peer.send(status));
  const connection = await Connection.open(peer, options);
  peer.onRequest = () => {};
  return { peer, connection };
}

describe("Connection", () => {
  it("starts with a delimiter and ignores everything before the first response", async () => {
    const peer = new Peer();
    const events = vi.fn();
    peer.onRequest = () =>
      queueMicrotask(() => {
        peer.raw(new Uint8Array([0x41, 0x42, 0]));
        peer.send(removed(99), status);
      });
    const connection = await Connection.open(peer, { onEvent: events });
    expect(peer.written[0]![0]).toBe(0);
    expect(peer.requests[0]!.command.case).toBe("getStatus");
    expect(connection.status.id).toBe("A1");
    expect(events).not.toHaveBeenCalled();
  });

  it("preserves capability kinds, collection-qualified ranges and resource values in status", async () => {
    const peer = new Peer();
    const ranges = [UsageRange_Kind.REMAP_INPUT, UsageRange_Kind.REMAP_OUTPUT, UsageRange_Kind.UNSPECIFIED, 99].map((kind) => ({
      kind, collection: { usagePage: 1, usage: 5 }, usagePage: 9, min: 1, max: 32,
    }));
    const resources = [
      { type: ResourceType.MEMORY, count: 0, limit: 4096 },
      { type: ResourceType.OUTPUTS, limit: 8 },
      { type: ResourceType.LAYERS, count: 2 },
    ];
    peer.onRequest = () => queueMicrotask(() => peer.send({
      kind: { case: "response", value: { result: { case: "status", value: {
        id: "A1", errors: [ErrorCode.USB_REQUIRED_CAPACITY_EXCEEDED, ErrorCode.USB_RESERVED_CAPACITY_EXCEEDED], profileSupport: { ranges, resources },
      } } } },
    }));
    const connection = await Connection.open(peer);
    expect(connection.status.errors).toEqual([ErrorCode.USB_REQUIRED_CAPACITY_EXCEEDED, ErrorCode.USB_RESERVED_CAPACITY_EXCEEDED]);
    expect(connection.status.profileSupport).toMatchObject({ ranges, resources });
    // Absent values stay absent rather than reading as zero.
    const [memory, outputs, layers] = connection.status.profileSupport!.resources;
    expect([memory!.count, memory!.limit, outputs!.count, outputs!.limit, layers!.count, layers!.limit]).toEqual([0, 4096, undefined, 8, 2, undefined]);
    await connection.close();
  });

  it("sends one request at a time and answers them in order", async () => {
    const { peer, connection } = await open();
    const first = connection.stopScan();
    const second = connection.startScan([Transport.BLE], 30);
    await Promise.resolve();
    expect(peer.requests.map((r) => r.command.case)).toEqual(["getStatus", "stopScan"]);
    peer.send(ok);
    await first;
    expect(peer.requests.map((r) => r.command.case)).toEqual(["getStatus", "stopScan", "startScan"]);
    expect(peer.requests[2]!.command.value).toMatchObject({ transports: [Transport.BLE], seconds: 30 });
    peer.send(ok);
    await second;
  });

  it("delivers responses and events synchronously in stream order", async () => {
    const order: string[] = [];
    const { peer, connection } = await open({
      onEvent: (e: { kind: { case?: string } }) => order.push(`event:${e.kind.case}`),
      onResponse: (r: Request) => order.push(`response:${r.command.case}`),
    });
    const listed = connection.listDevices().then(() => order.push("resolved"));
    await Promise.resolve();
    peer.send(removed(2), { kind: { case: "response", value: { result: { case: "devices", value: { entries: [{ entry: { case: "device", value: { id: 1 } } }], end: true } } } } }, removed(3));
    expect(order).toEqual(["response:getStatus", "event:deviceRemoved", "response:listDevices", "event:deviceRemoved"]);
    await listed;
    expect(order.at(-1)).toBe("resolved");
  });

  it("rejects an error response with its code and reason and keeps going", async () => {
    const { peer, connection } = await open();
    const failing = connection.setDevice({ device: 1, enabled: true });
    await Promise.resolve();
    peer.send({ kind: { case: "response", value: { result: { case: "error", value: { code: ErrorCode.NO_CAPACITY, reason: 1 } } } } });
    const error = await failing.catch((e: unknown) => e);
    expect(error).toBeInstanceOf(CordialError);
    expect(error).toMatchObject({ command: "setDevice", code: ErrorCode.NO_CAPACITY, reason: 1 });
    const next = connection.getStatus();
    await Promise.resolve();
    peer.send(status);
    expect((await next).name).toBe("Desk");
  });

  it("sends page cursors, numeric IDs and setting changes as given", async () => {
    const { peer, connection } = await open();
    const page = connection.listProfiles(7);
    await Promise.resolve();
    peer.send({ kind: { case: "response", value: { result: { case: "profiles", value: { entries: [{ entry: { case: "profile", value: { id: 8, name: "Work" } } }] } } } } });
    expect(await page).toMatchObject({ entries: [{ entry: { case: "profile", value: { id: 8, name: "Work" } } }], end: false });
    const saved = connection.setSettings({
      device: 4,
      changes: [
        { integration: 1, group: { case: "wheel", value: { invert: { change: { case: "value", value: false } } } } },
        { integration: 1, group: { case: "pointerSensor", value: { index: 0, dpi: { change: { case: "value", value: 0n } } } } },
        { integration: 1, group: { case: "wheel", value: { invert: { change: { case: "forget", value: {} } } } } },
      ],
    });
    await Promise.resolve();
    peer.send({ kind: { case: "response", value: {} } });
    await saved;
    const [list, set] = peer.requests.slice(1);
    expect(list!.command).toMatchObject({ case: "listProfiles", value: { after: 7 } });
    // False and zero stay explicit writes rather than reading as unset.
    const changes = set!.command.case === "setSettings" ? set!.command.value.changes.map((c) => c.group) : [];
    expect(changes).toMatchObject([
      { case: "wheel", value: { invert: { change: { case: "value", value: false } } } },
      { case: "pointerSensor", value: { index: 0, dpi: { change: { case: "value", value: 0n } } } },
      { case: "wheel", value: { invert: { change: { case: "forget" } } } },
    ]);
  });

  it("reads a listing page by page, passing the key of the last entry received", async () => {
    const { peer, connection } = await open();
    const all = connection.listAllSettings(4);
    const sensor = (index: number) => ({ integration: 1, group: { case: "pointerSensor" as const, value: { index, dpi: { value: 800n } } } });
    const wheel = { integration: 1, group: { case: "wheel" as const, value: { invert: { value: true } } } };
    const keyboard = { integration: 1, group: { case: "keyboard" as const, value: { functionRow: { value: 1 } } } };
    await Promise.resolve();
    peer.send({ kind: { case: "response", value: { result: { case: "settings", value: { device: 4, objects: [keyboard, sensor(0)] } } } } });
    await new Promise((r) => setTimeout(r, 0));
    peer.send({ kind: { case: "response", value: { result: { case: "settings", value: { device: 4, objects: [sensor(1), wheel], end: true } } } } });
    expect((await all).map((o) => o.group.case)).toEqual(["keyboard", "pointerSensor", "pointerSensor", "wheel"]);
    const [first, second] = peer.requests.slice(1);
    expect(first!.command).toMatchObject({ case: "listSettings", value: { device: 4 } });
    expect(first!.command.case === "listSettings" && first!.command.value.after).toBeUndefined();
    // Component index 0 is sent, not left out.
    expect(second!.command).toMatchObject({ case: "listSettings", value: { device: 4, after: { integration: 1, group: SettingsGroup.POINTER_SENSOR, index: 0 } } });
  });

  it("rejects an empty page that does not end the listing", async () => {
    const { peer, connection } = await open();
    const all = connection.listAllDevices();
    await Promise.resolve();
    peer.send({ kind: { case: "response", value: { result: { case: "devices", value: { entries: [] } } } } });
    await expect(all).rejects.toBeInstanceOf(UnexpectedResponseError);
  });

  it("continues a listing after the last entry of a known kind", async () => {
    const { peer, connection } = await open();
    const all = connection.listAllDevices();
    const devices = (entries: object[], end = false) => ({ kind: { case: "response", value: { result: { case: "devices", value: { entries, end } } } } }) as never;
    const newer = { entry: { case: undefined } };
    await Promise.resolve();
    peer.send(devices([{ entry: { case: "device", value: { id: 3 } } }, newer]));
    await new Promise((r) => setTimeout(r, 0));
    peer.send(devices([newer, { entry: { case: "unreadable", value: 5 } }, newer]));
    await new Promise((r) => setTimeout(r, 0));
    peer.send(devices([newer]));
    await expect(all).rejects.toBeInstanceOf(UnexpectedResponseError);
    const afters = peer.requests.flatMap((r) => (r.command.case === "listDevices" ? [r.command.value.after] : []));
    expect(afters).toEqual([0, 3, 5]);
  });

  it("answers profile creation with the new ID", async () => {
    const { peer, connection } = await open();
    const created = connection.createProfile("Work");
    await Promise.resolve();
    peer.send({ kind: { case: "response", value: { result: { case: "profileCreated", value: { profile: 9 } } } } });
    expect(await created).toBe(9);
  });

  it("reads unknown error codes as unknown", () => {
    expect(new CordialError("x", 999 as ErrorCode).code).toBe(ErrorCode.UNKNOWN);
  });

  it("refuses a request over the size limit without sending it", async () => {
    const { peer, connection } = await open();
    const error = await connection.setAdapter({ name: "x".repeat(2000) }).catch((e: unknown) => e);
    expect(error).toMatchObject({ code: ErrorCode.TOO_LONG });
    expect(peer.requests).toHaveLength(1);
  });

  it("ends the session on bad input, an unrequested response or a missing response", async () => {
    for (const trigger of [
      (peer: Peer) => peer.raw(new Uint8Array([5, 1, 0])),
      (peer: Peer) => peer.send(ok),
    ]) {
      const closed = vi.fn();
      const { peer, connection } = await open({ onClose: closed });
      trigger(peer);
      expect(closed).toHaveBeenCalledOnce();
      expect(connection.closed).toBe(true);
      expect(peer.closed).toBe(true);
    }
    vi.useFakeTimers();
    try {
      const closed = vi.fn();
      const { connection } = await open({ onClose: closed, timeoutMs: 100 });
      const pending = connection.refreshDevice(1).catch((e: unknown) => e);
      await vi.advanceTimersByTimeAsync(150);
      expect(await pending).toBeInstanceOf(ConnectionClosedError);
      expect(closed.mock.calls[0]![0].message).toBe("refreshDevice got no response");
    } finally {
      vi.useRealTimers();
    }
  });

  it("rejects pending requests when the port goes away", async () => {
    const closed = vi.fn();
    const { peer, connection } = await open({ onClose: closed });
    const pending = connection.listSettings(1).catch((e: unknown) => e);
    const queued = connection.listProfiles().catch((e: unknown) => e);
    peer.unplug();
    expect(await pending).toBeInstanceOf(ConnectionClosedError);
    expect(await queued).toBeInstanceOf(ConnectionClosedError);
    expect(closed).toHaveBeenCalledOnce();
    await expect(connection.getStatus()).rejects.toBeInstanceOf(ConnectionClosedError);
  });

  it("fails to open when the session ends right after the first response", async () => {
    const peer = new Peer();
    const closed = vi.fn();
    peer.onRequest = () =>
      queueMicrotask(() => {
        peer.send(status);
        peer.unplug();
      });
    await expect(Connection.open(peer, { onClose: closed })).rejects.toBeInstanceOf(ConnectionClosedError);
    expect(peer.closed).toBe(true);
    expect(closed).not.toHaveBeenCalled();
  });

  it("fails to open, closing the stream, when nothing answers", async () => {
    const peer = new Peer();
    await expect(Connection.open(peer, { openTimeoutMs: 20 })).rejects.toThrow("getStatus got no response");
    expect(peer.closed).toBe(true);
  });
});
