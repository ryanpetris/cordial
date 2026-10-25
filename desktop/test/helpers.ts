import { vi } from "vitest";
import type { PortInfo } from "@cordial/client";
import { IntegrationKind } from "@cordial/protocol";
import { FakeAdapter, type FakeOptions } from "../src/fake/adapter.ts";
import { Controller } from "../src/core/controller.ts";
import { information } from "../src/core/convert.ts";
import { AdapterSession } from "../src/core/session.ts";
import { DEFAULT_PREFERENCES, type AppState, type DeviceRecord, type SettingField, type SettingFields, type SettingsGroupName } from "../src/shared/state.ts";

export const until = async (check: () => boolean, ms = 3000) => {
  const end = Date.now() + ms;
  while (!check()) {
    if (Date.now() > end) throw new Error("condition not reached");
    await new Promise((r) => setTimeout(r, 5));
  }
};

/** A device record as the app keeps it. */
export function record(id: number, patch: Partial<DeviceRecord> = {}): DeviceRecord {
  return {
    id,
    transport: "ble",
    name: String(id),
    kinds: ["keyboard"],
    state: "disconnected",
    enabled: true,
    trusted: true,
    blocked: false,
    paused: false,
    inactive: null,
    errors: [],
    security: null,
    hidpp: null,
    information: information(undefined),
    roles: [],
    profiles: [],
    outputPreferences: [],
    outputs: [],
    ...patch,
  };
}

export async function openSession(options: FakeOptions = {}) {
  const fake = new FakeAdapter(options);
  const hooks = { changed: vi.fn(), closed: vi.fn(), event: vi.fn(), log: vi.fn() };
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
    profileAlert: vi.fn(),
    log: vi.fn(),
    ports: fakes,
    listPorts: async () => (ports.list ? ports.list() : Object.entries(deps.ports).filter(([, f]) => f.present).map(([path, f]) => ({ path, serial: f.serial }))),
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

/** A HID++ setting field's identity. */
export const field = <G extends SettingsGroupName>(group: G, name: SettingFields[G], index: number | null = null) =>
  ({ integration: IntegrationKind.HIDPP, group, index, field: name }) as SettingField;
