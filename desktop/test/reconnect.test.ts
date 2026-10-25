import { ErrorCode } from "@cordial/protocol";
import { describe, expect, it, vi } from "vitest";
import { FakeAdapter, profile } from "../src/fake/adapter.ts";
import { settingsCurrent } from "../src/shared/settings.ts";
import type { Action } from "../src/shared/state.ts";
import { controller, field, until } from "./helpers.ts";

async function start() {
  const fake = new FakeAdapter({ profiles: Array.from({ length: 12 }, (_, i) => profile(i + 1, String(i + 1))) });
  const harness = controller({ "/a": fake });
  await harness.c.manager.rescan();
  await until(() => harness.c.state().devices.length === 4 && harness.c.state().devices.every((d) => d.settings !== null));
  return { fake, ...harness };
}

describe("transport recovery", () => {
  it("identifies session logs before and after reconnecting on another port", async () => {
    const { fake, c, deps } = await start();
    const failures = () => deps.log.mock.calls.map(([message]) => message).filter((message) => message.includes("listing devices failed"));
    try {
      fake.failures.listDevices = [ErrorCode.STORAGE_FAILED];
      await c.act({ type: "devices.reload", adapterId: fake.id });
      await until(() => failures().length === 1);
      fake.unplug();
      delete deps.ports["/a"];
      deps.ports["/b"] = fake;
      fake.failures.listDevices = [ErrorCode.STORAGE_FAILED];
      await c.manager.rescan();
      await until(() => failures().length === 2);
      expect(c.manager.connected.get(fake.id)!.path).toBe("/b");
      expect(failures()).toEqual([
        expect.stringContaining(`${fake.id}: listing devices failed:`),
        expect.stringContaining(`${fake.id}: listing devices failed:`),
      ]);
    } finally {
      await c.stop();
    }
  });

  describe.each([null, "", "unrelated"])("without a usable USB serial number (%s)", (serial) => {
    it.each([
      { source: "session", path: "/a" },
      { source: "session", path: "/b" },
      { source: "port list", path: "/a" },
      { source: "port list", path: "/b" },
    ])("removes state immediately after $source loss and discovers $path afresh", async ({ source, path }) => {
      const fake = new FakeAdapter({ profiles: Array.from({ length: 12 }, (_, i) => profile(i + 1, String(i + 1))) });
      const { c, deps, ports } = controller({ "/a": fake });
      const list = vi.fn(async () => Object.keys(deps.ports).map((path) => ({ path, serial })));
      ports.list = list;
      await c.manager.rescan();
      await until(() => c.state().devices.length === 4 && c.state().devices.every((d) => d.settings !== null));
      const entry = c.manager.connected.get(fake.id)!;
      await entry.session.readPage("next");
      expect(c.state().adapters[0]!.profilePage!.profiles[0]!.id).toBe(11);
      expect(entry.serial).toBeNull();
      vi.useFakeTimers();
      try {
        if (source === "session") fake.unplug();
        else {
          delete deps.ports["/a"];
          await c.manager.rescan();
        }
        expect(c.manager.connected.size).toBe(0);
        expect(entry.recovery).toBeNull();
        expect(entry.session.closed).toBe(true);
        expect(c.state().adapters).toEqual([]);
        expect(c.state().devices).toEqual([]);
        list.mockClear();
        await vi.advanceTimersByTimeAsync(16000);
        expect(list).not.toHaveBeenCalled();
        expect(ports.opened).toBe(1);
        expect(vi.getTimerCount()).toBe(0);

        delete deps.ports["/a"];
        deps.ports[path] = fake;
        fake.devices = fake.devices.filter((d) => d.id === 1);
        await c.manager.rescan();
        await vi.advanceTimersByTimeAsync(100);
        const fresh = c.manager.connected.get(fake.id)!;
        expect(fresh.session).not.toBe(entry.session);
        expect(fresh.path).toBe(path);
        expect(fresh.recovery).toBeNull();
        expect(c.state().adapters[0]).toMatchObject({ connection: "connected", profilePage: { previous: false, next: true } });
        expect(c.state().adapters[0]!.profilePage!.profiles[0]!.id).toBe(1);
        expect(c.state().devices.map((d) => d.device.id)).toEqual([1]);
        expect(c.state().devices[0]!.available).toBe(true);
        expect(ports.opened).toBe(2);
      } finally {
        await c.stop();
        expect(vi.getTimerCount()).toBe(0);
        vi.useRealTimers();
      }
    });
  });

  it.each([
    { path: "/a", serial: null },
    { path: "/b", serial: null },
    { path: "/a", serial: "" },
    { path: "/b", serial: "unrelated" },
  ])("does not resume a known adapter from candidate $path with serial $serial", async ({ path, serial }) => {
    const { fake, c, deps, ports } = await start();
    const entry = c.manager.connected.get(fake.id)!;
    const before = c.state().devices.map((d) => d.device);
    vi.useFakeTimers();
    try {
      fake.unplug();
      delete deps.ports["/a"];
      deps.ports[path] = fake;
      ports.list = async () => [{ path, serial }];
      await c.manager.rescan();
      await vi.advanceTimersByTimeAsync(100);
      expect(fake.received.some((r) => r.command.case === "getStatus")).toBe(true);
      expect(c.manager.connected.get(fake.id)).toBe(entry);
      expect(entry.session.closed).toBe(true);
      expect(c.state().adapters[0]!.connection).toBe("reconnecting");
      expect(c.state().devices.map((d) => d.device)).toEqual(before);
      await vi.advanceTimersByTimeAsync(14900);
      expect(c.state().adapters).toEqual([]);
      expect(c.state().devices).toEqual([]);
    } finally {
      await c.stop();
      expect(vi.getTimerCount()).toBe(0);
      vi.useRealTimers();
    }
  });

  it("discovers a fresh unknown-serial session when an enumeration spans the old session's loss", async () => {
    const fake = new FakeAdapter();
    const { c, ports } = controller({ "/a": fake });
    ports.list = async () => [{ path: "/a", serial: null }];
    await c.manager.rescan();
    await until(() => c.manager.connected.get(fake.id)?.session.available === true);
    const old = c.manager.connected.get(fake.id)!.session;
    const listing = Promise.withResolvers<void>();
    ports.list = async () => {
      await listing.promise;
      return [{ path: "/a", serial: null }];
    };
    vi.useFakeTimers();
    try {
      const scan = c.manager.rescan();
      fake.unplug();
      expect(c.state().adapters).toEqual([]);
      await vi.advanceTimersByTimeAsync(0);
      listing.resolve();
      await scan;
      await vi.advanceTimersByTimeAsync(100);
      expect(c.manager.connected.get(fake.id)!.session).not.toBe(old);
      expect(c.state().adapters[0]!.connection).toBe("connected");
      expect(ports.opened).toBe(2);
    } finally {
      listing.resolve();
      await c.stop();
      expect(vi.getTimerCount()).toBe(0);
      vi.useRealTimers();
    }
  });

  it.each([false, true])("uses current port metadata after manual Connect, readable serial: %s", async (readable) => {
    const { fake, c, ports } = await start();
    await c.manager.disconnect(fake.id);
    ports.list = async () => [{ path: "/a", serial: readable ? fake.serial : null }];
    expect(await c.manager.connect(fake.id)).toBeNull();
    await until(() => c.manager.connected.get(fake.id)?.session.available === true);
    const entry = c.manager.connected.get(fake.id)!;
    vi.useFakeTimers();
    try {
      fake.unplug();
      expect(c.manager.connected.has(fake.id)).toBe(readable);
      expect(entry.recovery !== null).toBe(readable);
      expect(entry.serial).toBe(readable ? fake.serial : null);
      expect(c.manager.disconnected.size).toBe(0);
    } finally {
      await c.stop();
      expect(vi.getTimerCount()).toBe(0);
      vi.useRealTimers();
    }
  });

  it("requires the serial prefix and handshake identity to agree before resuming", async () => {
    const { fake, c, deps, ports } = await start();
    const entry = c.manager.connected.get(fake.id)!;
    const other = new FakeAdapter({ adapterId: "BBBB0002" });
    vi.useFakeTimers();
    try {
      delete deps.ports["/a"];
      fake.unplug();
      deps.ports["/b"] = other;
      ports.list = async () => [{ path: "/b", serial: fake.serial }];
      await c.manager.rescan();
      await vi.advanceTimersByTimeAsync(100);
      expect(c.manager.connected.get(fake.id)).toBe(entry);
      expect(entry.session.closed).toBe(true);
      expect(c.manager.connected.get(other.id)?.serial).toBeNull();
      other.unplug();
      expect(c.manager.connected.has(other.id)).toBe(false);
      deps.ports["/b"] = fake;
      await c.manager.rescan();
      await vi.advanceTimersByTimeAsync(500);
      expect(c.manager.connected.get(fake.id)).toBe(entry);
      expect(entry.path).toBe("/b");
      expect(entry.session.available).toBe(true);
    } finally {
      await c.stop();
      expect(vi.getTimerCount()).toBe(0);
      vi.useRealTimers();
    }
  });

  it("keeps enumerating while another port's handshake is stalled", async () => {
    const { fake, c, deps } = await start();
    const slow = new FakeAdapter({ adapterId: "BBBB0002" });
    const release = slow.hold();
    vi.useFakeTimers();
    try {
      delete deps.ports["/a"];
      fake.unplug();
      await vi.advanceTimersByTimeAsync(12000);
      deps.ports["/slow"] = slow;
      void c.manager.rescan();
      await vi.advanceTimersByTimeAsync(1000);
      deps.ports["/b"] = fake;
      await vi.advanceTimersByTimeAsync(2000);
      expect(c.manager.connected.get(fake.id)?.session.available).toBe(true);
      expect(c.manager.connected.get(fake.id)?.path).toBe("/b");
    } finally {
      release();
      await c.stop();
      vi.useRealTimers();
    }
  });

  it("connects a held adapter without waiting for an unrelated probe", async () => {
    const { fake, c, deps } = await start();
    await c.manager.disconnect(fake.id);
    const slow = new FakeAdapter({ adapterId: "BBBB0002" });
    const release = slow.hold();
    deps.ports["/slow"] = slow;
    vi.useFakeTimers();
    try {
      void c.manager.rescan();
      await vi.advanceTimersByTimeAsync(0);
      let finished = false;
      void c.manager.connect(fake.id).then(() => { finished = true; });
      await vi.advanceTimersByTimeAsync(100);
      expect(finished).toBe(true);
    } finally {
      release();
      await c.stop();
      vi.useRealTimers();
    }
  });

  it("keeps a returning adapter with a persistent fault past the outage deadline", async () => {
    const { fake, c } = await start();
    const records = c.state().devices.map((d) => d.device);
    vi.useFakeTimers();
    try {
      fake.unplug();
      fake.ready = false;
      fake.faults = [{ code: ErrorCode.STORAGE_FAILED }];
      await vi.advanceTimersByTimeAsync(16000);
      expect(c.state().adapters[0]).toMatchObject({ connection: "connected", readiness: "waiting", status: { ready: false } });
      expect(c.state().devices.map((d) => d.device)).toEqual(records);
      expect(c.state().devices.every((d) => !d.available)).toBe(true);
      fake.faults = [];
      fake.changeAdapter({ ready: true });
      await vi.advanceTimersByTimeAsync(100);
      expect(c.state().devices.every((d) => d.available)).toBe(true);
    } finally {
      await c.stop();
      vi.useRealTimers();
    }
  });

  it("keeps settings and profiles visible but unwritable until their reads succeed", async () => {
    const { fake, c } = await start();
    const before = c.state();
    fake.unplug();
    let release = () => {};
    const write = fake.write.bind(fake);
    const spy = vi.spyOn(fake, "write").mockImplementation(async (bytes) => {
      if (fake.received.at(-1)?.command.case === "listDevices") release = fake.hold();
      await write(bytes);
    });
    try {
      await c.manager.rescan();
      await until(() => c.manager.connected.get(fake.id)!.session.available && fake.received.some((r) => r.command.case === "listProfiles"));
      const state = c.state();
      expect(state.devices.every((d) => d.available)).toBe(true);
      expect(state.devices.map((d) => d.settings)).toEqual(before.devices.map((d) => d.settings));
      expect(state.adapters[0]!.profilePage!.profiles).toEqual(before.adapters[0]!.profilePage!.profiles);
      expect(settingsCurrent(state.devices.find((d) => d.device.id === 1)!)).toBe(false);
      const writes: Action[] = [
        { type: "settings.save", key: `${fake.id}/1`, changes: [{ type: "set", setting: field("backlight", "brightness"), value: 5 }] },
        { type: "profile.copy", adapterId: fake.id, profile: 1, name: "Copy" },
        { type: "profile.delete", adapterId: fake.id, profile: 1 },
        { type: "device.update", key: `${fake.id}/1`, profiles: [1] },
        { type: "adapter.settings", adapterId: fake.id, interfaces: { "1": { profile: 1 } } },
      ];
      for (const action of writes) expect(await c.act(action)).toMatchObject({ ok: false });
      fake.failures.listSettings = [ErrorCode.STORAGE_FAILED];
      spy.mockRestore();
      release();
      await until(() => !!c.state().devices.find((d) => d.device.id === 1)!.settingsError);
      expect(await c.act(writes[0]!)).toMatchObject({ ok: false });
      expect(c.state().adapters).toHaveLength(1);
      await c.act({ type: "device.reload", key: `${fake.id}/1` });
      await until(() => c.state().devices.find((d) => d.device.id === 1)!.settingsError === null);
      expect(settingsCurrent(c.state().devices.find((d) => d.device.id === 1)!)).toBe(true);
      expect(await c.act(writes[0]!)).toMatchObject({ ok: true });
    } finally {
      spy.mockRestore();
      release();
      await c.stop();
    }
  });

  it("keeps a failed profile refresh visible and disables writes until retry succeeds", async () => {
    const { fake, c } = await start();
    const page = c.state().adapters[0]!.profilePage;
    fake.unplug();
    fake.failures.listProfiles = [ErrorCode.STORAGE_FAILED];
    try {
      await c.manager.rescan();
      await until(() => !!c.state().adapters[0]!.profilePage?.error);
      expect(c.state().adapters[0]!.profilePage!.profiles).toEqual(page!.profiles);
      expect(c.state().adapters[0]!.profilesAvailable).toBe(false);
      const action: Action = { type: "profile.copy", adapterId: fake.id, profile: 1, name: "Copy" };
      expect(await c.act(action)).toMatchObject({ ok: false });
      await c.act({ type: "adapter.reload", adapterId: fake.id });
      await until(() => c.state().adapters[0]!.profilesAvailable);
      expect(await c.act(action)).toMatchObject({ ok: true });
      fake.unreadableProfiles.add(1);
      await c.act({ type: "adapter.reload", adapterId: fake.id });
      await until(() => c.state().adapters[0]!.profilePage!.unreadable.length === 1);
      expect(await c.act(action)).toMatchObject({ ok: false });
      expect(await c.act({ ...action, profile: 2 })).toMatchObject({ ok: true });
    } finally {
      await c.stop();
    }
  });

  it("still discovers unrelated ports when a late enumeration outlives recovery", async () => {
    const { fake, c, ports } = await start();
    const other = new FakeAdapter({ adapterId: "BBBB0002" });
    const listing = Promise.withResolvers<void>();
    ports.list = async () => {
      await listing.promise;
      return [{ path: "/a", serial: fake.serial }, { path: "/b", serial: other.serial }];
    };
    ports.open = async () => other.open();
    vi.useFakeTimers();
    try {
      fake.unplug();
      void c.manager.rescan();
      await vi.advanceTimersByTimeAsync(15000);
      listing.resolve();
      await vi.advanceTimersByTimeAsync(0);
      expect([...c.manager.connected.keys()]).toEqual([other.id]);
    } finally {
      listing.resolve();
      await c.stop();
      vi.useRealTimers();
    }
  });

  it("closes a replacement that ends while the previous port is still releasing", async () => {
    const { fake, c, deps } = await start();
    const old = c.manager.connected.get(fake.id)!.session;
    const release = Promise.withResolvers<void>();
    const close = fake.close.bind(fake);
    vi.spyOn(fake, "close").mockImplementation(async () => { await release.promise; await close(); });
    const replacement = new FakeAdapter({ adapterId: fake.id });
    try {
      fake.unplug();
      delete deps.ports["/a"];
      deps.ports["/b"] = replacement;
      await c.manager.rescan();
      await until(() => replacement.received.some((r) => r.command.case === "getStatus"));
      // Let the handshake finish before ending the replacement transport.
      await new Promise((resolve) => setTimeout(resolve, 10));
      replacement.unplug();
      release.resolve();
      await until(() => deps.log.mock.calls.some(([message]) => message === "/b: session ended while opening"));
      expect(c.manager.connected.get(fake.id)!.session).toBe(old);
      expect(old.closed).toBe(true);
      expect(deps.log.mock.calls.some(([message]) => message.includes("listing serial ports failed"))).toBe(false);
      await c.manager.rescan();
      await until(() => old.available);
      expect(c.manager.connected.get(fake.id)!.session).toBe(old);
    } finally {
      release.resolve();
      await c.stop();
    }
  });

  it.each(["same", "new"])("retries an early failure on the %s port without hotplug or a fresh session", async (port) => {
    const { fake, c, deps, ports } = await start();
    await c.request({ type: "preferences", preferences: { notifyConnections: true } });
    const session = c.manager.connected.get(fake.id)!.session;
    await session.readPage("next");
    await session.readPage("first", "picker");
    const before = c.state();
    let attempts = 0;
    ports.open = async () => {
      if (++attempts <= 7) throw new Error("port busy");
      return fake.open();
    };
    vi.useFakeTimers();
    try {
      fake.unplug();
      await vi.advanceTimersByTimeAsync(3500);
      expect(attempts).toBe(7);
      expect(c.state().adapters[0]!.connection).toBe("reconnecting");
      expect(c.state().devices.map((d) => d.device)).toEqual(before.devices.map((d) => d.device));
      expect(c.state().devices.map((d) => d.settings)).toEqual(before.devices.map((d) => d.settings));
      expect(c.state().adapters[0]!.profilePage).toEqual(before.adapters[0]!.profilePage);
      expect(c.state().adapters[0]!.pickerPage).toEqual(before.adapters[0]!.pickerPage);
      if (port === "new") {
        delete deps.ports["/a"];
        deps.ports["/b"] = fake;
      }
      await vi.advanceTimersByTimeAsync(500);
      expect(c.manager.connected.get(fake.id)!.session).toBe(session);
      expect(c.manager.connected.get(fake.id)!.path).toBe(port === "new" ? "/b" : "/a");
      expect(c.state().adapters[0]!.connection).toBe("connected");
      expect(c.state().devices.every((d) => d.available)).toBe(true);
      expect(c.state().adapters[0]!.profilePage).toEqual(before.adapters[0]!.profilePage);
      expect(c.state().adapters[0]!.pickerPage).toEqual(before.adapters[0]!.pickerPage);
      expect(deps.connection).not.toHaveBeenCalled();
      await vi.advanceTimersByTimeAsync(12000);
      expect(c.state().adapters).toHaveLength(1);
      fake.unplug();
      expect(c.state().adapters[0]!.connection).toBe("reconnecting");
      await vi.advanceTimersByTimeAsync(500);
      expect(c.state().adapters[0]!.connection).toBe("connected");
      expect(c.manager.connected.get(fake.id)!.session).toBe(session);
      expect(deps.connection).not.toHaveBeenCalled();
    } finally {
      await c.stop();
      expect(vi.getTimerCount()).toBe(0);
      vi.useRealTimers();
    }
  });

  it.each(["open", "list"])("expires at the original deadline after repeated %s failures", async (failure) => {
    const { fake, c, ports } = await start();
    const original = c.state().devices.map((d) => d.device);
    if (failure === "open") ports.open = async () => { throw new Error("port busy"); };
    else fake.failures.listDevices = Array(40).fill(ErrorCode.STORAGE_FAILED);
    vi.useFakeTimers();
    try {
      fake.unplug();
      await vi.advanceTimersByTimeAsync(14999);
      expect(c.state().adapters[0]!.connection).toBe("reconnecting");
      expect(c.state().devices.map((d) => d.device)).toEqual(original);
      expect(c.state().devices.every((d) => !d.available)).toBe(true);
      await vi.advanceTimersByTimeAsync(1);
      expect(c.state().adapters).toEqual([]);
      expect(c.state().devices).toEqual([]);
      await vi.advanceTimersByTimeAsync(500);
      expect(c.state().adapters).toEqual([]);
    } finally {
      await c.stop();
      expect(vi.getTimerCount()).toBe(0);
      vi.useRealTimers();
    }
  });

  it("keeps records unavailable while the replacement is listing and refuses actions", async () => {
    const { fake, c } = await start();
    const original = c.state().devices;
    const connection = c.manager.connected.get(fake.id)!.session.connection;
    fake.unplug();
    const open = fake.open.bind(fake);
    let release = () => {};
    vi.spyOn(fake, "open").mockImplementationOnce(() => {
      const stream = open();
      const write = stream.write.bind(stream);
      vi.spyOn(stream, "write").mockImplementation(async (bytes) => {
        if (fake.received.some((r) => r.command.case === "getStatus")) release = fake.hold();
        await write(bytes);
      });
      return stream;
    });
    try {
      await c.manager.rescan();
      await until(() => c.manager.connected.get(fake.id)!.session.connection !== connection && fake.received.some((r) => r.command.case === "listDevices"));
      expect(c.state().devices.map((d) => d.device)).toEqual(original.map((d) => d.device));
      expect(c.state().devices.every((d) => !d.available)).toBe(true);
      const actions: Action[] = [
        { type: "device.connect", key: `${fake.id}/1` },
        { type: "device.disconnect", key: `${fake.id}/1` },
        { type: "device.update", key: `${fake.id}/1`, trusted: false },
        { type: "device.refresh", key: `${fake.id}/1` },
        { type: "settings.save", key: `${fake.id}/1`, changes: [] },
        { type: "adapter.name", adapterId: fake.id, name: "Desk" },
        { type: "profile.delete", adapterId: fake.id, profile: 1 },
        { type: "scan.start", adapterId: fake.id },
      ];
      const sent = fake.received.length;
      for (const action of actions) expect(await c.act(action)).toMatchObject({ ok: false });
      expect(fake.received).toHaveLength(sent);
      vi.mocked(fake.write).mockRestore();
      release();
      await until(() => c.state().devices.every((d) => d.available));
    } finally {
      release();
      await c.stop();
    }
  });

  it.each(["expiry", "disconnect", "shutdown"])("closes a late open after %s", async (end) => {
    const { fake, c, ports } = await start();
    const opening = Promise.withResolvers<void>();
    let started = false;
    ports.open = async () => {
      started = true;
      await opening.promise;
      return fake.open();
    };
    vi.useFakeTimers();
    try {
      fake.unplug();
      await vi.advanceTimersByTimeAsync(500);
      const pending = c.manager.rescan();
      expect(started).toBe(true);
      if (end === "expiry") await vi.advanceTimersByTimeAsync(14500);
      else if (end === "disconnect") await c.manager.disconnect(fake.id);
      else await c.stop();
      const close = vi.spyOn(fake, "close");
      opening.resolve();
      await pending;
      await vi.advanceTimersByTimeAsync(0);
      expect(close).toHaveBeenCalled();
      expect(c.manager.connected.size).toBe(0);
      await vi.advanceTimersByTimeAsync(500);
      expect(c.manager.connected.size).toBe(0);
    } finally {
      opening.resolve();
      await c.stop();
      expect(vi.getTimerCount()).toBe(0);
      vi.useRealTimers();
    }
  });

  it.each(["before", "during"])("does not open a port listing that started %s the recovery window after its deadline", async (when) => {
    const { fake, c, ports } = await start();
    const listing = Promise.withResolvers<void>();
    ports.list = async () => {
      await listing.promise;
      return [{ path: "/a", serial: fake.serial }];
    };
    const opened = ports.opened;
    vi.useFakeTimers();
    try {
      const early = when === "before" ? c.manager.rescan() : null;
      fake.unplug();
      await vi.advanceTimersByTimeAsync(500);
      const pending = early ?? c.manager.rescan();
      await vi.advanceTimersByTimeAsync(14500);
      listing.resolve();
      await pending;
      expect(c.manager.connected.size).toBe(0);
      expect(ports.opened).toBe(opened);
    } finally {
      listing.resolve();
      await c.stop();
      expect(vi.getTimerCount()).toBe(0);
      vi.useRealTimers();
    }
  });

  it("keeps an interrupted save's unknown outcome without replaying it", async () => {
    const { fake, c } = await start();
    const release = fake.hold();
    try {
      const pending = c.act({ type: "settings.save", key: `${fake.id}/1`, changes: [{ type: "set", setting: field("backlight", "brightness"), value: 5 }] });
      await until(() => fake.received.some((r) => r.command.case === "setSettings"));
      fake.unplug();
      release();
      expect(await pending).toMatchObject({ ok: false, settingsSave: { items: [{ status: "unknown" }] } });
      expect(c.state().devices.find((d) => d.device.id === 1)!.settingsSave!.items[0]!.status).toBe("unknown");
      await c.manager.rescan();
      await until(() => c.state().devices.every((d) => d.available));
      expect(fake.received.some((r) => r.command.case === "setSettings")).toBe(false);
      expect(c.state().devices.find((d) => d.device.id === 1)!.settingsSave!.items[0]!.status).toBe("unknown");
    } finally {
      release();
      await c.stop();
    }
  });

  it("keeps a saved pairing result and its device through an immediate failure", async () => {
    const { fake, c, deps } = await start();
    try {
      await c.act({ type: "scan.start", adapterId: fake.id });
      await until(() => !!c.state().scan?.candidates.length);
      await c.act({ type: "pair.start", adapterId: fake.id, candidateId: 1 });
      await until(() => !!c.state().pairing?.prompt);
      await c.act({ type: "pair.reply", accept: true });
      await until(() => c.state().pairing?.phase === "connected");
      const pairing = c.state().pairing;
      const device = c.state().devices.find((d) => d.key === pairing!.deviceKey)!;
      delete deps.ports["/a"];
      fake.unplug();
      expect(c.state().pairing).toEqual(pairing);
      expect(c.state().devices.find((d) => d.key === device.key)).toMatchObject({ available: false, device: device.device, settings: device.settings });
      deps.ports["/b"] = fake;
      await c.manager.rescan();
      await until(() => c.state().devices.find((d) => d.key === device.key)?.available === true);
      expect(c.state().pairing).toEqual(pairing);
      expect(c.state().scan).toBeNull();
      expect(await c.act({ type: "pair.start", adapterId: fake.id, candidateId: 1 })).toMatchObject({ ok: false });
      expect(fake.received.some((r) => r.command.case === "startPairing")).toBe(false);
    } finally {
      await c.stop();
    }
  });

  it.each(["reconnect", "not ready"])("invalidates an active prompt and candidates after %s", async (loss) => {
    const { fake, c } = await start();
    try {
      await c.act({ type: "scan.start", adapterId: fake.id });
      await until(() => !!c.state().scan?.candidates.length);
      await c.act({ type: "pair.start", adapterId: fake.id, candidateId: 1 });
      await until(() => !!c.state().pairing?.prompt);
      if (loss === "reconnect") {
        fake.unplug();
        await c.manager.rescan();
        await until(() => c.manager.connected.get(fake.id)!.session.available);
      } else {
        fake.changeAdapter({ ready: false });
        await until(() => !c.manager.connected.get(fake.id)!.session.available);
        expect(c.state().adapters[0]!.connection).toBe("connected");
      }
      expect(c.state().scan).toBeNull();
      expect(c.state().pairing).toMatchObject({ phase: "failed", prompt: null, message: "The adapter couldn't confirm whether the device was paired." });
      expect(await c.act({ type: "pair.reply", accept: true })).toMatchObject({ ok: false });
      expect(fake.received.some((r) => r.command.case === "acceptPrompt")).toBe(false);
    } finally {
      await c.stop();
    }
  });

  it("retains records when the returning adapter is not ready to list them", async () => {
    const { fake, c } = await start();
    const devices = c.state().devices.map((d) => d.device);
    fake.unplug();
    fake.ready = false;
    const saved = fake.devices;
    fake.devices = [];
    try {
      await c.manager.rescan();
      await until(() => fake.received.some((r) => r.command.case === "listDevices"));
      expect(c.state().devices.map((d) => d.device)).toEqual(devices);
      expect(c.state().devices.every((d) => !d.available)).toBe(true);
      fake.devices = saved;
      fake.changeAdapter({ ready: true });
      await until(() => c.state().devices.every((d) => d.available));
      expect(c.state().devices).toHaveLength(4);
    } finally {
      await c.stop();
    }
  });

  it.each(["expiry", "shutdown"])("cancels a stalled reconnect handshake at %s", async (end) => {
    const { fake, c, ports } = await start();
    ports.open = async () => {
      const stream = fake.open();
      vi.spyOn(stream, "write").mockResolvedValue();
      return stream;
    };
    vi.useFakeTimers();
    try {
      fake.unplug();
      if (end === "expiry") await vi.advanceTimersByTimeAsync(14500);
      const pending = c.manager.rescan();
      await vi.advanceTimersByTimeAsync(0);
      if (end === "expiry") await vi.advanceTimersByTimeAsync(500);
      else await c.stop();
      await pending;
      expect(c.manager.connected.size).toBe(0);
    } finally {
      await c.stop();
      expect(vi.getTimerCount()).toBe(0);
      vi.useRealTimers();
    }
  });

  it("keeps one session when two ports answer with the returning adapter's identity", async () => {
    const { fake, c, deps } = await start();
    const session = c.manager.connected.get(fake.id)!.session;
    const duplicate = new FakeAdapter({ adapterId: fake.id });
    const closed = vi.spyOn(duplicate, "close");
    vi.useFakeTimers();
    fake.unplug();
    deps.ports["/b"] = duplicate;
    try {
      await vi.advanceTimersByTimeAsync(500);
      expect(c.state().adapters[0]?.connection).toBe("connected");
      expect(c.manager.connected.size).toBe(1);
      expect(c.manager.connected.get(fake.id)!.session).toBe(session);
      expect(closed).toHaveBeenCalledOnce();
    } finally {
      await c.stop();
      vi.useRealTimers();
    }
  });
});
