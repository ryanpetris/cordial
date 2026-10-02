// One adapter's session: a Connection and the adapter's state, kept current
// from responses and events in the order the adapter sent them. Each device
// record, settings list and warning list arrives whole, so the latest one
// wins. Opening lists the devices, then each device's warnings and settings.
import { Connection, ConnectionClosedError, CordialError, UnexpectedResponseError, type ByteStream } from "@cordial/client";
import type { Event, Request, Response } from "@cordial/protocol";
import type { AdapterStatus, DeviceRecord, DeviceWarning, Setting } from "../shared/state.ts";
import { errorText } from "../shared/text.ts";
import * as convert from "./convert.ts";

export interface SessionHooks {
  /** Some adapter state visible to the user changed. */
  changed(): void;
  /** The session ended by itself: unplugged, failed or protocol violation. */
  closed(error: Error): void;
  /** A scan or pairing event, which the controller tracks. */
  event(event: Event): void;
  /** The adapter answered a request; events after this follow its result. */
  answered?(request: Request, response: Response): void;
  log(message: string): void;
}

/** Why a failed request failed, in words. */
export const failure = (error: unknown) =>
  error instanceof CordialError ? errorText(convert.wireError(error))
    : error instanceof ConnectionClosedError ? "The adapter disconnected before the change finished."
      : error instanceof UnexpectedResponseError ? "The adapter returned an unexpected result."
      : (error as Error).message;

export class AdapterSession {
  status: AdapterStatus;
  readonly devices = new Map<string, DeviceRecord>();
  readonly settings = new Map<string, Setting[]>();
  readonly settingsErrors = new Map<string, string>();
  readonly warnings = new Map<string, DeviceWarning[]>();
  readonly warningsErrors = new Map<string, string>();
  /** The device list has been read. */
  listed = false;

  readonly #hooks: SessionHooks;
  #connection!: Connection;
  /** Commands in flight, per device. */
  readonly #pending = new Map<string, string[]>();
  #syncing: Promise<void> | null = null;
  #syncAgain = false;

  private constructor(hooks: SessionHooks) {
    this.#hooks = hooks;
    this.status = null as unknown as AdapterStatus;
  }

  /** Starts a session and reads the adapter's status; rejects if the port isn't a working adapter. */
  static async open(stream: ByteStream, hooks: SessionHooks): Promise<AdapterSession> {
    const session = new AdapterSession(hooks);
    session.#connection = await Connection.open(stream, {
      onEvent: (event) => session.#event(event),
      onResponse: (request, response) => session.#response(request, response),
      onClose: (error) => hooks.closed(error),
      log: hooks.log,
    });
    // Callbacks during the handshake have already applied the status and any later adapter event.
    session.status ??= convert.status(session.#connection.status);
    return session;
  }

  get adapterId(): string {
    return this.status.id;
  }

  get closed(): boolean {
    return this.#connection.closed;
  }

  get connection(): Connection {
    return this.#connection;
  }

  /** Reads the devices and keeps them current. */
  run() {
    this.#sync();
  }

  /** Sends a command for a device, tracking it as pending while it runs. */
  async perform<T>(device: string, command: string, run: (c: Connection) => Promise<T>): Promise<T> {
    const list = this.#pending.get(device) ?? [];
    list.push(command);
    this.#pending.set(device, list);
    this.#hooks.changed();
    try {
      return await run(this.#connection);
    } finally {
      list.splice(list.indexOf(command), 1);
      if (!list.length) this.#pending.delete(device);
      this.#hooks.changed();
    }
  }

  pendingFor(id: string): string[] {
    return [...(this.#pending.get(id) ?? [])];
  }

  /** Lists the devices, then reads each device's warnings and settings (single flight). */
  #sync() {
    if (this.#syncing) {
      this.#syncAgain = true;
      return;
    }
    this.#syncing = (async () => {
      do {
        this.#syncAgain = false;
        try {
          await this.#connection.listDevices();
          this.listed = true;
          this.#hooks.changed();
          for (const id of [...this.devices.keys()]) await this.#readLists(id);
        } catch (error) {
          if (this.closed) break;
          this.#hooks.log(`listing devices failed: ${failure(error)}`);
        }
      } while (this.#syncAgain && !this.closed);
      this.#syncing = null;
    })();
  }

  async #readLists(id: string) {
    if (!this.devices.has(id) || this.closed) return;
    await this.#connection.listWarnings(id).then(
      () => this.warningsErrors.delete(id),
      (error: unknown) => this.#readFailed(id, this.warningsErrors, "warnings", error),
    );
    if (!this.devices.has(id) || this.closed) return;
    await this.#connection.listSettings(id).then(
      () => this.settingsErrors.delete(id),
      (error: unknown) => this.#readFailed(id, this.settingsErrors, "settings", error),
    );
    this.#hooks.changed();
  }

  #readFailed(id: string, errors: Map<string, string>, what: string, error: unknown) {
    if (this.closed || !this.devices.has(id)) return;
    const reason = failure(error);
    this.#hooks.log(`${what} of ${id} unavailable: ${reason}`);
    errors.set(id, reason);
  }

  /** Reads a device's warning and settings lists again, as after a failed read. */
  reload(id: string) {
    void this.#readLists(id);
  }

  // ---- State --------------------------------------------------------------

  #response(request: Request, response: Response) {
    const result = response.result;
    this.#hooks.answered?.(request, response);
    switch (result.case) {
      case "status":
        this.#setStatus(result.value);
        break;
      case "devices": {
        const listed = new Map(result.value.devices.map((d) => [d.id, convert.device(d)]));
        for (const id of this.devices.keys()) if (!listed.has(id)) this.#remove(id);
        for (const [id, d] of listed) this.devices.set(id, d);
        break;
      }
      case "device":
        this.#putDevice(convert.device(result.value), false);
        break;
      case "settings":
        if (this.devices.has(result.value.device)) this.settings.set(result.value.device, convert.settings(result.value.settings));
        break;
      case "warnings":
        if (this.devices.has(result.value.device)) this.warnings.set(result.value.device, result.value.warnings.map(convert.warning));
        break;
    }
    this.#hooks.changed();
  }

  #event(event: Event) {
    const kind = event.kind;
    switch (kind.case) {
      case "adapter": {
        const wasReady = this.status?.ready;
        this.#setStatus(kind.value);
        // Devices listed before the adapter was ready may be incomplete. Before open() returns,
        // run() lists them anyway.
        if (wasReady === false && this.status.ready && this.#connection) this.#sync();
        break;
      }
      case "device":
        this.#putDevice(convert.device(kind.value), true);
        break;
      case "deviceRemoved":
        this.#remove(kind.value.id);
        break;
      case "settings":
        if (this.devices.has(kind.value.device)) {
          this.settings.set(kind.value.device, convert.settings(kind.value.settings));
          this.settingsErrors.delete(kind.value.device);
        }
        break;
      case "warnings":
        if (this.devices.has(kind.value.device)) {
          this.warnings.set(kind.value.device, kind.value.warnings.map(convert.warning));
          this.warningsErrors.delete(kind.value.device);
        }
        break;
      default:
        this.#hooks.event(event);
        return;
    }
    this.#hooks.changed();
  }

  #setStatus(status: Parameters<typeof convert.status>[0]) {
    const next = convert.status(status);
    // Another adapter can't answer on this session; keep the identity it opened with.
    if (!this.status || next.id === this.status.id) this.status = next;
  }

  #putDevice(device: DeviceRecord, event: boolean) {
    const known = this.devices.has(device.id);
    this.devices.set(device.id, device);
    // A newly paired device's lists are read once; events keep them current.
    if (!known && event && this.listed) void this.#readLists(device.id);
  }

  #remove(id: string) {
    this.devices.delete(id);
    this.settings.delete(id);
    this.settingsErrors.delete(id);
    this.warnings.delete(id);
    this.warningsErrors.delete(id);
  }

  /** Ends the session; the adapter stops any scan and unsaved pairing. */
  async close() {
    await this.#connection.close();
  }
}
