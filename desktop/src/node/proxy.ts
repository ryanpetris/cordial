// The Cordial CLI as a converter for an adapter on an older protocol revision.
// The CLI opens the port, completes the adapter's handshake and writes one
// startup frame: COBS-encoded UTF-8 JSON with its compiled protocol revision
// and schema, the result and the adapter's hello. After `ready`, its standard
// input and output carry a session on the current protocol, starting with the
// hello. The CLI never reopens the port. It exits when the session ends, and
// its exit ends the stream like an unplugged port.
import { spawn, type ChildProcessWithoutNullStreams } from "node:child_process";
import { accessSync, constants, statSync } from "node:fs";
import { homedir } from "node:os";
import { isAbsolute, join } from "node:path";
import type { ByteStream } from "@cordial/client";
import { CURRENT_PROTOCOL_VERSION, DELIMITER, FrameDecoder, OPERATIONAL_SCHEMA_SHA256, decodeAdapterHello, type AdapterHello } from "@cordial/protocol";
import { HelperError } from "../core/manager.ts";

/** The longest startup record, decoded. An adapter hello of 1024 bytes takes 2048 in hex. */
const STARTUP_BYTES = 4096;
/** How long the CLI may take to write its startup record. */
const READY_MS = 5000;
/** How long the CLI has to exit after its input ends, and again after SIGTERM, before SIGKILL. */
const EXIT_MS = 1000;
/** The longest stderr line logged. */
const LINE_CHARS = 1024;
const RESULTS = ["ready", "incompatible-client", "unsupported-native", "open-failed", "handshake-failed"];

export interface ProxyHost {
  log(message: string): void;
  env?: NodeJS.ProcessEnv;
  home?: string;
  spawn?: (command: string, args: string[], options: { shell: false; stdio: ["pipe", "pipe", "pipe"] }) => ChildProcessWithoutNullStreams;
}

/**
 * The CLI to start: `CORDIAL_CLI_PATH` when set, otherwise the first of the usual install
 * locations that exists. A candidate that isn't an executable regular file is refused rather than
 * passed over.
 */
export function findCli(env: NodeJS.ProcessEnv, home: string): string {
  const usable = (path: string) => {
    if (!statSync(path).isFile()) throw new HelperError("missing", `${path} is not a regular file`);
    try {
      accessSync(path, constants.X_OK);
    } catch {
      throw new HelperError("missing", `${path} is not executable`);
    }
    return path;
  };
  const override = env.CORDIAL_CLI_PATH;
  if (override) {
    if (!isAbsolute(override)) throw new HelperError("missing", "CORDIAL_CLI_PATH is not an absolute path");
    try {
      return usable(override);
    } catch (error) {
      throw error instanceof HelperError ? error : new HelperError("missing", `CORDIAL_CLI_PATH: ${(error as Error).message}`);
    }
  }
  for (const path of [join(home, ".local", "bin", "cordial"), "/usr/local/bin/cordial", "/usr/bin/cordial"]) {
    try {
      statSync(path);
    } catch (error) {
      if ((error as NodeJS.ErrnoException).code === "ENOENT") continue;
      throw new HelperError("missing", `${path}: ${(error as Error).message}`);
    }
    return usable(path);
  }
  throw new HelperError("missing", "the Cordial CLI is not installed");
}

/** The CLI's output after its startup record, and its input. Ending in any way stops the CLI, and
 * close listeners hear of it once the CLI has exited. */
class CliStream implements ByteStream {
  readonly #child: ChildProcessWithoutNullStreams;
  readonly #exited: Promise<void>;
  readonly #decoder = new FrameDecoder(STARTUP_BYTES);
  #startup: { resolve(bytes: Uint8Array): void; reject(error: Error): void } | null = null;
  readonly startup = new Promise<Uint8Array>((resolve, reject) => (this.#startup = { resolve, reject }));
  #dataListeners: ((chunk: Uint8Array) => void)[] = [];
  #closeListeners: ((error: Error | null) => void)[] = [];
  /** Output after the startup record that arrived before anything listened. */
  #buffered: Uint8Array[] = [];
  #ended: Error | null = null;
  #stopping: Promise<void> | null = null;

  constructor(child: ChildProcessWithoutNullStreams, log: (message: string) => void) {
    this.#child = child;
    this.#exited = new Promise((resolve) => child.once("close", () => resolve()));
    // The startup promise may be abandoned once the CLI is stopping.
    this.startup.catch(() => {});
    child.stdout.on("data", (chunk: Uint8Array) => this.#receive(new Uint8Array(chunk.buffer, chunk.byteOffset, chunk.length)));
    // The end of output follows everything the CLI wrote, which its exit event may not.
    child.stdout.once("end", () => this.#end(new Error("the Cordial CLI exited")));
    child.stdout.on("error", (error) => this.#end(error));
    child.stdin.on("error", (error) => this.#end(error));
    child.once("error", (error) => this.#end(error));
    let line = "";
    child.stderr.setEncoding("utf8");
    child.stderr.on("data", (text: string) => {
      const lines = (line + text).split("\n");
      line = lines.pop()!.slice(0, LINE_CHARS);
      for (const l of lines) if (l.trim()) log(`Cordial CLI: ${l.slice(0, LINE_CHARS)}`);
    });
    child.stderr.on("end", () => line.trim() && log(`Cordial CLI: ${line}`));
    child.stderr.on("error", () => {});
  }

  #receive(chunk: Uint8Array) {
    let start = 0;
    // The startup record ends at the first delimiter; what follows belongs to the session.
    while (this.#startup && start < chunk.length) {
      const end = chunk.indexOf(DELIMITER, start);
      const next = end === -1 ? chunk.length : end + 1;
      for (const frame of this.#decoder.push(chunk.subarray(start, next))) {
        const startup = this.#startup!;
        this.#startup = null;
        if (frame.ok) startup.resolve(frame.bytes);
        else startup.reject(new HelperError("incompatible", `the Cordial CLI's startup record is ${frame.error === "too_long" ? "too long" : "malformed"}`));
      }
      start = next;
    }
    if (start === chunk.length || this.#ended) return;
    const rest = chunk.subarray(start);
    if (this.#dataListeners.length) for (const listener of this.#dataListeners) listener(rest);
    else this.#buffered.push(rest.slice());
  }

  #end(error: Error) {
    if (this.#ended) return;
    this.#ended = error;
    this.#startup?.reject(error);
    this.#startup = null;
    void this.#stop().then(() => {
      for (const listener of this.#closeListeners.splice(0)) listener(error);
    });
  }

  /** Ends the CLI's input, then signals it until it exits. Settles once it has exited. */
  #stop() {
    return (this.#stopping ??= (async () => {
      const child = this.#child;
      child.stdin.end();
      const term = setTimeout(() => child.kill("SIGTERM"), EXIT_MS);
      const kill = setTimeout(() => child.kill("SIGKILL"), 2 * EXIT_MS);
      await this.#exited;
      clearTimeout(term);
      clearTimeout(kill);
    })());
  }

  /** Stops the CLI with `error` as the reason, as at a timeout or cancellation. */
  fail(error: Error) {
    this.#end(error);
  }

  onData(listener: (chunk: Uint8Array) => void) {
    this.#dataListeners.push(listener);
    for (const chunk of this.#buffered.splice(0)) listener(chunk);
  }
  onClose(listener: (error: Error | null) => void) {
    this.#closeListeners.push(listener);
  }
  write(bytes: Uint8Array) {
    return new Promise<void>((resolve, reject) => {
      if (this.#ended) return reject(this.#ended);
      this.#child.stdin.write(bytes, (error) => (error ? reject(error) : resolve()));
    });
  }
  close() {
    this.#closeListeners = [];
    this.#end(new Error("closed"));
    return this.#stop();
  }
}

/** Checks the CLI's startup record against this app's revision and the adapter's direct hello, and
 * returns the hello the adapter gave the CLI. */
function checkStartup(bytes: Uint8Array, native: AdapterHello): AdapterHello {
  let record: Record<string, unknown> | null = null;
  try {
    record = JSON.parse(new TextDecoder("utf-8", { fatal: true }).decode(bytes));
  } catch {}
  if (typeof record !== "object" || record === null || record.kind !== "cordial-proxy-startup" || !RESULTS.includes(record.result as string))
    throw new HelperError("incompatible", "the Cordial CLI sent an unexpected startup record");
  if (record.protocol !== CURRENT_PROTOCOL_VERSION || record.schema !== OPERATIONAL_SCHEMA_SHA256 || record.result === "incompatible-client")
    throw new HelperError("incompatible", `the Cordial CLI uses protocol ${String(record.protocol)} with schema ${String(record.schema)}`);
  if (record.result === "unsupported-native") throw new HelperError("unsupported", `the Cordial CLI doesn't support protocol ${native.protocolVersion}`);
  if (record.result !== "ready") throw new Error(`the Cordial CLI couldn't open the adapter: ${String(record.result)}`);
  const hex = record.nativeHello;
  if (typeof hex !== "string" || !/^(?:[0-9a-f]{2})+$/.test(hex)) throw new HelperError("incompatible", "the Cordial CLI sent no adapter hello");
  let hello: AdapterHello;
  try {
    hello = decodeAdapterHello(Buffer.from(hex, "hex"));
  } catch (error) {
    throw new HelperError("incompatible", `the Cordial CLI sent an invalid adapter hello: ${(error as Error).message}`);
  }
  if (hello.protocolVersion !== native.protocolVersion || (hello.serialNumber !== undefined && native.serialNumber !== undefined && hello.serialNumber !== native.serialNumber))
    throw new Error("another adapter answered the Cordial CLI");
  return hello;
}

/**
 * Starts the CLI for the adapter on `path`, which answered directly with the older `native` hello,
 * and resolves once the CLI is ready with its stream and the hello the adapter gave the CLI. Every
 * failure stops the CLI and settles only once it has exited.
 */
export async function openProxy(path: string, native: AdapterHello, signal: AbortSignal, host: ProxyHost): Promise<{ stream: ByteStream; hello: AdapterHello }> {
  const cli = findCli(host.env ?? process.env, host.home ?? homedir());
  if (signal.aborted) throw new Error("opening cancelled");
  const child = (host.spawn ?? spawn)(
    cli,
    ["--port", path, "proxy", "--protocol", String(CURRENT_PROTOCOL_VERSION), "--schema", OPERATIONAL_SCHEMA_SHA256],
    { shell: false, stdio: ["pipe", "pipe", "pipe"] },
  );
  const stream = new CliStream(child, host.log);
  let stopped: Error | null = null;
  const stop = (error: Error) => {
    stopped = error;
    stream.fail(error);
  };
  const cancel = () => stop(new Error("opening cancelled"));
  signal.addEventListener("abort", cancel, { once: true });
  const timer = setTimeout(() => stop(new Error("the Cordial CLI didn't start in time")), READY_MS);
  try {
    let bytes: Uint8Array;
    try {
      bytes = await stream.startup;
    } catch (error) {
      if (stopped || error instanceof HelperError || child.pid !== undefined) throw error;
      throw new HelperError("missing", `the Cordial CLI didn't start the proxy: ${(error as Error).message}`);
    }
    return { stream, hello: checkStartup(bytes, native) };
  } catch (error) {
    await stream.close();
    throw error;
  } finally {
    clearTimeout(timer);
    signal.removeEventListener("abort", cancel);
  }
}
