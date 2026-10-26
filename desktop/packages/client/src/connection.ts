// One session with a Dongle over a ByteStream. Opening sends a delimiter and the
// empty ClientHello query, skips any responses and events the Dongle sends before
// its AdapterHello, and continues only when that hello is valid and names the
// current protocol revision; it then reads GetStatus.
// Requests go out one at a time and each Response answers the oldest
// unanswered request; Events arrive in between.
import { create, fromBinary, toBinary, type MessageInitShape } from "@bufbuild/protobuf";
import {
  ClientHelloSchema,
  CURRENT_PROTOCOL_VERSION,
  DELIMITER,
  ErrorCode,
  ErrorOutcome,
  ErrorReason,
  ErrorSchema,
  FrameDecoder,
  HandshakeError,
  MAX_HANDSHAKE_BYTES,
  MAX_REQUEST_BYTES,
  MessageSchema,
  RequestSchema,
  decodeAdapterHello,
  encodeClientHello,
  encodeFrame,
  type AdapterHello,
  type Device,
  type DeviceList,
  type DeviceListEntry,
  type DeviceSettings,
  type Error as ErrorMessage,
  type Event,
  type Feature,
  type FeatureList,
  type FileEntry,
  type FileList,
  type Profile,
  type ProfileList,
  type ProfileListEntry,
  type ProfileRule,
  type ProfileRules,
  type Request,
  type Response,
  type SetAdapterSchema,
  type SetDeviceSchema,
  type SetProfileRulesSchema,
  type SetSettingsSchema,
  type SettingsObject,
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
  /** Codes, reasons and outcomes keep their numbers, including ones this client doesn't know. */
  readonly code: ErrorCode;
  readonly reason: ErrorReason;
  readonly outcome: ErrorOutcome;
  /** Further failures, as the Dongle sent them; they never replace this one. */
  readonly related: ErrorMessage[];
  constructor(command: string, error: MessageInitShape<typeof ErrorSchema>) {
    const { code = ErrorCode.UNKNOWN, reason = ErrorReason.UNKNOWN, outcome = ErrorOutcome.UNSPECIFIED } = error;
    super(`${command} failed: ${ErrorCode[code] ?? code}`);
    this.name = "CordialError";
    this.command = command;
    this.code = code;
    this.reason = reason;
    this.outcome = outcome;
    this.related = (error.related ?? []).map((r) => create(ErrorSchema, r));
  }
}

/** The Dongle answered a command with a result of another kind. */
export class UnexpectedResponseError extends Error {
  readonly command: string;
  constructor(command: string) {
    super(`${command} got an unexpected response`);
    this.name = "UnexpectedResponseError";
    this.command = command;
  }
}

/** The ID a device or profile listing entry is listed by; undefined for an entry of a kind this
 * client doesn't know. */
export const entryId = (e: DeviceListEntry | ProfileListEntry): number | undefined =>
  e.entry.case === "device" || e.entry.case === "profile" ? e.entry.value.id : e.entry.case === "error" ? e.entry.value.id : undefined;

/** The adapter answered the handshake with another protocol revision. The stream is closed. */
export class ProtocolVersionError extends Error {
  /** The adapter's hello, with its revision and whatever metadata it sent. */
  readonly hello: AdapterHello;
  constructor(hello: AdapterHello) {
    super(`the adapter uses protocol ${hello.protocolVersion}, not ${CURRENT_PROTOCOL_VERSION}`);
    this.name = "ProtocolVersionError";
    this.hello = hello;
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
  /** How long opening may take in all, from the hello to the status. */
  openTimeoutMs?: number;
  /** How long any later response may stall without incoming bytes before the session ends. */
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

/** Whether `bytes` have the Message envelope: one or more length-delimited fields 1 or 2, a
 * response or an event. Only the envelope is checked, since the payload's revision isn't known
 * before the hello. No valid AdapterHello has this shape. */
function isMessage(bytes: Uint8Array): boolean {
  let at = 0;
  const varint = () => {
    let value = 0;
    for (let i = 0; i < 10; i++) {
      const byte = bytes[at++];
      if (byte === undefined) return -1;
      value += (byte & 0x7f) * 2 ** (7 * i);
      if (byte < 0x80) return value;
    }
    return -1;
  };
  while (at < bytes.length) {
    const key = varint();
    if (key !== 0x0a && key !== 0x12) return false;
    const length = varint();
    if (length < 0 || length > bytes.length - at) return false;
    at += length;
  }
  return bytes.length > 0;
}

export class Connection {
  readonly #stream: ByteStream;
  readonly #options: ConnectionOptions;
  /** Bounded while waiting for the adapter's hello, then unbounded. */
  #decoder = new FrameDecoder(MAX_HANDSHAKE_BYTES);
  #hello: AdapterHello | null = null;
  #greeting: { resolve(): void; reject(error: Error): void } | null = null;
  readonly #queue: Pending[] = [];
  /** The request written and not yet answered. */
  #sent: Pending | null = null;
  #timer: ReturnType<typeof setTimeout> | undefined;
  #status: Status | null = null;
  #closed = false;

  private constructor(stream: ByteStream, options: ConnectionOptions) {
    this.#stream = stream;
    this.#options = options;
    stream.onData((chunk) => this.#receive(chunk));
    stream.onClose((error) => this.#fail(error ?? new Error("serial port closed")));
  }

  /**
   * Starts a session on an open stream: exchanges hellos, then reads the Dongle's status. Rejects,
   * closing the stream, when nothing there completes the handshake in time, or with
   * ProtocolVersionError when the adapter uses another protocol revision.
   */
  static async open(stream: ByteStream, options: ConnectionOptions = {}): Promise<Connection> {
    const connection = new Connection(stream, options);
    const timeoutMs = options.openTimeoutMs ?? OPEN_TIMEOUT_MS;
    // Input can't extend this deadline, unlike the request timeout.
    const deadline = setTimeout(() => connection.#fail(new Error(connection.#hello ? "getStatus got no response" : "the adapter sent no hello")), timeoutMs);
    try {
      await new Promise<void>((resolve, reject) => {
        connection.#greeting = { resolve, reject };
        // The delimiter ends whatever an earlier client left half-written.
        const hello = encodeClientHello(create(ClientHelloSchema));
        stream.write(new Uint8Array([DELIMITER, ...encodeFrame(hello)])).catch((error: Error) => connection.#fail(new Error(`serial write failed: ${error.message}`)));
      });
      const response = await connection.#enqueue({ case: "getStatus", value: {} }, timeoutMs);
      if (response.result.case !== "status") throw new Error("the first response is not a status");
      // The session can end after the response and before this continuation runs, before
      // onClose could report it.
      if (connection.#closed) throw new ConnectionClosedError();
      connection.#status = response.result.value;
      return connection;
    } catch (error) {
      connection.#fail(error as Error);
      await stream.close().catch(() => {});
      throw error;
    } finally {
      clearTimeout(deadline);
    }
  }

  /** The adapter's hello, which may omit any of its metadata. */
  get hello(): AdapterHello {
    return this.#hello!;
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

  #enqueue(command: Command, timeoutMs: number): Promise<Response> {
    if (this.#closed) return Promise.reject(new ConnectionClosedError());
    const request = create(RequestSchema, { command });
    const bytes = toBinary(RequestSchema, request);
    // Never sent, so it had no effect.
    if (bytes.length > MAX_REQUEST_BYTES) return Promise.reject(new CordialError(command.case, { code: ErrorCode.TOO_LONG, outcome: ErrorOutcome.NO_EFFECT }));
    const frame = encodeFrame(bytes);
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
    this.#armTimeout();
    this.#stream.write(pending.frame).catch((error: Error) => this.#fail(new Error(`serial write failed: ${error.message}`)));
  }

  #armTimeout() {
    clearTimeout(this.#timer);
    const pending = this.#sent;
    if (pending) this.#timer = setTimeout(() => this.#fail(new Error(`${pending.request.command.case} got no response`)), pending.timeoutMs);
  }

  async #call<C extends ResultCase>(command: Command, result: C): Promise<ResultOf<C>> {
    const response = await this.send(command);
    if (response.result.case !== result) throw new UnexpectedResponseError(command.case);
    return response.result.value as ResultOf<C>;
  }

  /** A command whose success carries nothing; a result a newer Dongle adds is ignored. */
  async #void(command: Command): Promise<void> {
    await this.send(command);
  }

  getStatus(): Promise<Status> {
    return this.#call({ case: "getStatus", value: {} }, "status");
  }
  /** Changes any of the name, the platform, the enabled transports and the configuration
   * interfaces. An empty name restores the default name. Success means the Dongle saved exactly
   * what was sent; an adapter event follows when anything changed. */
  setAdapter(update: MessageInitShape<typeof SetAdapterSchema>): Promise<void> {
    return this.#void({ case: "setAdapter", value: update });
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
  startPairing(candidate: number): Promise<void> {
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
  /** Every saved device, including the IDs and read errors of records the Dongle couldn't read. */
  listDevices(): Promise<DeviceList> {
    return this.#call({ case: "listDevices", value: {} }, "devices");
  }
  async listAllDevices(): Promise<DeviceListEntry[]> {
    return (await this.listDevices()).entries;
  }
  getDevice(device: number): Promise<Device> {
    return this.#call({ case: "getDevice", value: { device } }, "device");
  }
  /** Changes only the fields set in `update`. Success means the Dongle saved exactly what was
   * sent; a device event follows when anything changed. */
  setDevice(update: MessageInitShape<typeof SetDeviceSchema>): Promise<void> {
    return this.#void({ case: "setDevice", value: update });
  }
  connectDevice(device: number): Promise<Device> {
    return this.#call({ case: "connectDevice", value: { device } }, "device");
  }
  disconnectDevice(device: number): Promise<Device> {
    return this.#call({ case: "disconnectDevice", value: { device } }, "device");
  }
  /** device_removed follows once the device is gone. */
  unpairDevice(device: number): Promise<void> {
    return this.#void({ case: "unpairDevice", value: { device } });
  }
  refreshDevice(device: number): Promise<void> {
    return this.#void({ case: "refreshDevice", value: { device } });
  }
  /** Every settings object of the device. */
  listSettings(device: number): Promise<DeviceSettings> {
    return this.#call({ case: "listSettings", value: { device } }, "settings");
  }
  async listAllSettings(device: number): Promise<SettingsObject[]> {
    return (await this.listSettings(device)).objects;
  }
  /** Saves and forgets settings in one write, applied in order. Success means the Dongle saved
   * exactly what was sent; a settings_changed event follows. */
  setSettings(update: MessageInitShape<typeof SetSettingsSchema>): Promise<void> {
    return this.#void({ case: "setSettings", value: update });
  }
  /** Every saved profile, including the IDs and read errors of records the Dongle couldn't read. */
  listProfiles(): Promise<ProfileList> {
    return this.#call({ case: "listProfiles", value: {} }, "profiles");
  }
  async listAllProfiles(): Promise<ProfileListEntry[]> {
    return (await this.listProfiles()).entries;
  }
  getProfile(profile: number): Promise<Profile> {
    return this.#call({ case: "getProfile", value: { profile } }, "profile");
  }
  /** Saves a new, empty profile named `name` and resolves with its ID; a profile event follows. */
  async createProfile(name: string): Promise<number> {
    return (await this.#call({ case: "createProfile", value: { name } }, "profileCreated")).profile;
  }
  /** Saves a new profile named `name` with a copy of the source's rules and resolves with its ID;
   * a profile event follows. */
  async copyProfile(profile: number, name: string): Promise<number> {
    return (await this.#call({ case: "copyProfile", value: { profile, name } }, "profileCreated")).profile;
  }
  /** profile_removed follows. */
  deleteProfile(profile: number): Promise<void> {
    return this.#void({ case: "deleteProfile", value: { profile } });
  }
  /** Every rule of the profile. */
  listProfileRules(profile: number): Promise<ProfileRules> {
    return this.#call({ case: "listProfileRules", value: { profile } }, "profileRules");
  }
  async listAllProfileRules(profile: number): Promise<ProfileRule[]> {
    return (await this.listProfileRules(profile)).rules;
  }
  /** Saves and forgets rules in one write, applied in order. Success means the Dongle saved exactly
   * what was sent. */
  setProfileRules(update: MessageInitShape<typeof SetProfileRulesSchema>): Promise<void> {
    return this.#void({ case: "setProfileRules", value: update });
  }
  /** Every feature of the device. Development firmware only. */
  listFeatures(device: number): Promise<FeatureList> {
    return this.#call({ case: "listFeatures", value: { device } }, "features");
  }
  async listAllFeatures(device: number): Promise<Feature[]> {
    return (await this.listFeatures(device)).features;
  }
  /** Every entry of a directory. Development firmware only. */
  listFiles(path: string): Promise<FileList> {
    return this.#call({ case: "listFiles", value: { path } }, "files");
  }
  async listAllFiles(path: string): Promise<FileEntry[]> {
    return (await this.listFiles(path)).entries;
  }
  async readFile(path: string): Promise<Uint8Array> {
    return (await this.#call({ case: "readFile", value: { path } }, "file")).data;
  }

  // ---- Input ----------------------------------------------------------------

  #receive(chunk: Uint8Array): void {
    if (this.#closed) return;
    if (!this.#hello) return this.#handshake(chunk);
    if (chunk.length) this.#armTimeout();
    for (const frame of this.#decoder.push(chunk)) {
      if (this.#closed) return;
      if (!frame.ok) return this.#fail(new Error(`the Dongle sent a ${frame.error === "too_long" ? "too long" : "malformed"} frame`));
      let message;
      try {
        message = fromBinary(MessageSchema, frame.bytes);
      } catch {
        return this.#fail(new Error("the Dongle sent an undecodable frame"));
      }
      const kind = message.kind;
      if (kind.case === "response") this.#respond(kind.value);
      else if (kind.case === "event") this.#notify(() => this.#options.onEvent?.(kind.value));
    }
  }

  /** Reads frames until the adapter's hello, one frame at a time so the bytes after it go to the
   * unbounded decoder. Responses and events the Dongle sent before the hello, such as those queued
   * for an earlier client, are dropped, and so is any frame longer than a hello can be, since it
   * may be an event. Any other frame fails the handshake. */
  #handshake(chunk: Uint8Array): void {
    let start = 0;
    while (start < chunk.length) {
      const end = chunk.indexOf(DELIMITER, start);
      const next = end === -1 ? chunk.length : end + 1;
      const [frame] = this.#decoder.push(chunk.subarray(start, next));
      start = next;
      if (!frame || (!frame.ok && frame.error === "too_long")) continue;
      let hello: AdapterHello;
      try {
        if (!frame.ok) throw new HandshakeError(frame.error);
        hello = decodeAdapterHello(frame.bytes);
      } catch (error) {
        if (frame.ok && isMessage(frame.bytes)) continue;
        return this.#fail(new Error(`the adapter sent an invalid hello: ${(error as Error).message}`));
      }
      if (hello.protocolVersion !== CURRENT_PROTOCOL_VERSION) return this.#fail(new ProtocolVersionError(hello));
      this.#hello = hello;
      this.#decoder = new FrameDecoder();
      this.#greeting?.resolve();
      this.#greeting = null;
      return this.#receive(chunk.subarray(next));
    }
  }

  #respond(response: Response) {
    const pending = this.#sent;
    if (!pending) return this.#fail(new Error("the Dongle sent a response to no request"));
    this.#sent = null;
    clearTimeout(this.#timer);
    this.#notify(() => this.#options.onResponse?.(pending.request, response));
    const result = response.result;
    if (result.case === "error")
      pending.reject(new CordialError(pending.request.command.case ?? "request", result.value));
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
    this.#greeting?.reject(error);
    this.#greeting = null;
    const closed = error instanceof ConnectionClosedError ? error : new ConnectionClosedError(error.message);
    for (const pending of [...(this.#sent ? [this.#sent] : []), ...this.#queue.splice(0)]) pending.reject(closed);
    this.#sent = null;
    this.#decoder.reset();
  }

  #fail(error: Error) {
    if (this.#closed) return;
    const opened = this.#status !== null;
    this.#stop(error);
    void this.#stream.close().catch(() => {});
    if (opened) this.#options.onClose?.(error);
  }
}
