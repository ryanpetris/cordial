import { describe, expect, it, vi } from "vitest";
import { FakeAdapter } from "../src/fake/adapter.ts";
import { controller, until } from "./helpers.ts";

describe("reconnect probe ownership", () => {
  describe.each(["/a", "/b"])("on %s", (path) => {
    it.each(["expiry", "disconnect"])("registers an unrelated unknown-serial adapter whose handshake overlaps %s", async (end) => {
      const original = new FakeAdapter({ adapterId: "AAAA0001" });
      const other = new FakeAdapter({ adapterId: "BBBB0002" });
      const { c, deps, ports } = controller({ "/a": original });
      const release = other.hold();
      try {
        await c.manager.rescan();
        await until(() => c.manager.connected.get(original.id)?.session.available === true);
        vi.useFakeTimers();
        delete deps.ports["/a"];
        original.unplug();
        await vi.advanceTimersByTimeAsync(14500);
        expect(c.state().adapters[0]!.connection).toBe("reconnecting");

        deps.ports[path] = other;
        ports.list = async () => [{ path, serial: null }];
        await c.manager.rescan();
        await vi.advanceTimersByTimeAsync(0);
        expect(other.received.some((request) => request.command.case === "getStatus")).toBe(true);
        expect(c.manager.connected.has(other.id)).toBe(false);

        if (end === "expiry") await vi.advanceTimersByTimeAsync(500);
        else await c.manager.disconnect(original.id);
        expect(c.manager.connected.has(original.id)).toBe(false);

        release();
        await vi.advanceTimersByTimeAsync(0);
        expect(c.manager.connected.get(other.id)).toMatchObject({ path, session: { available: true } });
        expect(c.state().adapters.find((adapter) => adapter.id === other.id)?.connection).toBe("connected");
        expect(ports.opened).toBe(2);
      } finally {
        release();
        await c.stop();
        if (vi.isFakeTimers()) expect(vi.getTimerCount()).toBe(0);
        vi.useRealTimers();
      }
    });
  });
});
