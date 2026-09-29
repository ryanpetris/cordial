// Finds Cordial adapters and keeps one session open to each. A USB serial
// port with the Cordial ID is only a candidate: it becomes an adapter once
// the protocol handshake confirms it. Removal is silent.
import { AdapterSession } from "./session.ts";
import type { PortInfo, Transport } from "./transport.ts";
import type { Status } from "../protocol/types.ts";

export interface ManagerDeps {
  listPorts(): Promise<PortInfo[]>;
  openTransport(path: string): Promise<Transport>;
  log(message: string): void;
  /** Adapter list or any adapter's state changed. */
  changed(): void;
  /** A session was confirmed and registered. */
  opened?(id: string, session: AdapterSession): void;
}

export interface Connected {
  id: string;
  path: string;
  session: AdapterSession;
}

/** An adapter the user disconnected; its port is not opened until Connect. */
export interface Disconnected {
  id: string;
  /** Present while plugged in. */
  path: string | null;
  status: Status;
  connecting: boolean;
  error: string | null;
}

/** Rescans after a hotplug event, allowing udev time to finish setting up. */
const BURST_MS = [0, 300, 1000, 3000];
/** A session that fails this soon after opening isn't reopened automatically. */
const UNSTABLE_MS = 10000;

export class AdapterManager {
  readonly connected = new Map<string, Connected>();
  readonly disconnected = new Map<string, Disconnected>();
  readonly #deps: ManagerDeps;
  readonly #probing = new Set<string>();
  readonly #opened = new Map<AdapterSession, number>();
  /** Closes still releasing their ports, by adapter ID. */
  readonly #closing = new Map<string, Promise<void>>();
  #timers: ReturnType<typeof setTimeout>[] = [];
  #scanning: Promise<void> | null = null;
  #scanAgain = false;
  #stopped = false;

  constructor(deps: ManagerDeps) {
    this.#deps = deps;
  }

  /** Rescans now and a few times shortly after, as after a hotplug event. */
  burst() {
    for (const t of this.#timers) clearTimeout(t);
    this.#timers = BURST_MS.map((ms) => setTimeout(() => void this.rescan(), ms));
  }

  /** Compares the port list with known adapters (serialized). */
  rescan(): Promise<void> {
    if (this.#scanning) {
      this.#scanAgain = true;
      return this.#scanning;
    }
    this.#scanning = (async () => {
      do {
        this.#scanAgain = false;
        try {
          await this.#scan();
        } catch (error) {
          this.#deps.log(`listing serial ports failed: ${(error as Error).message}`);
        }
      } while (this.#scanAgain && !this.#stopped);
      this.#scanning = null;
    })();
    return this.#scanning;
  }

  async #scan() {
    if (this.#stopped) return;
    const ports = await this.#deps.listPorts();
    const paths = new Set(ports.map((p) => p.path));
    let changed = false;
    for (const [id, c] of this.connected)
      if (!paths.has(c.path)) {
        this.connected.delete(id);
        c.session.close().catch(() => {});
        changed = true;
      }
    // Without serial numbers (Web Serial), a disconnected adapter is known by its port.
    const holds = (d: Disconnected, p: PortInfo) => (p.serial === null ? p.path === d.path : p.serial === d.id);
    for (const d of this.disconnected.values()) {
      const path = ports.find((p) => holds(d, p))?.path ?? null;
      if (path !== d.path) {
        d.path = path;
        d.error = null;
        changed = true;
      }
    }
    if (changed) this.#deps.changed();
    const known = new Set([...this.connected.values()].map((c) => c.path));
    await Promise.all(
      ports
        .filter((p) => !known.has(p.path) && !this.#probing.has(p.path) && ![...this.disconnected.values()].some((d) => holds(d, p)))
        .map((p) => this.#probe(p)),
    );
  }

  async #probe(port: PortInfo) {
    this.#probing.add(port.path);
    try {
      const session = await this.#open(port.path);
      if (!session) return;
      const id = session.adapterId;
      if (this.connected.has(id) || this.disconnected.has(id) || this.#stopped) {
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
      // The USB serial number is the adapter ID (docs/protocol/transport.md); it is how a
      // disconnected adapter is recognized without opening its port.
      if (port.serial !== null && port.serial !== id) {
        this.#deps.log(`${port.path}: adapter ${id} reports USB serial ${port.serial || "(none)"}; ignoring it`);
        await session.close();
        return;
      }
      this.#register(id, port.path, session);
    } finally {
      this.#probing.delete(port.path);
    }
  }

  async #open(path: string): Promise<AdapterSession | null> {
    let transport: Transport;
    try {
      transport = await this.#deps.openTransport(path);
    } catch (error) {
      this.#deps.log(`${path}: ${(error as Error).message}`);
      return null;
    }
    let session: AdapterSession | undefined;
    try {
      session = await AdapterSession.open(transport, {
        changed: () => this.#deps.changed(),
        closed: (error) => session && this.#closed(session, error),
        log: (message) => this.#deps.log(`${path}: ${message}`),
      });
      return session;
    } catch (error) {
      this.#deps.log(`${path} is not a usable Cordial adapter: ${(error as Error).message}`);
      return null;
    }
  }

  #register(id: string, path: string, session: AdapterSession) {
    this.connected.set(id, { id, path, session });
    this.#opened.set(session, Date.now());
    this.#deps.opened?.(id, session);
    session.run();
    this.#deps.changed();
  }

  /** A session ended on its own: remove it silently, and reopen once if the
   * port is still there, for example after a transient failure. */
  #closed(session: AdapterSession, error: Error) {
    const entry = [...this.connected.values()].find((c) => c.session === session);
    const opened = this.#opened.get(session) ?? 0;
    this.#opened.delete(session);
    if (!entry) return;
    this.connected.delete(entry.id);
    this.#deps.log(`${entry.path}: session ended: ${error.message}`);
    this.#deps.changed();
    if (Date.now() - opened >= UNSTABLE_MS && !this.#stopped)
      this.#timers.push(setTimeout(() => void this.rescan(), 500));
  }

  /** Closes an adapter's session and keeps it listed as disconnected. */
  async disconnect(id: string) {
    const c = this.connected.get(id);
    if (!c) return;
    this.connected.delete(id);
    this.disconnected.set(id, { id, path: c.path, status: { ...c.session.status, name: c.session.view.name ?? c.session.status.name }, connecting: false, error: null });
    this.#deps.changed();
    const closing = c.session.close();
    this.#closing.set(id, closing);
    await closing;
    this.#closing.delete(id);
  }

  /** Opens a disconnected adapter again. Resolves with an error message. */
  async connect(id: string): Promise<string | null> {
    const d = this.disconnected.get(id);
    if (!d || d.connecting) return null;
    if (!d.path) return "The adapter isn't plugged in.";
    d.connecting = true;
    d.error = null;
    this.#deps.changed();
    // The previous session must release the port first.
    await this.#closing.get(id);
    const path = d.path;
    this.#probing.add(path);
    try {
      const session = await this.#open(path);
      d.connecting = false;
      if (!session || session.adapterId !== id) {
        await session?.close();
        d.error = "Couldn't connect. Is another program using it?";
        this.#deps.changed();
        return d.error;
      }
      this.disconnected.delete(id);
      this.#register(id, path, session);
      return null;
    } finally {
      this.#probing.delete(path);
    }
  }

  /** Closes every session in an orderly way. */
  async stop() {
    this.#stopped = true;
    for (const t of this.#timers) clearTimeout(t);
    const sessions = [...this.connected.values()].map((c) => c.session);
    this.connected.clear();
    await Promise.all(sessions.map((s) => s.close().catch(() => {})));
  }
}
