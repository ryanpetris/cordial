// One adapter's control session: the docs/protocol/transport.md handshake, heartbeat,
// readiness, monitoring and snapshot synchronization over a Transport.
import { LineDecoder } from "../protocol/framing.ts";
import {
  AdapterError,
  HEARTBEAT_INTERVAL_MS,
  MAX_REQUEST_ID,
  SessionClosedError,
  type ArgsOf,
  type Capability,
  type ChunkOf,
  type CommandName,
  type Event,
  type ResultOf,
  type Status,
  type WireError,
} from "../protocol/types.ts";
import { statusProblem } from "../protocol/status.ts";
import { codeText } from "../shared/text.ts";
import { eventProblem, requestProblem, responseProblem } from "../protocol/validate.ts";
import type { Transport } from "./transport.ts";
import { AdapterView } from "./view.ts";

const HANDSHAKE_MS = 4000;
const HEARTBEAT_TIMEOUT_MS = 4000;
const CLEANUP_MS = 1000;
const STATUS_DELAY_MS = 250;
const READ_MS = 5000;
const RESERVED = new Set<CommandName>([
  "adapter.protocol", "adapter.status", "adapter.capabilities", "session.heartbeat",
  "adapter.wait_ready", "session.monitor.set", "pairing.reply", "request.cancel",
]);

export interface SessionHooks {
  /** Some adapter state visible to the user changed. */
  changed(): void;
  /** The session ended by itself: unplugged, failed or protocol violation. */
  closed(error: Error): void;
  log(message: string): void;
}

export interface RequestOptions<C extends CommandName> {
  onChunk?: (chunk: ChunkOf<C>) => void;
  /** Events carrying this request's ID, such as discovery results. */
  onEvent?: (event: Event) => void;
  /** A timeout fails the whole session; used where silence means a dead link. */
  timeoutMs?: number;
  /** Bounds a read's wait while heartbeats continue checking the link. */
  waitMs?: number;
}

export interface Started<C extends CommandName> {
  id: number;
  result: Promise<ResultOf<C>>;
}

interface Pending {
  command: CommandName;
  deviceId?: string;
  chunks: number;
  abandoned: boolean;
  onChunk?: (chunk: never) => void;
  onEvent?: (event: Event) => void;
  resolve(result: unknown): void;
  reject(error: Error): void;
  timer?: ReturnType<typeof setTimeout>;
}

export type Readiness = { state: "waiting" } | { state: "ready" } | { state: "failed"; error: WireError };

/** A settings list whose chunks disagree; retried like `busy`. */
class InconsistentList extends AdapterError {
  constructor() {
    super("hidpp.setting.list", { code: "busy" });
  }
}

const sleep = (ms: number) => new Promise<void>((resolve) => setTimeout(resolve, ms));

export class AdapterSession {
  readonly view = new AdapterView();
  status!: Status;
  capabilities: Capability[] = [];
  readiness: Readiness = { state: "waiting" };
  closed = false;

  readonly #transport: Transport;
  readonly #hooks: SessionHooks;
  readonly #decoder = new LineDecoder();
  readonly #pending = new Map<number, Pending>();
  #nextId = 1;
  #writes: Promise<void> = Promise.resolve();
  #heartbeat: ReturnType<typeof setInterval> | undefined;
  #statusTimer: ReturnType<typeof setTimeout> | undefined;
  #syncing = false;
  #syncAgain = false;
  #fetching = false;
  #monitoring = false;
  /** Orderly shutdown has started. */
  #closing = false;
  /** A valid response arrived; earlier stray input is ignored, later is fatal. */
  #confirmed = false;
  readonly #settled = new Map<number, Promise<void>>();
  readonly #watched = new Set<string>();

  private constructor(transport: Transport, hooks: SessionHooks) {
    this.#transport = transport;
    this.#hooks = hooks;
    transport.onData((chunk) => this.#receive(chunk));
    transport.onClose((error) => this.#fail(error ?? new Error("serial port closed")));
  }

  /**
   * Opens a control session and confirms the adapter: separator, then
   * protocol discovery, capabilities and a matching status. Rejects if the port isn't a working
   * Cordial adapter; the transport is closed in that case.
   */
  static async open(transport: Transport, hooks: SessionHooks): Promise<AdapterSession> {
    const session = new AdapterSession(transport, hooks);
    try {
      await session.#handshake();
    } catch (error) {
      await session.#shutdownTransport();
      session.#fail(error as Error, false);
      throw error;
    }
    return session;
  }

  async #handshake() {
    // Opening the port raised DTR and discarded most earlier input. A packet
    // from the previous session can still arrive, so input that doesn't
    // answer these requests is ignored until the first valid response.
    const { protocol } = await this.request("adapter.protocol", {}, { timeoutMs: HANDSHAKE_MS });
    if (protocol !== 1) throw new Error(`unsupported adapter protocol: ${protocol}`);
    const capabilities = await this.request("adapter.capabilities", {}, { timeoutMs: HANDSHAKE_MS });
    const status = await this.request("adapter.status", {}, { timeoutMs: HANDSHAKE_MS });
    const problem = statusProblem(status, capabilities);
    if (problem) throw new Error(`not a usable version 1 adapter: ${problem}`);
    this.capabilities = capabilities;
    this.status = status;
    this.view.setAdapter(status.revision, status.host_platform, status.name);
    await this.request("session.heartbeat", {}, { timeoutMs: HANDSHAKE_MS });
    this.#heartbeat = setInterval(() => void this.#beat(), HEARTBEAT_INTERVAL_MS);
  }


  get adapterId(): string {
    return this.status.adapter_id;
  }

  /** Waits for readiness, then builds and maintains the device view. */
  run() {
    void this.#waitReady();
  }

  async #waitReady() {
    try {
      // The adapter answers within 30 seconds and the heartbeat covers a dead
      // link. A timeout means it is still starting, so wait again.
      let ready;
      for (;;) {
        try {
          ready = await this.request("adapter.wait_ready", {});
          break;
        } catch (error) {
          if (!(error instanceof AdapterError && error.wire.code === "timeout") || this.closed) throw error;
          this.#hooks.log("adapter still starting; waiting again");
        }
      }
      const status = ready.status;
      if (
        status.boot_id !== this.status.boot_id ||
        status.session_id !== this.status.session_id ||
        status.adapter_id !== this.status.adapter_id ||
        !status.radio_ready ||
        !status.storage_ready
      ) {
        throw new Error("readiness reported a different adapter session");
      }
      if (!this.#installStatus(status)) return;
      this.readiness = { state: "ready" };
      this.#hooks.changed();
      this.resync();
    } catch (error) {
      if (this.closed) return;
      if (error instanceof AdapterError) {
        this.readiness = { state: "failed", error: error.wire };
        this.#hooks.changed();
      } else {
        this.#fail(error as Error);
      }
    }
  }

  /** Installs a status of this session; anything else ends the session. */
  #installStatus(status: Status): boolean {
    const problem =
      status.adapter_id !== this.status.adapter_id ||
      status.boot_id !== this.status.boot_id ||
      status.session_id !== this.status.session_id
        ? "status reported a different adapter session"
        : statusProblem(status, this.capabilities);
    if (problem) {
      this.#fail(new Error(problem));
      return false;
    }
    this.status = status;
    this.view.setAdapter(status.revision, status.host_platform, status.name);
    return true;
  }

  // ---- Requests ----------------------------------------------------------

  /** Sends a command and resolves with its terminal result. */
  request<C extends CommandName>(command: C, args: ArgsOf<C>, options: RequestOptions<C> = {}) {
    return this.start(command, args, options).result;
  }

  /** Sends a command, exposing its request ID for replies and cancellation. */
  start<C extends CommandName>(command: C, args: ArgsOf<C>, options: RequestOptions<C> = {}): Started<C> {
    if (this.closed) return { id: 0, result: Promise.reject(new SessionClosedError()) };
    const limit = this.status?.limits.max_pending_requests ?? 4;
    const ordinary = [...this.#pending.values()].filter((p) => !RESERVED.has(p.command)).length;
    if (this.#pending.size >= limit + 8 || (!RESERVED.has(command) && ordinary >= limit))
      return { id: 0, result: Promise.reject(new Error("Too many pending adapter requests")) };
    if (this.#nextId > MAX_REQUEST_ID) {
      this.#fail(new Error("request IDs exhausted"));
      return { id: 0, result: Promise.reject(new SessionClosedError()) };
    }
    const id = this.#nextId++;
    const message = { v: command === "adapter.protocol" ? 0 : 1, id, cmd: command, args };
    const problem = requestProblem(command, message);
    if (problem) return { id, result: Promise.reject(new Error(`invalid ${command} request: ${problem}`)) };
    const result = new Promise<ResultOf<C>>((resolve, reject) => {
      const pending: Pending = {
        command,
        deviceId: args && "device_id" in args ? String(args.device_id) : undefined,
        chunks: 0,
        abandoned: false,
        onChunk: options.onChunk as Pending["onChunk"],
        onEvent: options.onEvent,
        resolve: resolve as (r: unknown) => void,
        reject,
      };
      if (options.timeoutMs)
        pending.timer = setTimeout(
          () => this.#fail(new Error(`${command} got no response`)),
          options.timeoutMs,
        );
      else if (options.waitMs)
        pending.timer = setTimeout(() => {
          pending.abandoned = true;
          pending.onChunk = undefined;
          pending.onEvent = undefined;
          reject(new Error(`${command} got no response`));
          const ordinary = [...this.#pending.values()].filter((p) => !RESERVED.has(p.command));
          if (ordinary.length >= limit && ordinary.every((p) => p.abandoned))
            this.#fail(new Error("adapter reads stopped responding"));
        }, options.waitMs);
      this.#pending.set(id, pending);
    });
    this.#hooks.changed();
    const settled = result.then(
      () => {},
      () => {},
    );
    this.#settled.set(id, settled);
    void settled.then(() => this.#settled.delete(id));
    const line = `${JSON.stringify(message)}\n`;
    this.#writes = this.#writes
      .then(() => (this.closed ? undefined : this.#transport.write(line)))
      .catch((error: Error) => this.#fail(new Error(`serial write failed: ${error.message}`)));
    return { id, result };
  }

  pendingFor(id: string) {
    return [...this.#pending].filter(([, p]) => p.deviceId === id).map(([requestId, p]) => ({ id: requestId, command: p.command }));
  }

  #receive(chunk: Uint8Array) {
    for (const line of this.#decoder.push(chunk)) {
      if (this.closed) return;
      let message: unknown;
      try {
        if ("error" in line) throw new Error(`adapter sent an ${line.error} line`);
        message = JSON.parse(line.text);
      } catch (error) {
        if (!this.#confirmed) {
          this.#hooks.log("ignoring input left from an earlier session");
          continue;
        }
        this.#fail(error instanceof SyntaxError ? new Error("adapter sent invalid JSON") : (error as Error));
        return;
      }
      this.#dispatch(message);
    }
  }

  #dispatch(message: unknown) {
    const m = message as { type?: unknown; id?: unknown };
    if (m.type === "response") {
      const pending = typeof m.id === "number" ? this.#pending.get(m.id) : undefined;
      if (!pending) {
        this.#hooks.log(`ignoring response to unknown request ${String(m.id)}`);
        return;
      }
      const problem = responseProblem(pending.command, message, this.status?.limits);
      if (problem && !this.#confirmed) return this.#hooks.log("ignoring input left from an earlier session");
      if (problem) return this.#fail(new Error(`invalid ${pending.command} response: ${problem}`));
      this.#confirmed = true;
      const r = message as { id: number; done: boolean; ok: boolean; result?: unknown; error?: WireError };
      if (!r.done) {
        const limit = pending.command === "device.list" ? this.status.limits.saved_devices
          : pending.command.startsWith("hidpp.setting.") ? this.status.limits.hidpp_settings : Infinity;
        if (++pending.chunks > limit) return this.#fail(new Error(`oversized ${pending.command} response`));
        pending.onChunk?.(r.result as never);
        return;
      }
      this.#pending.delete(r.id);
      clearTimeout(pending.timer);
      this.#hooks.changed();
      if (r.ok) pending.resolve(r.result);
      else pending.reject(new AdapterError(pending.command, r.error!));
      return;
    }
    // Events can't belong to this session before its first response.
    if (!this.#confirmed) return this.#hooks.log("ignoring input left from an earlier session");
    const problem = eventProblem(message, this.status?.limits);
    if (problem) return this.#fail(new Error(`invalid event: ${problem}`));
    const event = message as Event;
    if (event.event === "protocol.error") {
      this.#fail(new Error(`adapter reported protocol error ${event.data.code}`));
      return;
    }
    if (event.request_id !== undefined) {
      this.#pending.get(event.request_id)?.onEvent?.(event);
      return;
    }
    if (!this.view.event(event)) return;
    if (event.event.startsWith("device.") && event.event !== "device.info.changed") this.#refreshStatusSoon();
    this.#afterChange();
  }

  // ---- Synchronization ---------------------------------------------------

  /** Follows up on view changes: lost events, new devices, stale settings. */
  #afterChange() {
    this.#hooks.changed();
    if (this.readiness.state !== "ready") return;
    if (!this.view.valid) this.resync();
    else this.#fetchSoon();
  }

  /** Rebuilds the device view from a fresh snapshot (single flight). */
  resync() {
    if (this.#closing) return;
    if (this.#syncing) {
      this.#syncAgain = true;
      return;
    }
    this.#syncing = true;
    void (async () => {
      let attempt = 0;
      do {
        this.#syncAgain = false;
        try {
          await this.#snapshot();
          attempt = 0;
        } catch (error) {
          this.view.abortSnapshot();
          if (this.closed) break;
          // A list interrupted by a revision change reports busy; retry.
          const busy = error instanceof AdapterError && error.wire.code === "busy";
          this.#hooks.log(`device snapshot failed: ${(error as Error).message}`);
          await sleep(busy ? 100 : Math.min(30000, 1000 * 2 ** attempt++));
          this.#syncAgain = true;
        }
      } while (this.#syncAgain && !this.closed && !this.#closing);
      this.#syncing = false;
      if (!this.closed) this.#fetchSoon();
    })();
  }

  async #snapshot() {
    if (this.#closing) return;
    this.view.beginSnapshot();
    await this.request("session.monitor.set", { enabled: true }, { waitMs: READ_MS });
    if (this.#closing) return;
    this.#monitoring = true;
    const rows: ChunkOf<"device.list">[] = [];
    const end = await this.request("device.list", { filter: "saved" }, { onChunk: (row) => rows.push(row), waitMs: READ_MS });
    const ids = new Set(rows.map((r) => r.device.device_id));
    if (rows.some((r) => r.revision !== end.revision) || ids.size !== rows.length || rows.length !== end.count)
      throw new Error("inconsistent device snapshot");
    this.view.installSnapshot(
      rows.map((r) => r.device),
      end.revision,
    );
    if (!this.#installStatus(await this.request("adapter.status", {}, { waitMs: READ_MS }))) return;
    if (!this.view.valid) this.#syncAgain = true;
    this.#hooks.changed();
  }

  #fetchSoon() {
    this.#fetch().catch((error: Error) => {
      if (!this.closed) this.#hooks.log(`reading device details failed: ${error.message}`);
    });
  }

  /** Reads missing device information and watched settings lists. */
  async #fetch() {
    if (this.#fetching || this.closed || !this.view.valid) return;
    this.#fetching = true;
    try {
      for (;;) {
        const info = this.view.infoNeeded()[0];
        const settings = info === undefined ? this.view.settingsNeeded(this.#watched)[0] : undefined;
        if (info !== undefined) await this.#readInfo(info);
        else if (settings !== undefined) await this.#readSettings(settings);
        else break;
        this.#hooks.changed();
        if (this.closed || !this.view.valid) break;
      }
    } finally {
      this.#fetching = false;
    }
  }

  /** Runs a read, retrying while the adapter reports busy. */
  async #retryBusy<T>(read: () => Promise<T>): Promise<T> {
    for (let attempt = 1; ; attempt++) {
      try {
        return await read();
      } catch (error) {
        if (!(error instanceof AdapterError && error.wire.code === "busy") || attempt === 4 || this.closed) throw error;
        await sleep(250 * attempt);
      }
    }
  }

  async #readInfo(id: string) {
    const epoch = this.view.beginInfo(id);
    try {
      const info = await this.#retryBusy(() => this.request("device.info", { device_id: id }, { waitMs: READ_MS }));
      if (info.device_id !== id) throw new Error(`device.info answered for ${info.device_id}`);
      this.view.installInfo(info, epoch);
    } catch (error) {
      const reason = error instanceof AdapterError ? codeText(error.wire.code) : (error as Error).message;
      this.#hooks.log(`information of ${id} unavailable: ${reason}`);
      this.view.infoFailed(id, epoch, reason);
    }
  }

  /** Reads a device's information again and installs the snapshot. */
  async refreshInfo(id: string) {
    const epoch = this.view.beginInfo(id);
    const info = await this.request("device.info.refresh", { device_id: id });
    if (info.device_id !== id) throw new Error("the adapter answered for a different device");
    this.view.installInfo(info, epoch);
    this.#hooks.changed();
  }

  async #readSettings(id: string) {
    const epoch = this.view.beginSettings(id);
    try {
      const { end, rows } = await this.#retryBusy(async () => {
        const rows: ChunkOf<"hidpp.setting.list">[] = [];
        const end = await this.request("hidpp.setting.list", { device_id: id }, { onChunk: (row) => rows.push(row), waitMs: READ_MS });
        // All chunks and the summary share the captured revision.
        const keys = new Set(rows.map((r) => r.setting.key));
        const consistent =
          end.device_id === id &&
          rows.length === end.count &&
          keys.size === rows.length &&
          rows.every((r) => r.revision === end.revision && r.device_id === id);
        // An inconsistent list is read again, like a busy one.
        if (!consistent) throw new InconsistentList();
        return { end, rows };
      });
      this.view.installSettings(
        id,
        end.revision,
        rows.map((r) => r.setting),
        end.settings_state,
        end.settings_error,
        epoch,
      );
    } catch (error) {
      const reason =
        error instanceof InconsistentList ? "The adapter returned an inconsistent settings list."
          : error instanceof AdapterError ? codeText(error.wire.code) : (error as Error).message;
      this.#hooks.log(`settings of ${id} unavailable: ${reason}`);
      this.view.settingsFailed(id, epoch, reason);
    }
  }

  /** Keeps a device's settings list loaded while the UI shows it. */
  watchSettings(ids: Iterable<string>) {
    const next = [...ids];
    for (const id of next) if (!this.#watched.has(id)) this.view.retrySettings(id, false);
    this.#watched.clear();
    for (const id of next) this.#watched.add(id);
    this.#fetchSoon();
  }

  reloadSettings(id: string) {
    this.view.retrySettings(id);
    this.#fetchSoon();
  }

  /** Reads status again soon; capacity changes have no event of their own. */
  #refreshStatusSoon() {
    if (this.#statusTimer || this.readiness.state !== "ready") return;
    this.#statusTimer = setTimeout(() => {
      this.#statusTimer = undefined;
      this.request("adapter.status", {}, { waitMs: READ_MS })
        .then((status) => {
          if (this.#installStatus(status)) this.#hooks.changed();
        })
        .catch(() => {});
    }, STATUS_DELAY_MS);
  }

  /** Call after a mutation so counts and capacity follow it. */
  mutated() {
    this.#refreshStatusSoon();
  }

  async #beat() {
    if (this.#closing) return;
    try {
      const result = await this.request("session.heartbeat", {}, { timeoutMs: HEARTBEAT_TIMEOUT_MS });
      // After a missed deadline (for example across suspend) the adapter
      // turned monitoring off; turn it on again and resynchronize.
      if (!result.monitor && this.#monitoring && !this.#closing) {
        this.#monitoring = false;
        this.view.lose();
        this.resync();
      }
    } catch (error) {
      if (!this.closed && !this.#closing) this.#fail(new Error(`heartbeat failed: ${(error as Error).message}`));
    }
  }

  // ---- Shutdown ----------------------------------------------------------

  /** Orderly close: stop monitoring and cancel scans and pairing, then release. */
  async close() {
    if (this.closed || this.#closing) return;
    this.#closing = true;
    this.#monitoring = false;
    clearInterval(this.#heartbeat);
    const cancellable = [...this.#pending]
      .filter(([, p]) => p.command === "discovery.scan" || p.command === "pairing.start")
      .map(([id]) => id);
    // Wait for the cancelled requests' own responses too, so none is still
    // in flight when the port closes.
    const cleanup = [
      this.request("session.monitor.set", { enabled: false }),
      ...cancellable.map((id) => this.request("request.cancel", { request_id: id })),
      ...cancellable.map((id) => this.#settled.get(id) ?? Promise.resolve()),
    ].map((p) => p.catch(() => {}));
    await Promise.race([Promise.all(cleanup), sleep(CLEANUP_MS)]);
    this.#stop(new SessionClosedError());
    await this.#shutdownTransport();
  }

  async #shutdownTransport() {
    await this.#transport.close().catch(() => {});
  }

  #stop(error: Error) {
    this.closed = true;
    clearInterval(this.#heartbeat);
    clearTimeout(this.#statusTimer);
    for (const pending of this.#pending.values()) {
      clearTimeout(pending.timer);
      pending.reject(error instanceof SessionClosedError ? error : new SessionClosedError(error.message));
    }
    this.#pending.clear();
  }

  #fail(error: Error, notify = true) {
    if (this.closed) return;
    this.#stop(error);
    void this.#transport.close();
    if (notify) this.#hooks.closed(error);
  }
}
