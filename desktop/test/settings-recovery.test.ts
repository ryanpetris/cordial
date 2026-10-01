import { describe, expect, it, vi } from "vitest";
import { AdapterSession } from "../src/core/session.ts";
import { AdapterView } from "../src/core/view.ts";
import { device, FakeAdapter, info, setting } from "../src/fake/adapter.ts";
import type { Event } from "../src/protocol/types.ts";
import { responseProblem } from "../src/protocol/validate.ts";
import { batteryOf } from "../src/core/controller.ts";
import { batteryLevel } from "../src/shared/battery.ts";
import { batteryStale } from "../src/shared/text.ts";
import { controller, openSession, until } from "./helpers.ts";

type Message = Record<string, any>;

async function intercepted(fake: FakeAdapter, intercept: (message: Message, deliver: (message: Message) => void) => void) {
  const transport = fake.open();
  const session = await AdapterSession.open({
    onClose: (listener) => transport.onClose(listener),
    write: (line) => transport.write(line),
    close: () => transport.close(),
    onData(listener) {
      const deliver = (m: Message) => listener(new TextEncoder().encode(`${JSON.stringify(m)}\n`));
      transport.onData((bytes) => {
        for (const line of new TextDecoder().decode(bytes).split("\n").filter(Boolean)) {
          try { intercept(JSON.parse(line), deliver); } catch { listener(bytes); }
        }
      });
    },
  }, { changed: vi.fn(), closed: vi.fn(), log: vi.fn() });
  return session;
}

describe("settings read recovery", () => {
  it("retries unavailable lists after device changes and page reopening", async () => {
    const { fake, session } = await openSession();
    await until(() => session.view.valid && session.view.infoNeeded().length === 0);
    fake.failures["hidpp.setting.list"] = ["settings_unavailable"];
    session.watchSettings(["d_1"]);
    await until(() => !!session.view.settings("d_1")?.loadError);
    fake.changeDevice("d_1", { settings_state: "ready" });
    await until(() => !!session.view.settings("d_1")?.current);
    fake.failures["hidpp.setting.list"] = ["settings_unavailable"];
    session.reloadSettings("d_1");
    await until(() => !!session.view.settings("d_1")?.loadError);
    session.watchSettings([]);
    session.watchSettings(["d_1"]);
    await until(() => !!session.view.settings("d_1")?.current);
    expect(fake.received.filter((m) => m.cmd === "hidpp.setting.list")).toHaveLength(4);
    session.watchSettings([]);
    session.watchSettings(["d_1"]);
    expect(session.view.settings("d_1")?.current).toBe(true);
    expect(fake.received.filter((m) => m.cmd === "hidpp.setting.list")).toHaveLength(4);
    await session.close();
  });

  it("keeps updates racing a first manual information snapshot", async () => {
    const fake = new FakeAdapter();
    let held: (() => void) | undefined;
    const session = await intercepted(fake, (m, send) => {
      const request = fake.received.find((r) => r.id === m.id);
      if (request?.cmd === "device.info.refresh") held = () => send(m);
      else send(m);
    });
    session.view.installSnapshot(fake.devices.map((d) => d.device), fake.revision);
    await session.request("session.monitor.set", { enabled: true });
    const refreshing = session.refreshInfo("d_2");
    await until(() => !!held);
    fake.changeInfo("d_2", { battery_percent: 4 });
    held!();
    await refreshing;
    expect(session.view.info("d_2")!.find((f) => f.key === "battery_percent")?.value).toBe(4);
    expect(session.view.infoCurrent("d_2")).toBe(true);
    await session.close();
  });

  it("does not mark a refresh from a lost epoch current", () => {
    const view = new AdapterView();
    view.installSnapshot([device("d_1")], 0);
    const epoch = view.beginInfo("d_1");
    view.lose();
    view.installInfo({ device_id: "d_1", revision: 0, fields: info({ battery_percent: 4 }) }, epoch);
    expect(view.infoCurrent("d_1")).toBe(false);
    expect(view.infoNeeded()).toEqual([]);
    view.installSnapshot([device("d_1")], 0);
    expect(view.infoNeeded()).toEqual(["d_1"]);
  });

  it("keeps a previously complete information cache stale during a failed replacement", () => {
    const view = new AdapterView();
    const d = device("d_1");
    view.installSnapshot([d], 0);
    view.installInfo({ device_id: "d_1", revision: 0, fields: info({ battery_percent: 4 }) }, view.beginInfo("d_1"));
    expect(view.infoCurrent("d_1")).toBe(true);
    view.lose();
    view.installSnapshot([d], 0);
    const epoch = view.beginInfo("d_1");
    expect(view.infoCurrent("d_1")).toBe(false);
    view.infoFailed("d_1", epoch, "no response");
    expect(view.infoCurrent("d_1")).toBe(false);
    expect(batteryLevel(batteryOf(view.info("d_1"), view.infoCurrent("d_1")), 20)).toBeNull();
    view.installInfo({ device_id: "d_1", revision: 0, fields: info({ battery_percent: 4 }) }, epoch - 1);
    expect(view.infoError("d_1")).toBe("no response");
    expect(view.infoCurrent("d_1")).toBe(false);
  });

  it("continues other reads when one reply never arrives, with a healthy heartbeat", async () => {
    const fake = new FakeAdapter();
    const session = await intercepted(fake, (m, send) => {
      const request = fake.received.find((r) => r.id === m.id);
      if (request?.cmd === "device.info" && (request.args as Message).device_id === "d_1") return;
      send(m);
    });
    session.watchSettings(["d_2"]);
    session.run();
    await until(() => !!session.view.settings("d_2")?.current, 7000);
    expect(session.closed).toBe(false);
    expect(session.view.infoError("d_1")).toContain("no response");
    expect(session.view.info("d_2")).not.toBeNull();
    expect(fake.received.filter((r) => r.cmd === "session.heartbeat").length).toBeGreaterThan(1);
    await session.close();
  });

  it("bounds a missing snapshot wait and recovers from a later snapshot", async () => {
    const fake = new FakeAdapter();
    let dropped = false;
    const session = await intercepted(fake, (m, send) => {
      const request = fake.received.find((r) => r.id === m.id);
      if (request?.cmd === "device.list" && !dropped && m.done) { dropped = true; return; }
      send(m);
    });
    session.run();
    await until(() => session.view.valid, 8000);
    expect(fake.received.filter((r) => r.cmd === "device.list")).toHaveLength(2);
    expect(session.closed).toBe(false);
    await session.close();
  });

  it("does not treat an evicted snapshot event as covered", () => {
    const view = new AdapterView();
    view.installSnapshot([device("d_1")], 0);
    view.beginSnapshot();
    for (let revision = 1; revision <= 300; revision++)
      view.event({ v: 1, type: "event", event: "device.changed", data: { revision, device: device("d_1") } } as Event);
    view.installSnapshot([device("d_1")], 0);
    expect(view.valid).toBe(false);
    view.beginSnapshot();
    view.installSnapshot([device("d_1")], 300);
    expect(view.valid).toBe(true);
  });

  it("keeps reset observations stale when an older settings list finishes", () => {
    const view = new AdapterView();
    const d = device("d_1", { state: "connected", normalization_state: "active", settings_state: "ready" });
    view.installSnapshot([d], 0);
    const epoch = view.beginSettings("d_1");
    const s = setting("backlight.enabled", { managed: true, desired: true, observed: true, state: "applied" });
    view.installSettings("d_1", 0, [s], "ready", null, epoch);
    view.event({ v: 1, type: "event", event: "device.changed", data: { revision: 1, device: { ...d, normalization_state: "resetting" } } });
    view.installSettings("d_1", 0, [s], "ready", null, epoch);
    expect(view.settings("d_1")).toMatchObject({ current: false, settings: [{ fresh: false, state: "pending" }] });
  });
});

describe("settings actions", () => {
  it("permits saving and reading with HID++ off while Apply remains gated", async () => {
    const fake = new FakeAdapter();
    const { c, state } = controller({ "/fake": fake });
    try {
      await c.manager.rescan();
      await until(() => state()?.devices.every((d) => d.info) === true);
      const key = `${fake.id}/d_1`;
      fake.changeDevice("d_1", { hidpp_enabled: false, normalization_state: "off" });
      expect(await c.act({ type: "setting.set", key, setting: "backlight.enabled", value: false })).toEqual({ ok: true });
      expect(await c.act({ type: "settings.refresh", key })).toEqual({ ok: true });
      expect(await c.act({ type: "settings.apply", key })).toMatchObject({ ok: false, message: expect.stringContaining("Logitech features are off") });
    } finally { await c.stop(); }
  });
  it("merges streamed rows and retains success and partial failure counts", async () => {
    const fake = new FakeAdapter();
    const { c, state } = controller({ "/fake": fake });
    await c.manager.rescan();
    await until(() => state()?.devices.every((d) => d.info) === true);
    const key = `${fake.id}/d_1`;
    await c.act({ type: "settings.watch", key });
    await until(() => !!state()?.devices.find((d) => d.key === key)?.settings?.current);
    fake.find("d_1").settings.find((s) => s.key === "backlight.level")!.observed = 6;
    expect(await c.act({ type: "settings.refresh", key })).toEqual({ ok: true });
    let entry = c.state().devices.find((d) => d.key === key)!;
    expect(entry.settings?.settings.find((s) => s.key === "backlight.level")?.observed).toBe(6);
    expect(entry.settings?.result).toMatchObject({ kind: "refresh", counts: { count: 6, read: 6 }, error: null });
    await c.stop();

    const partial = new FakeAdapter();
    const deps = controller({ "/fake": partial });
    const onData = partial.onData.bind(partial);
    partial.onData = (listener) => {
      onData((bytes) => {
        const line = new TextDecoder().decode(bytes).trim();
        let m: Message;
        try { m = JSON.parse(line); } catch { listener(bytes); return; }
        const request = partial.received.find((r) => r.id === m.id);
        if (request?.cmd === "hidpp.setting.apply" && m.done)
          m = { ...m, ok: false, result: undefined, error: { code: "settings_apply_failed", details: { ...m.result, failed: 1, unchanged: 5 } } };
        listener(new TextEncoder().encode(`${JSON.stringify(m)}\n`));
      });
    };
    await deps.c.manager.rescan();
    await until(() => deps.state()?.devices.every((d) => d.info) === true);
    await deps.c.act({ type: "settings.watch", key: `${partial.id}/d_1` });
    await until(() => !!deps.c.state().devices[0]?.settings?.current);
    const result = await deps.c.act({ type: "settings.apply", key: `${partial.id}/d_1` });
    expect(result).toMatchObject({ ok: false, inline: true });
    expect(deps.c.state().devices[0]?.settings?.result).toMatchObject({ kind: "apply", counts: { failed: 1, unchanged: 5 }, error: expect.any(String) });
    await deps.c.stop();
  });

  it("shares device busy state, allows forgetting offline, and guards platform storage", async () => {
    const fake = new FakeAdapter();
    const { c, state } = controller({ "/fake": fake });
    await c.manager.rescan();
    await until(() => state()?.devices.every((d) => d.info) === true);
    const key = `${fake.id}/d_1`;
    fake.changeDevice("d_1", { normalization_state: "resetting" });
    for (const action of [
      { type: "setting.set", key, setting: "backlight.enabled", value: false },
      { type: "setting.forget", key, setting: "backlight.enabled" },
      { type: "settings.refresh", key }, { type: "settings.apply", key },
      { type: "device.hidpp", key, value: false },
    ] as const) expect(await c.act(action)).toMatchObject({ ok: false });
    fake.changeDevice("d_1", { normalization_state: "active" });
    expect(await c.act({ type: "device.hidpp", key, value: false })).toEqual({ ok: true });
    fake.changeDevice("d_1", { state: "disconnected", normalization_state: "pending", settings_state: "pending" });
    expect(await c.act({ type: "setting.forget", key, setting: "backlight.enabled" })).toEqual({ ok: true });
    expect(await c.act({ type: "setting.set", key, setting: "backlight.enabled", value: false })).toMatchObject({ ok: false });
    c.manager.connected.get(fake.id)!.session.status.storage_ready = false;
    const count = fake.received.length;
    expect(await c.act({ type: "adapter.platform", adapterId: fake.id, platform: "mac" })).toMatchObject({ ok: false });
    expect(fake.received).toHaveLength(count);
    await c.stop();
  });
});

describe("protocol and battery parity", () => {
  it("enforces ordinary limits while keeping eight reserved request slots", async () => {
    const fake = new FakeAdapter();
    let silent = false;
    const session = await intercepted(fake, (m, send) => { if (!silent) send(m); });
    session.status.limits.max_pending_requests = 4;
    silent = true;
    const waiting = [];
    for (let i = 0; i < 4; i++) waiting.push(session.request("device.info", { device_id: "d_1" }).catch(() => {}));
    await expect(session.request("device.info", { device_id: "d_2" })).rejects.toThrow("Too many pending");
    for (let i = 0; i < 8; i++) waiting.push(session.request("adapter.status", {}).catch(() => {}));
    await expect(session.request("session.heartbeat", {})).rejects.toThrow("Too many pending");
    expect(session.pendingFor("d_1")).toHaveLength(4);
    await session.close();
    await Promise.all(waiting);
  });

  it("fails the session when pending requests prevent a heartbeat", async () => {
    let beat: (() => void) | undefined;
    const interval = vi.spyOn(globalThis, "setInterval").mockImplementationOnce((callback) => {
      beat = callback as () => void;
      return setTimeout(() => {}, 60_000);
    });
    const fake = new FakeAdapter();
    let silent = false;
    const session = await intercepted(fake, (m, send) => { if (!silent) send(m); });
    interval.mockRestore();
    try {
      session.status.limits.max_pending_requests = 4;
      silent = true;
      for (let i = 0; i < 12; i++) void session.request("adapter.status", {}).catch(() => {});
      beat!();
      await until(() => session.closed, 1000);
    } finally { await session.close(); }
  });

  it("recovers when all ordinary request slots belong to abandoned reads", async () => {
    const fake = new FakeAdapter();
    let silent = false;
    const session = await intercepted(fake, (m, send) => { if (!silent) send(m); });
    session.status.limits.max_pending_requests = 4;
    silent = true;
    const reads = Array.from({ length: 4 }, () => session.request("device.info", { device_id: "d_1" }, { waitMs: 10 }).catch(() => {}));
    await Promise.all(reads);
    expect(session.closed).toBe(true);
  });

  it("rejects lists beyond negotiated row and choice bounds", async () => {
    const { session } = await openSession();
    await until(() => session.view.valid && !session.view.infoNeeded().length);
    const row = setting("wheel.mode", { type: "enum", choices: ["ratchet", "freespin"] });
    const envelope = { v: 1, type: "response", id: 1, done: false, ok: true, result: { device_id: "d_1", revision: 1, setting: row } };
    expect(responseProblem("hidpp.setting.list", envelope, { ...session.status.limits, hidpp_setting_choices: 1 })).not.toBeNull();
    session.status.limits.hidpp_settings = 1;
    await expect(session.request("hidpp.setting.list", { device_id: "d_1" })).rejects.toThrow("oversized");
    expect(session.closed).toBe(true);
  });

  it("rejects malformed records beyond their JSON shape", () => {
    const s = setting("backlight.level", { type: "integer", observed: 3, min: 0, max: 7, step: 1 });
    const reply = (row: unknown) => ({ v: 1, type: "response", id: 1, done: false, ok: true, result: { device_id: "d_1", revision: 1, setting: row } });
    expect(responseProblem("hidpp.setting.list", reply(s))).toBeNull();
    for (const patch of [{ type: "bool", observed: true }, { min: 8 }, { step: 0 }, { managed: true, desired: null }, { choices: [1, 1] }])
      expect(responseProblem("hidpp.setting.list", reply({ ...s, ...patch }))).not.toBeNull();
    const fields = info({ name: "é".repeat(33) });
    const full = { v: 1, type: "response", id: 1, done: true, ok: true, result: { device_id: "d_1", revision: 1, fields } };
    expect(responseProblem("device.info", full)).not.toBeNull();
    full.result.fields = [fields[0]!, fields[0]!];
    expect(responseProblem("device.info", full)).not.toBeNull();
  });

  it("retains stale battery values without generating low-battery decisions", () => {
    const b = batteryOf(info({ battery_percent: 4, battery_charging: false }, false));
    expect(b).toEqual({ percent: 4, charging: false, percentFresh: false, chargingFresh: false });
    expect(batteryLevel(b, 20)).toBeNull();
    expect(batteryLevel(batteryOf(info({ battery_percent: 4 }), false), 20)).toBeNull();
    expect(batteryLevel(batteryOf(info({ battery_percent: 4 })), 20)).toBe("critical");
    expect(batteryStale({ percent: 72, charging: true, percentFresh: true, chargingFresh: false })).toBe(true);
  });
});
