import type { ByteStream } from "@cordial/client";
import { CURRENT_PROTOCOL_VERSION } from "@cordial/protocol";
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
        expect(other.hellos).toBe(1);
        expect(other.received).toEqual([]);
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

  it("closes a reconnected session that is waiting for the old one to close before stop settles", async () => {
    const fake = new FakeAdapter({ adapterId: "AAAA0001" });
    const { c, deps, ports } = controller({ "/a": fake });
    let open = 0;
    let latest: ByteStream | null = null;
    ports.open = async () => {
      open++;
      // The first port takes a while to release; the fake is one object across sessions.
      const delay = open === 1 ? 200 : 20;
      const inner = fake.open();
      const stream: ByteStream = {
        onData: (listener) => inner.onData(listener),
        onClose: (listener) => inner.onClose(listener),
        write: (bytes) => inner.write(bytes),
        close: async () => {
          await new Promise((resolve) => setTimeout(resolve, delay));
          if (latest === stream) await inner.close();
          open--;
        },
      };
      latest = stream;
      return stream;
    };
    await c.manager.rescan();
    await until(() => c.manager.connected.get(fake.id)?.session.available === true);
    // USB re-enumerates on another port before the old one has closed.
    delete deps.ports["/a"];
    deps.ports["/b"] = fake;
    await c.manager.rescan();
    await until(() => open === 2 && fake.received.some((r) => r.command.case === "getStatus"));
    await new Promise((resolve) => setTimeout(resolve, 20));
    expect(c.manager.connected.get(fake.id)!.session.closed).toBe(true);
    await c.stop();
    expect(open).toBe(0);
  });
});
