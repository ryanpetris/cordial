import { describe, expect, it } from "vitest";
import { FakeAdapter } from "../src/fake/adapter.ts";
import { controller, until } from "./helpers.ts";

const loaded = (s: ReturnType<ReturnType<typeof controller>["state"]>, devices: number) =>
  !!s && s.devices.length === devices && s.devices.every((d) => d.info);

describe("Controller", () => {
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
    expect(mouse.battery).toEqual({ percent: 12, charging: false });
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
