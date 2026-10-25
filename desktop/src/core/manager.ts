// Finds Cordial adapters and keeps one session open to each. A USB serial
// port with the Cordial ID is only a candidate: it becomes an adapter once
// the adapter answers its first request. Removal is silent.
import { serialMatches, type ByteStream, type PortInfo } from "@cordial/client";
import type { Event, Request, Response } from "@cordial/protocol";
import type { AdapterStatus } from "../shared/state.ts";
import { clean } from "../shared/text.ts";
import { AdapterSession } from "./session.ts";

export interface ManagerDeps {
  listPorts(): Promise<PortInfo[]>;
  openTransport(path: string): Promise<ByteStream>;
  log(message: string): void;
  /** Adapter list or any adapter's state changed. */
  changed(): void;
  /** A session's scan or pairing event. */
  event?(session: AdapterSession, event: Event): void;
  /** A request the session's adapter answered, in order with its events. */
  answered?(session: AdapterSession, request: Request, response: Response): void;
}

export interface Connected {
  id: string;
  path: string;
  serial: string | null;
  session: AdapterSession;
  recovery: { deadline: ReturnType<typeof setTimeout>; retry: ReturnType<typeof setInterval> } | null;
}

/** An adapter the user disconnected; its port is not opened until Connect. */
export interface Disconnected {
  id: string;
  /** Present while plugged in. */
  path: string | null;
  status: AdapterStatus;
  connecting: boolean;
  error: string | null;
}

/** Rescans after a hotplug event, allowing udev time to finish setting up. */
const BURST_MS = [0, 300, 1000, 3000];
/** Keeps configuration pages mounted while USB re-enumerates. */
const RECONNECT_MS = 15000;
const RETRY_MS = 500;

export class AdapterManager {
  readonly connected = new Map<string, Connected>();
  readonly disconnected = new Map<string, Disconnected>();
  readonly #deps: ManagerDeps;
  readonly #probing = new Map<string, Promise<void>>();
  readonly #opening = new Map<string, { cancel(): Promise<void> | null; owners: Connected[] }>();
  /** Closes still releasing their ports, by adapter ID. */
  readonly #closing = new Map<string, { path: string; done: Promise<void> }>();
  #timers: ReturnType<typeof setTimeout>[] = [];
  #scanning: Promise<void> | null = null;
  #scanAgain = false;
  #stopped = false;

  constructor(deps: ManagerDeps) {
    this.#deps = deps;
  }

  /** Rescans now and a few times shortly after, as after a hotplug event. */
  burst() {
    if (this.#stopped) return;
    for (const t of this.#timers) clearTimeout(t);
    this.#timers = BURST_MS.map((ms) => setTimeout(() => void this.rescan(), ms));
  }

  /** Serializes port enumeration; each path probes independently of the next enumeration. */
  rescan(): Promise<void> {
    if (this.#scanning) {
      this.#scanAgain = true;
      return this.#scanning;
    }
    this.#scanning = (async () => {
      const recovering = new Map([...this.connected].filter(([, c]) => c.serial !== null));
      do {
        this.#scanAgain = false;
        try {
          await this.#scan(recovering);
        } catch (error) {
          this.#deps.log(`listing serial ports failed: ${(error as Error).message}`);
        }
      } while (this.#scanAgain && !this.#stopped);
      this.#scanning = null;
    })();
    return this.#scanning;
  }

  async #scan(recovering: Map<string, Connected>) {
    if (this.#stopped) return;
    for (const [id, c] of this.connected) if (c.serial !== null) recovering.set(id, c);
    const ports = await this.#deps.listPorts();
    if (this.#stopped) return;
    const paths = new Set(ports.map((p) => p.path));
    let changed = false;
    for (const c of this.connected.values()) {
      if (!paths.has(c.path) && !c.session.closed) {
        this.#recover(c);
        this.#close(c);
        changed = true;
      } else if (c.recovery && !c.session.closed && !c.session.available) {
        c.session.relist();
      }
    }
    // A disconnected adapter is known by the adapter ID that begins its USB serial number, or by
    // its port where serial numbers can't be read (Web Serial).
    const holds = (d: Disconnected, p: PortInfo) => (p.serial === null ? p.path === d.path : serialMatches(p.serial, d.id));
    for (const d of this.disconnected.values()) {
      const path = ports.find((p) => holds(d, p))?.path ?? null;
      if (path !== d.path) {
        d.path = path;
        d.error = null;
        changed = true;
      }
    }
    if (changed) this.#deps.changed();
    for (const [id, c] of this.connected) if (c.recovery) recovering.set(id, c);
    const known = new Set([
      ...[...this.connected.values()].filter((c) => !c.session.closed).map((c) => c.path),
      ...[...this.#closing.values()].map((c) => c.path),
    ]);
    for (const port of ports) {
      if (known.has(port.path) || this.#probing.has(port.path) || [...this.disconnected.values()].some((d) => holds(d, port))) continue;
      // A late enumeration cannot reopen an adapter whose recovery has ended.
      if ([...recovering.values()].some((c) => this.connected.get(c.id) !== c &&
        port.serial !== null && serialMatches(port.serial, c.id))) continue;
      const probe = this.#probe(port, recovering).finally(() => this.#probing.delete(port.path));
      this.#probing.set(port.path, probe);
    }
  }

  async #probe(port: PortInfo, recovering: Map<string, Connected>) {
    try {
      const session = await this.#open(port.path, [...recovering.values()].filter((c) => port.serial !== null && serialMatches(port.serial, c.id)));
      if (!session) return;
      const id = session.adapterId;
      const entry = this.connected.get(id);
      const expired = recovering.has(id) && recovering.get(id) !== entry;
      if (expired || this.#stopped || this.disconnected.has(id) || (entry &&
        (!entry.recovery || !entry.session.closed || !port.serial || !serialMatches(port.serial, id)))) {
        await session.close();
        // A disconnected adapter found on another port, such as after being
        // plugged in again without a readable serial number, stays disconnected.
        const d = this.disconnected.get(id);
        if (d && d.path !== port.path) {
          d.path = port.path;
          d.error = null;
          this.#deps.changed();
        }
        return;
      }
      // A session that ended before it was registered had no entry to remove.
      if (session.closed) {
        this.#deps.log(`${port.path}: session ended while opening`);
        return;
      }
      if (entry) {
        await this.#closing.get(id)?.done;
        if (session.closed) this.#deps.log(`${port.path}: session ended while opening`);
        if (session.closed || this.connected.get(id) !== entry || this.#stopped || !entry.recovery || !entry.session.closed) {
          await session.close();
          return;
        }
        entry.path = port.path;
        entry.serial = port.serial;
        entry.session.replace(session);
        if (!entry.session.status.ready) this.#clearRecovery(entry);
        this.#deps.changed();
      } else this.#register(id, port.path, session, port.serial);
    } catch (error) {
      this.#deps.log(`${port.path}: opening session failed: ${(error as Error).message}`);
    }
  }

  async #open(path: string, owners: Connected[] = []): Promise<AdapterSession | null> {
    let cancelled = false;
    let closing: Promise<void> | null = null;
    let cancelConnection = () => {};
    this.#opening.set(path, { owners, cancel: () => {
      cancelled = true;
      cancelConnection();
      return closing;
    } });
    try {
      let transport: ByteStream;
      try {
        transport = await this.#deps.openTransport(path);
      } catch (error) {
        this.#deps.log(`${path}: ${(error as Error).message}`);
        return null;
      }
      if (cancelled || this.#stopped) {
        await transport.close().catch(() => {});
        return null;
      }
      let session: AdapterSession | undefined;
      try {
        session = await AdapterSession.open({
          onData: (listener) => transport.onData(listener),
          onClose: (listener) => {
            transport.onClose(listener);
            cancelConnection = () => listener(new Error("opening cancelled"));
          },
          write: (bytes) => transport.write(bytes),
          close: () => closing ??= transport.close(),
        }, {
          changed: () => {
            for (const c of this.connected.values())
              if (c.recovery && !c.session.closed && (c.session.available || !c.session.status.ready)) this.#clearRecovery(c);
            this.#deps.changed();
          },
          closed: (error) => session && this.#closed(session, error),
          event: (event) => session && this.#deps.event?.(session, event),
          answered: (request, response) => session && this.#deps.answered?.(session, request, response),
          log: (message) => this.#deps.log(`${session?.adapterId ?? path}: ${message}`),
        });
        return session;
      } catch (error) {
        this.#deps.log(`${path} is not a usable Cordial adapter: ${(error as Error).message}`);
        return null;
      }
    } finally {
      this.#opening.delete(path);
    }
  }

  #register(id: string, path: string, session: AdapterSession, serial: string | null) {
    this.connected.set(id, { id, path, serial: serial && serialMatches(serial, id) ? serial : null, session, recovery: null });
    session.run();
    this.#deps.changed();
  }

  #closed(session: AdapterSession, error: Error) {
    const entry = [...this.connected.values()].find((c) => c.session === session);
    if (!entry) return;
    this.#recover(entry);
    void this.#close(entry);
    this.#deps.log(`${entry.path}: session ended: ${error.message}`);
    this.#deps.changed();
  }

  #clearRecovery(entry: Connected) {
    if (!entry.recovery) return;
    clearTimeout(entry.recovery.deadline);
    clearInterval(entry.recovery.retry);
    entry.recovery = null;
  }

  #close(entry: Connected) {
    const closing = entry.session.close().catch(() => {});
    this.#closing.set(entry.id, { path: entry.path, done: closing });
    void closing.finally(() => {
      if (this.#closing.get(entry.id)?.done === closing) this.#closing.delete(entry.id);
    });
    return closing;
  }

  #cancelOpening(entry: Connected) {
    for (const opening of this.#opening.values())
      if (opening.owners.includes(entry)) opening.cancel();
  }

  #recover(entry: Connected) {
    if (entry.recovery || this.#stopped) return;
    if (entry.serial === null) {
      this.connected.delete(entry.id);
      return;
    }
    entry.recovery = {
      deadline: setTimeout(() => {
        if (this.connected.get(entry.id) !== entry) return;
        this.#clearRecovery(entry);
        this.connected.delete(entry.id);
        this.#cancelOpening(entry);
        void this.#close(entry);
        this.#deps.changed();
      }, RECONNECT_MS),
      retry: setInterval(() => void this.rescan(), RETRY_MS),
    };
  }

  /** Closes an adapter's session and keeps it listed as disconnected. */
  async disconnect(id: string) {
    const c = this.connected.get(id);
    if (!c) return;
    this.#clearRecovery(c);
    this.#cancelOpening(c);
    this.connected.delete(id);
    this.disconnected.set(id, { id, path: c.path, status: c.session.status, connecting: false, error: null });
    this.#deps.changed();
    await this.#close(c);
  }

  /** Opens a disconnected adapter again. Resolves with an error message. */
  async connect(id: string): Promise<string | null> {
    if (this.#stopped) return "This adapter is no longer available.";
    const d = this.disconnected.get(id);
    if (!d || d.connecting) return null;
    if (!d.path) return "The adapter isn't plugged in.";
    d.connecting = true;
    d.error = null;
    this.#deps.changed();
    // The previous session must release the port first.
    await this.#closing.get(id)?.done;
    if (d.path) await this.#probing.get(d.path);
    if (this.#stopped) {
      d.connecting = false;
      return "This adapter is no longer available.";
    }
    const path = d.path;
    if (!path) {
      d.connecting = false;
      return "The adapter isn't plugged in.";
    }
    const serial = (await this.#deps.listPorts().catch(() => [])).find((p) => p.path === path)?.serial ?? null;
    const session = await this.#open(path);
    d.connecting = false;
    if (this.#stopped) {
      await session?.close();
      return "This adapter is no longer available.";
    }
    if (!session || session.closed || session.adapterId !== id) {
      await session?.close();
      const name = clean(d.status.name) || "the adapter";
      d.error = session && session.adapterId !== id
        ? `Cordial couldn't connect to ${name}. Another adapter answered on its port.`
        : `Cordial couldn't connect to ${name}. Close any other app that uses it, then try again.`;
      this.#deps.changed();
      return d.error;
    }
    this.disconnected.delete(id);
    this.#register(id, path, session, serial);
    return null;
  }

  /** Closes every session in an orderly way. */
  async stop() {
    this.#stopped = true;
    const openings = [...this.#opening.values()].map((opening) => opening.cancel()?.catch(() => {}));
    for (const t of this.#timers) clearTimeout(t);
    for (const c of this.connected.values()) this.#clearRecovery(c);
    const sessions = [...this.connected.values()].map((c) => c.session);
    this.connected.clear();
    await Promise.all([...openings, ...[...this.#closing.values()].map((c) => c.done), ...sessions.map((s) => s.close().catch(() => {}))]);
  }
}
