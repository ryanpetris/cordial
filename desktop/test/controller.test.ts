import { describe, expect, it, vi } from "vitest";
import { FakeAdapter } from "../src/fake/adapter.ts";
import { AdapterManager } from "../src/core/manager.ts";
import { controller, until } from "./helpers.ts";

const loaded = (s: ReturnType<ReturnType<typeof controller>["state"]>, devices: number) =>
  !!s && s.devices.length === devices && s.devices.every((d) => d.info);

describe("Controller", () => {
  it("cancels every scan when starts overlap", async () => {
    const fake = new FakeAdapter({ adapterId: "AAAA0001" });
    const { c, state } = controller({ "/a": fake });
    await c.manager.rescan();
    await until(() => loaded(state(), 4));
    const start = () => c.act({ type: "scan.start", adapterId: fake.id });
    await start();
    expect(await Promise.all([start(), start()])).toEqual([{ ok: true }, { ok: true }]);
    await until(() => !!c.state().scan?.running && !!c.state().scan?.candidates.length);
    await c.act({ type: "scan.stop" });
    const scans = fake.received.filter((m) => m.cmd === "discovery.scan");
    const cancelled = fake.received.filter((m) => m.cmd === "request.cancel").map((m) => (m.args as { request_id: number }).request_id);
    expect(scans).toHaveLength(3);
    expect(cancelled).toEqual(scans.map((m) => m.id));
    expect(c.state().scan).toBeNull();
    await c.stop();
  });

  it("keeps the first pairing tracked when two starts await scan cancellation", async () => {
    const fake = new FakeAdapter({ adapterId: "AAAA0001" });
    const { c, state } = controller({ "/a": fake });
    await c.manager.rescan();
    await until(() => loaded(state(), 4));
    await c.act({ type: "scan.start", adapterId: fake.id });
    await until(() => !!c.state().scan?.candidates.length);
    const cancelled = Promise.withResolvers<void>();
    const write = fake.write.bind(fake);
    let cancelling = false;
    fake.write = async (text) => {
      if (JSON.parse(text).cmd === "request.cancel") {
        cancelling = true;
        await cancelled.promise;
      }
      await write(text);
    };
    const first = c.act({ type: "pair.start", adapterId: fake.id, candidateId: "c_1" });
    await until(() => cancelling);
    const second = c.act({ type: "pair.start", adapterId: fake.id, candidateId: "c_2" });
    cancelled.resolve();
    expect(await first).toEqual({ ok: true });
    expect(await second).toEqual({ ok: false, message: "Another device is being added." });
    await until(() => !!c.state().pairing?.prompt);
    expect(fake.received.filter((m) => m.cmd === "pairing.start")).toHaveLength(1);
    expect(c.state().pairing?.candidateId).toBe("c_1");
    await c.act({ type: "pair.cancel" });
    await until(() => c.state().pairing?.phase === "cancelled");
    await c.stop();
  });

  it.each(["scan.start", "pair.start"] as const)("cancels a pending %s when the dialog closes", async (type) => {
    const fake = new FakeAdapter({ adapterId: "AAAA0001" });
    const { c, state } = controller({ "/a": fake });
    await c.manager.rescan();
    await until(() => loaded(state(), 4));
    await c.act({ type: "scan.start", adapterId: fake.id });
    await until(() => !!c.state().scan?.candidates.length);
    const cancelled = Promise.withResolvers<void>();
    const write = fake.write.bind(fake);
    let cancelling = false;
    fake.write = async (text) => {
      if (JSON.parse(text).cmd === "request.cancel") {
        cancelling = true;
        await cancelled.promise;
      }
      await write(text);
    };
    const start = c.act(type === "scan.start"
      ? { type, adapterId: fake.id }
      : { type, adapterId: fake.id, candidateId: "c_1" });
    await until(() => cancelling);
    const stop = c.act({ type: "scan.stop" });
    const dismiss = c.act({ type: "pair.dismiss" });
    cancelled.resolve();
    await Promise.all([start, stop, dismiss]);
    expect(fake.received.filter((m) => m.cmd === "discovery.scan")).toHaveLength(1);
    expect(fake.received.filter((m) => m.cmd === "pairing.start")).toHaveLength(0);
    expect(c.state().scan).toBeNull();
    expect(c.state().pairing).toBeNull();
    await c.stop();
  });

  it("stops scanning while a dismissed pairing is still tearing down", async () => {
    const a = new FakeAdapter({ adapterId: "AAAA0001" });
    const b = new FakeAdapter({ adapterId: "BBBB0002" });
    const held = Promise.withResolvers<() => void>();
    let pairingId: number | undefined;
    const onData = a.onData.bind(a);
    a.onData = (listener) => onData((chunk) => {
      const messages = new TextDecoder().decode(chunk).split("\n").filter(Boolean).map((line) => JSON.parse(line));
      if (messages.some((m) => m.type === "response" && m.id === pairingId && m.done)) {
        held.resolve(() => listener(chunk));
        return;
      }
      listener(chunk);
    });
    const { c, state } = controller({ "/a": a, "/b": b });
    await c.manager.rescan();
    await until(() => loaded(state(), 8));
    await c.act({ type: "scan.start", adapterId: a.id });
    await until(() => !!c.state().scan?.candidates.length);
    await c.act({ type: "pair.start", adapterId: a.id, candidateId: "c_1" });
    await until(() => !!c.state().pairing?.prompt);
    pairingId = Number(a.received.find((m) => m.cmd === "pairing.start")!.id);
    await c.act({ type: "pair.cancel" });
    const release = await held.promise;
    await c.act({ type: "pair.dismiss" });
    await c.act({ type: "scan.start", adapterId: b.id });
    await until(() => !!c.state().scan?.candidates.length);
    const pairing = c.act({ type: "pair.start", adapterId: b.id, candidateId: "c_1" });
    await Promise.resolve();
    const stop = c.act({ type: "scan.stop" });
    try {
      await until(() => b.received.some((m) => m.cmd === "request.cancel"), 1000);
      expect(await stop).toEqual({ ok: true });
      expect(await pairing).toEqual({ ok: true });
      expect(c.state().scan).toBeNull();
      expect(b.received.filter((m) => m.cmd === "pairing.start")).toHaveLength(0);
    } finally {
      release();
      await Promise.all([pairing, stop]);
      await c.stop();
    }
  });

  it.each(["scan.stop", "pair.cancel", "pair.start"] as const)("unblocks discovery when %s loses its cancellation acknowledgement", async (type) => {
    const fake = new FakeAdapter({ adapterId: "AAAA0001" });
    let dropCancel = false;
    const onData = fake.onData.bind(fake);
    fake.onData = (listener) => onData((chunk) => {
      const messages = new TextDecoder().decode(chunk).split("\n").filter(Boolean).map((line) => JSON.parse(line));
      if (dropCancel && messages.some((m) => m.type === "response"
        && fake.received.some((r) => r.id === m.id && r.cmd === "request.cancel"))) return;
      listener(chunk);
    });
    const { c, state } = controller({ "/a": fake });
    await c.manager.rescan();
    await until(() => loaded(state(), 4));
    await c.act({ type: "scan.start", adapterId: fake.id });
    await until(() => !!c.state().scan?.candidates.length);
    if (type === "pair.cancel") {
      await c.act({ type: "pair.start", adapterId: fake.id, candidateId: "c_1" });
      await until(() => !!c.state().pairing?.prompt);
    }
    const session = c.manager.connected.get(fake.id)!.session;
    dropCancel = true;
    vi.useFakeTimers();
    try {
      const action = c.act(type === "pair.start"
        ? { type, adapterId: fake.id, candidateId: "c_1" }
        : { type });
      await vi.advanceTimersByTimeAsync(1);
      const heartbeat = session.request("session.heartbeat", {});
      await vi.advanceTimersByTimeAsync(1);
      await expect(heartbeat).resolves.toHaveProperty("monitor");
      const next = c.act({ type: "scan.stop" });
      await vi.advanceTimersByTimeAsync(5000);
      expect(session.closed).toBe(true);
      await action;
      expect(await next).toEqual({ ok: true });
      expect(c.state().scan).toBeNull();
    } finally {
      vi.useRealTimers();
      await c.stop();
    }
  });

  it("closes a reconnect that finishes opening after shutdown", async () => {
    const fake = new FakeAdapter({ adapterId: "AAAA0001" });
    const open = vi.fn(async () => fake.open());
    const manager = new AdapterManager({
      listPorts: async () => [{ path: "/a", serial: fake.id }],
      openTransport: open,
      changed: vi.fn(),
      log: vi.fn(),
    });
    await manager.rescan();
    await until(() => manager.connected.get(fake.id)!.session.view.valid);
    await manager.disconnect(fake.id);
    const opening = Promise.withResolvers<void>();
    const close = vi.spyOn(fake, "close");
    let started = false;
    open.mockImplementationOnce(async () => {
      started = true;
      await opening.promise;
      return fake.open();
    });
    const reconnect = manager.connect(fake.id);
    await until(() => started);
    await manager.stop();
    opening.resolve();
    expect(await reconnect).toBe("That adapter is no longer available.");
    expect(manager.connected.size).toBe(0);
    expect(close).toHaveBeenCalledOnce();
    expect(await manager.connect(fake.id)).toBe("That adapter is no longer available.");
  });

  it.skipIf(!global.gc)("releases manually disconnected sessions for garbage collection", async () => {
    const fake = new FakeAdapter({ adapterId: "AAAA0001" });
    const { c, state } = controller({ "/a": fake });
    await c.manager.rescan();
    await until(() => loaded(state(), 4));
    const reference = new WeakRef(c.manager.connected.get(fake.id)!.session);
    await c.manager.disconnect(fake.id);
    await new Promise((r) => setTimeout(r, 1100));
    for (let i = 0; i < 5; i++) {
      global.gc!();
      await new Promise((r) => setTimeout(r, 20));
    }
    expect(reference.deref()).toBeUndefined();
    await c.stop();
  });

  it("combines confirmed adapters and hides a port that isn't one", async () => {
    const a = new FakeAdapter({ adapterId: "AAAA0001" });
    const b = new FakeAdapter({ adapterId: "BBBB0002", board: "xiao_esp32s3" });
    const { c, deps, state } = controller({ "/a": a, "/b": b });
    const broken = new FakeAdapter({ adapterId: "CCCC0003" });
    broken.write = async () => {
      throw new Error("write failed");
    };
    deps.ports["/c"] = broken;
    await c.manager.rescan();
    await until(() => loaded(state(), 8));
    const s = state()!;
    expect(s.adapters.map((x) => [x.name, x.connection])).toEqual([
      ["Pico W", "connected"],
      ["XIAO ESP32-S3", "connected"],
    ]);
    expect(new Set(s.devices.map((d) => d.adapterId))).toEqual(new Set(["AAAA0001", "BBBB0002"]));
    const mouse = s.devices.find((d) => d.key === "AAAA0001/d_2")!;
    expect(mouse.name).toBe("Example Mouse");
    expect(mouse.kind).toBe("mouse");
    expect(mouse.battery).toEqual({ percent: 12, charging: false, percentFresh: true, chargingFresh: true });
    await c.stop();
  });

  it("removes an unplugged adapter silently", async () => {
    const a = new FakeAdapter({ adapterId: "AAAA0001" });
    const { c, deps, state } = controller({ "/a": a });
    await c.manager.rescan();
    await until(() => loaded(state(), 4));
    delete deps.ports["/a"];
    a.unplug();
    await until(() => state()!.adapters.length === 0 && state()!.devices.length === 0);
    await c.stop();
  });

  it("keeps a manually disconnected adapter listed without opening it", async () => {
    const a = new FakeAdapter({ adapterId: "AAAA0001" });
    const { c, state } = controller({ "/a": a });
    await c.manager.rescan();
    await until(() => loaded(state(), 4));
    expect(await c.act({ type: "adapter.disconnect", adapterId: "AAAA0001" })).toEqual({ ok: true });
    await until(() => state()!.adapters[0]?.connection === "disconnected");
    expect(state()!.devices).toEqual([]);
    const sessions = a.received.length;
    await c.manager.rescan();
    expect(a.received.length).toBe(sessions);
    expect(state()!.adapters[0]?.connection).toBe("disconnected");
    expect(await c.act({ type: "adapter.connect", adapterId: "AAAA0001" })).toEqual({ ok: true });
    await until(() => loaded(state(), 4));
    await c.stop();
  });

  it("identifies adapters by handshake when ports have no USB serial number", async () => {
    const a = new FakeAdapter({ adapterId: "AAAA0001" });
    const { c, deps, ports, state } = controller({ "/a": a });
    ports.list = async () => Object.keys(deps.ports).map((path) => ({ path, serial: null }));
    await c.manager.rescan();
    await until(() => loaded(state(), 4));
    expect(await c.act({ type: "adapter.disconnect", adapterId: "AAAA0001" })).toEqual({ ok: true });
    await until(() => state()!.adapters[0]?.connection === "disconnected");
    // The disconnected adapter is known by its port, which isn't opened again.
    const opened = ports.opened;
    await c.manager.rescan();
    expect(ports.opened).toBe(opened);
    expect(state()!.adapters[0]?.connection).toBe("disconnected");
    expect(await c.act({ type: "adapter.connect", adapterId: "AAAA0001" })).toEqual({ ok: true });
    await until(() => loaded(state(), 4));
    expect(await c.act({ type: "adapter.disconnect", adapterId: "AAAA0001" })).toEqual({ ok: true });
    delete deps.ports["/a"];
    await c.manager.rescan();
    expect(await c.act({ type: "adapter.connect", adapterId: "AAAA0001" })).toEqual({ ok: false, message: "The adapter isn't plugged in." });
    await until(() => state()!.adapters.length === 0);
    // Plugged in again on another port: identified by handshake, shown, still disconnected.
    deps.ports["/b"] = a;
    await c.manager.rescan();
    await until(() => state()!.adapters[0]?.connection === "disconnected");
    expect(await c.act({ type: "adapter.connect", adapterId: "AAAA0001" })).toEqual({ ok: true });
    await until(() => loaded(state(), 4));
    await c.stop();
  });

  it("notifies once for low and once for critical battery, then resets", async () => {
    const a = new FakeAdapter({ adapterId: "AAAA0001" });
    const { c, deps, state } = controller({ "/a": a });
    await c.manager.rescan();
    await until(() => loaded(state(), 4));
    await until(() => deps.lowBattery.mock.calls.length === 1);
    expect(deps.lowBattery.mock.calls[0]![0]).toMatchObject({ key: "AAAA0001/d_2", level: "low", percent: 12 });
    a.changeInfo("d_2", { battery_percent: 11 });
    await until(() => state()!.devices.find((d) => d.key === "AAAA0001/d_2")!.battery?.percent === 11);
    a.changeInfo("d_2", { battery_percent: 4 });
    await until(() => deps.lowBattery.mock.calls.length === 2);
    expect(deps.lowBattery.mock.calls[1]![0]).toMatchObject({ level: "critical" });
    // Unknown doesn't reset; charging does.
    const battery = () => state()!.devices.find((d) => d.key === "AAAA0001/d_2")!.battery;
    a.changeInfo("d_2", { battery_percent: null, battery_charging: null });
    await until(() => battery() === null);
    a.changeInfo("d_2", { battery_percent: 4, battery_charging: false });
    await until(() => battery()?.percent === 4);
    a.changeInfo("d_2", { battery_charging: true });
    await until(() => battery()?.charging === true);
    expect(deps.lowBattery).toHaveBeenCalledTimes(2);
    a.changeInfo("d_2", { battery_charging: false });
    await until(() => deps.lowBattery.mock.calls.length === 3);
    await c.stop();
  });

  it("changes device settings and reports adapter errors in words", async () => {
    const a = new FakeAdapter({ adapterId: "AAAA0001" });
    const { c, state } = controller({ "/a": a });
    await c.manager.rescan();
    await until(() => loaded(state(), 4));
    expect(await c.act({ type: "device.trusted", key: "AAAA0001/d_3", value: false })).toEqual({ ok: true });
    await until(() => state()!.devices.find((d) => d.key === "AAAA0001/d_3")!.device.trusted === false);
    const result = await c.act({ type: "device.connect", key: "AAAA0001/d_4" });
    expect(result).toEqual({ ok: false, message: expect.stringContaining("needs pairing again") });
    await c.act({ type: "settings.watch", key: "AAAA0001/d_1" });
    await until(() => !!state()!.devices.find((d) => d.key === "AAAA0001/d_1")!.settings?.current);
    expect(await c.act({ type: "setting.set", key: "AAAA0001/d_1", setting: "backlight.level", value: 5 })).toEqual({ ok: true });
    await until(
      () =>
        state()!.devices.find((d) => d.key === "AAAA0001/d_1")!.settings!.settings.find((s) => s.key === "backlight.level")!
          .desired === 5,
    );
    await c.act({ type: "adapter.platform", adapterId: "AAAA0001", platform: "windows" });
    await until(() => state()!.adapters[0]!.platform === "windows");
    await c.stop();
  });

  it("searches, pairs with numeric comparison and connects", async () => {
    const a = new FakeAdapter({ adapterId: "AAAA0001" });
    const { c, state } = controller({ "/a": a });
    await c.manager.rescan();
    await until(() => loaded(state(), 4));
    await c.act({ type: "scan.start", adapterId: "AAAA0001" });
    await until(() => state()!.scan?.candidates.length === 4);
    expect(a.received.find((m) => m.cmd === "discovery.scan")!.args).toEqual({ transport: "both", duration_ms: 0 });
    await c.act({ type: "pair.start", adapterId: "AAAA0001", candidateId: "c_1" });
    await until(() => !!state()!.pairing?.prompt);
    expect(state()!.pairing!.prompt!.prompt.value).toBe("042731");
    expect(await c.act({ type: "pair.reply", accept: true })).toEqual({ ok: true });
    await until(() => state()!.pairing?.phase === "connected");
    expect(state()!.devices.some((d) => d.name === "Example Keys Mini")).toBe(true);
    await c.act({ type: "pair.dismiss" });
    await c.act({ type: "scan.stop" });
    expect(c.state().pairing).toBeNull();
    await c.stop();
  });

  it("reports a rejected pairing", async () => {
    const a = new FakeAdapter({ adapterId: "AAAA0001", pairingMethod: "enter_passkey" });
    const { c, state } = controller({ "/a": a });
    await c.manager.rescan();
    await until(() => loaded(state(), 4));
    await c.act({ type: "scan.start", adapterId: "AAAA0001" });
    await until(() => state()!.scan?.candidates.length === 4);
    await c.act({ type: "pair.start", adapterId: "AAAA0001", candidateId: "c_2" });
    await until(() => state()!.pairing?.prompt?.prompt.method === "enter_passkey");
    await c.act({ type: "pair.reply", accept: false });
    await until(() => state()!.pairing?.phase === "failed");
    expect(state()!.pairing!.message).toMatch(/rejected/);
    await c.stop();
  });

  it("distinguishes cancelled pairing from a later failed pairing", async () => {
    const fake = new FakeAdapter();
    const { c, state } = controller({ "/fake": fake });
    try {
      await c.manager.rescan();
      await until(() => loaded(state(), 4));
      await c.act({ type: "scan.start", adapterId: fake.id });
      await until(() => !!state()?.scan?.candidates.length);
      await c.act({ type: "pair.start", adapterId: fake.id, candidateId: "c_1" });
      await until(() => !!state()?.pairing?.prompt);
      await c.act({ type: "pair.cancel" });
      await until(() => state()?.pairing?.phase === "cancelled");
      await c.act({ type: "pair.dismiss" });
      await c.act({ type: "pair.start", adapterId: fake.id, candidateId: "c_2" });
      await until(() => !!state()?.pairing?.prompt);
      await c.act({ type: "pair.reply", accept: false });
      await until(() => state()?.pairing?.phase === "failed");
    } finally { await c.stop(); }
  });
});

it("renames device-owned names, preserves duplicate labels and reconnects", async () => {
  const a = new FakeAdapter({ adapterId: "AAAA0001" });
  const b = new FakeAdapter({ adapterId: "BBBB0002" });
  const { c, state } = controller({ "/a": a, "/b": b });
  await c.manager.rescan();
  await until(() => loaded(state(), 8));
  expect(state()!.adapters.map((x) => x.name)).toEqual(["Pico W", "Pico W"]);
  expect(await c.act({ type: "adapter.name", adapterId: a.id, name: "  Desk \u{10400}  " })).toEqual({ ok: true });
  await until(() => state()!.adapters.some((x) => x.name === "Desk \u{10400}"));
  expect(a.name).toBe("Desk \u{10400}");
  expect(await c.act({ type: "adapter.platform", adapterId: a.id, platform: "mac" })).toEqual({ ok: true });
  expect(a.name).toBe("Desk \u{10400}");
  a.failures["adapter.name.set"] = ["storage_full"];
  expect(await c.act({ type: "adapter.name", adapterId: a.id, name: "Failed" })).toMatchObject({ ok: false });
  expect(a.name).toBe("Desk \u{10400}");
  expect(await c.act({ type: "adapter.name", adapterId: a.id, name: "é".repeat(33) })).toMatchObject({ ok: false });
  await c.act({ type: "adapter.disconnect", adapterId: a.id });
  await until(() => state()!.adapters.some((x) => x.id === a.id && x.connection === "disconnected"));
  expect(state()!.adapters.find((x) => x.id === a.id)?.name).toBe("Desk \u{10400}");
  expect(await c.act({ type: "adapter.name", adapterId: a.id, name: "Offline" })).toMatchObject({ ok: false });
  await c.act({ type: "adapter.connect", adapterId: a.id });
  await until(() => state()!.adapters.some((x) => x.id === a.id && x.readiness === "ready"));
  expect(state()!.adapters.find((x) => x.id === a.id)).toMatchObject({ name: "Desk \u{10400}", platform: "mac" });
  a.failures["adapter.name.set"] = ["storage_full"];
  expect(await c.act({ type: "adapter.name", adapterId: a.id, name: null })).toMatchObject({ ok: false });
  expect(a.name).toBe("Desk \u{10400}");
  expect(await c.act({ type: "adapter.name", adapterId: a.id, name: null })).toEqual({ ok: true });
  await until(() => state()!.adapters.find((x) => x.id === a.id)?.name === "Pico W");
  expect(a.platform).toBe("mac");
  expect(a.received.some((r) => r.cmd === "adapter.name.set" && (r.args as { name: string | null }).name === null)).toBe(true);
  await c.stop();
});
