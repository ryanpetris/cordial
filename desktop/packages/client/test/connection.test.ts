import { create, fromBinary, toBinary, type MessageInitShape } from "@bufbuild/protobuf";
import {
  AdapterHelloSchema,
  CURRENT_PROTOCOL_VERSION,
  ErrorCode,
  ErrorOutcome,
  ErrorReason,
  FrameDecoder,
  MessageSchema,
  RequestSchema,
  ResourceType,
  Transport,
  UsageRange_Kind,
  decodeClientHello,
  encodeAdapterHello,
  encodeFrame,
  type AdapterHelloSchema as AdapterHelloInit,
  type ClientHello,
  type Request,
  type ResponseSchema,
} from "@cordial/protocol";
import { describe, expect, it, vi } from "vitest";
import { Connection, ConnectionClosedError, CordialError, ProtocolVersionError, UnexpectedResponseError, type ByteStream } from "../src/index.ts";

/** A stream whose far end the test plays: it answers each hello, records requests and sends
 * messages. */
class Peer implements ByteStream {
  hellos: ClientHello[] = [];
  requests: Request[] = [];
  written: Uint8Array[] = [];
  closed = false;
  #data: ((chunk: Uint8Array) => void)[] = [];
  #close: ((error: Error | null) => void)[] = [];
  readonly #decoder = new FrameDecoder();
  onRequest: (request: Request) => void = () => {};
  /** Answers each client hello; by default with the current revision. */
  onHello: (hello: ClientHello) => void = () => queueMicrotask(() => this.hello({ protocolVersion: CURRENT_PROTOCOL_VERSION }));

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
      if (!frame.bytes.length) {
        const hello = decodeClientHello(frame.bytes);
        this.hellos.push(hello);
        this.onHello(hello);
        continue;
      }
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
  hello(hello: MessageInitShape<typeof AdapterHelloInit>) {
    this.raw(encodeFrame(encodeAdapterHello(create(AdapterHelloSchema, hello))));
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
  it("sends a delimiter and the empty hello first and skips empty frames before the adapter's hello", async () => {
    const peer = new Peer();
    const events = vi.fn();
    peer.onHello = () =>
      queueMicrotask(() => {
        expect(peer.requests).toEqual([]);
        peer.raw(new Uint8Array([0, 0]));
        peer.hello({ protocolVersion: CURRENT_PROTOCOL_VERSION, firmwareVersion: "1.2.3", serialNumber: "S1", name: "Desk" });
      });
    peer.onRequest = () => queueMicrotask(() => peer.send(status));
    const connection = await Connection.open(peer, { onEvent: events });
    expect([...peer.written[0]!]).toEqual([0, 1, 0]);
    expect(peer.hellos).toHaveLength(1);
    expect(peer.requests.map((r) => r.command.case)).toEqual(["getStatus"]);
    expect(connection.hello).toMatchObject({ protocolVersion: CURRENT_PROTOCOL_VERSION, firmwareVersion: "1.2.3", serialNumber: "S1", name: "Desk" });
    expect(connection.status.id).toBe("A1");
    expect(events).not.toHaveBeenCalled();
  });

  it.each([
    ["garbage", new Uint8Array([0x41, 0x42, 0])],
    ["a hello with version zero", encodeFrame(new Uint8Array([8, 0]))],
    ["a hello with two versions", encodeFrame(new Uint8Array([8, 1, 8, 1]))],
    ["a hello without a version", encodeFrame(new Uint8Array([0x1a, 1, 0x31]))],
    ["an empty frame", encodeFrame(new Uint8Array())],
    ["a response followed by a version", encodeFrame(new Uint8Array([0x0a, 0, 8, 1]))],
    ["a truncated response", encodeFrame(new Uint8Array([0x0a, 2, 0x08]))],
  ])("refuses %s in place of the hello even when a valid hello follows", async (_, bytes) => {
    const peer = new Peer();
    const closed = vi.fn();
    peer.onHello = () => queueMicrotask(() => {
      peer.raw(bytes);
      peer.hello({ protocolVersion: CURRENT_PROTOCOL_VERSION });
    });
    peer.onRequest = () => queueMicrotask(() => peer.send(status));
    const error = await Connection.open(peer, { onClose: closed }).catch((e: unknown) => e);
    expect(error).toMatchObject({ message: expect.stringMatching(/^the adapter sent an invalid hello/) });
    expect(error).not.toBeInstanceOf(ProtocolVersionError);
    expect(peer.requests).toEqual([]);
    expect(peer.written).toHaveLength(1);
    expect(peer.closed).toBe(true);
    expect(closed).not.toHaveBeenCalled();
  });

  it("skips responses and events before the adapter's hello, however long", async () => {
    const peer = new Peer();
    const events = vi.fn();
    const long = { kind: { case: "event", value: { kind: { case: "device", value: { id: 1, name: "x".repeat(2000) } } } } } as const;
    peer.onHello = () => queueMicrotask(() => {
      peer.send(status, removed(1), long);
      peer.hello({ protocolVersion: CURRENT_PROTOCOL_VERSION });
    });
    peer.onRequest = () => queueMicrotask(() => peer.send(status));
    const connection = await Connection.open(peer, { onEvent: events });
    expect(peer.requests.map((r) => r.command.case)).toEqual(["getStatus"]);
    expect(events).not.toHaveBeenCalled();
    await connection.close();
  });

  it("skips oversized frames that arrive in pieces before the adapter's hello", async () => {
    const peer = new Peer();
    const events = vi.fn();
    const long = peer.frame({ kind: { case: "event", value: { kind: { case: "device", value: { id: 1, name: "x".repeat(5000) } } } } });
    // Bytes past the hello limit are dropped undecoded, so this isn't reported as malformed COBS.
    const garbage = new Uint8Array([...new Uint8Array(1100).fill(7), 0]);
    peer.onHello = () => queueMicrotask(() => {
      for (const frame of [long, garbage]) for (let at = 0; at < frame.length; at += 100) peer.raw(frame.subarray(at, at + 100));
      peer.hello({ protocolVersion: CURRENT_PROTOCOL_VERSION });
    });
    peer.onRequest = () => queueMicrotask(() => peer.send(status));
    const connection = await Connection.open(peer, { onEvent: events });
    expect(peer.requests.map((r) => r.command.case)).toEqual(["getStatus"]);
    expect(events).not.toHaveBeenCalled();
    await connection.close();
  });

  it("sends no request when an oversized frame never ends before the deadline", async () => {
    const peer = new Peer();
    peer.onHello = () => queueMicrotask(() => {
      for (let i = 0; i < 10; i++) peer.raw(new Uint8Array(1000).fill(7));
    });
    await expect(Connection.open(peer, { openTimeoutMs: 20 })).rejects.toThrow("the adapter sent no hello");
    expect(peer.requests).toEqual([]);
    expect(peer.closed).toBe(true);
  });

  it("sends no request when only responses and events arrive before the deadline", async () => {
    const peer = new Peer();
    peer.onHello = () => queueMicrotask(() => peer.send(status, removed(1)));
    await expect(Connection.open(peer, { openTimeoutMs: 20 })).rejects.toThrow("the adapter sent no hello");
    expect(peer.requests).toEqual([]);
    expect(peer.closed).toBe(true);
  });

  it("opens with a hello that has no metadata", async () => {
    const { connection } = await open();
    expect(connection.hello.protocolVersion).toBe(CURRENT_PROTOCOL_VERSION);
    expect([connection.hello.firmwareVersion, connection.hello.serialNumber, connection.hello.name]).toEqual([undefined, undefined, undefined]);
    await connection.close();
  });

  it("decodes frames after the hello without the hello's size limit", async () => {
    const peer = new Peer();
    const events = vi.fn();
    const long = { kind: { case: "event", value: { kind: { case: "device", value: { id: 1, name: "x".repeat(2000) } } } } } as const;
    peer.onHello = () => queueMicrotask(() => peer.raw(new Uint8Array([
      ...encodeFrame(encodeAdapterHello(create(AdapterHelloSchema, { protocolVersion: CURRENT_PROTOCOL_VERSION }))),
      ...peer.frame(long),
    ])));
    peer.onRequest = () => queueMicrotask(() => peer.send(status));
    const connection = await Connection.open(peer, { onEvent: events });
    expect(events).toHaveBeenCalledOnce();
    expect(events.mock.calls[0]![0].kind.value.name).toHaveLength(2000);
    await connection.close();
  });

  it("keeps a partial frame that follows the hello in the same chunk", async () => {
    const peer = new Peer();
    const events = vi.fn();
    const tail = [...peer.frame(removed(1)), ...peer.frame({ kind: { case: "event", value: { kind: { case: "device", value: { id: 2, name: "x".repeat(2000) } } } } })];
    const split = tail.length - 500;
    peer.onHello = () => queueMicrotask(() => {
      peer.raw(new Uint8Array([...encodeFrame(encodeAdapterHello(create(AdapterHelloSchema, { protocolVersion: CURRENT_PROTOCOL_VERSION }))), ...tail.slice(0, split)]));
      peer.raw(new Uint8Array(tail.slice(split)));
    });
    peer.onRequest = () => queueMicrotask(() => peer.send(status));
    const connection = await Connection.open(peer, { onEvent: events });
    expect(events.mock.calls.map(([event]) => event.kind.case)).toEqual(["deviceRemoved", "device"]);
    expect(events.mock.calls[1]![0].kind.value.name).toHaveLength(2000);
    await connection.close();
  });

  it.each([CURRENT_PROTOCOL_VERSION + 1, CURRENT_PROTOCOL_VERSION + 100])("refuses an adapter on protocol %i without sending a request", async (version) => {
    const peer = new Peer();
    const closed = vi.fn();
    peer.onHello = () => queueMicrotask(() => peer.hello({ protocolVersion: version, firmwareVersion: "9.0.0", name: "Desk" }));
    const error = await Connection.open(peer, { onClose: closed }).catch((e: unknown) => e);
    expect(error).toBeInstanceOf(ProtocolVersionError);
    expect((error as ProtocolVersionError).hello).toMatchObject({ protocolVersion: version, firmwareVersion: "9.0.0", name: "Desk" });
    expect((error as ProtocolVersionError).hello.serialNumber).toBeUndefined();
    expect(peer.requests).toEqual([]);
    expect(peer.closed).toBe(true);
    expect(closed).not.toHaveBeenCalled();
  });

  it("gives up at the open deadline however much input keeps arriving", async () => {
    vi.useFakeTimers();
    try {
      const peer = new Peer();
      peer.onHello = () => {};
      const opening = Connection.open(peer, { openTimeoutMs: 500 }).catch((e: unknown) => e);
      // Empty frames, then bytes that never end a frame.
      peer.raw(new Uint8Array([0, 0]));
      for (let i = 0; i < 10; i++) {
        peer.raw(new Uint8Array([0x41, 0x42]));
        await vi.advanceTimersByTimeAsync(60);
      }
      expect(await opening).toMatchObject({ message: "the adapter sent no hello" });
      expect(peer.requests).toEqual([]);
      expect(peer.closed).toBe(true);
    } finally {
      vi.useRealTimers();
    }
  });

  it("includes the status in the open deadline", async () => {
    const peer = new Peer();
    await expect(Connection.open(peer, { openTimeoutMs: 20 })).rejects.toThrow("getStatus got no response");
    expect(peer.requests.map((r) => r.command.case)).toEqual(["getStatus"]);
    expect(peer.closed).toBe(true);
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
        id: "A1", errors: [{ code: ErrorCode.USB_REQUIRED_CAPACITY_EXCEEDED }, { code: ErrorCode.USB_RESERVED_CAPACITY_EXCEEDED }], profileSupport: { ranges, resources },
      } } } },
    }));
    const connection = await Connection.open(peer);
    expect(connection.status.errors.map((e) => e.code)).toEqual([ErrorCode.USB_REQUIRED_CAPACITY_EXCEEDED, ErrorCode.USB_RESERVED_CAPACITY_EXCEEDED]);
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
    peer.send(removed(2), { kind: { case: "response", value: { result: { case: "devices", value: { entries: [{ entry: { case: "device", value: { id: 1 } } }] } } } } }, removed(3));
    expect(order).toEqual(["response:getStatus", "event:deviceRemoved", "response:listDevices", "event:deviceRemoved"]);
    await listed;
    expect(order.at(-1)).toBe("resolved");
  });

  it("rejects an error response with every detail and keeps going", async () => {
    const { peer, connection } = await open();
    const failing = connection.setDevice({ device: 1, enabled: true });
    await Promise.resolve();
    const related = [{ code: ErrorCode.STORAGE_FAILED, reason: ErrorReason.STORAGE, outcome: ErrorOutcome.UNKNOWN, related: [{ code: ErrorCode.TIMEOUT }] }];
    peer.send({ kind: { case: "response", value: { result: { case: "error", value: { code: ErrorCode.NO_CAPACITY, reason: ErrorReason.ENABLED_DEVICES, outcome: ErrorOutcome.NO_EFFECT, related } } } } });
    const error = await failing.catch((e: unknown) => e);
    expect(error).toBeInstanceOf(CordialError);
    expect(error).toMatchObject({ command: "setDevice", code: ErrorCode.NO_CAPACITY, reason: ErrorReason.ENABLED_DEVICES, outcome: ErrorOutcome.NO_EFFECT });
    expect((error as CordialError).related).toMatchObject([{ code: ErrorCode.STORAGE_FAILED, reason: ErrorReason.STORAGE, outcome: ErrorOutcome.UNKNOWN, related: [{ code: ErrorCode.TIMEOUT }] }]);
    const next = connection.getStatus();
    await Promise.resolve();
    peer.send(status);
    expect((await next).name).toBe("Desk");
  });

  it("sends numeric IDs and setting changes as given", async () => {
    const { peer, connection } = await open();
    const list = connection.listProfiles();
    await Promise.resolve();
    peer.send({ kind: { case: "response", value: { result: { case: "profiles", value: { entries: [{ entry: { case: "profile", value: { id: 8, name: "Work" } } }] } } } } });
    expect(await list).toMatchObject({ entries: [{ entry: { case: "profile", value: { id: 8, name: "Work" } } }] });
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
    const [request, set] = peer.requests.slice(1);
    expect(request!.command).toMatchObject({ case: "listProfiles", value: {} });
    // False and zero stay explicit writes rather than reading as unset.
    const changes = set!.command.case === "setSettings" ? set!.command.value.changes.map((c) => c.group) : [];
    expect(changes).toMatchObject([
      { case: "wheel", value: { invert: { change: { case: "value", value: false } } } },
      { case: "pointerSensor", value: { index: 0, dpi: { change: { case: "value", value: 0n } } } },
      { case: "wheel", value: { invert: { change: { case: "forget" } } } },
    ]);
  });

  it("rejects a list response of the wrong kind", async () => {
    const { peer, connection } = await open();
    const pending = connection.listDevices();
    peer.send(status);
    await expect(pending).rejects.toBeInstanceOf(UnexpectedResponseError);
    await connection.close();
  });

  it("answers profile creation with the new ID", async () => {
    const { peer, connection } = await open();
    const created = connection.createProfile("Work");
    await Promise.resolve();
    peer.send({ kind: { case: "response", value: { result: { case: "profileCreated", value: { profile: 9 } } } } });
    expect(await created).toBe(9);
  });

  it("keeps the numbers of error details it doesn't know", () => {
    const error = new CordialError("x", { code: 999 as ErrorCode, reason: 998 as ErrorReason, outcome: 997 as ErrorOutcome, related: [{ code: 996 as ErrorCode, reason: 995 as ErrorReason }] });
    expect(error).toMatchObject({ code: 999, reason: 998, outcome: 997, related: [{ code: 996, reason: 995, outcome: 0, related: [] }] });
    expect(error.message).toBe("x failed: 999");
  });

  it("refuses a request over the size limit without sending it", async () => {
    const { peer, connection } = await open();
    const error = await connection.setAdapter({ name: "x".repeat(2000) }).catch((e: unknown) => e);
    expect(error).toMatchObject({ code: ErrorCode.TOO_LONG, outcome: ErrorOutcome.NO_EFFECT });
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
    peer.onHello = () => {};
    await expect(Connection.open(peer, { openTimeoutMs: 20 })).rejects.toThrow("the adapter sent no hello");
    expect(peer.closed).toBe(true);
  });
});

const listKinds = ["devices", "profiles", "settings", "profileRules", "features", "files"] as const;
type ListKind = typeof listKinds[number];

function listResult(kind: ListKind, start: number, count: number): NonNullable<MessageInitShape<typeof ResponseSchema>["result"]> {
  const ids = Array.from({ length: count }, (_, i) => start + i + 1);
  switch (kind) {
    case "devices": return { case: kind, value: { entries: ids.map((id) => ({ entry: { case: "error", value: { id, error: { code: ErrorCode.STORAGE_FAILED } } } })) } };
    case "profiles": return { case: kind, value: { entries: ids.map((id) => ({ entry: { case: "error", value: { id, error: { code: ErrorCode.STORAGE_FAILED } } } })) } };
    case "settings": return { case: kind, value: { device: 7, objects: ids.map((integration) => ({ integration })) } };
    case "profileRules": return { case: kind, value: { profile: 7, rules: ids.map((usage) => ({ input: { usagePage: 9, usage }, collection: { usagePage: 1, usage: 5 } })) } };
    case "features": return { case: kind, value: { features: ids.map((integration) => ({ integration })) } };
    case "files": return { case: kind, value: { entries: ids.map((id) => ({ name: `entry-${id}` })) } };
  }
}

function listMessage(kind: ListKind, start: number, count: number) {
  return create(MessageSchema, { kind: { case: "response", value: { result: listResult(kind, start, count) } } });
}

function readList(connection: Connection, kind: ListKind) {
  switch (kind) {
    case "devices": return connection.listDevices();
    case "profiles": return connection.listProfiles();
    case "settings": return connection.listSettings(7);
    case "profileRules": return connection.listProfileRules(7);
    case "features": return connection.listFeatures(7);
    case "files": return connection.listFiles("/records");
  }
}

function streamingFrame(messages: ReturnType<typeof listMessage>[]) {
  const pieces = messages.map((message) => toBinary(MessageSchema, message));
  const bytes = new Uint8Array(pieces.reduce((total, piece) => total + piece.length, 0));
  let offset = 0;
  for (const piece of pieces) {
    bytes.set(piece, offset);
    offset += piece.length;
  }
  return encodeFrame(bytes);
}

describe.each(listKinds)("streamed %s", (kind) => {
  it("merges ordinary nested messages into one complete list without followup requests", async () => {
    const responses = vi.fn();
    const { peer, connection } = await open({ onResponse: responses });
    responses.mockClear();
    const pending = readList(connection, kind);
    const expected = listMessage(kind, 0, 300);
    const frame = streamingFrame(Array.from({ length: 300 }, (_, i) => listMessage(kind, i, 1)));
    expect(frame.length).toBeGreaterThan(1024);
    for (let offset = 0; offset < frame.length - 1; offset += 17) {
      peer.raw(frame.subarray(offset, Math.min(offset + 17, frame.length - 1)));
    }
    expect(responses).not.toHaveBeenCalled();
    expect(peer.requests).toHaveLength(2);
    peer.raw(frame.subarray(-1));
    expect(expected.kind.case).toBe("response");
    if (expected.kind.case !== "response") throw new Error("missing response");
    expect(await pending).toEqual(expected.kind.value.result.value);
    expect(responses).toHaveBeenCalledOnce();
    expect(peer.requests).toHaveLength(2);
    await connection.close();
  });

  it("accepts an explicitly selected empty list", async () => {
    const { peer, connection } = await open();
    const pending = readList(connection, kind);
    const message = listMessage(kind, 0, 0);
    peer.raw(streamingFrame([message]));
    if (message.kind.case !== "response") throw new Error("missing response");
    expect(await pending).toEqual(message.kind.value.result.value);
    expect(peer.requests).toHaveLength(2);
    await connection.close();
  });

  it("replaces partial list data with a terminal error before notifying listeners", async () => {
    const responses = vi.fn();
    const { peer, connection } = await open({ onResponse: responses });
    responses.mockClear();
    const pending = readList(connection, kind);
    const error = create(MessageSchema, { kind: { case: "response", value: { result: { case: "error", value: { code: ErrorCode.NOT_READY } } } } });
    peer.raw(streamingFrame([listMessage(kind, 0, 1), error]));
    await expect(pending).rejects.toMatchObject({ code: ErrorCode.NOT_READY });
    expect(responses).toHaveBeenCalledOnce();
    expect(responses.mock.calls[0]![1].result.case).toBe("error");
    await connection.close();
  });
});

describe("stream progress", () => {
  it("keeps a progressing response alive beyond the timeout and still times out a stall", async () => {
    vi.useFakeTimers();
    try {
      const { peer, connection } = await open({ timeoutMs: 100 });
      const pending = connection.listDevices();
      const frame = streamingFrame([listMessage("devices", 0, 20)]);
      for (let offset = 0; offset < frame.length; offset += 10) {
        await vi.advanceTimersByTimeAsync(60);
        peer.raw(frame.subarray(offset, offset + 10));
        expect(connection.closed).toBe(false);
      }
      expect((await pending).entries).toHaveLength(20);
      const stalled = connection.listDevices().catch((error: unknown) => error);
      peer.raw(frame.subarray(0, 5));
      await vi.advanceTimersByTimeAsync(101);
      expect(await stalled).toBeInstanceOf(ConnectionClosedError);
    } finally {
      vi.useRealTimers();
    }
  });

  it("discards incomplete data when the session closes", async () => {
    const responses = vi.fn();
    const { peer, connection } = await open({ onResponse: responses });
    responses.mockClear();
    const pending = connection.listDevices().catch((error: unknown) => error);
    const frame = streamingFrame([listMessage("devices", 0, 20)]);
    peer.raw(frame.subarray(0, -1));
    peer.unplug();
    peer.raw(frame.subarray(-1));
    expect(await pending).toBeInstanceOf(ConnectionClosedError);
    expect(responses).not.toHaveBeenCalled();
    const next = await open();
    const fresh = next.connection.listDevices();
    next.peer.raw(streamingFrame([listMessage("devices", 0, 0)]));
    expect((await fresh).entries).toEqual([]);
    await next.connection.close();
  });
});


it("includes the command envelope in the request limit and never splits profile rule writes", async () => {
  const { peer, connection } = await open();
  let name = "";
  while (toBinary(RequestSchema, create(RequestSchema, { command: { case: "setAdapter", value: { name } } })).length < 1024) name += "x";
  const accepted = connection.setAdapter({ name });
  peer.send(ok);
  await accepted;
  expect(toBinary(RequestSchema, peer.requests[1]!).length).toBe(1024);
  await expect(connection.setAdapter({ name: name + "x" })).rejects.toMatchObject({ code: ErrorCode.TOO_LONG });
  const change = { change: { case: "forget", value: { input: { usagePage: 9, usage: 1 }, collection: { usagePage: 1, usage: 5 } } } } as const;
  await expect(connection.setProfileRules({ profile: 7, changes: Array.from({ length: 100 }, () => change) })).rejects.toMatchObject({ code: ErrorCode.TOO_LONG });
  expect(peer.requests).toHaveLength(2);
  await connection.close();
});
