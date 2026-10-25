// Runs the Cordial CLI's storage migration for the desktop app as `cordial migrate --ipc`. Controls
// go to its stdin and events come from its stdout, each a protobuf message prefixed by its length
// in four little-endian bytes. stdout carries nothing else; stderr lines go to the log.
import { spawn as nodeSpawn, type ChildProcessWithoutNullStreams } from "node:child_process";
import { accessSync, constants, statSync } from "node:fs";
import { delimiter, join } from "node:path";
import { create, fromBinary, toBinary } from "@bufbuild/protobuf";
import { MIGRATION_MAX_FRAME_BYTES, MigrationControlSchema, MigrationEventSchema } from "@cordial/protocol";
import { clean } from "../shared/text.ts";
import type { MigrationHost } from "../core/migration.ts";

/** Longest stderr line logged; the rest of a longer line is dropped. */
const MAX_LINE = 1024;

/** `bytes` with its length prefix. */
export function frame(bytes: Uint8Array): Uint8Array {
  const out = new Uint8Array(bytes.length + 4);
  new DataView(out.buffer).setUint32(0, bytes.length, true);
  out.set(bytes, 4);
  return out;
}

/** Splits length-prefixed frames out of a byte stream whose write boundaries mean nothing. */
export class FrameReader {
  #buffer = new Uint8Array(0);

  /** Adds received bytes and returns the frames they complete; throws on a length outside
   * 1..MIGRATION_MAX_FRAME_BYTES, before buffering its payload. */
  push(chunk: Uint8Array): Uint8Array[] {
    const buffer = new Uint8Array(this.#buffer.length + chunk.length);
    buffer.set(this.#buffer);
    buffer.set(chunk, this.#buffer.length);
    const frames: Uint8Array[] = [];
    let at = 0;
    while (buffer.length - at >= 4) {
      const length = new DataView(buffer.buffer, at).getUint32(0, true);
      if (length < 1 || length > MIGRATION_MAX_FRAME_BYTES) throw new Error(`frame length ${length} is out of range`);
      if (buffer.length - at - 4 < length) break;
      frames.push(buffer.slice(at + 4, at + 4 + length));
      at += 4 + length;
    }
    this.#buffer = buffer.slice(at);
    return frames;
  }

  /** Whether part of a frame is waiting for the rest. */
  get partial(): boolean {
    return this.#buffer.length > 0;
  }
}

/** The first executable file named `name` in `path`, without running anything. */
export function findExecutable(name: string, path = process.env.PATH ?? ""): string | null {
  for (const dir of path.split(delimiter)) {
    if (!dir) continue;
    const file = join(dir, name);
    try {
      if (!statSync(file).isFile()) continue;
      accessSync(file, constants.X_OK);
      return file;
    } catch {
      // Not here.
    }
  }
  return null;
}

type Spawn = (command: string, args: string[]) => ChildProcessWithoutNullStreams;

export function migrationHost(
  log: (message: string) => void,
  spawn: Spawn = (command, args) => nodeSpawn(command, args, { stdio: "pipe", windowsHide: true }),
  find: () => string | null = () => findExecutable(process.platform === "win32" ? "cordial.exe" : "cordial"),
): MigrationHost {
  return {
    find,
    start(path, handlers) {
      const diagnostic = (text: string) => log(`cordial migrate: ${clean(text).slice(0, MAX_LINE)}`);
      let exited = false;
      const exit = (code: number | null, signal: string | null) => {
        if (exited) return;
        exited = true;
        handlers.exit(code, signal);
      };
      let child: ChildProcessWithoutNullStreams;
      try {
        child = spawn(path, ["migrate", "--ipc"]);
      } catch (error) {
        diagnostic((error as Error).message);
        queueMicrotask(() => exit(null, null));
        return { send: () => {} };
      }
      const reader = new FrameReader();
      let broken = false;
      const malformed = (reason: string) => {
        broken = true;
        handlers.malformed(reason);
      };
      // Output after a framing failure is still read, so the CLI never blocks writing it.
      child.stdout.on("data", (chunk: Buffer) => {
        if (broken) return;
        let frames: Uint8Array[];
        try {
          frames = reader.push(chunk);
        } catch (error) {
          return malformed((error as Error).message);
        }
        for (const bytes of frames) {
          let event;
          try {
            event = fromBinary(MigrationEventSchema, bytes);
          } catch {
            return malformed("undecodable event");
          }
          handlers.event(event);
          if (broken) return;
        }
      });
      child.stdout.on("end", () => {
        if (!broken && reader.partial) malformed("output ended inside a frame");
      });
      let line = "";
      child.stderr.setEncoding("utf8");
      child.stderr.on("data", (text: string) => {
        const lines = (line + text).split("\n");
        line = lines.pop()!.slice(0, MAX_LINE);
        for (const l of lines) diagnostic(l);
      });
      child.stderr.on("end", () => {
        if (line) diagnostic(line);
      });
      // Writing after the CLI exits fails; its exit reports that.
      child.stdin.on("error", (error) => diagnostic(error.message));
      child.on("error", (error) => {
        diagnostic(error.message);
        if (child.pid === undefined) exit(null, null);
      });
      // After the process has exited and closed its output.
      child.on("close", exit);
      return {
        send(control) {
          if (!exited && child.stdin.writable) child.stdin.write(frame(toBinary(MigrationControlSchema, create(MigrationControlSchema, control))));
        },
      };
    },
  };
}
