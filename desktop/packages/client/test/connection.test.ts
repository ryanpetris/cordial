import { create, fromBinary, toBinary, type MessageInitShape } from "@bufbuild/protobuf";
import { ErrorCode, FrameDecoder, MessageSchema, RequestSchema, Transport, encodeFrame, type Request } from "@cordial/protocol";
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
        { integration: 1, key: "wheel.invert", change: { case: "value", value: { value: { case: "bool", value: true } } } },
        { integration: 1, key: "wheel.invert", change: { case: "forget", value: {} } },
      ],
    });
    await Promise.resolve();
    peer.send({ kind: { case: "response", value: {} } });
    await saved;
    const [list, set] = peer.requests.slice(1);
    expect(list!.command).toMatchObject({ case: "listProfiles", value: { after: 7 } });
    expect(set!.command.case === "setSettings" && set!.command.value.changes.map((c) => c.change.case)).toEqual(["value", "forget"]);
  });

  it("reads a listing page by page, passing the key of the last entry received", async () => {
    const { peer, connection } = await open();
    const all = connection.listAllSettings(4);
    const setting = (key: string) => ({ integration: 1, key, type: { case: "bool" as const, value: {} } });
    await Promise.resolve();
    peer.send({ kind: { case: "response", value: { result: { case: "settings", value: { device: 4, settings: [setting("a"), setting("b")] } } } } });
    await new Promise((r) => setTimeout(r, 0));
    peer.send({ kind: { case: "response", value: { result: { case: "settings", value: { device: 4, settings: [setting("c")], end: true } } } } });
    expect((await all).map((s) => s.key)).toEqual(["a", "b", "c"]);
    const [first, second] = peer.requests.slice(1);
    expect(first!.command).toMatchObject({ case: "listSettings", value: { device: 4 } });
    expect(first!.command.case === "listSettings" && first!.command.value.after).toBeUndefined();
    expect(second!.command).toMatchObject({ case: "listSettings", value: { device: 4, after: { integration: 1, key: "b" } } });
  });

  it("rejects an empty page that does not end the listing", async () => {
    const { peer, connection } = await open();
    const all = connection.listAllDevices();
    await Promise.resolve();
    peer.send({ kind: { case: "response", value: { result: { case: "devices", value: { entries: [] } } } } });
    await expect(all).rejects.toBeInstanceOf(UnexpectedResponseError);
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
    const queued = connection.listWarnings(1).catch((e: unknown) => e);
    peer.unplug();
    expect(await pending).toBeInstanceOf(ConnectionClosedError);
    expect(await queued).toBeInstanceOf(ConnectionClosedError);
    expect(closed).toHaveBeenCalledOnce();
    await expect(connection.getStatus()).rejects.toBeInstanceOf(ConnectionClosedError);
  });

  it("fails to open, closing the stream, when nothing answers", async () => {
    const peer = new Peer();
    await expect(Connection.open(peer, { openTimeoutMs: 20 })).rejects.toThrow("getStatus got no response");
    expect(peer.closed).toBe(true);
  });
});
