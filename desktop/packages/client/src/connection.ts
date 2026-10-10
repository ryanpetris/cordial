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
  type DeviceList,
  type DeviceListEntry,
  type DeviceSettings,
  type DeviceWarning,
  type DeviceWarnings,
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
  type Setting,
  type SettingRefSchema,
  type Status,
  type Transport,
  type UsageSchema,
} from "@cordial/protocol";
import { featureRef, ruleInput, settingRef } from "./order.ts";
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

/** The Dongle answered a command with a result of another kind. */
export class UnexpectedResponseError extends Error {
  readonly command: string;
  constructor(command: string) {
    super(`${command} got an unexpected response`);
    this.name = "UnexpectedResponseError";
    this.command = command;
  }
}

/** One page of a listing: its entries in the listing's order, and whether nothing follows them. */
export interface Page<E> {
  entries: E[];
  end: boolean;
}

/**
 * Reads a listing to its end. Each request passes the key of the last entry received as `after`,
 * undefined for the first; the Dongle chooses how many entries each page holds. An entry whose
 * key is undefined, such as one of a kind newer than this client, is kept but never continues the
 * listing: the next page follows the last entry with a key. A page that does not end the listing
 * and has no entry with a key breaks the protocol, or can't be continued, and rejects with
 * UnexpectedResponseError.
 */
export async function readAll<E, K>(command: string, read: (after: K | undefined) => Promise<Page<E>>, key: (entry: E) => K | undefined): Promise<E[]> {
  const all: E[] = [];
  let after: K | undefined;
  for (;;) {
    const page = await read(after);
    all.push(...page.entries);
    if (page.end) return all;
    const last = page.entries.map(key).findLast((k) => k !== undefined);
    if (last === undefined) throw new UnexpectedResponseError(command);
    after = last;
  }
}

/** The ID a device or profile listing entry is listed by; undefined for an entry of a kind this
 * client doesn't know. */
export const entryId = (e: DeviceListEntry | ProfileListEntry): number | undefined =>
  e.entry.case === "device" || e.entry.case === "profile" ? e.entry.value.id : e.entry.case === "unreadable" ? e.entry.value : undefined;

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
      // The session can end after the response and before this continuation runs, before
      // onClose could report it.
      if (connection.#closed) throw new ConnectionClosedError();
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
  /** One page of saved devices with IDs above `after`, the ID of the last entry received. */
  listDevices(after = 0): Promise<DeviceList> {
    return this.#call({ case: "listDevices", value: { after } }, "devices");
  }
  /** Every saved device, and the IDs of those whose record could not be read. */
  listAllDevices(): Promise<DeviceListEntry[]> {
    return readAll("listDevices", (after = 0) => this.listDevices(after), entryId);
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
  /** One page of a device's warnings following `after`, the last warning received. */
  listWarnings(device: number, after?: DeviceWarning): Promise<DeviceWarnings> {
    return this.#call({ case: "listWarnings", value: { device, after } }, "warnings");
  }
  listAllWarnings(device: number): Promise<DeviceWarning[]> {
    return readAll("listWarnings", async (after: DeviceWarning | undefined) => {
      const page = await this.listWarnings(device, after);
      return { entries: page.warnings, end: page.end };
    }, (w) => w);
  }
  /** One page of a device's settings following `after`, the last setting received. */
  listSettings(device: number, after?: MessageInitShape<typeof SettingRefSchema>): Promise<DeviceSettings> {
    return this.#call({ case: "listSettings", value: { device, after } }, "settings");
  }
  listAllSettings(device: number): Promise<Setting[]> {
    return readAll("listSettings", async (after: MessageInitShape<typeof SettingRefSchema> | undefined) => {
      const page = await this.listSettings(device, after);
      return { entries: page.settings, end: page.end };
    }, settingRef);
  }
  /** Saves and forgets settings in one write, applied in order. Success means the Dongle saved
   * exactly what was sent; a settings_changed event follows. */
  setSettings(update: MessageInitShape<typeof SetSettingsSchema>): Promise<void> {
    return this.#void({ case: "setSettings", value: update });
  }
  /** One page of saved profiles with IDs above `after`, the ID of the last entry received. */
  listProfiles(after = 0): Promise<ProfileList> {
    return this.#call({ case: "listProfiles", value: { after } }, "profiles");
  }
  /** Every saved profile, and the IDs of those whose record could not be read. */
  listAllProfiles(): Promise<ProfileListEntry[]> {
    return readAll("listProfiles", (after = 0) => this.listProfiles(after), entryId);
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
  /** One page of a profile's rules whose inputs follow `after`, the input of the last rule received. */
  listProfileRules(profile: number, after?: MessageInitShape<typeof UsageSchema>): Promise<ProfileRules> {
    return this.#call({ case: "listProfileRules", value: { profile, after } }, "profileRules");
  }
  listAllProfileRules(profile: number): Promise<ProfileRule[]> {
    return readAll("listProfileRules", async (after: MessageInitShape<typeof UsageSchema> | undefined) => {
      const page = await this.listProfileRules(profile, after);
      return { entries: page.rules, end: page.end };
    }, ruleInput);
  }
  /** Saves and forgets rules in one write, applied in order. Success means the Dongle saved exactly
   * what was sent. */
  setProfileRules(update: MessageInitShape<typeof SetProfileRulesSchema>): Promise<void> {
    return this.#void({ case: "setProfileRules", value: update });
  }
  /** One page of a device's features following `after`, the last feature received. */
  listFeatures(device: number, after?: ReturnType<typeof featureRef>): Promise<FeatureList> {
    return this.#call({ case: "listFeatures", value: { device, after } }, "features");
  }
  listAllFeatures(device: number): Promise<Feature[]> {
    return readAll("listFeatures", async (after: ReturnType<typeof featureRef> | undefined) => {
      const page = await this.listFeatures(device, after);
      return { entries: page.features, end: page.end };
    }, featureRef);
  }
  /** One page of a directory's entries whose names follow `after`, the last name received. */
  listFiles(path: string, after = ""): Promise<FileList> {
    return this.#call({ case: "listFiles", value: { path, after } }, "files");
  }
  listAllFiles(path: string): Promise<FileEntry[]> {
    return readAll("listFiles", async (after: string | undefined) => {
      const page = await this.listFiles(path, after);
      return { entries: page.entries, end: page.end };
    }, (e) => e.name);
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
