// One session with a Dongle over a ByteStream. Requests go out one at a time
// and each Response answers the oldest unanswered request; Events arrive in
// between. Opening sends a delimiter and GetStatus, and everything the Dongle
// sent before the answer to that first request is ignored.
import { create, fromBinary, toBinary, type MessageInitShape } from "@bufbuild/protobuf";
import {
  CapacityReason,
  DELIMITER,
  ErrorCode,
  FrameDecoder,
  MAX_REQUEST_BYTES,
  MessageSchema,
  RequestSchema,
  encodeFrame,
  type Device,
  type DeviceSettings,
  type DeviceWarnings,
  type Event,
  type Feature,
  type FileEntry,
  type ForgetSettingsSchema,
  type Request,
  type Response,
  type SetAdapterSchema,
  type SetDeviceSchema,
  type SetSettingsSchema,
  type Status,
  type Transport,
} from "@cordial/protocol";
import type { ByteStream } from "./stream.ts";

/** A request, as the command it carries. */
export type Command = Exclude<MessageInitShape<typeof RequestSchema>["command"], { case: undefined } | undefined>;
type ResultCase = NonNullable<Response["result"]["case"]>;
type ResultOf<C extends ResultCase> = Extract<Response["result"], { case: C }>["value"];

/** The Dongle answered a request with an error. */
export class CordialError extends Error {
  /** The command's field name, such as `setDevice`. */
  readonly command: string;
  readonly code: ErrorCode;
  readonly reason: CapacityReason;
  readonly outcomeUnknown: boolean;
  constructor(command: string, code: ErrorCode, reason = CapacityReason.UNKNOWN, outcomeUnknown = false) {
    // Codes and reasons this client doesn't know read as UNKNOWN.
    const known = code in ErrorCode ? code : ErrorCode.UNKNOWN;
    super(`${command} failed: ${ErrorCode[known]}`);
    this.name = "CordialError";
    this.command = command;
    this.code = known;
    this.reason = reason in CapacityReason ? reason : CapacityReason.UNKNOWN;
    this.outcomeUnknown = outcomeUnknown;
  }
}

/** The session ended before a request was answered. */
export class ConnectionClosedError extends Error {
  constructor(message = "connection closed") {
    super(message);
    this.name = "ConnectionClosedError";
  }
}

export interface ConnectionOptions {
  /** Every event, in the order the Dongle sent it relative to responses. */
  onEvent?: (event: Event) => void;
  /**
   * Every response with its request, called before the request's promise
   * settles, in the same order as events. State kept from both stays in the
   * Dongle's order, which promise continuations alone would not guarantee.
   */
  onResponse?: (request: Request, response: Response) => void;
  /** The session ended by itself: the port closed or the Dongle broke the protocol. */
  onClose?: (error: Error) => void;
  log?: (message: string) => void;
  /** How long the first response may take. */
  openTimeoutMs?: number;
  /** How long any later response may take before the session ends; a lost
   * response would leave every later one answering the wrong request. */
  timeoutMs?: number;
}

interface Pending {
  request: Request;
  frame: Uint8Array;
  timeoutMs: number;
  resolve(response: Response): void;
  reject(error: Error): void;
}

const OPEN_TIMEOUT_MS = 4000;
const TIMEOUT_MS = 10_000;

export class Connection {
  readonly #stream: ByteStream;
  readonly #options: ConnectionOptions;
  readonly #decoder = new FrameDecoder();
  readonly #queue: Pending[] = [];
  /** The request written and not yet answered. */
  #sent: Pending | null = null;
  #timer: ReturnType<typeof setTimeout> | undefined;
  #status: Status | null = null;
  #closed = false;
  /** The first response has arrived; earlier input is ignored, later bad input is fatal. */
  #confirmed = false;

  private constructor(stream: ByteStream, options: ConnectionOptions) {
    this.#stream = stream;
    this.#options = options;
    stream.onData((chunk) => this.#receive(chunk));
    stream.onClose((error) => this.#fail(error ?? new Error("serial port closed")));
  }

  /**
   * Starts a session on an open stream and reads the Dongle's status. Rejects,
   * closing the stream, when nothing there answers as a Dongle.
   */
  static async open(stream: ByteStream, options: ConnectionOptions = {}): Promise<Connection> {
    const connection = new Connection(stream, options);
    try {
      // The delimiter ends whatever an earlier client left half-written.
      const response = await connection.#enqueue({ case: "getStatus", value: {} }, options.openTimeoutMs ?? OPEN_TIMEOUT_MS, true);
      if (response.result.case !== "status") throw new Error("the first response is not a status");
      connection.#status = response.result.value;
      return connection;
    } catch (error) {
      connection.#fail(error as Error);
      await stream.close().catch(() => {});
      throw error;
    }
  }

  /** The Dongle's status when the session started. */
  get status(): Status {
    return this.#status!;
  }

  get closed() {
    return this.#closed;
  }

  // ---- Requests -----------------------------------------------------------

  /** Sends a command and resolves with its response; an error response rejects with CordialError. */
  send(command: Command): Promise<Response> {
    return this.#enqueue(command, this.#options.timeoutMs ?? TIMEOUT_MS);
  }

  #enqueue(command: Command, timeoutMs: number, first = false): Promise<Response> {
    if (this.#closed) return Promise.reject(new ConnectionClosedError());
    const request = create(RequestSchema, { command });
    const bytes = toBinary(RequestSchema, request);
    if (bytes.length > MAX_REQUEST_BYTES) return Promise.reject(new CordialError(command.case, ErrorCode.TOO_LONG));
    const encoded = encodeFrame(bytes);
    const frame = first ? new Uint8Array([DELIMITER, ...encoded]) : encoded;
    return new Promise<Response>((resolve, reject) => {
      this.#queue.push({ request, frame, timeoutMs, resolve, reject });
      this.#next();
    });
  }

  #next() {
    if (this.#sent || this.#closed) return;
    const pending = this.#queue.shift();
    if (!pending) return;
    this.#sent = pending;
    this.#timer = setTimeout(() => this.#fail(new Error(`${pending.request.command.case} got no response`)), pending.timeoutMs);
    this.#stream.write(pending.frame).catch((error: Error) => this.#fail(new Error(`serial write failed: ${error.message}`)));
  }

  async #call<C extends ResultCase>(command: Command, result: C): Promise<ResultOf<C>> {
    const response = await this.send(command);
    if (response.result.case !== result) throw new Error(`${command.case} got an unexpected response`);
    return response.result.value as ResultOf<C>;
  }

  /** A command whose success carries nothing; a result a newer Dongle adds is ignored. */
  async #void(command: Command): Promise<void> {
    await this.send(command);
  }

  getStatus(): Promise<Status> {
    return this.#call({ case: "getStatus", value: {} }, "status");
  }
  /** Changes the name, the platform or both. An empty name restores the default name. */
  setAdapter(update: MessageInitShape<typeof SetAdapterSchema>): Promise<Status> {
    return this.#call({ case: "setAdapter", value: update }, "status");
  }
  enterBootloader(): Promise<void> {
    return this.#void({ case: "enterBootloader", value: {} });
  }
  /** Candidates follow as scan_found events and the end as scan_done. Zero seconds means 10. */
  startScan(transports: Transport[], seconds = 0): Promise<void> {
    return this.#void({ case: "startScan", value: { transports, seconds } });
  }
  stopScan(): Promise<void> {
    return this.#void({ case: "stopScan", value: {} });
  }
  /** Progress follows as pairing events. */
  startPairing(candidate: string): Promise<void> {
    return this.#void({ case: "startPairing", value: { candidate } });
  }
  /** Answers the open prompt; `value` is the code the user typed, if it asked for one. */
  acceptPrompt(value = ""): Promise<void> {
    return this.#void({ case: "acceptPrompt", value: { value } });
  }
  rejectPrompt(): Promise<void> {
    return this.#void({ case: "rejectPrompt", value: {} });
  }
  cancelPairing(): Promise<void> {
    return this.#void({ case: "cancelPairing", value: {} });
  }
  async listDevices(): Promise<Device[]> {
    return (await this.#call({ case: "listDevices", value: {} }, "devices")).devices;
  }
  getDevice(device: string): Promise<Device> {
    return this.#call({ case: "getDevice", value: { device } }, "device");
  }
  /** Changes only the fields set in `update`. */
  setDevice(update: MessageInitShape<typeof SetDeviceSchema>): Promise<Device> {
    return this.#call({ case: "setDevice", value: update }, "device");
  }
  connectDevice(device: string): Promise<Device> {
    return this.#call({ case: "connectDevice", value: { device } }, "device");
  }
  disconnectDevice(device: string): Promise<Device> {
    return this.#call({ case: "disconnectDevice", value: { device } }, "device");
  }
  /** device_removed follows once the device is gone. */
  unpairDevice(device: string): Promise<void> {
    return this.#void({ case: "unpairDevice", value: { device } });
  }
  refreshDevice(device: string): Promise<void> {
    return this.#void({ case: "refreshDevice", value: { device } });
  }
  listWarnings(device: string): Promise<DeviceWarnings> {
    return this.#call({ case: "listWarnings", value: { device } }, "warnings");
  }
  listSettings(device: string): Promise<DeviceSettings> {
    return this.#call({ case: "listSettings", value: { device } }, "settings");
  }
  setSettings(update: MessageInitShape<typeof SetSettingsSchema>): Promise<DeviceSettings> {
    return this.#call({ case: "setSettings", value: update }, "settings");
  }
  forgetSettings(update: MessageInitShape<typeof ForgetSettingsSchema>): Promise<DeviceSettings> {
    return this.#call({ case: "forgetSettings", value: update }, "settings");
  }
  async listFeatures(device: string): Promise<Feature[]> {
    return (await this.#call({ case: "listFeatures", value: { device } }, "features")).features;
  }
  async listFiles(path: string): Promise<FileEntry[]> {
    return (await this.#call({ case: "listFiles", value: { path } }, "files")).entries;
  }
  async readFile(path: string): Promise<Uint8Array> {
    return (await this.#call({ case: "readFile", value: { path } }, "file")).data;
  }

  // ---- Input ----------------------------------------------------------------

  #receive(chunk: Uint8Array) {
    for (const frame of this.#decoder.push(chunk)) {
      if (this.#closed) return;
      let problem: string | null = null;
      let message;
      if (!frame.ok) problem = `the Dongle sent a ${frame.error === "too_long" ? "too long" : "malformed"} frame`;
      else
        try {
          message = fromBinary(MessageSchema, frame.bytes);
        } catch {
          problem = "the Dongle sent an undecodable frame";
        }
      if (!message) {
        if (this.#confirmed) return this.#fail(new Error(problem!));
        this.#options.log?.("ignoring input left from an earlier session");
        continue;
      }
      const kind = message.kind;
      if (kind.case === "response") this.#respond(kind.value);
      // Events can't belong to this session before its first response.
      else if (kind.case === "event" && this.#confirmed) this.#notify(() => this.#options.onEvent?.(kind.value));
    }
  }

  #respond(response: Response) {
    const pending = this.#sent;
    if (!pending) {
      if (this.#confirmed) this.#fail(new Error("the Dongle sent a response to no request"));
      return;
    }
    this.#confirmed = true;
    this.#sent = null;
    clearTimeout(this.#timer);
    this.#notify(() => this.#options.onResponse?.(pending.request, response));
    const result = response.result;
    if (result.case === "error")
      pending.reject(new CordialError(pending.request.command.case ?? "request", result.value.code, result.value.reason, result.value.outcomeUnknown));
    else pending.resolve(response);
    this.#next();
  }

  /** Runs a listener; one that throws can't stop the session's own bookkeeping. */
  #notify(listener: () => void) {
    try {
      listener();
    } catch (error) {
      this.#options.log?.(`a listener failed: ${(error as Error).message}`);
    }
  }

  // ---- Shutdown -------------------------------------------------------------

  /** Ends the session: unanswered requests reject and the stream closes. */
  async close() {
    if (this.#closed) return;
    this.#stop(new ConnectionClosedError());
    await this.#stream.close().catch(() => {});
  }

  #stop(error: Error) {
    this.#closed = true;
    clearTimeout(this.#timer);
    const closed = error instanceof ConnectionClosedError ? error : new ConnectionClosedError(error.message);
    for (const pending of [...(this.#sent ? [this.#sent] : []), ...this.#queue.splice(0)]) pending.reject(closed);
    this.#sent = null;
  }

  #fail(error: Error) {
    if (this.#closed) return;
    const opened = this.#status !== null;
    this.#stop(error);
    void this.#stream.close().catch(() => {});
    if (opened) this.#options.onClose?.(error);
  }
}
