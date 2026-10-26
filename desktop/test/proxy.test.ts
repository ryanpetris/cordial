import type { ChildProcessWithoutNullStreams } from "node:child_process";
import { EventEmitter } from "node:events";
import { chmodSync, mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { PassThrough, Writable } from "node:stream";
import { create } from "@bufbuild/protobuf";
import { AdapterHelloSchema, CURRENT_PROTOCOL_VERSION, OPERATIONAL_SCHEMA_SHA256, encodeAdapterHello, encodeFrame, type AdapterHello } from "@cordial/protocol";
import { afterAll, describe, expect, it, vi } from "vitest";
import { HelperError } from "../src/core/manager.ts";
import { findCli, openProxy } from "../src/node/proxy.ts";
import { until } from "./helpers.ts";

/** A child process the test plays. It exits when its input ends and on SIGTERM unless told to
 * linger or ignore it. */
class Child extends EventEmitter {
  readonly stdin: Writable;
  readonly stdout = new PassThrough();
  readonly stderr = new PassThrough();
  pid: number | undefined = 1234;
  exitCode: number | null = null;
  signalCode: NodeJS.Signals | null = null;
  readonly signals: string[] = [];
  readonly input: Buffer[] = [];
  inputEnded = false;
  lingers = false;
  ignoreTerm = false;
  /** Holds each write's callback until released, as a full pipe would. */
  holdWrites = false;
  readonly #held: (() => void)[] = [];

  constructor() {
    super();
    this.stdin = new Writable({
      write: (chunk: Buffer, _encoding, done) => {
        this.input.push(chunk);
        if (this.holdWrites) this.#held.push(() => done());
        else done();
      },
      final: (done) => {
        this.inputEnded = true;
        if (!this.lingers) setImmediate(() => this.exit(0));
        done();
      },
    });
  }
  releaseWrites() {
    for (const done of this.#held.splice(0)) done();
  }
  kill(signal: NodeJS.Signals) {
    this.signals.push(signal);
    if (signal === "SIGKILL" || !this.ignoreTerm) this.exit(null, signal);
    return true;
  }
  exit(code: number | null, signal: NodeJS.Signals | null = null) {
    if (this.exitCode !== null || this.signalCode !== null) return;
    this.exitCode = code;
    this.signalCode = signal;
    this.emit("exit", code, signal);
    this.stdout.end();
    this.stderr.end();
    setImmediate(() => this.emit("close", code, signal));
  }
  /** Writes the startup record, and `after` in the same chunk. */
  startup(record: object, after: number[] = []) {
    this.stdout.write(Buffer.from([...encodeFrame(new TextEncoder().encode(JSON.stringify(record))), ...after]));
  }
}

const native = create(AdapterHelloSchema, { protocolVersion: 1, serialNumber: "S1", firmwareVersion: "0.9.0" });
const hex = (hello: AdapterHello) => Buffer.from(encodeAdapterHello(hello)).toString("hex");
const ready = (patch: object = {}) => ({ kind: "cordial-proxy-startup", protocol: CURRENT_PROTOCOL_VERSION, schema: OPERATIONAL_SCHEMA_SHA256, result: "ready", nativeHello: hex(native), ...patch });

const dir = mkdtempSync(join(tmpdir(), "cordial-proxy-"));
const executable = join(dir, "cordial");
writeFileSync(executable, "");
chmodSync(executable, 0o755);
afterAll(() => rmSync(dir, { recursive: true, force: true }));

function start(options: { signal?: AbortSignal } = {}) {
  const child = new Child();
  const spawn = vi.fn(() => child as unknown as ChildProcessWithoutNullStreams);
  const log = vi.fn();
  const started = openProxy("/dev/port", native, options.signal ?? new AbortController().signal, { log, spawn, env: { CORDIAL_CLI_PATH: executable }, home: dir });
  return { child, spawn, log, started, opening: started.then((s) => s.stream) };
}

describe("the Cordial CLI proxy", () => {
  it("starts the CLI with the exact arguments and passes the session through, including bytes after the startup record", async () => {
    const { child, spawn, started } = start();
    child.startup(ready(), [0, 7, 8]);
    const { stream, hello } = await started;
    expect(hello).toMatchObject({ protocolVersion: 1, serialNumber: "S1", firmwareVersion: "0.9.0" });
    expect(spawn).toHaveBeenCalledWith(executable, ["--port", "/dev/port", "proxy", "--protocol", String(CURRENT_PROTOCOL_VERSION), "--schema", OPERATIONAL_SCHEMA_SHA256], { shell: false, stdio: ["pipe", "pipe", "pipe"] });
    const data: number[][] = [];
    stream.onData((chunk) => data.push([...chunk]));
    child.stdout.write(Buffer.from([9]));
    await new Promise(setImmediate);
    expect(data).toEqual([[0, 7, 8], [9]]);
    await stream.write(new Uint8Array([1, 2]));
    expect(Buffer.concat(child.input)).toEqual(Buffer.from([1, 2]));
    await stream.close();
  });

  it("reads a startup record split across chunks", async () => {
    const { child, opening } = start();
    const bytes = encodeFrame(new TextEncoder().encode(JSON.stringify(ready())));
    for (const byte of bytes) child.stdout.write(Buffer.from([byte]));
    const stream = await opening;
    await stream.close();
  });

  it("logs the CLI's diagnostics line by line, keeping long lines bounded", async () => {
    const { child, log, opening } = start();
    child.stderr.write("first\nsec");
    child.stderr.write(`ond\n${"x".repeat(5000)}`);
    child.startup(ready());
    const stream = await opening;
    child.exit(0);
    await new Promise((r) => setTimeout(r, 10));
    expect(log.mock.calls.map(([m]) => m)).toEqual(["Cordial CLI: first", "Cordial CLI: second", `Cordial CLI: ${"x".repeat(1024)}`]);
    await stream.close();
  });

  it("waits for each write to reach the CLI's input", async () => {
    const { child, opening } = start();
    child.startup(ready());
    const stream = await opening;
    child.holdWrites = true;
    let written = false;
    const writing = stream.write(new Uint8Array([1])).then(() => (written = true));
    await new Promise(setImmediate);
    expect(written).toBe(false);
    child.releaseWrites();
    await writing;
    await stream.close();
  });

  it.each([
    ["another protocol", ready({ protocol: CURRENT_PROTOCOL_VERSION + 1 })],
    ["another schema", ready({ schema: "0".repeat(64) })],
    ["incompatible-client", ready({ result: "incompatible-client" })],
    ["another kind of record", ready({ kind: "other" })],
    ["an unknown result", ready({ result: "other" })],
    ["no adapter hello", ready({ nativeHello: null })],
    ["an invalid adapter hello", ready({ nativeHello: "0800" })],
    ["uppercase hex", ready({ nativeHello: hex(native).toUpperCase() })],
  ])("refuses a CLI that reports %s as incompatible, once it has exited", async (_, record) => {
    const { child, opening } = start();
    child.startup(record);
    const error = await opening.catch((e: unknown) => e);
    expect(error).toBeInstanceOf(HelperError);
    expect(error).toMatchObject({ kind: "incompatible" });
    expect(child.inputEnded).toBe(true);
    expect(child.exitCode !== null || child.signalCode !== null).toBe(true);
  });

  it("refuses a startup record that isn't JSON or is too long", async () => {
    for (const bytes of [new Uint8Array([0xff, 0xfe]), new TextEncoder().encode(JSON.stringify({ ...ready(), padding: "x".repeat(4096) }))]) {
      const { child, opening } = start();
      child.stdout.write(Buffer.from(encodeFrame(bytes)));
      await expect(opening).rejects.toMatchObject({ kind: "incompatible" });
    }
  });

  it("reports an adapter the CLI doesn't support", async () => {
    const { child, opening } = start();
    child.startup(ready({ result: "unsupported-native", nativeHello: null }));
    await expect(opening).rejects.toMatchObject({ kind: "unsupported" });
  });

  it("reads the startup record of a CLI that exits right after writing it", async () => {
    const { child, opening } = start();
    // Node can report the exit before the output that preceded it.
    child.emit("exit", 1, null);
    child.startup(ready({ result: "unsupported-native", nativeHello: null }));
    child.exit(1);
    await expect(opening).rejects.toMatchObject({ kind: "unsupported" });
  });

  it.each(["open-failed", "handshake-failed"])("fails without a refusal when the CLI reports %s", async (result) => {
    const { child, opening } = start();
    child.startup(ready({ result, nativeHello: null }));
    const error = await opening.catch((e: unknown) => e);
    expect(error).not.toBeInstanceOf(HelperError);
    expect((error as Error).message).toContain(result);
  });

  it("fails without a refusal when another adapter answered the CLI", async () => {
    for (const hello of [{ ...native, serialNumber: "S2" }, { ...native, protocolVersion: 3 }]) {
      const { child, opening } = start();
      child.startup(ready({ nativeHello: hex(create(AdapterHelloSchema, hello)) }));
      const error = await opening.catch((e: unknown) => e);
      expect(error).not.toBeInstanceOf(HelperError);
      expect((error as Error).message).toBe("another adapter answered the Cordial CLI");
    }
  });

  it("accepts an adapter hello without a serial number", async () => {
    const { child, opening } = start();
    child.startup(ready({ nativeHello: hex(create(AdapterHelloSchema, { protocolVersion: 1 })) }));
    await (await opening).close();
  });

  it.each([0, 2])("treats CLI exit %s before startup as a disconnect", async (code) => {
    const exited = start();
    exited.child.exit(code);
    const error = await exited.opening.catch((error: unknown) => error);
    expect(error).toBeInstanceOf(Error);
    expect(error).not.toBeInstanceOf(HelperError);
    expect((error as Error).message).toBe("the Cordial CLI exited");
  });

  it("treats a CLI that can't start as missing", async () => {
    const failed = start();
    failed.child.pid = undefined;
    failed.child.emit("error", new Error("spawn EACCES"));
    failed.child.exit(-13);
    await expect(failed.opening).rejects.toMatchObject({ kind: "missing" });
  });

  it("stops a CLI that doesn't start in time with SIGTERM, then SIGKILL, and settles once it has exited", async () => {
    vi.useFakeTimers();
    try {
      const { child, opening } = start();
      child.lingers = true;
      child.ignoreTerm = true;
      let settled = false;
      const result = opening.catch((e: unknown) => e).finally(() => (settled = true));
      await vi.advanceTimersByTimeAsync(5000);
      expect(child.inputEnded).toBe(true);
      expect(child.signals).toEqual([]);
      await vi.advanceTimersByTimeAsync(1000);
      expect(child.signals).toEqual(["SIGTERM"]);
      expect(settled).toBe(false);
      await vi.advanceTimersByTimeAsync(1000);
      expect(child.signals).toEqual(["SIGTERM", "SIGKILL"]);
      await vi.runAllTimersAsync();
      vi.useRealTimers();
      const error = await result;
      expect(error).not.toBeInstanceOf(HelperError);
      expect((error as Error).message).toBe("the Cordial CLI didn't start in time");
    } finally {
      vi.useRealTimers();
    }
  });

  it("stops a starting CLI when cancelled and settles once it has exited", async () => {
    const abort = new AbortController();
    const { child, opening } = start({ signal: abort.signal });
    abort.abort();
    await expect(opening).rejects.toThrow("opening cancelled");
    expect(child.inputEnded).toBe(true);
    expect(child.exitCode !== null || child.signalCode !== null).toBe(true);
  });

  it.each([
    ["exits", (child: Child) => child.exit(0)],
    ["is killed", (child: Child) => child.exit(null, "SIGSEGV")],
    ["closes its output", (child: Child) => child.stdout.end()],
    ["fails", (child: Child) => child.emit("error", new Error("EIO"))],
  ])("ends the stream once, after the CLI has exited, when the CLI %s", async (_, end) => {
    const { child, opening } = start();
    child.startup(ready());
    const stream = await opening;
    const closed = vi.fn();
    stream.onClose(closed);
    end(child);
    await until(() => closed.mock.calls.length > 0);
    expect(closed).toHaveBeenCalledOnce();
    expect(child.exitCode !== null || child.signalCode !== null).toBe(true);
    await expect(stream.write(new Uint8Array([1]))).rejects.toBeInstanceOf(Error);
    await stream.close();
    expect(closed).toHaveBeenCalledOnce();
  });

  it("closes by ending the CLI's input and settles once it has exited, without reporting a close", async () => {
    const { child, opening } = start();
    child.startup(ready());
    const stream = await opening;
    const closed = vi.fn();
    stream.onClose(closed);
    child.lingers = true;
    child.stdin.on("finish", () => setTimeout(() => child.exit(0), 20));
    await stream.close();
    expect(child.exitCode).toBe(0);
    expect(child.signals).toEqual([]);
    expect(closed).not.toHaveBeenCalled();
  });
});

it("runs a real process as the CLI, carries its session and waits for it to exit", async () => {
  const script = join(dir, "echo-cli");
  const record = JSON.stringify(ready());
  // The record has no zero byte and is shorter than 254 bytes, so its COBS encoding is one block.
  writeFileSync(script, `#!/usr/bin/env node
const record = Buffer.from(${JSON.stringify(record)});
process.stdout.write(Buffer.concat([Buffer.from([record.length + 1]), record, Buffer.from([0])]));
process.stderr.write(process.argv.slice(2).join(" ") + "\\n");
process.stdin.pipe(process.stdout);
`);
  chmodSync(script, 0o755);
  const log = vi.fn();
  const { stream } = await openProxy("/dev/port", native, new AbortController().signal, { log, env: { CORDIAL_CLI_PATH: script }, home: dir });
  const received: number[] = [];
  stream.onData((chunk) => received.push(...chunk));
  const closed = vi.fn();
  stream.onClose(closed);
  await stream.write(new Uint8Array([5, 6, 0]));
  await until(() => received.length === 3);
  expect(received).toEqual([5, 6, 0]);
  await stream.close();
  expect(log).toHaveBeenCalledWith(`Cordial CLI: --port /dev/port proxy --protocol ${CURRENT_PROTOCOL_VERSION} --schema ${OPERATIONAL_SCHEMA_SHA256}`);
  expect(closed).not.toHaveBeenCalled();
});

describe("finding the Cordial CLI", () => {
  const home = join(dir, "home");
  const local = join(home, ".local", "bin", "cordial");
  mkdirSync(join(home, ".local", "bin"), { recursive: true });

  it("uses an absolute CORDIAL_CLI_PATH without looking elsewhere", () => {
    writeFileSync(local, "");
    chmodSync(local, 0o755);
    expect(findCli({ CORDIAL_CLI_PATH: executable }, home)).toBe(executable);
    for (const path of ["cordial", "./cordial", join(dir, "absent"), dir]) expect(() => findCli({ CORDIAL_CLI_PATH: path }, home)).toThrow(HelperError);
    rmSync(local);
  });

  it("uses the CLI installed for the user first", () => {
    writeFileSync(local, "");
    chmodSync(local, 0o755);
    expect(findCli({}, home)).toBe(local);
    rmSync(local);
  });

  it("refuses the first candidate that isn't executable rather than passing over it", () => {
    writeFileSync(local, "");
    chmodSync(local, 0o644);
    expect(() => findCli({}, home)).toThrow(`${local} is not executable`);
    expect(() => findCli({ CORDIAL_CLI_PATH: local }, home)).toThrow(HelperError);
    rmSync(local);
  });
});
