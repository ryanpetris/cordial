import { vi } from "vitest";
import { FakeAdapter, type FakeOptions } from "../src/fake/adapter.ts";
import { Controller } from "../src/core/controller.ts";
import { AdapterSession } from "../src/core/session.ts";
import type { PortInfo } from "../src/core/transport.ts";
import { DEFAULT_PREFERENCES, type AppState } from "../src/shared/state.ts";

export const until = async (check: () => boolean, ms = 3000) => {
  const end = Date.now() + ms;
  while (!check()) {
    if (Date.now() > end) throw new Error("condition not reached");
    await new Promise((r) => setTimeout(r, 5));
  }
};

export async function openSession(options: FakeOptions = {}) {
  const fake = new FakeAdapter(options);
  const hooks = { changed: vi.fn(), closed: vi.fn(), log: vi.fn() };
  const session = await AdapterSession.open(fake.open(), hooks);
  session.run();
  return { fake, session, hooks };
}

/** A controller whose ports are simulated adapters, keyed by path. */
export function controller(fakes: Record<string, FakeAdapter>) {
  let state: AppState | null = null;
  // Tests may replace the port list and count opens after construction.
  const ports = { list: null as null | (() => Promise<PortInfo[]>), opened: 0 };
  const deps = {
    preferences: { ...DEFAULT_PREFERENCES },
    savePreferences: vi.fn(),
    hostPlatform: "linux" as const,
    published: vi.fn((s: AppState) => (state = s)),
    lowBattery: vi.fn(),
    connection: vi.fn(),
    log: vi.fn(),
    ports: fakes,
    listPorts: async () => (ports.list ? ports.list() : Object.entries(deps.ports).map(([path, f]) => ({ path, serial: f.id }))),
    openTransport: async (path: string) => {
      ports.opened++;
      const f = deps.ports[path];
      if (!f) throw new Error("no such port");
      return f.open();
    },
  };
  const c = new Controller(deps);
  return { c, deps, ports, state: () => state };
}
