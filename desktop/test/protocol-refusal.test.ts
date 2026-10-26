import { CURRENT_PROTOCOL_VERSION } from "@cordial/protocol";
import { describe, expect, it, vi } from "vitest";
import { FakeAdapter } from "../src/fake/adapter.ts";
import { MIGRATION_TEXT, REFUSALS } from "../src/shared/text.ts";
import { controller, until } from "./helpers.ts";

const NEWER = CURRENT_PROTOCOL_VERSION + 1;

describe("an adapter on a newer protocol", () => {
  it("is listed as refused with what its hello reported, and receives no requests", async () => {
    const fake = new FakeAdapter({ adapterId: "AAAA0001" });
    fake.protocolVersion = NEWER;
    const { c, deps } = controller({ "/a": fake });
    try {
      await c.manager.rescan();
      await until(() => c.manager.refused.size === 1);
      expect(fake.hellos).toBe(1);
      expect(fake.received).toEqual([]);
      expect(c.manager.connected.size).toBe(0);
      expect(c.state().adapters).toMatchObject([{
        id: "port:/a",
        name: fake.name,
        connection: "disconnected",
        connectError: REFUSALS.newer,
        status: null,
        refused: { firmwareVersion: "0.0.0-dev", serialNumber: fake.id },
      }]);
      expect(deps.log).toHaveBeenCalledWith(`/a: ${REFUSALS.newer}`);
    } finally {
      await c.stop();
    }
  });

  it("stays listed without a name, firmware version or serial number", async () => {
    const fake = new FakeAdapter();
    fake.protocolVersion = NEWER;
    fake.helloMetadata = false;
    const { c } = controller({ "/a": fake });
    try {
      await c.manager.rescan();
      await until(() => c.manager.refused.size === 1);
      expect(c.state().adapters).toMatchObject([{ id: "port:/a", name: "Adapter", connectError: REFUSALS.newer, refused: { firmwareVersion: null, serialNumber: null } }]);
    } finally {
      await c.stop();
    }
  });

  it.each([
    { firmware: "1.2\u2066.3", serial: "SER\u202eIAL\u0000", expected: { firmwareVersion: "1.2.3", serialNumber: "SERIAL" } },
    { firmware: "\u202e\u0000", serial: "\u2066\n", expected: { firmwareVersion: null, serialNumber: null } },
  ])("cleans refused metadata for display without changing identity: $serial", async ({ firmware, serial, expected }) => {
    const fake = new FakeAdapter();
    fake.protocolVersion = NEWER;
    const { c } = controller({ "/a": fake });
    try {
      await c.manager.rescan();
      await until(() => c.manager.refused.size === 1);
      const hello = c.manager.refused.get("/a")!.hello;
      hello.firmwareVersion = firmware;
      hello.serialNumber = serial;
      expect(c.state().adapters[0]!.refused).toEqual(expected);
      expect(hello.serialNumber).toBe(serial);
    } finally {
      await c.stop();
    }
  });

  it("shows only what the adapter now on its port reports", async () => {
    const first = new FakeAdapter({ adapterId: "AAAA0001" });
    first.protocolVersion = NEWER;
    const second = new FakeAdapter({ adapterId: "BBBB0002" });
    second.protocolVersion = NEWER;
    second.helloMetadata = false;
    const { c, deps } = controller({ "/a": first });
    try {
      await c.manager.rescan();
      await until(() => c.manager.refused.size === 1);
      deps.ports["/a"] = second;
      expect(await c.manager.connect("port:/a")).toBe(REFUSALS.newer);
      expect(c.state().adapters).toMatchObject([{ id: "port:/a", name: "Adapter", refused: { firmwareVersion: null, serialNumber: null } }]);

      // An adapter on the current protocol is known by its own status.
      deps.ports["/a"] = first;
      first.protocolVersion = CURRENT_PROTOCOL_VERSION;
      expect(await c.manager.connect("port:/a")).toBeNull();
      await until(() => c.manager.connected.get(first.id)?.session.available === true);
      expect(c.state().adapters.map((a) => a.id)).toEqual([first.id]);
    } finally {
      await c.stop();
    }
  });

  it("isn't opened again until a refresh or Connect, and leaves the list when unplugged", async () => {
    const fake = new FakeAdapter();
    fake.protocolVersion = NEWER;
    const { c, deps, ports } = controller({ "/a": fake });
    try {
      await c.manager.rescan();
      await until(() => c.manager.refused.size === 1);
      for (let i = 0; i < 5; i++) await c.manager.rescan();
      expect(ports.opened).toBe(1);

      // A refresh or a hotplug event opens it once, however many rescans follow.
      vi.useFakeTimers();
      c.manager.burst();
      await vi.advanceTimersByTimeAsync(3100);
      vi.useRealTimers();
      await until(() => c.manager.refused.get("/a")?.retry === false && ports.opened === 2);
      await c.manager.rescan();
      expect(ports.opened).toBe(2);
      expect(c.state().adapters).toHaveLength(1);

      expect(await c.manager.connect("port:/a")).toBe(REFUSALS.newer);
      expect(ports.opened).toBe(3);

      fake.protocolVersion = CURRENT_PROTOCOL_VERSION;
      expect(await c.manager.connect("port:/a")).toBeNull();
      await until(() => c.manager.connected.get(fake.id)?.session.available === true);
      expect(c.manager.refused.size).toBe(0);
      expect(c.state().adapters.map((a) => [a.id, a.connection, a.connectError])).toEqual([[fake.id, "connected", null]]);

      fake.unplug();
      fake.protocolVersion = NEWER;
      await until(() => c.manager.refused.size === 1);
      delete deps.ports["/a"];
      await c.manager.rescan();
      expect(c.state().adapters).toEqual([]);
    } finally {
      vi.useRealTimers();
      await c.stop();
    }
  });

  it("ends the recovery of a returning adapter that is now refused", async () => {
    const fake = new FakeAdapter();
    const { c } = controller({ "/a": fake });
    try {
      await c.manager.rescan();
      await until(() => c.manager.connected.get(fake.id)?.session.available === true);
      fake.protocolVersion = NEWER;
      fake.unplug();
      await until(() => c.manager.refused.size === 1);
      expect(c.manager.connected.size).toBe(0);
      expect(c.state().adapters.map((a) => a.id)).toEqual(["port:/a"]);
    } finally {
      await c.stop();
    }
  });

  it("explains why a disconnected adapter can't connect again", async () => {
    const fake = new FakeAdapter();
    const { c } = controller({ "/a": fake });
    try {
      await c.manager.rescan();
      await until(() => c.manager.connected.get(fake.id)?.session.available === true);
      await c.manager.disconnect(fake.id);
      fake.protocolVersion = NEWER;
      expect(await c.manager.connect(fake.id)).toBe(REFUSALS.newer);
      expect(c.state().adapters).toMatchObject([{ id: fake.id, connection: "disconnected", connectError: REFUSALS.newer }]);
      expect(fake.received).toEqual([]);
    } finally {
      await c.stop();
    }
  });

  it("never starts a migration, even when the adapter needs one", async () => {
    const fake = new FakeAdapter();
    fake.protocolVersion = NEWER;
    fake.storageFormat = 1;
    fake.migrationRequired = true;
    const migration = { find: vi.fn(() => "/usr/bin/cordial"), start: vi.fn() };
    const { c } = controller({ "/a": fake }, { migration });
    try {
      await c.manager.rescan();
      await until(() => c.manager.refused.size === 1);
      expect(c.state().adapters).toMatchObject([{ id: "port:/a", status: null, migration: null }]);
      expect((await c.act({ type: "migration.start", adapterId: "port:/a" })).ok).toBe(false);
      await new Promise((r) => setTimeout(r, 20));
      expect(migration.start).not.toHaveBeenCalled();
    } finally {
      await c.stop();
    }
  });

  it("ends a Connect in progress as busy when adapters are suspended", async () => {
    const fake = new FakeAdapter();
    fake.protocolVersion = NEWER;
    const { c } = controller({ "/a": fake });
    try {
      await c.manager.rescan();
      await until(() => c.manager.refused.size === 1);
      const release = fake.hold();
      const connecting = c.manager.connect("port:/a");
      await until(() => fake.hellos === 1);
      const suspending = c.manager.suspend();
      release();
      await suspending;
      expect(await connecting).toBe(MIGRATION_TEXT.busy);
      expect(c.state().adapters).toMatchObject([{ id: "port:/a", connection: "disconnected" }]);
    } finally {
      await c.stop();
    }
  });

  it("isn't opened again by Connect while adapters are suspended", async () => {
    const fake = new FakeAdapter();
    fake.protocolVersion = NEWER;
    const { c, ports } = controller({ "/a": fake });
    try {
      await c.manager.rescan();
      await until(() => c.manager.refused.size === 1);
      await c.manager.suspend();
      expect(await c.manager.connect("port:/a")).toBe(MIGRATION_TEXT.busy);
      expect(ports.opened).toBe(1);
      expect(c.state().adapters).toMatchObject([{ id: "port:/a", connection: "disconnected", connectError: REFUSALS.newer }]);
      c.manager.resume();
      await until(() => ports.opened === 2 && c.manager.refused.get("/a")?.retry === false);
    } finally {
      await c.stop();
    }
  });
});
