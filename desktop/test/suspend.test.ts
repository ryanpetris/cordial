import type { ByteStream, PortInfo } from "@cordial/client";
import { describe, expect, it, vi } from "vitest";
import { FakeAdapter } from "../src/fake/adapter.ts";
import { MIGRATION_TEXT } from "../src/shared/text.ts";
import { controller, until } from "./helpers.ts";

/** A promise with its resolver. */
function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((r) => (resolve = r));
  return { promise, resolve };
}

/** Records whether a promise has settled. */
function track(promise: Promise<unknown>) {
  const state = { done: false };
  void promise.then(() => (state.done = true));
  return state;
}

/** Counts open streams across the harness's ports. */
function counted(h: ReturnType<typeof controller>) {
  let open = 0;
  const wrap = (stream: ByteStream): ByteStream => {
    open++;
    let closed = false;
    const done = () => {
      if (!closed) open--;
      closed = true;
    };
    stream.onClose(done);
    return { onData: (l) => stream.onData(l), onClose: (l) => stream.onClose(l), write: (b) => stream.write(b), close: async () => { await stream.close(); done(); } };
  };
  return { wrap, open: () => open };
}

describe("suspending every adapter", () => {
  it("cancels a held handshake and waits for its port to close", async () => {
    const fake = new FakeAdapter();
    const h = controller({ "/a": fake });
    const streams = counted(h);
    h.ports.open = async (path) => streams.wrap(h.deps.ports[path]!.open());
    const release = fake.hold();
    void h.c.manager.rescan();
    await until(() => fake.received.length === 1);
    await h.c.manager.suspend();
    expect(streams.open()).toBe(0);
    release();
    await new Promise((r) => setTimeout(r, 20));
    expect(h.c.manager.connected.size).toBe(0);
    expect(h.ports.opened).toBe(1);
    await h.c.stop();
  });

  it("waits for a port still opening and closes it once it opens", async () => {
    const fake = new FakeAdapter();
    const h = controller({ "/a": fake });
    const streams = counted(h);
    const opening = deferred<ByteStream>();
    h.ports.open = () => opening.promise;
    void h.c.manager.rescan();
    await until(() => h.ports.opened === 1);
    const suspended = track(h.c.manager.suspend());
    await new Promise((r) => setTimeout(r, 20));
    expect(suspended.done).toBe(false);
    opening.resolve(streams.wrap(fake.open()));
    await until(() => suspended.done);
    expect(streams.open()).toBe(0);
    expect(fake.received).toEqual([]);
    expect(h.c.manager.connected.size).toBe(0);
    await h.c.stop();
  });

  it("closes every session and opens nothing until resumed", async () => {
    const a = new FakeAdapter({ adapterId: "000000000000A001" });
    const b = new FakeAdapter({ adapterId: "000000000000B001" });
    const h = controller({ "/a": a, "/b": b });
    const streams = counted(h);
    h.ports.open = async (path) => streams.wrap(h.deps.ports[path]!.open());
    await h.c.manager.rescan();
    await until(() => h.c.manager.connected.size === 2);
    await h.c.manager.suspend();
    expect(streams.open()).toBe(0);
    expect(h.c.manager.connected.size).toBe(0);
    h.c.manager.burst();
    await h.c.manager.rescan();
    expect(await h.c.manager.connect(a.id)).toBe(MIGRATION_TEXT.busy);
    await new Promise((r) => setTimeout(r, 50));
    expect(h.ports.opened).toBe(2);
    h.c.manager.resume();
    await until(() => h.c.manager.connected.size === 2);
    expect(streams.open()).toBe(2);
    await h.c.stop();
  });

  it("clears reconnect timers and pending rescans", async () => {
    const fake = new FakeAdapter();
    const h = controller({ "/a": fake });
    await h.c.manager.rescan();
    await until(() => h.c.state().devices.length > 0);
    vi.useFakeTimers();
    try {
      fake.unplug();
      expect(h.c.manager.connected.get(fake.id)!.recovery).not.toBeNull();
      h.c.manager.burst();
      expect(vi.getTimerCount()).toBeGreaterThan(0);
      await h.c.manager.suspend();
      expect(vi.getTimerCount()).toBe(0);
      const opened = h.ports.opened;
      await vi.advanceTimersByTimeAsync(20000);
      expect(h.ports.opened).toBe(opened);
      expect(h.c.state().adapters).toEqual([]);
    } finally {
      await h.c.stop();
      vi.useRealTimers();
    }
  });

  it("opens nothing from an enumeration that finishes after suspending", async () => {
    const fake = new FakeAdapter();
    const h = controller({ "/a": fake });
    const listing = deferred<PortInfo[]>();
    h.ports.list = () => listing.promise;
    void h.c.manager.rescan();
    const suspended = track(h.c.manager.suspend());
    await new Promise((r) => setTimeout(r, 20));
    expect(suspended.done).toBe(false);
    listing.resolve([{ path: "/a", serial: fake.serial }]);
    await until(() => suspended.done);
    await new Promise((r) => setTimeout(r, 20));
    expect(h.ports.opened).toBe(0);
    await h.c.stop();
  });

  it("waits for a Connect in progress and keeps its adapter disconnected", async () => {
    const fake = new FakeAdapter();
    const h = controller({ "/a": fake });
    const streams = counted(h);
    h.ports.open = async (path) => streams.wrap(h.deps.ports[path]!.open());
    await h.c.manager.rescan();
    await until(() => h.c.manager.connected.size === 1);
    await h.c.manager.disconnect(fake.id);
    const release = fake.hold();
    const connecting = h.c.manager.connect(fake.id);
    await until(() => fake.received.length === 1);
    const suspended = track(h.c.manager.suspend());
    release();
    await until(() => suspended.done);
    expect(await connecting).toBe(MIGRATION_TEXT.busy);
    expect(streams.open()).toBe(0);
    expect(h.c.manager.connected.size).toBe(0);
    expect(h.c.manager.disconnected.get(fake.id)).toMatchObject({ connecting: false });
    await h.c.stop();
  });
});
