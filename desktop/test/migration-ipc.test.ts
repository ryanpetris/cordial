import { EventEmitter } from "node:events";
import { chmodSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { delimiter, join } from "node:path";
import { PassThrough } from "node:stream";
import type { ChildProcessWithoutNullStreams } from "node:child_process";
import { create, fromBinary, toBinary } from "@bufbuild/protobuf";
import { MIGRATION_MAX_FRAME_BYTES, MigrationControlSchema, MigrationEventSchema, type MigrationEvent } from "@cordial/protocol";
import { describe, expect, it, vi } from "vitest";
import { FrameReader, findExecutable, frame, migrationHost } from "../src/node/migration.ts";
import type { MigrationHandlers } from "../src/core/migration.ts";

const prefix = (length: number) => {
  const bytes = new Uint8Array(4);
  new DataView(bytes.buffer).setUint32(0, length, true);
  return bytes;
};

const eventFrame = (event: Parameters<typeof create<typeof MigrationEventSchema>>[1]) =>
  frame(toBinary(MigrationEventSchema, create(MigrationEventSchema, event)));

function fakeChild() {
  return Object.assign(new EventEmitter(), { stdout: new PassThrough(), stderr: new PassThrough(), stdin: new PassThrough(), pid: 1 as number | undefined });
}

function start(child = fakeChild()) {
  const log = vi.fn();
  const spawn = vi.fn(() => child as unknown as ChildProcessWithoutNullStreams);
  const events: MigrationEvent[] = [];
  const handlers: MigrationHandlers = { event: (e) => events.push(e), malformed: vi.fn(), exit: vi.fn() };
  const process = migrationHost(log, spawn, () => "/cli").start("/cli", handlers);
  return { child, log, spawn, events, handlers, process };
}

const tick = () => new Promise((r) => setImmediate(r));

describe("migration IPC framing", () => {
  it("joins frames split at any byte and splits frames sharing a chunk", () => {
    const a = new Uint8Array([1, 2, 3]);
    const b = new Uint8Array([4]);
    const bytes = new Uint8Array([...frame(a), ...frame(b)]);
    const reader = new FrameReader();
    const frames = [...bytes].flatMap((byte) => reader.push(new Uint8Array([byte])));
    expect(frames).toEqual([a, b]);
    expect(reader.partial).toBe(false);
    expect(new FrameReader().push(bytes)).toEqual([a, b]);
  });

  it("refuses a zero or oversized length before its payload arrives", () => {
    expect(() => new FrameReader().push(prefix(0))).toThrow();
    expect(() => new FrameReader().push(prefix(MIGRATION_MAX_FRAME_BYTES + 1))).toThrow();
    const reader = new FrameReader();
    expect(reader.push(prefix(MIGRATION_MAX_FRAME_BYTES))).toEqual([]);
    expect(reader.partial).toBe(true);
    expect(reader.push(new Uint8Array(MIGRATION_MAX_FRAME_BYTES))).toHaveLength(1);
  });
});

describe("migration host", () => {
  it("starts the CLI without a shell and sends framed controls", () => {
    const { spawn, child, process } = start();
    expect(spawn).toHaveBeenCalledWith("/cli", ["migrate", "--ipc"]);
    process.send({ action: { case: "start", value: { adapterId: "000000000000A001", portHint: "/a" } } });
    process.send({ action: { case: "cancel", value: {} } });
    const frames = new FrameReader().push(child.stdin.read() as Uint8Array);
    expect(frames.map((f) => fromBinary(MigrationControlSchema, f).action.case)).toEqual(["start", "cancel"]);
    expect(fromBinary(MigrationControlSchema, frames[0]!).action.value).toMatchObject({ adapterId: "000000000000A001", portHint: "/a" });
  });

  it("decodes events split across chunks, including ones this app doesn't know", () => {
    const { child, events, handlers } = start();
    // Field 99 is an event this app doesn't know.
    const bytes = new Uint8Array([...eventFrame({ event: { case: "waiting", value: { adapterId: "A" } } }), ...frame(new Uint8Array([0x98, 0x06, 0x01]))]);
    child.stdout.write(bytes.subarray(0, 3));
    child.stdout.write(bytes.subarray(3, 9));
    child.stdout.write(bytes.subarray(9));
    expect(events.map((e) => e.event.case)).toEqual(["waiting", undefined]);
    expect(handlers.malformed).not.toHaveBeenCalled();
  });

  it.each([
    ["an undecodable message", frame(new Uint8Array([0xff]))],
    ["an oversized length", prefix(MIGRATION_MAX_FRAME_BYTES + 1)],
    ["a zero length", prefix(0)],
  ])("stops reading events after %s", (_, bad) => {
    const { child, events, handlers } = start();
    child.stdout.write(new Uint8Array([...bad, ...eventFrame({ event: { case: "success", value: { adapterId: "A", targetFormat: 2 } } })]));
    child.stdout.write(eventFrame({ event: { case: "success", value: { adapterId: "A", targetFormat: 2 } } }));
    expect(events).toEqual([]);
    expect(handlers.malformed).toHaveBeenCalledTimes(1);
  });

  it("reports output that ends inside a frame", async () => {
    const { child, handlers } = start();
    child.stdout.end(eventFrame({ event: { case: "cancelled", value: {} } }).subarray(0, 3));
    await tick();
    expect(handlers.malformed).toHaveBeenCalledTimes(1);
  });

  it("logs stderr lines cleaned and bounded", async () => {
    const { child, log } = start();
    child.stderr.write("first\x1b[31m line\nsecond ");
    child.stderr.write(`${"x".repeat(5000)}\n`);
    child.stderr.end("last");
    await tick();
    const lines = log.mock.calls.map(([m]) => m as string);
    expect(lines[0]).toBe("cordial migrate: first [31m line");
    expect(lines[1]!.length).toBeLessThanOrEqual("cordial migrate: ".length + 1024);
    expect(lines[2]).toBe("cordial migrate: last");
  });

  it("reports the exit only once the process has exited and closed its output", () => {
    const { child, handlers } = start();
    child.emit("exit", 0, null);
    expect(handlers.exit).not.toHaveBeenCalled();
    child.emit("close", 0, null);
    child.emit("close", 0, null);
    expect(handlers.exit).toHaveBeenCalledExactlyOnceWith(0, null);
  });

  it("reports a CLI that couldn't start as exited", async () => {
    const missing = start(Object.assign(fakeChild(), { pid: undefined }));
    missing.child.emit("error", new Error("spawn ENOENT"));
    missing.child.emit("close", -2, null);
    expect(missing.handlers.exit).toHaveBeenCalledExactlyOnceWith(null, null);

    const handlers: MigrationHandlers = { event: vi.fn(), malformed: vi.fn(), exit: vi.fn() };
    migrationHost(vi.fn(), () => { throw new Error("EACCES"); }, () => "/cli").start("/cli", handlers);
    expect(handlers.exit).not.toHaveBeenCalled();
    await tick();
    expect(handlers.exit).toHaveBeenCalledExactlyOnceWith(null, null);
  });

  it("finds an executable on PATH without running it", () => {
    const dir = mkdtempSync(join(tmpdir(), "cordial-path-"));
    const other = mkdtempSync(join(tmpdir(), "cordial-path-"));
    try {
      writeFileSync(join(dir, "cordial"), "");
      writeFileSync(join(other, "cordial"), "");
      chmodSync(join(other, "cordial"), 0o755);
      expect(findExecutable("cordial", [dir, other].join(delimiter))).toBe(join(other, "cordial"));
      expect(findExecutable("cordial", dir)).toBeNull();
    } finally {
      rmSync(dir, { recursive: true });
      rmSync(other, { recursive: true });
    }
  });
});
